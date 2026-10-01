//! Bounded DNS resolution and IPv4/IPv6 connection racing for IMAP probes.

#[cfg(not(test))]
use std::process::{Command, Stdio};
#[cfg(not(test))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::mpsc,
    time::{Duration, Instant},
};

/// Bound resolver subprocesses even if many UI or verification requests start
/// at once. Unlike an in-process `getaddrinfo` call, each child can be killed
/// when its caller's deadline expires, so a broken resolver cannot consume
/// this capacity permanently.
#[cfg(not(test))]
const MAX_OUTSTANDING_DNS_LOOKUPS: usize = 16;
pub(super) const MAX_DNS_ADDRESSES: usize = 64;
/// RFC 8305 connection attempt delay between address attempts.
const CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(250);
/// Addresses raced per connection; enough for both families on real hosts.
const MAX_CONNECTION_ATTEMPTS: usize = 8;

#[cfg(not(test))]
static OUTSTANDING_DNS_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

pub(super) type DnsResult = std::io::Result<Vec<SocketAddr>>;

/// Releases one outstanding-lookup slot when its resolver child exits.
#[cfg(not(test))]
struct OutstandingLookup(&'static AtomicUsize);

#[cfg(not(test))]
impl Drop for OutstandingLookup {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Do not leave a resolver child behind if waiting or reading its bounded
/// response fails unexpectedly.
#[cfg(not(test))]
struct ResolverChild(std::process::Child);

#[cfg(not(test))]
impl Drop for ResolverChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(super) fn resolve_dns_with_deadline(
    address: &str,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> DnsResult {
    // Unit-test binaries do not dispatch through the application CLI. Keep
    // protocol tests independent of a recursively spawned test harness; the
    // production resolver subprocess is exercised through its CLI contract.
    #[cfg(test)]
    {
        let _ = (deadline, cancelled);
        resolve_address(address)
    }

    #[cfg(not(test))]
    {
        OUTSTANDING_DNS_LOOKUPS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_OUTSTANDING_DNS_LOOKUPS).then_some(current + 1)
            })
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "too many system DNS lookups are in progress",
                )
            })?;
        let _slot = OutstandingLookup(&OUTSTANDING_DNS_LOOKUPS);
        if cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "DNS resolution cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "DNS resolution timed out",
            ));
        }

        let executable = std::env::current_exe()?;
        let mut command = Command::new(executable);
        crate::process::apply_dns_environment(&mut command);
        command
            .arg("--internal-dns-resolve")
            .arg(address)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // On Unix, a Linux parent-death signal also prevents an abandoned helper
        // if the controller itself crashes. Windows needs no breakaway Job Object
        // for this single child: Child::kill directly terminates the resolver.
        #[cfg(unix)]
        crate::process::configure_process_group(&mut command);
        let mut child = ResolverChild(command.spawn()?);
        let output = wait_for_child_output(&mut child.0, deadline, cancelled)?;
        parse_dns_output(&output)
    }
}

fn wait_for_child_output(
    child: &mut std::process::Child,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> std::io::Result<Vec<u8>> {
    loop {
        if cancelled() || Instant::now() >= deadline {
            // The helper performs only one system resolver call and emits at
            // most MAX_DNS_ADDRESSES lines. Killing it bounds both the stuck
            // resolver and the output pipe without blocking this caller.
            let _ = child.kill();
            let _ = child.wait();
            if cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "DNS resolution cancelled",
                ));
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "DNS resolution timed out",
            ));
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(std::io::Error::other("system DNS lookup failed"));
            }
            let mut output = Vec::new();
            child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("DNS resolver output was unavailable"))?
                .take((MAX_DNS_ADDRESSES * 64 + 1) as u64)
                .read_to_end(&mut output)?;
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn parse_dns_output(output: &[u8]) -> DnsResult {
    if output.len() > MAX_DNS_ADDRESSES * 64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DNS resolver output exceeded its safety limit",
        ));
    }
    let text = std::str::from_utf8(output)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let mut addresses = Vec::new();
    for line in text.lines() {
        if addresses.len() == MAX_DNS_ADDRESSES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "DNS response exceeded the address limit",
            ));
        }
        addresses.push(
            line.parse::<SocketAddr>()
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
        );
    }
    Ok(addresses)
}

fn resolve_address(address: &str) -> DnsResult {
    address.to_socket_addrs().and_then(collect_dns_addresses)
}

/// Hidden subcommand used only by the bounded parent resolver process. It
/// deliberately emits a small, line-oriented address list, not resolver or
/// environment diagnostics that could leak into normal application output.
pub(super) fn internal_dns_resolver_main(arguments: &[std::ffi::OsString]) -> i32 {
    if arguments.len() != 1 {
        return 2;
    }
    let Some(address) = arguments[0].to_str() else {
        return 2;
    };
    let result = resolve_address(address);
    let Ok(addresses) = result else {
        return 1;
    };
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for address in addresses {
        if writeln!(output, "{address}").is_err() {
            return 1;
        }
    }
    0
}

pub(super) fn collect_dns_addresses<I>(mut addresses: I) -> std::io::Result<Vec<SocketAddr>>
where
    I: Iterator<Item = SocketAddr>,
{
    let mut collected = Vec::with_capacity(MAX_DNS_ADDRESSES);
    for _ in 0..MAX_DNS_ADDRESSES {
        let Some(address) = addresses.next() else {
            return Ok(collected);
        };
        collected.push(address);
    }
    if addresses.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("DNS response exceeded the {MAX_DNS_ADDRESSES}-address limit"),
        ));
    }
    Ok(collected)
}

/// RFC 8305 ordering: alternate address families, starting with the family
/// the resolver listed first, so one unreachable family cannot hold every
/// early attempt.
pub(super) fn interleave_address_families(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let first_is_v6 = addresses.first().is_some_and(SocketAddr::is_ipv6);
    let (mut preferred, mut other): (Vec<_>, Vec<_>) = addresses
        .into_iter()
        .partition(|address| address.is_ipv6() == first_is_v6);
    preferred.reverse();
    other.reverse();
    let mut ordered = Vec::with_capacity(preferred.len() + other.len());
    while let Some(address) = preferred.pop() {
        ordered.push(address);
        if let Some(address) = other.pop() {
            ordered.push(address);
        }
    }
    ordered.extend(other.into_iter().rev());
    ordered
}

pub(super) fn connect_racing(
    addresses: Vec<SocketAddr>,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<TcpStream, String> {
    let candidates = interleave_address_families(addresses)
        .into_iter()
        .take(MAX_CONNECTION_ATTEMPTS)
        .collect::<Vec<_>>();
    let (sender, receiver) = mpsc::channel::<std::io::Result<TcpStream>>();
    let mut started = 0_usize;
    let mut failed = 0_usize;
    let mut last_error = None;
    let mut next_start = Instant::now();
    loop {
        if cancelled() {
            return Err("connection cancelled".into());
        }
        let now = Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            break;
        }
        if started < candidates.len() && (now >= next_start || failed == started) {
            let address = candidates[started];
            let sender = sender.clone();
            // A losing attempt that connects late drops its stream when the
            // send fails; each attempt uses the shared overall deadline.
            std::thread::Builder::new()
                .name("mailswiftsync-connect".into())
                .spawn(move || {
                    let _ = sender.send(TcpStream::connect_timeout(&address, remaining));
                })
                .map_err(|error| format!("could not start connection attempt: {error}"))?;
            started += 1;
            next_start = now + CONNECTION_ATTEMPT_DELAY;
        }
        let mut wait = remaining.min(Duration::from_millis(50));
        if started < candidates.len() {
            wait = wait.min(next_start.saturating_duration_since(now));
        }
        match receiver.recv_timeout(wait.max(Duration::from_millis(1))) {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => {
                failed += 1;
                last_error = Some(error);
                if failed == candidates.len() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Err(format!(
        "could not connect to any resolved address: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "connection deadline exceeded".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::process::{Command, Stdio};

    #[test]
    fn dns_lookup_resolves_literals() {
        let addresses = resolve_address("127.0.0.1:993").unwrap();
        assert!(addresses.iter().any(|address| address.ip().is_loopback()));
    }

    #[cfg(unix)]
    #[test]
    fn dns_deadline_terminates_and_reaps_the_resolver_child() {
        let mut child = Command::new("/bin/sleep")
            .arg("10")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let result =
            wait_for_child_output(&mut child, started + Duration::from_millis(50), &|| false);
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn dns_cancellation_terminates_and_reaps_the_resolver_child() {
        let mut child = Command::new("/bin/sleep")
            .arg("10")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let cancel_at = Instant::now() + Duration::from_millis(50);
        let result =
            wait_for_child_output(&mut child, Instant::now() + Duration::from_secs(2), &|| {
                Instant::now() >= cancel_at
            });
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn dns_output_parser_rejects_malformed_and_unbounded_output() {
        assert!(parse_dns_output(b"not-an-address\n").is_err());
        assert!(parse_dns_output(&vec![b'x'; MAX_DNS_ADDRESSES * 64 + 1]).is_err());
    }

    #[test]
    fn address_families_are_interleaved_starting_with_the_first() {
        let v6 = |port| SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port));
        let v4 = |port| SocketAddr::from(([127, 0, 0, 1], port));
        assert_eq!(
            interleave_address_families(vec![v6(1), v6(2), v6(3), v4(4)]),
            vec![v6(1), v4(4), v6(2), v6(3)]
        );
        assert_eq!(
            interleave_address_families(vec![v4(1), v6(2), v4(3), v6(4)]),
            vec![v4(1), v6(2), v4(3), v6(4)]
        );
    }

    /// A blackholed first address must not consume the whole budget. The
    /// first candidate is a non-routable TEST-NET address that never answers;
    /// the second is a listening loopback socket.
    #[test]
    fn blackholed_first_address_does_not_starve_a_healthy_one() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let healthy = listener.local_addr().unwrap();
        let blackholed = SocketAddr::from(([192, 0, 2, 1], healthy.port()));
        let started = Instant::now();
        let stream = connect_racing(
            vec![blackholed, healthy],
            Instant::now() + Duration::from_secs(8),
            &|| false,
        )
        .expect("the healthy address must win");
        assert_eq!(stream.peer_addr().unwrap(), healthy);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn dns_address_collection_rejects_excessive_responses() {
        let addresses =
            (0..=MAX_DNS_ADDRESSES).map(|port| SocketAddr::from(([127, 0, 0, 1], port as u16)));
        let error = collect_dns_addresses(addresses).unwrap_err();
        assert!(error.to_string().contains("address limit"));
    }
}
