//! `mailswiftsync control`: the optional central control plane for MSP
//! fleets. It adds authenticated users, organizational roles, tenant
//! isolation, scoped API keys, and a tamper-evident audit log in one SQLite
//! file owned by the service. The desktop application and each worker's
//! local ledger are unchanged; mailbox credentials never reach this service.
//!
//! See docs/control-plane.md.

mod agent;
mod api;
mod audit;
mod authn;
mod authz;
mod jwt;
mod oidc;
mod plan_upload;
mod plans;
mod server;
mod store;
mod workers;

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "Usage:
  mailswiftsync control init <state.db> --admin-email <email> --admin-password-file <file> [--admin-name <name>]
  mailswiftsync control serve <state.db> [--listen 127.0.0.1:9470] [--tls-cert <pem> --tls-key <pem>] [--oidc-config <toml>] [--allow-non-loopback]
  mailswiftsync control audit-verify <state.db> [--checkpoint <file> [--public-key <hex>]]
  mailswiftsync control audit-checkpoint <state.db> --output <file> [--signing-key <ed25519-pkcs8> [--key-id <id>]]
  mailswiftsync control worker plan|enroll|run ...  (see `control worker`)
  mailswiftsync control plan upload <plan.json> ...  (see `control plan`)";
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
        oidc: Option<PathBuf>,
    },
    AuditVerify {
        state: PathBuf,
        checkpoint: Option<PathBuf>,
        public_key: Option<String>,
    },
    AuditCheckpoint {
        state: PathBuf,
        output: PathBuf,
        signing_key: Option<PathBuf>,
        key_id: String,
    },
    Worker(agent::Command),
    Plan(plan_upload::Command),
}

fn parse<I: IntoIterator<Item = OsString>>(arguments: I) -> Result<Command, String> {
    let mut arguments = arguments.into_iter().map(|argument| {
        argument
            .into_string()
            .map_err(|_| "arguments must be valid UTF-8".to_owned())
    });
    let subcommand = arguments.next().transpose()?.ok_or(USAGE)?;
    if subcommand == "worker" {
        let rest = arguments.collect::<Result<Vec<_>, _>>()?;
        return agent::parse(rest.into_iter()).map(Command::Worker);
    }
    if subcommand == "plan" {
        let rest = arguments.collect::<Result<Vec<_>, _>>()?;
        return plan_upload::parse(rest.into_iter()).map(Command::Plan);
    }
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
            | "--tls-key"
            | "--oidc-config"
            | "--checkpoint"
            | "--public-key"
            | "--output"
            | "--signing-key"
            | "--key-id") => {
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
            allowed(&["--listen", "--tls-cert", "--tls-key", "--oidc-config"])?;
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
            Ok(Command::Serve {
                state,
                listen,
                tls,
                oidc: options.remove("--oidc-config").map(PathBuf::from),
            })
        }
        "audit-verify" => {
            allowed(&["--checkpoint", "--public-key"])?;
            let checkpoint = options.remove("--checkpoint").map(PathBuf::from);
            let public_key = options.remove("--public-key");
            if public_key.is_some() && checkpoint.is_none() {
                return Err("--public-key verifies a --checkpoint".into());
            }
            Ok(Command::AuditVerify {
                state,
                checkpoint,
                public_key,
            })
        }
        "audit-checkpoint" => {
            allowed(&["--output", "--signing-key", "--key-id"])?;
            let signing_key = options.remove("--signing-key").map(PathBuf::from);
            let key_id = options.remove("--key-id");
            if key_id.is_some() && signing_key.is_none() {
                return Err("--key-id names a --signing-key".into());
            }
            Ok(Command::AuditCheckpoint {
                state,
                output: options.remove("--output").ok_or(USAGE)?.into(),
                signing_key,
                key_id: key_id.unwrap_or_else(|| "control-audit".into()),
            })
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
        Command::Serve {
            state,
            listen,
            tls,
            oidc,
        } => {
            let store = store::Store::open(&state)?;
            let oidc = oidc.as_deref().map(oidc::Oidc::load).transpose()?;
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
            if let Some(oidc) = &oidc {
                eprintln!(
                    "OIDC sign-in enabled for: {}",
                    oidc.providers
                        .iter()
                        .map(|provider| provider.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            server::serve(listener, Arc::new(api::Context { store, oidc }), tls)
        }
        Command::Worker(command) => agent::run(command),
        Command::Plan(command) => plan_upload::run(command),
        Command::AuditVerify {
            state,
            checkpoint,
            public_key,
        } => {
            let store = store::Store::open(&state)?;
            let count = store.verify_audit_chain()?;
            println!("Audit log intact: {count} entries verified");
            if let Some(checkpoint) = checkpoint {
                let verdict = audit::check_checkpoint(&checkpoint, public_key.as_deref(), |seq| {
                    store.audit_hash_at(seq)
                })?;
                println!("Audit checkpoint: {verdict}");
            }
            Ok(())
        }
        Command::AuditCheckpoint {
            state,
            output,
            signing_key,
            key_id,
        } => {
            let store = store::Store::open(&state)?;
            let (entries, head) = store.verify_audit_chain_head()?;
            let document = audit::checkpoint(entries, head)?;
            let text =
                serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
            crate::atomic_artifact::write_private_atomic(&output, &text)
                .map_err(|error| format!("could not write {}: {error}", output.display()))?;
            if let Some(signing_key) = signing_key {
                crate::reports::signing::sign_file(&output, &signing_key, &key_id)?;
            }
            println!(
                "Audit checkpoint written to {} at entry {}; publish it outside this service",
                output.display(),
                document["seq"]
            );
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
                oidc: None,
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
        assert_eq!(
            parse(arguments(&[
                "audit-checkpoint",
                "s.db",
                "--output",
                "c.json"
            ]))
            .unwrap(),
            Command::AuditCheckpoint {
                state: "s.db".into(),
                output: "c.json".into(),
                signing_key: None,
                key_id: "control-audit".into(),
            }
        );
        assert!(parse(arguments(&["audit-checkpoint", "s.db"])).is_err());
        assert!(
            parse(arguments(&[
                "audit-checkpoint",
                "s.db",
                "--output",
                "c",
                "--key-id",
                "k"
            ]))
            .is_err()
        );
        assert!(parse(arguments(&["audit-verify", "s.db", "--public-key", "00"])).is_err());
        assert!(parse(arguments(&["serve", "s.db", "--checkpoint", "c"])).is_err());
    }

    #[test]
    fn a_published_checkpoint_catches_a_rebuilt_audit_chain() {
        let directory =
            std::env::temp_dir().join(format!("mss-audit-checkpoint-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let state = directory.join("control.db");
        let password = authn::test_secret();
        let store = store::Store::open(&state).unwrap();
        store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        store
            .login("admin@msp.example", &password, "r", None, false)
            .unwrap();
        drop(store);

        let key = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .unwrap();
        let key_path = directory.join("audit-key.pk8");
        let mut key_file = crate::credentials::open_secret_file(&key_path).unwrap();
        std::io::Write::write_all(&mut key_file, key.as_ref()).unwrap();
        drop(key_file);
        let public_key = crate::reports::integrity::hex_encode(
            ring::signature::KeyPair::public_key(
                &ring::signature::Ed25519KeyPair::from_pkcs8(key.as_ref()).unwrap(),
            )
            .as_ref(),
        );
        let checkpoint = directory.join("checkpoint.json");
        run(Command::AuditCheckpoint {
            state: state.clone(),
            output: checkpoint.clone(),
            signing_key: Some(key_path),
            key_id: "audit".into(),
        })
        .unwrap();
        let verify = |key: &str| {
            run(Command::AuditVerify {
                state: state.clone(),
                checkpoint: Some(checkpoint.clone()),
                public_key: Some(key.into()),
            })
        };
        verify(&public_key).unwrap();
        assert!(verify(&"00".repeat(32)).is_err(), "the signer is pinned");

        // Someone with write access to the file rebuilds the whole chain
        // around an edited entry. The chain verifies again on its own...
        let raw = rusqlite::Connection::open(&state).unwrap();
        raw.execute_batch(
            "DROP TRIGGER audit_log_no_update;
             UPDATE audit_log SET detail='rewritten' WHERE seq=(SELECT MIN(seq) FROM audit_log);",
        )
        .unwrap();
        let rows = raw
            .prepare("SELECT seq,at,request_id,actor,tenant_id,action,resource,decision,detail FROM audit_log ORDER BY seq")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut previous = [0_u8; 32];
        for (seq, at, request_id, actor, tenant, action, resource, decision, detail) in rows {
            let hash = audit::chain_hash(
                &previous,
                &audit::AuditEntry {
                    at,
                    request_id: &request_id,
                    actor: &actor,
                    tenant: tenant.as_deref(),
                    action: &action,
                    resource: &resource,
                    allowed: decision == "allow",
                    detail: &detail,
                },
            );
            raw.execute(
                "UPDATE audit_log SET prev_hash=?2,hash=?3 WHERE seq=?1",
                rusqlite::params![seq, previous.as_slice(), hash.as_slice()],
            )
            .unwrap();
            previous = hash;
        }
        drop(raw);
        assert!(
            store::Store::open(&state)
                .unwrap()
                .verify_audit_chain()
                .is_ok()
        );
        // ...but no longer matches what was published.
        let error = verify(&public_key).unwrap_err();
        assert!(error.contains("rewritten after the checkpoint"), "{error}");
        let _ = std::fs::remove_dir_all(directory);
    }
}
