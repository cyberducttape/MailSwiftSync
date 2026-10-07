//! The packaged binary's `--internal-dns-resolve` helper, run the way the
//! controller runs it: a cleared environment, null stdin, and a bounded
//! line-per-address stdout. Deadline, cancellation, crash, and parallelism
//! handling on the controller side are covered with stub helpers in
//! `src/imap_probe/resolver.rs`, because a real system resolver cannot be
//! made to hang on demand.

use std::net::SocketAddr;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn resolve(arguments: &[&std::ffi::OsStr]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mailswiftsync"));
    // Mirror `process::apply_dns_environment`: only resolver settings reach
    // the helper, never the controller's environment.
    command.env_clear();
    for name in ["PATH", "HOME", "LOCALDOMAIN", "RES_OPTIONS", "SYSTEMROOT"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .arg("--internal-dns-resolve")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn resolve_address(address: &str) -> Output {
    resolve(&[std::ffi::OsStr::new(address)])
}

fn addresses(output: &Output) -> Vec<SocketAddr> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| {
            line.parse()
                .unwrap_or_else(|_| panic!("not an address: {line:?}"))
        })
        .collect()
}

#[test]
fn literal_and_named_hosts_resolve_to_socket_addresses_only() {
    let output = resolve_address("127.0.0.1:993");
    assert!(output.status.success(), "{}", output.status);
    assert_eq!(addresses(&output), vec!["127.0.0.1:993".parse().unwrap()]);
    assert!(output.stderr.is_empty());

    let output = resolve_address("[::1]:143");
    assert!(output.status.success(), "{}", output.status);
    assert_eq!(addresses(&output), vec!["[::1]:143".parse().unwrap()]);

    // A name goes through the platform resolver in the helper process.
    let output = resolve_address("localhost:993");
    assert!(output.status.success(), "{}", output.status);
    let resolved = addresses(&output);
    assert!(!resolved.is_empty() && resolved.len() <= 64, "{resolved:?}");
    assert!(
        resolved
            .iter()
            .all(|address| address.ip().is_loopback() && address.port() == 993),
        "{resolved:?}"
    );
}

#[test]
fn unresolvable_or_malformed_hosts_fail_without_output() {
    // RFC 6761 reserves `.invalid`; it never resolves.
    for address in [
        "mailswiftsync-dns-test.invalid:993",
        "missing-port.example.test",
        "127.0.0.1:not-a-port",
        "",
    ] {
        let output = resolve_address(address);
        assert_eq!(output.status.code(), Some(1), "{address:?}");
        assert!(output.stdout.is_empty(), "{address:?}");
        assert!(output.stderr.is_empty(), "{address:?}");
    }
}

#[test]
fn helper_rejects_anything_but_one_address_argument() {
    assert_eq!(resolve(&[]).status.code(), Some(2));
    let extra = resolve(&["127.0.0.1:993".as_ref(), "127.0.0.1:143".as_ref()]);
    assert_eq!(extra.status.code(), Some(2));
    assert!(extra.stdout.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let not_utf8 = resolve(&[std::ffi::OsStr::from_bytes(b"\xff:993")]);
        assert_eq!(not_utf8.status.code(), Some(2));
    }
}

#[test]
fn parallel_helpers_answer_independently_and_promptly() {
    let started = Instant::now();
    let lookups = (0..32_u16)
        .map(|index| {
            std::thread::spawn(move || {
                let port = 1000 + index;
                let output = resolve_address(&format!("127.0.0.1:{port}"));
                assert!(output.status.success(), "{}", output.status);
                assert_eq!(
                    addresses(&output),
                    vec![SocketAddr::from(([127, 0, 0, 1], port))]
                );
            })
        })
        .collect::<Vec<_>>();
    for lookup in lookups {
        lookup.join().unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(30));
}
