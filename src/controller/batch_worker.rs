use super::batch::BatchExecutionMode;
use super::batch_scheduler::{JobLoader, JobSource, run_batch_schedule};
use super::batch_work_item::{BatchAttemptRunner, send_job_finished};
use super::rate_domains::{RateDomainLimiter, RateDomainPath, SideIdentity};
use crate::{
    Event, StreamOutcome,
    process::ProcessLaunchLimiter,
    runner::{ResolvedImapsyncIdentity, resolve_imapsync_identity, send_reliable_event},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

const MAX_BATCH_PENDING_EVENTS: usize = 4_096;

pub(crate) type OAuthRefreshLocks = Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>;

pub(crate) fn source_endpoint(form: &crate::Form) -> String {
    canonical_provider_endpoint(
        &form.profile.source_host,
        &form.profile.source_port,
        &form.profile.source_tls,
    )
}

pub(crate) fn destination_endpoint(form: &crate::Form) -> String {
    canonical_provider_endpoint(
        &form.profile.destination_host,
        &form.profile.destination_port,
        crate::effective_destination_tls(&form.profile.destination_tls),
    )
}

#[cfg(test)]
pub(crate) fn provider_scope_key(form: &crate::Form) -> String {
    format!("{}|{}", source_endpoint(form), destination_endpoint(form))
}

/// Authentication principal that a provider may meter: the OAuth refresh
/// credential, else the keyring credential, else (empty) the user itself.
fn rate_principal(auth: &str, oauth_refresh_id: &str, credential_id: &str) -> String {
    if crate::migration_plan::auth_method_is_oauth(auth) && !oauth_refresh_id.trim().is_empty() {
        format!("oauth:{}", oauth_refresh_id.trim())
    } else if !credential_id.trim().is_empty() {
        format!("keyring:{}", credential_id.trim())
    } else {
        String::new()
    }
}

/// Place one mailbox job in the global → provider → tenant → credential →
/// mailbox rate-domain hierarchy for both sides.
pub(crate) fn rate_domain_path(form: &crate::Form) -> RateDomainPath {
    let profile = &form.profile;
    let source_endpoint = source_endpoint(form);
    let destination_endpoint = destination_endpoint(form);
    let source_principal = rate_principal(
        &profile.source_auth,
        &profile.source_oauth_refresh_credential_id,
        &profile.source_credential_id,
    );
    let destination_principal = rate_principal(
        &profile.destination_auth,
        &profile.destination_oauth_refresh_credential_id,
        &profile.destination_credential_id,
    );
    RateDomainPath::new(
        &SideIdentity {
            endpoint: &source_endpoint,
            user: &profile.source_user,
            tenant: &profile.source_rate_tenant,
            principal: &source_principal,
        },
        &SideIdentity {
            endpoint: &destination_endpoint,
            user: &profile.destination_user,
            tenant: &profile.destination_rate_tenant,
            principal: &destination_principal,
        },
    )
}

fn canonical_provider_endpoint(host: &str, configured_port: &str, tls_mode: &str) -> String {
    let default_port = crate::default_imap_port(tls_mode);
    let fallback = || {
        format!(
            "invalid:{}:{}",
            host.trim().to_ascii_lowercase(),
            configured_port.trim()
        )
    };
    let Ok((host, embedded_port)) = crate::endpoint::parts(host, default_port) else {
        return fallback();
    };
    let port = if configured_port.trim().is_empty() {
        embedded_port
    } else {
        match configured_port.trim().parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => return fallback(),
        }
    };
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(address) => address.to_string(),
        Err(_) => host.trim_end_matches('.').to_ascii_lowercase(),
    };
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(crate) struct BatchWorkerLaunch {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) receiver: mpsc::Receiver<Event>,
}

/// Immutable inputs for one batch execution. Keeping queue identity, retry
/// policy, and work items together prevents positional argument drift between
/// the controller and worker coordinator.
pub(crate) struct BatchExecutionContext {
    pub(crate) concurrency: usize,
    pub(crate) provider_rate_ceilings: crate::organization_policy::ProviderRateCeilings,
    pub(crate) launch_starts_per_second: usize,
    pub(crate) mode: BatchExecutionMode,
    pub(crate) retry_count: usize,
    pub(crate) job_count: usize,
    pub(crate) queue_job_ids: Vec<String>,
    pub(crate) child_run_ids: Vec<String>,
    pub(crate) queue_checkpoints: Vec<Option<String>>,
    pub(crate) batch_project_id: String,
    pub(crate) batch_run_id: String,
    /// Resolves admitted job IDs to executable jobs, page by page.
    pub(crate) loader: JobLoader,
    pub(crate) imapsync_executables: Vec<String>,
    pub(crate) verification_state_path: Option<std::path::PathBuf>,
    pub(crate) diagnostic_logger: Option<Arc<crate::DiagnosticLogger>>,
}

/// Create the bounded event channel, cancellation token, and batch worker as
/// one controller-owned operation. Callers only retain the handles needed to
/// render state and request cancellation; channel sizing and worker startup
/// policy do not leak into the egui composition root.
pub(crate) fn launch_batch_worker(context: BatchExecutionContext) -> BatchWorkerLaunch {
    let (tx, receiver) = mpsc::sync_channel(MAX_BATCH_PENDING_EVENTS);
    let cancel = Arc::new(AtomicBool::new(false));
    spawn_batch_worker(context, tx, Arc::clone(&cancel));
    BatchWorkerLaunch { cancel, receiver }
}

/// Admission controls shared by every attempt of one batch.
pub(crate) struct BatchLimiters {
    pub(crate) launch: Arc<ProcessLaunchLimiter>,
    pub(crate) rate: Arc<RateDomainLimiter>,
}

pub(crate) fn spawn_batch_worker(
    context: BatchExecutionContext,
    tx: mpsc::SyncSender<Event>,
    cancel: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let limiters = BatchLimiters {
        launch: Arc::new(ProcessLaunchLimiter::new(context.launch_starts_per_second)),
        rate: Arc::new(RateDomainLimiter::new_with_provider_ceilings(
            context.concurrency,
            context.provider_rate_ceilings.clone(),
        )),
    };
    spawn_batch_worker_with(context, tx, cancel, limiters)
}

pub(crate) fn spawn_batch_worker_with(
    context: BatchExecutionContext,
    tx: mpsc::SyncSender<Event>,
    cancel: Arc<AtomicBool>,
    limiters: BatchLimiters,
) -> thread::JoinHandle<()> {
    let BatchExecutionContext {
        concurrency,
        provider_rate_ceilings: _,
        launch_starts_per_second: _,
        mode,
        retry_count,
        job_count,
        queue_job_ids,
        child_run_ids,
        queue_checkpoints,
        batch_project_id,
        batch_run_id,
        loader,
        imapsync_executables,
        verification_state_path,
        diagnostic_logger,
    } = context;
    let oauth_refresh_locks: OAuthRefreshLocks = Arc::new(Mutex::new(HashMap::new()));
    thread::spawn(move || {
        let failed = Arc::new(AtomicBool::new(false));
        let terminal_jobs = Arc::new(Mutex::new(HashSet::new()));
        // Resolve each distinct imapsync executable once before any mailbox
        // process starts. Every child receives the same immutable identity;
        // there is no first-wave subscriber race and no lossy metadata send.
        let resolved_imapsync = imapsync_executables
            .into_iter()
            .map(|executable| {
                let identity = resolve_imapsync_identity(&executable);
                (executable, identity)
            })
            .collect::<HashMap<String, ResolvedImapsyncIdentity>>();
        let runner = Arc::new(BatchAttemptRunner {
            concurrency,
            mode,
            retry_count,
            tx: tx.clone(),
            cancel: Arc::clone(&cancel),
            failed: Arc::clone(&failed),
            terminal_jobs: Arc::clone(&terminal_jobs),
            launch_limiter: limiters.launch,
            provider_limiter: limiters.rate,
            batch_project_id,
            batch_run_id,
            resolved_imapsync: Arc::new(resolved_imapsync),
            oauth_refresh_locks,
            verification_state_path,
            diagnostic_logger,
        });
        let schedule = run_batch_schedule(
            runner,
            concurrency,
            JobSource::new(
                queue_job_ids.clone(),
                child_run_ids.clone(),
                queue_checkpoints,
                loader,
            ),
        );
        if schedule.enqueue_failed {
            let _ = tx.send(Event::Line(
                    "Batch work queue disconnected before all jobs were admitted; unresolved jobs require review before retrying."
                        .into(),
                ));
        }
        if schedule.worker_panicked {
            let unresolved = terminal_jobs
                .lock()
                .map(|terminal| {
                    (0..job_count)
                        .filter(|index| !terminal.contains(index))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|_| (0..job_count).collect());
            for index in unresolved {
                let Some(job_id) = queue_job_ids.get(index).cloned() else {
                    continue;
                };
                let Some(child_run_id) = child_run_ids.get(index).cloned() else {
                    continue;
                };
                let _ = tx.send(Event::RunLine {
                    run_id: child_run_id.clone(),
                    job_id: job_id.clone(),
                    text: format!(
                        "[{}] worker stopped unexpectedly; job moved to Attention",
                        index + 1
                    ),
                });
                let _ = tx.send(Event::JobState {
                    job_id: job_id.clone(),
                    child_run_id: child_run_id.clone(),
                    state: "Attention".into(),
                });
                if let Err(error) = send_job_finished(
                    &tx,
                    job_id,
                    child_run_id,
                    "attention".into(),
                    "worker stopped unexpectedly".into(),
                    None,
                ) {
                    eprintln!("reliable batch terminal event delivery failed: {error}");
                }
            }
            let _ = tx.send(Event::Line(
                    "A batch worker stopped unexpectedly; unresolved jobs require review before retrying."
                        .into(),
                ));
        }
        if let Err(error) = send_reliable_event(
            &tx,
            Event::Finished(if cancel.load(Ordering::Relaxed) {
                Err("batch cancelled".into())
            } else if failed.load(Ordering::Relaxed) {
                Err("one or more batch jobs failed".into())
            } else {
                Ok(StreamOutcome::Completed)
            }),
        ) {
            eprintln!("reliable batch terminal event delivery failed: {error}");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::provider_scope_key;

    fn form(
        source_host: &str,
        source_port: &str,
        destination_host: &str,
        destination_port: &str,
    ) -> crate::Form {
        let mut form = crate::Form::default();
        form.profile.source_host = source_host.into();
        form.profile.source_port = source_port.into();
        form.profile.destination_host = destination_host.into();
        form.profile.destination_port = destination_port.into();
        form
    }

    #[test]
    fn provider_scope_canonicalizes_equivalent_host_and_port_forms() {
        let implicit = form("imap.example.com", "", "dest.example.com", "");
        let explicit = form("IMAP.EXAMPLE.COM.", "993", "dest.example.com:993", "");
        let host_embedded = form("imap.example.com:993", "", "DEST.EXAMPLE.COM", "993");
        let key = provider_scope_key(&implicit);

        assert_eq!(key, provider_scope_key(&explicit));
        assert_eq!(key, provider_scope_key(&host_embedded));
        assert_eq!(key, "imap.example.com:993|dest.example.com:993");
    }

    #[test]
    fn provider_scope_uses_tls_specific_defaults_and_configured_port_precedence() {
        let implicit_tls = form("imap.example.com", "", "dest.example.com", "");
        let mut starttls = form("imap.example.com", "", "dest.example.com", "");
        starttls.profile.source_tls = "starttls".into();
        starttls.profile.destination_tls = "starttls".into();
        assert_ne!(
            provider_scope_key(&implicit_tls),
            provider_scope_key(&starttls)
        );
        assert_eq!(
            provider_scope_key(&starttls),
            "imap.example.com:143|dest.example.com:143"
        );

        let configured_overrides_embedded =
            form("imap.example.com:143", "993", "dest.example.com:143", "993");
        assert_eq!(
            provider_scope_key(&implicit_tls),
            provider_scope_key(&configured_overrides_embedded)
        );
    }

    #[test]
    fn provider_scope_canonicalizes_idn_and_ipv6_addresses() {
        let unicode = form("mail.bücher.example", "993", "[2001:0db8::1]", "993");
        let ascii = form("mail.xn--bcher-kva.example.", "", "[2001:db8::1]", "");
        assert_eq!(provider_scope_key(&unicode), provider_scope_key(&ascii));
    }

    #[test]
    fn provider_scope_keeps_distinct_effective_ports_separate() {
        let standard = form("imap.example.com", "", "dest.example.com", "");
        let alternate = form("imap.example.com", "1993", "dest.example.com", "");
        assert_ne!(
            provider_scope_key(&standard),
            provider_scope_key(&alternate)
        );
    }
}
