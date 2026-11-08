//! `mailswiftsync control plan upload`: send a `control worker plan`
//! document of any size to the control plane in pages, committing it as one
//! new plan revision. Sending a page again after a network failure is safe:
//! the service accepts an identical page and refuses a different one.

use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

pub(super) const USAGE: &str = "Usage:
  mailswiftsync control plan upload <plan.json> --server <url> --credential-file <file>
      --tenant <tenant-id> --project <project-id> --revision <current-revision> [--ca-cert <pem>]

Uploads the document `control worker plan` exported, in pages, as the
project's next plan revision. The credential is a session token or an API key
with project:write. A second person then approves the new revision.";

/// Plan files beyond this are refused before parsing.
const MAX_PLAN_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const PAGE_ATTEMPTS: u32 = 3;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Command {
    file: PathBuf,
    server: String,
    credential_file: PathBuf,
    tenant: String,
    project: String,
    revision: i64,
    ca_cert: Option<PathBuf>,
}

pub(super) fn parse(arguments: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut arguments = arguments.peekable();
    if arguments.next().as_deref() != Some("upload") {
        return Err(USAGE.into());
    }
    let mut file = None;
    let mut options = BTreeMap::<String, String>::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            flag @ ("--server" | "--credential-file" | "--tenant" | "--project" | "--revision"
            | "--ca-cert") => {
                let value = arguments.next().ok_or(USAGE)?;
                if options.insert(flag.to_owned(), value).is_some() {
                    return Err(format!("{flag} was given more than once"));
                }
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}\n{USAGE}"));
            }
            _ if file.is_none() => file = Some(PathBuf::from(argument)),
            _ => return Err(USAGE.into()),
        }
    }
    let mut identifier = |name: &str| -> Result<String, String> {
        let value = options.remove(name).ok_or(USAGE)?;
        if value.is_empty()
            || value.len() > 200
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(format!("{name} must be an identifier such as prj_abc123"));
        }
        Ok(value)
    };
    let tenant = identifier("--tenant")?;
    let project = identifier("--project")?;
    let revision = options
        .remove("--revision")
        .ok_or(USAGE)?
        .parse::<i64>()
        .ok()
        .filter(|revision| *revision >= 1)
        .ok_or("--revision must be the project's current plan revision (1 or more)")?;
    Ok(Command {
        file: file.ok_or(USAGE)?,
        server: options.remove("--server").ok_or(USAGE)?,
        credential_file: options.remove("--credential-file").ok_or(USAGE)?.into(),
        tenant,
        project,
        revision,
        ca_cert: options.remove("--ca-cert").map(PathBuf::from),
    })
}

/// The exported document, with its mailboxes kept in job-ID order.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanFile {
    format: String,
    format_version: u64,
    ledger_project_id: String,
    ledger_project_name: String,
    mailboxes: BTreeMap<String, Value>,
}

fn read_plan(path: &Path) -> Result<PlanFile, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let length = file
        .metadata()
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?
        .len();
    if length > MAX_PLAN_FILE_BYTES {
        return Err(format!(
            "{} is larger than {MAX_PLAN_FILE_BYTES} bytes",
            path.display()
        ));
    }
    serde_json::from_reader(std::io::BufReader::new(file)).map_err(|error| {
        format!(
            "{} is not a plan exported by `control worker plan`: {error}",
            path.display()
        )
    })
}

struct Client {
    http: reqwest::blocking::Client,
    base: url::Url,
    bearer: Zeroizing<String>,
}

impl Client {
    /// Send one request; `Err((retryable, message))` on failure.
    fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &Value,
    ) -> Result<Value, (bool, String)> {
        let url = self
            .base
            .join(path)
            .map_err(|error| (false, error.to_string()))?;
        let response = self
            .http
            .request(method, url)
            .bearer_auth(self.bearer.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .map_err(|error| (true, format!("request failed: {error}")))?;
        let status = response.status().as_u16();
        let mut text = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(response, MAX_RESPONSE_BYTES),
            &mut text,
        )
        .map_err(|error| (true, format!("could not read the response: {error}")))?;
        let body: Value = serde_json::from_slice(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(body);
        }
        let detail = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("no detail");
        Err((
            status == 429 || status >= 500,
            format!("the control plane answered {status}: {detail}"),
        ))
    }
}

pub(super) fn run(command: Command) -> Result<(), String> {
    let base = super::agent::server_url(&command.server)?;
    let bearer = crate::credentials::read_secret_file(&command.credential_file)
        .map_err(|error| format!("credential file refused: {error}"))?;
    let client = Client {
        http: super::agent::http_client(command.ca_cert.as_deref())?,
        base,
        bearer: Zeroizing::new(bearer.as_str().trim().to_owned()),
    };
    let plan = read_plan(&command.file)?;
    let total = plan.mailboxes.len();
    let header = json!({
        "format": plan.format,
        "format_version": plan.format_version,
        "ledger_project_id": plan.ledger_project_id,
        "ledger_project_name": plan.ledger_project_name,
    });
    let uploads = format!(
        "v1/tenants/{}/projects/{}/plan/uploads",
        command.tenant, command.project
    );
    let started = client
        .send(
            reqwest::Method::POST,
            &uploads,
            &json!({ "revision": command.revision, "header": header, "mailbox_count": total }),
        )
        .map_err(|(_, message)| message)?;
    let upload = started["upload_id"]
        .as_str()
        .ok_or("the control plane did not return an upload ID")?
        .to_owned();
    let page_size = started["max_page_mailboxes"]
        .as_u64()
        .filter(|size| *size >= 1)
        .ok_or("the control plane did not return a page size")? as usize;
    let pages = total.div_ceil(page_size);
    let mut entries = plan.mailboxes.into_iter();
    for page in 1..=pages {
        let mailboxes = entries
            .by_ref()
            .take(page_size)
            .collect::<serde_json::Map<_, _>>();
        let body = json!({ "mailboxes": mailboxes });
        let path = format!("{uploads}/{upload}/pages/{page}");
        let mut attempt = 1;
        loop {
            match client.send(reqwest::Method::PUT, &path, &body) {
                Ok(_) => break,
                Err((true, message)) if attempt < PAGE_ATTEMPTS => {
                    eprintln!("Page {page} of {pages} failed ({message}); retrying");
                    std::thread::sleep(Duration::from_secs(2 * u64::from(attempt)));
                    attempt += 1;
                }
                Err((_, message)) => return Err(format!("page {page} of {pages}: {message}")),
            }
        }
        eprintln!("Uploaded page {page} of {pages}");
    }
    let committed = client
        .send(
            reqwest::Method::POST,
            &format!("{uploads}/{upload}/commit"),
            &json!({}),
        )
        .map_err(|(_, message)| message)?;
    println!(
        "Stored plan revision {} with {} mailboxes. A second person must approve revision {} before work can be queued.",
        committed["plan_revision"], committed["plan_mailboxes"], committed["plan_revision"]
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> impl Iterator<Item = String> {
        values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn parses_upload_and_refuses_unsafe_identifiers() {
        let parsed = parse(arguments(&[
            "upload",
            "plan.json",
            "--server",
            "https://control.example",
            "--credential-file",
            "key",
            "--tenant",
            "ten_a",
            "--project",
            "prj_b",
            "--revision",
            "3",
        ]))
        .unwrap();
        assert_eq!(parsed.revision, 3);
        assert_eq!(parsed.project, "prj_b");
        for bad in [
            &[
                "upload",
                "p",
                "--server",
                "s",
                "--credential-file",
                "k",
                "--tenant",
                "../x",
                "--project",
                "p",
                "--revision",
                "1",
            ][..],
            &[
                "upload",
                "p",
                "--server",
                "s",
                "--credential-file",
                "k",
                "--tenant",
                "t",
                "--project",
                "p",
                "--revision",
                "0",
            ][..],
            &[
                "upload",
                "p",
                "--server",
                "s",
                "--credential-file",
                "k",
                "--tenant",
                "t",
                "--project",
                "p",
            ][..],
            &["download", "p"][..],
        ] {
            assert!(parse(arguments(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_plan_of_thousands_of_mailboxes_uploads_in_pages_over_http() {
        use super::super::store::{Actor, Store, test_plan};
        let directory =
            std::env::temp_dir().join(format!("mss-plan-upload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let store = Store::in_memory();
        let password = super::super::authn::test_secret();
        let admin = store
            .bootstrap_administrator("admin@msp.example", "Admin", &password)
            .unwrap();
        let actor = Actor {
            request_id: "test",
            label: format!("user:{admin}"),
        };
        let tenant = store.create_tenant("Acme", &actor).unwrap().id;
        let project = store
            .create_project(&tenant, "Move", &test_plan("ledger"), &actor)
            .unwrap()
            .id;
        let (token, _) = store
            .login("admin@msp.example", &password, "r", None, false)
            .unwrap();
        let credential_file = directory.join("token");
        crate::credentials::write_secret_file(&credential_file, token.as_str()).unwrap();

        let total = 4_500;
        let mut mailboxes = serde_json::Map::new();
        for index in 0..total {
            mailboxes.insert(
                format!("job-{index:05}"),
                json!({ "label": format!("Mailbox {index}"), "plan_sha256": "d".repeat(64) }),
            );
        }
        let plan_file = directory.join("plan.json");
        std::fs::write(
            &plan_file,
            json!({
                "format": super::super::agent::PLAN_FORMAT,
                "format_version": super::super::agent::PLAN_FORMAT_VERSION,
                "ledger_project_id": "ledger",
                "ledger_project_name": "Move",
                "mailboxes": mailboxes,
            })
            .to_string(),
        )
        .unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let context = std::sync::Arc::new(super::super::api::Context { store, oidc: None });
        let server_context = std::sync::Arc::clone(&context);
        std::thread::spawn(move || super::super::server::serve(listener, server_context, None));

        let command = |revision| Command {
            file: plan_file.clone(),
            server: format!("http://{address}"),
            credential_file: credential_file.clone(),
            tenant: tenant.clone(),
            project: project.clone(),
            revision,
            ca_cert: None,
        };
        run(command(1)).unwrap();
        let stored = context.store.project(&tenant, &project).unwrap();
        assert_eq!(stored.plan_revision, 2);
        assert_eq!(stored.plan_mailboxes, total as i64);
        assert!(!stored.approved);
        // A stale revision is refused before anything is sent.
        let error = run(command(1)).unwrap_err();
        assert!(error.contains("409"), "{error}");
        let _ = std::fs::remove_dir_all(directory);
    }
}
