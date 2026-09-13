use crate::imap_protocol::{
    advertises_capability, atom_eq, is_tagged_response, is_untagged_response,
};
use crate::oauth::{read_auth_continuation, read_auth_result};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pemfile::certs;
use sha2::{Digest, Sha256};
use std::{
    io::{BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};
use zeroize::Zeroizing;

pub(crate) fn endpoint_for_probe(host: &str, configured_port: &str) -> Result<String, String> {
    let host = host.trim();
    if configured_port.trim().is_empty() {
        return Ok(host.to_owned());
    }
    let port = configured_port
        .trim()
        .parse::<u16>()
        .map_err(|_| "invalid endpoint port".to_owned())?;
    if port == 0 {
        return Err("endpoint port must be between 1 and 65535".into());
    }
    let (host, _) = crate::endpoint::parts(host, 993)?;
    if host.contains(':') {
        Ok(format!("[{host}]:{port}"))
    } else {
        Ok(format!("{host}:{port}"))
    }
}

/// Command previews and fingerprints must not silently turn malformed input
/// into a different endpoint. Validation rejects the sentinel before launch;
/// this helper keeps tuple-based command-generation APIs fail-closed too.
pub(crate) fn command_endpoint_parts(host: &str, default_port: u16) -> (String, u16) {
    crate::endpoint::parts(host, default_port)
        .unwrap_or_else(|_| ("<invalid-endpoint>".to_owned(), 0))
}

pub(crate) fn command_port(configured_port: &str, endpoint_port: u16) -> u16 {
    match configured_port.trim().parse::<u16>() {
        Ok(port) if port != 0 => port,
        _ if configured_port.trim().is_empty() => endpoint_port,
        _ => 0,
    }
}

pub(crate) fn imap_quote(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("IMAP quoted value cannot contain control characters".into());
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn read_imap_tagged<S: Read>(
    stream: &mut S,
    tag: &str,
    response: &mut String,
    buffer: &mut [u8; 4096],
) -> Result<(), String> {
    loop {
        let count = stream.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err(format!("IMAP connection closed before {tag} completed"));
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.lines().any(|line| is_tagged_response(line, tag)) {
            return Ok(());
        }
        if response.len() > 1_048_576 {
            return Err("IMAP preflight response exceeded 1 MiB".into());
        }
    }
}

fn authenticated_list_command(request_special_use: bool) -> &'static [u8] {
    if request_special_use {
        b"a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
    } else {
        b"a005 LIST \"\" \"*\"\r\n"
    }
}

fn read_imap_greeting<S: Read>(stream: &mut S, host: &str) -> Result<String, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if response.contains("\r\n") || response.len() > 65_536 {
            break;
        }
    }
    if !is_untagged_response(&response, "OK") && !is_untagged_response(&response, "PREAUTH") {
        return Err(format!("{host}: server greeting was missing or invalid"));
    }
    Ok(response)
}

pub(crate) fn imap_command_succeeded(response: &str, tag: &str) -> bool {
    response.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some(tag) && fields.next().is_some_and(|status| atom_eq(status, "OK"))
    })
}

pub(crate) fn probe_tls_capabilities_with_transport(
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    transport: &str,
    ca_bundle: &str,
    certificate_pin_sha256: &str,
) -> Result<crate::core::ServerCapabilities, String> {
    let (server_name, port) = crate::endpoint::parts(host, crate::default_imap_port(transport))
        .map_err(|error| format!("Invalid IMAP host {host}: {error}"))?;
    let address = if server_name.contains(':') {
        format!("[{server_name}]:{port}")
    } else {
        format!("{server_name}:{port}")
    };
    let sockets = address
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .collect::<Vec<_>>();
    if sockets.is_empty() {
        return Err(format!("{host}: no address found"));
    }
    let mut last_error = None;
    let mut tcp = None;
    for socket in sockets {
        match TcpStream::connect_timeout(&socket, Duration::from_secs(8)) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut tcp = tcp.ok_or_else(|| {
        format!(
            "{host}: could not connect to any resolved address: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown connection error".into())
        )
    })?;
    tcp.set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|e| e.to_string())?;
    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !ca_bundle.trim().is_empty() {
        let file = std::fs::File::open(ca_bundle.trim())
            .map_err(|error| format!("{host}: could not open additional CA bundle: {error}"))?;
        let mut reader = BufReader::new(file);
        let mut loaded = 0;
        for certificate in certs(&mut reader) {
            let certificate = certificate
                .map_err(|error| format!("{host}: invalid certificate in CA bundle: {error}"))?;
            roots
                .add(certificate)
                .map_err(|error| format!("{host}: could not add CA certificate: {error}"))?;
            loaded += 1;
        }
        if loaded == 0 {
            return Err(format!("{host}: CA bundle contained no PEM certificates"));
        }
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|e| format!("{host}: invalid TLS server name: {e}"))?;

    if transport == "starttls" {
        let mut response = String::new();
        let mut buffer = [0; 4096];
        let greeting = read_imap_greeting(&mut tcp, host)?;
        tcp.write_all(b"s001 CAPABILITY\r\n")
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut tcp, "s001", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "s001")
            || !advertises_capability(&response, "STARTTLS")
        {
            return Err(format!("{host}: server does not advertise STARTTLS"));
        }
        tcp.write_all(b"s002 STARTTLS\r\n")
            .map_err(|e| e.to_string())?;
        read_imap_tagged(&mut tcp, "s002", &mut response, &mut buffer)?;
        if !imap_command_succeeded(&response, "s002") {
            return Err(format!("{host}: STARTTLS negotiation failed"));
        }
        let connection = ClientConnection::new(Arc::new(config), name)
            .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
        let mut stream = StreamOwned::new(connection, tcp);
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|error| format!("{host}: TLS handshake failed: {error}"))?;
        verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
        return complete_authenticated_imap_probe(
            stream,
            host,
            user,
            credential,
            auth_method,
            greeting,
        );
    }

    let connection = ClientConnection::new(Arc::new(config), name)
        .map_err(|e| format!("{host}: TLS configuration failed: {e}"))?;
    let mut stream = StreamOwned::new(connection, tcp);
    let greeting = read_imap_greeting(&mut stream, host)?;
    verify_certificate_pin(&stream, host, certificate_pin_sha256)?;
    complete_authenticated_imap_probe(stream, host, user, credential, auth_method, greeting)
}

fn verify_certificate_pin(
    stream: &StreamOwned<ClientConnection, TcpStream>,
    host: &str,
    expected: &str,
) -> Result<(), String> {
    if expected.trim().is_empty() {
        return Ok(());
    }
    let certificate = stream
        .conn
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .ok_or_else(|| format!("{host}: TLS peer did not provide a certificate"))?;
    let actual = format!("{:x}", Sha256::digest(certificate.as_ref()));
    if actual != expected.trim().to_ascii_lowercase() {
        return Err(format!(
            "{host}: TLS certificate SHA-256 pin mismatch (presented {actual})"
        ));
    }
    Ok(())
}

fn complete_authenticated_imap_probe<S: Read + Write>(
    mut stream: S,
    host: &str,
    user: &str,
    credential: &str,
    auth_method: &str,
    greeting: String,
) -> Result<crate::core::ServerCapabilities, String> {
    let mut response = String::new();
    let mut buffer = [0; 4096];
    stream
        .write_all(b"a001 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    read_imap_tagged(&mut stream, "a001", &mut response, &mut buffer)?;
    if !imap_command_succeeded(&response, "a001") {
        return Err(format!("{host}: pre-auth CAPABILITY failed"));
    }
    let preauth = greeting
        .lines()
        .any(|line| is_untagged_response(line, "PREAUTH"));
    if !preauth {
        if auth_method == "oauth2" {
            let encoded = crate::oauth::xoauth2_payload(user, credential);
            stream
                .write_all(b"a002 AUTHENTICATE XOAUTH2\r\n")
                .map_err(|e| e.to_string())?;
            response.clear();
            read_auth_continuation(&mut stream, "a002", &mut response, &mut buffer)?;
            stream
                .write_all(encoded.as_bytes())
                .and_then(|_| stream.write_all(b"\r\n"))
                .map_err(|e| e.to_string())?;
            response.clear();
            read_auth_result(&mut stream, "a002", &mut response, &mut buffer)?;
        } else {
            let quoted_password = Zeroizing::new(imap_quote(credential)?);
            let login = format!(
                "a002 LOGIN {} {}\r\n",
                imap_quote(user)?,
                quoted_password.as_str()
            );
            let login = Zeroizing::new(login);
            stream
                .write_all(login.as_bytes())
                .map_err(|e| e.to_string())?;
            read_imap_tagged(&mut stream, "a002", &mut response, &mut buffer)?;
        }
        if !imap_command_succeeded(&response, "a002") {
            return Err(format!("{host}: IMAP authentication failed"));
        }
    }
    // RFC 9051 permits capabilities to change after authentication, so the
    // post-auth response is the one used for readiness decisions.
    stream
        .write_all(b"a003 CAPABILITY\r\n")
        .map_err(|e| e.to_string())?;
    let mut post_auth_response = String::new();
    read_imap_tagged(&mut stream, "a003", &mut post_auth_response, &mut buffer)?;
    if !imap_command_succeeded(&post_auth_response, "a003") {
        return Err(format!("{host}: post-auth CAPABILITY failed"));
    }
    stream
        .write_all(b"a004 NAMESPACE\r\n")
        .map_err(|e| e.to_string())?;
    let mut _namespace_response = String::new();
    read_imap_tagged(&mut stream, "a004", &mut _namespace_response, &mut buffer)?;
    // NAMESPACE is useful for mapping, but not required by IMAP or by every
    // usable migration endpoint. LIST remains the authoritative inventory
    // gate; callers may surface this response as a compatibility warning.
    stream
        .write_all(authenticated_list_command(advertises_capability(
            &post_auth_response,
            "SPECIAL-USE",
        )))
        .map_err(|e| e.to_string())?;
    let mut list_response = String::new();
    read_imap_tagged(&mut stream, "a005", &mut list_response, &mut buffer)?;
    if !imap_command_succeeded(&list_response, "a005") {
        return Err(format!("{host}: folder inventory failed"));
    }
    if !list_response
        .lines()
        .any(|line| is_untagged_response(line, "LIST"))
    {
        return Err(format!(
            "{host}: folder inventory returned no untagged LIST records"
        ));
    }
    let caps =
        crate::core::ServerCapabilities::parse_with_inventory(&post_auth_response, &list_response);
    if caps.values.is_empty() {
        return Err(format!(
            "{host}: server did not return a CAPABILITY response"
        ));
    }
    let mut caps = caps;
    if caps.supports("QUOTA") {
        let mut quota_response = String::new();
        stream
            .write_all(b"a006 GETQUOTAROOT \"\"\r\n")
            .map_err(|e| e.to_string())?;
        // A provider can advertise QUOTA while denying the root query. Keep
        // that result explicitly unknown; quota support is advisory and must
        // not turn an otherwise valid mailbox into a false capacity failure.
        if read_imap_tagged(&mut stream, "a006", &mut quota_response, &mut buffer).is_ok() {
            caps.record_quota_response(&quota_response);
        }
    }
    let _ = stream.write_all(b"a007 LOGOUT\r\n");
    Ok(caps)
}

#[cfg(test)]
mod tests {
    use super::authenticated_list_command;

    #[test]
    fn list_request_asks_for_rfc6154_attributes_when_supported() {
        assert_eq!(
            authenticated_list_command(true),
            b"a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
        );
        assert_eq!(
            authenticated_list_command(false),
            b"a005 LIST \"\" \"*\"\r\n"
        );
    }
}
