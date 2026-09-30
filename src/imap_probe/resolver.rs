//! Bounded DNS resolution and IPv4/IPv6 connection racing for IMAP probes.

use std::{
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Lookups may outlive their caller because the system resolver is not
/// cancellable. Bound outstanding work so broken DNS cannot grow threads
/// without limit.
pub(super) const MAX_OUTSTANDING_DNS_LOOKUPS: usize = 16;
pub(super) const MAX_DNS_ADDRESSES: usize = 64;
/// RFC 8305 connection attempt delay between address attempts.
const CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(250);
/// Addresses raced per connection; enough for both families on real hosts.
const MAX_CONNECTION_ATTEMPTS: usize = 8;

static OUTSTANDING_DNS_LOOKUPS: AtomicUsize = AtomicUsize::new(0);

pub(super) type DnsResult = std::io::Result<Vec<SocketAddr>>;

/// Releases one outstanding-lookup slot when its resolver thread ends.
struct OutstandingLookup(&'static AtomicUsize);

impl Drop for OutstandingLookup {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn start_dns_lookup(address: String) -> Result<mpsc::Receiver<DnsResult>, String> {
    start_lookup_with(
        &OUTSTANDING_DNS_LOOKUPS,
        MAX_OUTSTANDING_DNS_LOOKUPS,
        move || address.to_socket_addrs().and_then(collect_dns_addresses),
    )
}

pub(super) fn start_lookup_with<F>(
    outstanding: &'static AtomicUsize,
    limit: usize,
    resolve: F,
) -> Result<mpsc::Receiver<DnsResult>, String>
where
    F: FnOnce() -> DnsResult + Send + 'static,
{
    outstanding
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < limit).then_some(current + 1)
        })
        .map_err(|_| {
            format!(
                "the system DNS resolver is not responding: {limit} lookups are still pending; retry once they finish"
            )
        })?;
    let slot = OutstandingLookup(outstanding);
    let (sender, receiver) = mpsc::channel();
    // On spawn failure the closure, and with it the slot, is dropped.
    std::thread::Builder::new()
        .name("mailswiftsync-dns".into())
        .spawn(move || {
            let _slot = slot;
            let _ = sender.send(resolve());
        })
        .map_err(|error| format!("could not start DNS lookup: {error}"))?;
    Ok(receiver)
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

    #[test]
    fn dns_lookup_resolves_literals() {
        let receiver = start_dns_lookup("127.0.0.1:993".to_owned()).unwrap();
        let addresses = receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(addresses.iter().any(|address| address.ip().is_loopback()));
    }

    /// Hung resolver calls cannot be cancelled. They must not wedge later
    /// lookups forever: the cap fails fast with a clear error, and capacity
    /// returns when a stuck call finishes.
    #[test]
    fn hung_dns_lookups_are_bounded_and_release_capacity() {
        static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);
        let (release_first, first_gate) = mpsc::channel::<()>();
        let (release_second, second_gate) = mpsc::channel::<()>();
        let hung = |gate: mpsc::Receiver<()>| {
            move || {
                let _ = gate.recv();
                Ok(Vec::new())
            }
        };
        let first = start_lookup_with(&OUTSTANDING, 2, hung(first_gate)).unwrap();
        let _second = start_lookup_with(&OUTSTANDING, 2, hung(second_gate)).unwrap();
        // The callers time out, but the threads are still stuck.
        assert!(first.recv_timeout(Duration::from_millis(20)).is_err());
        let error = start_lookup_with(&OUTSTANDING, 2, || Ok(Vec::new())).unwrap_err();
        assert!(error.contains("not responding"), "{error}");
        release_first.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while OUTSTANDING.load(Ordering::Acquire) >= 2 {
            assert!(Instant::now() < deadline, "capacity was never released");
            std::thread::sleep(Duration::from_millis(5));
        }
        let receiver = start_lookup_with(&OUTSTANDING, 2, || Ok(Vec::new())).unwrap();
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .is_empty()
        );
        release_second.send(()).unwrap();
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
