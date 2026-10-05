//! Probing and persisting the execution engine's identity before launch.

use super::*;

/// Ask an engine for its version without passing credentials or mailbox
/// arguments. This is best-effort metadata: an old wrapper may not implement
/// `--version`, in which case the run explicitly remains unversioned.
pub(crate) fn probe_engine_version(executable: &str) -> Option<String> {
    let cancel = AtomicBool::new(false);
    let mut candidates = vec![(executable.to_owned(), vec!["--version".into()])];
    if std::path::Path::new(executable)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("doveadm"))
    {
        let sibling = std::path::Path::new(executable)
            .parent()
            .map(|parent| parent.join("dovecot"))
            .unwrap_or_else(|| std::path::PathBuf::from("dovecot"));
        candidates.push((
            sibling.to_string_lossy().into_owned(),
            vec!["--version".into()],
        ));
    }
    for (candidate, args) in candidates {
        let Ok((status, lines, _)) = run_capture_lines(
            &candidate,
            &args,
            &[],
            &cancel,
            &[],
            Duration::from_secs(5),
            None,
            None,
        ) else {
            continue;
        };
        if status.exit_code == Some(0)
            && let Some(line) = lines
                .into_iter()
                .map(|line| line.trim().to_owned())
                .find(|line| !line.is_empty() && line.len() <= 512)
        {
            return Some(line);
        }
    }
    None
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedImapsyncIdentity {
    pub(crate) version: String,
    pub(crate) output_profile: verification::ImapsyncOutputProfile,
}

/// Resolve the executable identity before launch. An unreadable version is
/// recorded explicitly and maps to an unknown parser profile, so it can never
/// produce trusted verification evidence.
pub(crate) fn resolve_imapsync_identity(executable: &str) -> ResolvedImapsyncIdentity {
    let probed = probe_engine_version(executable);
    ResolvedImapsyncIdentity {
        output_profile: verification::imapsync_output_profile(probed.as_deref()),
        version: probed.unwrap_or_else(|| "unknown".into()),
    }
}

/// Persist the resolved engine identity before process registration. The
/// acknowledgment makes version/profile selection part of the durable launch
/// boundary instead of lossy telemetry.
pub(crate) fn persist_engine_identity_before_launch(
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    version: &str,
) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        crate::Event::EngineVersion {
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
            version: version.to_owned(),
            reply: reply_tx,
        },
    )?;
    reply_rx
        .recv_timeout(PROCESS_REGISTRATION_ACK_TIMEOUT)
        .map_err(|_| "engine identity was not durably acknowledged before launch".to_owned())?
}
