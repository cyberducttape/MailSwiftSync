//! Native TLS for the fleet API, so a non-loopback listener does not depend
//! on a reverse proxy to keep bearer tokens and customer metadata off the
//! wire in plaintext.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::Path;
use std::sync::Arc;

/// Certificate chains larger than this are a configuration mistake (a
/// bundle of unrelated roots, the wrong file), not a server certificate.
const MAX_CERTIFICATE_FILE_BYTES: u64 = 256 * 1024;

/// Build the server configuration from a PEM certificate chain (leaf first)
/// and a PEM private key. The key file goes through the same owner-only
/// secret-file checks as the tokens.
pub(crate) fn server_config(
    certificate_path: &Path,
    key_path: &Path,
) -> Result<Arc<rustls::ServerConfig>, String> {
    let certificate_bytes = read_certificate_file(certificate_path)?;
    let chain = CertificateDer::pem_slice_iter(&certificate_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not parse the certificate PEM: {error}"))?;
    if chain.is_empty() {
        return Err("the certificate file holds no PEM certificate".into());
    }
    let key_text = crate::credentials::read_secret_file(key_path)
        .map_err(|error| format!("TLS key refused: {error}"))?;
    let key = PrivateKeyDer::from_pem_slice(key_text.as_str().as_bytes())
        .map_err(|error| format!("could not parse the private key PEM: {error}"))?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|error| format!("the certificate and key do not form a usable pair: {error}"))?;
    Ok(Arc::new(config))
}

fn read_certificate_file(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_CERTIFICATE_FILE_BYTES {
        return Err(format!(
            "{} must be a PEM certificate file of at most {} KiB",
            path.display(),
            MAX_CERTIFICATE_FILE_BYTES / 1024
        ));
    }
    std::fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))
}
