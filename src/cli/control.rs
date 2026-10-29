//! `mailswiftsync control`: the optional central control plane for MSP
//! fleets. It adds authenticated users, organizational roles, tenant
//! isolation, scoped API keys, and a tamper-evident audit log in one SQLite
//! file owned by the service. The desktop application and each worker's
//! local ledger are unchanged; mailbox credentials never reach this service.
//!
//! See docs/control-plane.md.

mod api;
mod audit;
mod authn;
mod authz;
mod server;
mod store;
mod workers;

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "Usage:
  mailswiftsync control init <state.db> --admin-email <email> --admin-password-file <file> [--admin-name <name>]
  mailswiftsync control serve <state.db> [--listen 127.0.0.1:9470] [--tls-cert <pem> --tls-key <pem>] [--allow-non-loopback]
  mailswiftsync control audit-verify <state.db>";
const DEFAULT_LISTEN: &str = "127.0.0.1:9470";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Init {
        state: PathBuf,
        email: String,
        name: String,
        password_file: PathBuf,
    },
    Serve {
        state: PathBuf,
        listen: SocketAddr,
        tls: Option<(PathBuf, PathBuf)>,
    },
    AuditVerify {
        state: PathBuf,
    },
}

fn parse<I: IntoIterator<Item = OsString>>(arguments: I) -> Result<Command, String> {
    let mut arguments = arguments.into_iter().map(|argument| {
        argument
            .into_string()
            .map_err(|_| "arguments must be valid UTF-8".to_owned())
    });
    let subcommand = arguments.next().transpose()?.ok_or(USAGE)?;
    let mut state = None;
    let mut options = std::collections::BTreeMap::<String, String>::new();
    let mut allow_non_loopback = false;
    while let Some(argument) = arguments.next().transpose()? {
        match argument.as_str() {
            "--allow-non-loopback" if !allow_non_loopback => allow_non_loopback = true,
            flag @ ("--admin-email"
            | "--admin-name"
            | "--admin-password-file"
            | "--listen"
            | "--tls-cert"
            | "--tls-key") => {
                let value = arguments.next().transpose()?.ok_or(USAGE)?;
                if options.insert(flag.to_owned(), value).is_some() {
                    return Err(format!("{flag} was given more than once"));
                }
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ if state.is_none() => state = Some(PathBuf::from(argument)),
            _ => return Err(USAGE.into()),
        }
    }
    let state = state.ok_or(USAGE)?;
    let allowed = |names: &[&str]| {
        options
            .keys()
            .all(|key| names.contains(&key.as_str()))
            .then_some(())
            .ok_or_else(|| format!("an option does not apply to `control {subcommand}`\n{USAGE}"))
    };
    match subcommand.as_str() {
        "init" => {
            allowed(&["--admin-email", "--admin-name", "--admin-password-file"])?;
            let email = options.remove("--admin-email").ok_or(USAGE)?;
            Ok(Command::Init {
                state,
                name: options
                    .remove("--admin-name")
                    .unwrap_or_else(|| email.clone()),
                email,
                password_file: options.remove("--admin-password-file").ok_or(USAGE)?.into(),
            })
        }
        "serve" => {
            allowed(&["--listen", "--tls-cert", "--tls-key"])?;
            let listen = options
                .remove("--listen")
                .unwrap_or_else(|| DEFAULT_LISTEN.into())
                .parse::<SocketAddr>()
                .map_err(|_| "--listen needs an IP address and port, such as 127.0.0.1:9470")?;
            let tls = match (options.remove("--tls-cert"), options.remove("--tls-key")) {
                (Some(certificate), Some(key)) => Some((certificate.into(), key.into())),
                (None, None) => None,
                _ => return Err("--tls-cert and --tls-key must be given together".into()),
            };
            if !listen.ip().is_loopback() && tls.is_none() && !allow_non_loopback {
                return Err("without --tls-cert/--tls-key the control plane speaks plain HTTP, so a non-loopback --listen would expose passwords and bearer tokens on the network; enable TLS, or put a TLS reverse proxy in front and pass --allow-non-loopback to acknowledge".into());
            }
            Ok(Command::Serve { state, listen, tls })
        }
        "audit-verify" => {
            allowed(&[])?;
            Ok(Command::AuditVerify { state })
        }
        _ => Err(USAGE.into()),
    }
}

pub(super) fn control_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let command = match parse(arguments) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if let Err(error) = run(command) {
        eprintln!("Control plane: {error}");
        std::process::exit(1);
    }
    Ok(())
}

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Init {
            state,
            email,
            name,
            password_file,
        } => {
            let password = crate::credentials::read_secret_file(&password_file)
                .map_err(|error| format!("administrator password file refused: {error}"))?;
            let store = store::Store::open(&state)?;
            let id = store
                .bootstrap_administrator(&email, &name, password.as_str())
                .map_err(|error| format!("{error:?}"))?;
            println!(
                "Created administrator {email} ({id}) in {}",
                state.display()
            );
            Ok(())
        }
        Command::Serve { state, listen, tls } => {
            let store = store::Store::open(&state)?;
            let tls = tls
                .map(|(certificate, key)| crate::cli::serve::tls::server_config(&certificate, &key))
                .transpose()?;
            let listener = std::net::TcpListener::bind(listen)
                .map_err(|error| format!("could not listen on {listen}: {error}"))?;
            eprintln!(
                "Control plane serving {} on {}://{}",
                state.display(),
                if tls.is_some() { "https" } else { "http" },
                listener.local_addr().unwrap_or(listen)
            );
            server::serve(listener, Arc::new(api::Context { store }), tls)
        }
        Command::AuditVerify { state } => {
            let store = store::Store::open(&state)?;
            let count = store.verify_audit_chain()?;
            println!("Audit log intact: {count} entries verified");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_subcommands_and_refuses_misplaced_options() {
        assert_eq!(
            parse(arguments(&["serve", "state.db"])).unwrap(),
            Command::Serve {
                state: "state.db".into(),
                listen: DEFAULT_LISTEN.parse().unwrap(),
                tls: None,
            }
        );
        assert!(matches!(
            parse(arguments(&[
                "init",
                "s.db",
                "--admin-email",
                "a@b.example",
                "--admin-password-file",
                "p"
            ])),
            Ok(Command::Init { .. })
        ));
        assert!(parse(arguments(&["init", "s.db", "--listen", "0.0.0.0:1"])).is_err());
        assert!(parse(arguments(&["serve", "s.db", "--listen", "0.0.0.0:9470"])).is_err());
        assert!(
            parse(arguments(&[
                "serve",
                "s.db",
                "--listen",
                "0.0.0.0:9470",
                "--allow-non-loopback"
            ]))
            .is_ok()
        );
        assert!(parse(arguments(&["serve", "s.db", "--tls-cert", "c"])).is_err());
        assert!(
            parse(arguments(&[
                "serve", "s.db", "--listen", "a", "--listen", "b"
            ]))
            .is_err()
        );
        assert!(parse(arguments(&["bogus", "s.db"])).is_err());
    }
}
