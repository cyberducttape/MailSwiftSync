mod atomic_artifact;
mod bootstrap;
mod bulk_import;
mod cli;
mod command;
mod controller;
mod core;
mod credentials;
mod endpoint;
mod engine;
mod headless;
mod imap_probe;
mod imap_protocol;
mod migration_plan;
mod oauth;
mod output;
mod plan_identity;
mod process;
mod provider;
mod reports;
mod runner;
mod storage;
mod storage_paths;
mod ui;
mod verification;

#[cfg(test)]
use atomic_artifact::write_private_atomic;
#[cfg(test)]
use command::{parse_shell_words, remove_option, shell_quote};
#[cfg(test)]
use controller::batch_admission::apply_keyring_id;
#[cfg(test)]
use controller::batch_admission::canonical_destination_identity;
#[cfg(test)]
use controller::batch_admission::duplicate_destination;
#[cfg(test)]
use controller::batch_admission::matches_queue;
#[cfg(test)]
use controller::batch_admission::selection_value;
#[cfg(test)]
use controller::batch_admission::{durable_batch_profile_config, validate_batch_throttle};
use controller::failure::{
    FailureClass, classified_failure_detail, classify_failure, terminal_phase_advance_allowed,
};
#[cfg(test)]
use controller::failure::{is_transient_batch_error, transient_retry_delay};
use controller::{
    ActiveRunContext, BatchExecutionMode, BulkConfirmationSummary, BulkQueueSummary,
    BulkRetryScope, CapabilityProbeResult, LiveAuthProof, PendingDbEvent, RunKind,
    SingleRunAdmission, SingleRunWorkerSpec, SingleStartContext, SingleStartDecision,
    admit_single_run, batch_mailbox_state, capability_probe_result_matches,
    decode_persisted_batch_profile, durable_single_identity_matches, finish_batch_child,
    is_verified_terminal_state, persist_pending_events, process_event_is_current,
    run_line_is_current, single_start_decision, spawn_single_run_worker,
};
pub(crate) use controller::{Event, StreamOutcome};
#[cfg(test)]
use credentials::CleanupGuard;
use credentials::{
    SecretString, cleanup_paths, cleanup_stale_secret_directories, create_secret_directory,
    restrict_directory_permissions, secret_runtime_base, write_secret_file,
};
use headless::export_support_bundle;
#[cfg(test)]
use headless::headless_status;
use output::BoundedLineBuffer;
#[cfg(test)]
use process::ProcessLaunchLimiter;
#[cfg(all(test, unix))]
use process::terminate_process_group_by_pid;
use process::{
    InstanceLock, acquire_instance_lock, recorded_process_matches, terminate_recorded_process_group,
};
#[cfg(all(test, unix))]
use process::{configure_process_group, process_identity};
use provider::ProviderPreset;
#[cfg(test)]
use runner::run_streaming;
#[cfg(test)]
use runner::{dovecot_state_candidate, record_process_tail};
#[cfg(test)]
use std::process::Command;

use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};
#[cfg(test)]
use imap_probe::{
    command_endpoint_parts, command_port, endpoint_for_probe, fresh_imap_authentication_applies,
    imap_command_succeeded, imap_quote,
};
use migration_plan::{
    Form, Profile, auth_method_is_oauth, default_destination_tls, default_imap_port,
    effective_destination_tls,
};
#[cfg(test)]
use oauth::xoauth2_payload;
use plan_identity::{
    fingerprint_digest as plan_fingerprint_digest, snapshot_sha256 as plan_snapshot_sha256,
};
use reports::integrity::{evidence_digest, with_proof_digest};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::fmt::Display;
#[cfg(test)]
use std::sync::Mutex;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::Duration,
};
#[cfg(test)]
use std::{ffi::OsString, path::PathBuf};
#[cfg(test)]
use storage_paths::persistent_state_path_from;
use storage_paths::{persistent_state_path, restore_ledger};
#[cfg(test)]
use ui::display_state_key;
#[cfg(test)]
use ui::job_state_badge;
#[cfg(test)]
use ui::recommended_next_action;
use ui::{
    AppearancePreferences, ThemeColors, WorkspaceRefreshOptions, WorkspaceSnapshot, WorkspaceView,
    display_job_state, format_elapsed, format_phase_name, markdown_escape, needs_operator_review,
    password_visibility_id, preferred_project_id, project_health_state_counts, push_visible_output,
    render_account, status_color, successful_run_severity, successful_run_status, truncate_utf8,
};
use ui::{StatusMessage, StatusSeverity};
#[cfg(test)]
use ui::{contrast_ratio, next_ui_scale, password_reveal_allowed};

const MAX_VISIBLE_OUTPUT_LINES: usize = 10_000;
const MAX_VISIBLE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROCESS_TAIL_LINES: usize = 200;
const MAX_PROCESS_TAIL_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;
const MAX_BULK_IMPORT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_BULK_IMPORT_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BULK_IMPORT_ARCHIVE_ENTRIES: usize = 4_096;
const MAX_BULK_IMPORT_ROWS: usize = 100_000;
const MAX_BULK_IMPORT_COLUMNS: usize = 64;
const MAX_BULK_IMPORT_CELL_BYTES: usize = 64 * 1024;
const DOVECOT_SYNC_LOCK_WAIT_SECONDS: u64 = 300;
const MAX_PENDING_EVENTS: usize = 4_096;
const MAX_ACTIVITY_HISTORY_ROWS: u32 = 250;
// Keep one unusually noisy worker from monopolising an egui frame.
const MAX_EVENTS_PER_FRAME: usize = 250;
const DEFAULT_UI_SCALE: f32 = 1.10;
const MIN_UI_SCALE: f32 = 0.90;
const MAX_UI_SCALE: f32 = 1.50;

use bulk_import::{BulkImportResult, BulkJob, PendingSheetImport};

struct App {
    form: Form,
    output: BoundedLineBuffer,
    receiver: Option<Receiver<Event>>,
    status: StatusMessage,
    preview: bool,
    bulk_jobs: Vec<BulkJob>,
    bulk_open: bool,
    settings_open: bool,
    bulk_search: String,
    bulk_state_filter: String,
    bulk_selected_ids: HashSet<String>,
    /// Reused filtered-row index storage. Large batch views must not allocate
    /// a fresh index vector on every repaint.
    bulk_visible_indices: Vec<usize>,
    /// Lowercase searchable mailbox fields, rebuilt only when queue rows are
    /// imported or otherwise structurally changed.
    bulk_search_values: Vec<String>,
    bulk_filter_cache_search: String,
    bulk_filter_cache_state: String,
    bulk_filter_cache_generation: u64,
    bulk_jobs_generation: u64,
    bulk_message: String,
    advanced_open: bool,
    engine_open: bool,
    store: core::StateStore,
    /// Held for the lifetime of the application. An advisory OS lock is
    /// released automatically if the process crashes, so a later instance
    /// can safely perform orphan recovery without killing a live sibling.
    _instance_lock: Option<InstanceLock>,
    /// Startup could not prove that every recorded process identity was
    /// gone or owned by this application. No new execution is allowed until
    /// the operator confirms the host has been checked.
    process_review_required: bool,
    persistence_available: bool,
    profile_available: bool,
    /// The project currently selected by the operator for views and exports.
    /// This is deliberately separate from `active_run`, which is execution
    /// ownership and must never be inferred from UI selection.
    selected_project_id: Option<String>,
    /// The project browser normally keeps a compact recent index. This flag
    /// is set only when the operator explicitly asks for the searchable full
    /// project index.
    ui_all_projects_loaded: bool,
    /// A selected project that is not the current editable execution context
    /// is a historical read-only view. Keeping this explicit prevents the
    /// header/project browser from implying that the visible plan is safe to
    /// mutate or execute for that project.
    workspace_read_only: bool,
    projects_open: bool,
    project_search: String,
    /// Cached project-browser matches. The browser is repainted frequently;
    /// filtering by index avoids cloning every project on every frame.
    project_filter_query: String,
    project_filter_source_revision: u64,
    project_visible_indices: Vec<usize>,
    project_id: Option<String>,
    job_id: Option<String>,
    /// Process-local credential material used by the last successful dry
    /// preflight. Missing after restart intentionally requires revalidation.
    preflight_credential_fingerprint: Option<String>,
    run_id: Option<String>,
    active_run: Option<ActiveRunContext>,
    /// Non-secret plan values captured for the active execution. The egui
    /// form remains visible while a run is active, but edits must not mutate
    /// the plan presented to the operator or the next retry.
    locked_profile: Option<Profile>,
    cancel_requested: Option<Arc<AtomicBool>>,
    bulk_project_id: Option<String>,
    bulk_job_ids: Vec<String>,
    bulk_job_index_by_id: HashMap<String, usize>,
    /// Process-local credential material from the last successful dry
    /// validation for each durable queue row. Restored queues start empty.
    bulk_preflight_credential_fingerprints: Vec<Option<String>>,
    preflight: Vec<(String, String, bool)>,
    capability_receiver: Option<Receiver<CapabilityProbeResult>>,
    capability_probe_request_id: Option<String>,
    capability_probe_fingerprint: Option<String>,
    capability_observation_fingerprint: Option<String>,
    /// Fresh authentication completed immediately before an IMAPS live run.
    /// Both digests are captured at probe launch and must still match when
    /// the run is admitted.
    live_auth_receiver: Option<Receiver<Result<LiveAuthProof, String>>>,
    /// Loading an OS-keyring credential can involve IPC and must not block an
    /// egui frame. The cloned form is returned only after the worker has
    /// completed the load.
    start_credentials_receiver: Option<Receiver<Result<Form, String>>>,
    start_credentials_plan: Option<String>,
    credentials_ready_for_start: bool,
    live_auth_proof: Option<LiveAuthProof>,
    source_capabilities: Option<core::ServerCapabilities>,
    destination_capabilities: Option<core::ServerCapabilities>,
    live_confirm_open: bool,
    live_confirmed: bool,
    live_confirmation_plan: Option<String>,
    durability_error: bool,
    durability_recovery_pending: bool,
    stop_confirm_open: bool,
    keyring_open: bool,
    active_view: WorkspaceView,
    pending_evidence: Option<core::MailboxEvidence>,
    pending_batch_evidence: HashMap<String, core::MailboxEvidence>,
    pending_checkpoint: Option<String>,
    pending_batch_checkpoints: HashMap<String, String>,
    /// Execution diagnostics that could not yet be committed. These remain
    /// in memory and are retried before later terminal events are handled.
    pending_db_events: Vec<PendingDbEvent>,
    /// Events deferred because their durable predecessor could not be
    /// committed. Keeping them here prevents a child completion from being
    /// silently lost when SQLite is temporarily unavailable.
    deferred_events: VecDeque<Event>,
    run_started_at: Option<std::time::Instant>,
    dark_mode: bool,
    ui_scale: f32,
    bulk_live_confirm_open: bool,
    bulk_live_confirmed: bool,
    bulk_confirmation_summary: Option<BulkConfirmationSummary>,
    bulk_summary: Option<(u64, BulkQueueSummary)>,
    bulk_mode: BatchExecutionMode,
    bulk_clear_confirm_open: bool,
    pending_bulk_import: Option<std::path::PathBuf>,
    pending_sheet_import: Option<PendingSheetImport>,
    bulk_sheet_index: usize,
    bulk_import_receiver: Option<Receiver<Result<BulkImportResult, String>>>,
    bulk_live_run: bool,
    /// Live retry scope defaults to unresolved rows and is process-local UI
    /// state; durable child/run IDs remain the execution identity.
    bulk_retry_scope: BulkRetryScope,
    bulk_source_keyring_apply: String,
    bulk_destination_keyring_apply: String,
    verification_exception_operator: String,
    verification_exception_reason: String,
    verification_search: String,
    verification_filter: String,
    verification_visible_indices: Vec<usize>,
    verification_filter_cache_search: String,
    verification_filter_cache_state: String,
    verification_filter_cache_offset: u32,
    verification_filter_cache_revision: Option<i64>,
    verification_filter_cache_project: Option<String>,
    verification_filter_cache_rows: usize,
    activity_show_all: bool,
    activity_search: String,
    activity_status_filter: String,
    reopen_reason: String,
    /// Database-backed UI read model. Rendering consumes this cache instead
    /// of issuing SQLite queries on every egui repaint.
    ui_snapshot: WorkspaceSnapshot,
    historical_mailbox_offset: u32,
    verification_offset: u32,
    source_provider: ProviderPreset,
    destination_provider: ProviderPreset,
}
impl Default for App {
    fn default() -> Self {
        Self::from_state_path(None)
    }
}

fn main() -> eframe::Result<()> {
    cli::run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::contains_ascii_case_insensitive;
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use uuid::Uuid;

    fn dovecot_form() -> Form {
        let mut form = Form::default();
        form.profile.engine = core::Engine::Dovecot;
        form.profile.source_host = "old.example".into();
        form.profile.source_user = "old-user".into();
        form.profile.destination_host = "localhost".into();
        form.profile.destination_user = "new-user".into();
        form.source_password = String::from("secret").into();
        form.destination_password = String::from("unused").into();
        form
    }

    #[test]
    fn filter_matching_is_case_insensitive_without_changing_input() {
        let value = "Customer-09@Example.Test";
        assert!(contains_ascii_case_insensitive(value, "customer-09"));
        assert!(contains_ascii_case_insensitive(value, "EXAMPLE.TEST"));
        assert!(!contains_ascii_case_insensitive(value, "customer-10"));
        assert!(contains_ascii_case_insensitive(value, ""));
        assert_eq!(value, "Customer-09@Example.Test");
    }

    #[test]
    fn markdown_escape_protects_report_cells_and_line_structure() {
        assert_eq!(
            markdown_escape("folder|name\r\nsecond\\entry"),
            "folder\\|name second\\\\entry"
        );
    }

    #[test]
    fn dovecot_plan_uses_additive_sync_by_default() {
        let mut form = dovecot_form();
        form.dry_run = false;
        let (exe, args) = form.command(true);
        assert_eq!(exe, "doveadm");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "imapc_host=old.example"])
        );
        assert!(args.contains(&"sync".into()));
        assert!(args.contains(&"-1".into()));
        assert!(args.windows(2).any(|pair| {
            pair[0] == "-l" && pair[1] == DOVECOT_SYNC_LOCK_WAIT_SECONDS.to_string()
        }));
        assert!(args.windows(2).any(|pair| pair == ["-s", ""]));
        assert!(!args.iter().any(|arg| arg == "secret"));
    }

    #[test]
    fn live_dovecot_plan_uses_previous_checkpoint() {
        let mut form = dovecot_form();
        form.dry_run = false;
        let (_, args) = form.command_with_checkpoint(true, Some("AQAAAHm4+Jk="));
        assert!(args.windows(2).any(|pair| pair == ["-s", "AQAAAHm4+Jk="]));
    }

    #[test]
    fn dovecot_state_candidate_accepts_state_and_rejects_diagnostics() {
        assert_eq!(
            dovecot_state_candidate("AQAAAHm4+Jk="),
            Some("AQAAAHm4+Jk=".into())
        );
        assert!(dovecot_state_candidate("sync completed successfully").is_none());
        assert_eq!(
            dovecot_state_candidate("AQAAAHm4+Jk"),
            Some("AQAAAHm4+Jk".into())
        );
        assert!(dovecot_state_candidate("state-token_v2").is_none());
        assert!(dovecot_state_candidate("debug-output").is_none());
        assert!(dovecot_state_candidate("completed").is_none());
        assert!(dovecot_state_candidate(" short ").is_none());
        assert!(dovecot_state_candidate(" AQAAAHm4+Jk=").is_none());
        assert!(dovecot_state_candidate("AQAAAHm4+Jk= ").is_none());
        assert!(dovecot_state_candidate("deadbeef").is_none());
        assert!(dovecot_state_candidate("AQAAAA==").is_none());
        assert_eq!(dovecot_state_candidate("AAAAAA=="), Some("AAAAAA==".into()));
    }

    #[test]
    fn plan_snapshot_reference_is_stable_without_exposing_snapshot() {
        let snapshot = "dry_run = false\nsource_host = \"old.example\"";
        let reference = plan_snapshot_sha256(snapshot);
        assert_eq!(reference.len(), 64);
        assert_eq!(reference, plan_snapshot_sha256(snapshot));
        assert!(!reference.contains("old.example"));
        assert_ne!(reference, plan_snapshot_sha256("dry_run = true"));
    }

    #[test]
    fn evidence_digest_binds_run_plan_and_evidence_values() {
        let evidence = core::MailboxEvidence {
            source_messages: 10,
            destination_messages: 10,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: 0,
            failed_messages: 0,
            source_folders: 2,
            destination_folders: 2,
            authoritative: true,
        };
        let first = evidence_digest("run-one", "snapshot-one", &evidence);
        assert_eq!(first, evidence_digest("run-one", "snapshot-one", &evidence));
        assert_ne!(first, evidence_digest("run-two", "snapshot-one", &evidence));
        assert_ne!(first, evidence_digest("run-one", "snapshot-two", &evidence));
        let mut changed = evidence.clone();
        changed.destination_messages = 9;
        assert_ne!(first, evidence_digest("run-one", "snapshot-one", &changed));
    }

    #[test]
    fn migration_proof_digest_detects_semantic_tampering() {
        let original = serde_json::json!({
            "format": "mailswiftsync-project-report",
            "format_version": 1,
            "project": { "name": "Example migration" },
            "mailboxes": [],
            "runs": []
        });
        let proof = with_proof_digest(original).unwrap();
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-proof-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("proof.json");
        std::fs::write(&path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        assert!(
            reports::signing::verify_file(&path, None)
                .unwrap()
                .contains("verified")
        );

        let mut tampered = proof;
        tampered["project"]["name"] = "Altered migration".into();
        std::fs::write(&path, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();
        assert!(reports::signing::verify_file(&path, None).is_err());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn customer_proof_format_is_digest_verifiable() {
        let proof = with_proof_digest(serde_json::json!({
            "format": "mailswiftsync-customer-proof",
            "format_version": 1,
            "project": { "name": "Customer migration", "phase": "Verification" },
            "mailboxes": [],
            "runs": [],
            "note": "customer-safe"
        }))
        .unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-customer-proof-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("proof.json");
        std::fs::write(&path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        assert!(
            reports::signing::verify_file(&path, None)
                .unwrap()
                .contains("verified")
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn support_bundle_excludes_topology_and_diagnostic_material() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-support-bundle-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let state = directory.join("state.db");
        let output = directory.join("support.json");
        let store = core::StateStore::open(&state).unwrap();
        let project = store
            .create_project("Support fixture", "source.internal", "destination.internal")
            .unwrap();
        for index in 0..2 {
            let mailbox = format!("user-{index}@example.test");
            store.add_mailbox(&project.id, &mailbox, &mailbox).unwrap();
        }
        drop(store);

        headless::export_support_bundle_with_sample_limit(&state, &output, 1).unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["format"], "mailswiftsync-support-bundle");
        assert!(!text.contains("source.internal"));
        assert!(!text.contains("destination.internal"));
        assert_eq!(value["redaction"]["credentials"], "excluded");
        assert_eq!(value["redaction"]["diagnostic_text"], "excluded");
        assert_eq!(value["projects"][0]["mailbox_count"], 2);
        assert_eq!(value["projects"][0]["mailbox_sample_limit"], 1);
        assert_eq!(value["projects"][0]["mailboxes_truncated"], true);
        assert_eq!(
            value["projects"][0]["mailboxes"].as_array().unwrap().len(),
            1
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn signed_migration_proof_requires_valid_signature_and_trust_pin() {
        let original = serde_json::json!({
            "format": "mailswiftsync-project-report",
            "format_version": 2,
            "project": { "name": "Signed migration" },
            "mailboxes": [],
            "runs": []
        });
        let proof = with_proof_digest(original).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-signed-proof-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("proof.json");
        let key_path = directory.join("signing-key.pk8");
        std::fs::write(&path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        let key = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .unwrap();
        std::fs::write(&key_path, key.as_ref()).unwrap();
        #[cfg(windows)]
        credentials::restrict_file_permissions(&key_path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&key_path).unwrap().permissions();
            permissions.set_mode(0o600);
            std::fs::set_permissions(&key_path, permissions).unwrap();
        }
        reports::signing::sign_file(&path, &key_path, "test-key").unwrap();
        // Re-signing is a supported repair/rotation workflow. The previous
        // signature must not become part of the newly calculated digest.
        reports::signing::sign_file(&path, &key_path, "test-key-rotated").unwrap();
        let signed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let public_key = signed["proof_signature"]["public_key"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            reports::signing::verify_file(&path, Some(&public_key))
                .unwrap()
                .contains("signature valid")
        );
        assert!(reports::signing::verify_file(&path, Some(&"00".repeat(32))).is_err());
        let mut tampered = signed;
        tampered["project"]["name"] = "Altered migration".into();
        std::fs::write(&path, serde_json::to_string_pretty(&tampered).unwrap()).unwrap();
        assert!(reports::signing::verify_file(&path, None).is_err());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn preflight_plan_storage_uses_an_opaque_digest() {
        let fingerprint = "imapsync\n--timeout\u{1f}30\ncredential-source1=source-id";
        let digest = plan_fingerprint_digest(fingerprint);
        assert_eq!(digest.len(), 64);
        assert_ne!(digest, fingerprint);
        assert!(!digest.contains("imapsync"));
        assert!(!digest.contains("source-id"));
    }

    #[test]
    fn local_dovecot_credentials_use_child_environment() {
        let mut form = dovecot_form();
        form.source_password = String::from("secret").into();
        let prepared = form.prepared_command().unwrap();
        assert!(
            prepared
                .args
                .iter()
                .any(|arg| { arg == "imapc_password=$ENV:MAILSWIFTSYNC_IMAPC_PASSWORD" })
        );
        assert!(!prepared.args.iter().any(|arg| arg.contains("secret")));
        assert_eq!(
            prepared.env,
            vec![(
                "MAILSWIFTSYNC_IMAPC_PASSWORD".into(),
                credentials::SecretString::new("secret".into()),
            ),]
        );
    }

    #[test]
    fn imapsync_plan_includes_explicit_throttles() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.max_messages_per_second = 25;
        form.profile.max_bytes_per_second = 1_048_576;
        let args = form.args(true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxmessagespersecond", "25"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxbytespersecond", "1048576"])
        );
    }

    #[test]
    fn batch_throttles_are_divided_across_workers() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.max_messages_per_second = 25;
        form.profile.max_bytes_per_second = 1_048_576;
        let args = form.args_with_throttle_divisor(true, 4);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxmessagespersecond", "6"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--maxbytespersecond", "262144"])
        );
    }

    #[test]
    fn process_launch_limiter_honors_cancellation() {
        let limiter = ProcessLaunchLimiter::new(1);
        let cancel = AtomicBool::new(true);
        assert!(!limiter.acquire(&cancel));
    }

    #[test]
    fn process_launch_limiter_spaces_sequential_starts() {
        let limiter = ProcessLaunchLimiter::new(20);
        let cancel = AtomicBool::new(false);
        assert!(limiter.acquire(&cancel));
        let started = std::time::Instant::now();
        assert!(limiter.acquire(&cancel));
        assert!(started.elapsed() >= Duration::from_millis(35));
    }

    #[test]
    fn batch_throttle_rejects_target_below_worker_count() {
        let mut profile = Profile {
            max_messages_per_second: 1,
            ..Profile::default()
        };
        assert!(validate_batch_throttle(&profile, 2).is_err());
        profile.max_messages_per_second = 2;
        assert!(validate_batch_throttle(&profile, 2).is_ok());
        profile.max_bytes_per_second = 1;
        assert!(validate_batch_throttle(&profile, 2).is_err());
    }

    #[test]
    fn plan_snapshot_excludes_raw_extra_options() {
        let mut form = dovecot_form();
        form.profile.extra_options = "--timeout=30".into();
        let snapshot = form.plan_snapshot();
        assert!(snapshot.contains("extra_options_sha256"));
        let expected = format!("{:x}", Sha256::digest("--timeout\u{1f}30".as_bytes()));
        assert!(snapshot.contains(&expected));
        assert!(!snapshot.contains("--timeout"));
    }

    #[test]
    fn plan_snapshot_records_checkpoint_identity_without_checkpoint_value() {
        let mut form = dovecot_form();
        form.dry_run = false;
        let checkpoint = "AQAAAHm4+Jk=";
        let snapshot = form
            .plan_snapshot_with_checkpoint(Some(checkpoint))
            .unwrap();
        let digest = plan_snapshot_sha256(checkpoint);
        assert!(snapshot.contains("dovecot_checkpoint_sha256"));
        assert!(snapshot.contains(&digest));
        assert!(!snapshot.contains(checkpoint));
    }

    #[test]
    fn dry_or_non_dovecot_snapshots_do_not_claim_checkpoint_input() {
        let mut dovecot = dovecot_form();
        assert!(
            !dovecot
                .plan_snapshot_with_checkpoint(Some("AQAAAHm4+Jk="))
                .unwrap()
                .contains("dovecot_checkpoint_sha256")
        );

        dovecot.dry_run = false;
        dovecot.profile.engine = core::Engine::ImapSync;
        assert!(
            !dovecot
                .plan_snapshot_with_checkpoint(Some("AQAAAHm4+Jk="))
                .unwrap()
                .contains("dovecot_checkpoint_sha256")
        );
    }

    #[test]
    fn durable_batch_config_excludes_raw_extra_options() {
        let profile = Profile {
            extra_options: "--debug secret-bearing-value".into(),
            ..Profile::default()
        };
        let config = durable_batch_profile_config(&profile).unwrap();
        assert!(!config.contains("secret-bearing-value"));
        assert!(config.contains("extra_options"));
    }

    #[test]
    fn durable_single_identity_rejects_edited_plan() {
        let project = core::Project {
            id: "project".into(),
            name: "Pilot".into(),
            source_endpoint: "old.example".into(),
            destination_endpoint: "new.example".into(),
            phase: core::Phase::Preflight,
        };
        let mailbox = core::MailboxJob {
            id: "job".into(),
            source_mailbox: "alice@example.com".into(),
            destination_mailbox: "alice@example.com".into(),
            state: "ready".into(),
            config: None,
        };
        let mut profile = Profile {
            source_host: "old.example".into(),
            destination_host: "new.example".into(),
            source_user: "alice@example.com".into(),
            destination_user: "alice@example.com".into(),
            ..Profile::default()
        };
        assert!(durable_single_identity_matches(
            &project, &mailbox, &profile
        ));
        profile.destination_host = "other.example".into();
        assert!(!durable_single_identity_matches(
            &project, &mailbox, &profile
        ));
    }

    #[test]
    fn dovecot_dry_plan_is_non_mutating() {
        let form = dovecot_form();
        let (_, args) = form.command(true);
        assert!(args.windows(2).any(|pair| pair == ["mailbox", "list"]));
        assert!(!args.contains(&"backup".into()));
        assert!(!args.contains(&"sync".into()));
    }

    #[test]
    fn dovecot_destination_preflight_checks_user_and_mailboxes() {
        let form = dovecot_form();
        let commands = form.dovecot_destination_preflight_commands();
        assert_eq!(commands.len(), 2);
        assert!(
            commands[0]
                .1
                .windows(2)
                .any(|pair| pair == ["user", "new-user"])
        );
        assert!(
            commands[1]
                .1
                .windows(2)
                .any(|pair| pair == ["mailbox", "list"])
        );
        assert!(
            commands[1]
                .1
                .windows(2)
                .any(|pair| pair == ["-u", "new-user"])
        );
    }

    #[test]
    fn plan_fingerprint_binds_credential_references_without_passwords() {
        let mut first = dovecot_form();
        first.profile.source_credential_id = "source-prod".into();
        first.profile.destination_credential_id = "destination-prod".into();
        let mut second = first.clone();
        second.profile.source_credential_id = "source-other".into();
        assert_ne!(first.plan_fingerprint(), second.plan_fingerprint());
        assert!(!first.plan_fingerprint().contains("secret"));
        assert!(first.plan_fingerprint().contains("source-prod"));
        assert!(first.plan_fingerprint().contains("destination-prod"));
    }

    #[test]
    fn credential_fingerprint_changes_without_exposing_secret_material() {
        let mut first = dovecot_form();
        first.source_password = SecretString::from("source-one");
        first.destination_password = SecretString::from("destination-one");
        let mut second = first.clone();
        second.destination_password = SecretString::from("destination-two");

        assert_ne!(
            first.credential_fingerprint(),
            second.credential_fingerprint()
        );
        assert!(!first.credential_fingerprint().contains("source-one"));
        assert!(!first.credential_fingerprint().contains("destination-one"));
    }

    #[test]
    fn remote_dovecot_plan_uses_batch_ssh_to_destination() {
        let mut form = dovecot_form();
        form.profile.dovecot_ssh_user = "migration".into();
        let (exe, args) = form.command(true);
        assert_eq!(exe, "ssh");
        assert!(args.starts_with(&[
            "-o".into(),
            "BatchMode=yes".into(),
            "migration@localhost".into()
        ]));
        assert!(
            args.last()
                .is_some_and(|command| command.contains("doveadm"))
        );
        assert!(
            args.last()
                .is_some_and(|command| command.contains("imapc_host=old.example"))
        );
    }

    #[test]
    fn remote_dovecot_execution_is_rejected_without_secret_broker() {
        let mut form = dovecot_form();
        form.profile.dovecot_ssh_user = "migration".into();
        assert!(
            form.prepared_command()
                .err()
                .is_some_and(|error| error.contains("not available"))
        );
        form.profile.allow_remote_password_in_argv = true;
        assert!(
            form.prepared_command()
                .err()
                .is_some_and(|error| error.contains("not available"))
        );
    }

    #[test]
    fn remote_dovecot_rejects_ssh_option_like_targets() {
        let mut form = dovecot_form();
        form.profile.dovecot_execution = "ssh".into();
        form.profile.allow_remote_password_in_argv = true;
        form.profile.destination_host = "-oProxyCommand=unsafe".into();
        assert!(form.validate().unwrap_err().contains("SSH host"));
        form.profile.destination_host = "mail.example".into();
        form.profile.dovecot_ssh_user = "admin user".into();
        assert!(form.validate().unwrap_err().contains("SSH username"));
    }

    #[test]
    fn dovecot_execution_location_can_be_explicit() {
        let mut form = dovecot_form();
        form.profile.destination_host = "mail.example".into();
        form.profile.dovecot_execution = "local".into();
        assert!(form.local_doveadm());
        form.profile.dovecot_execution = "ssh".into();
        assert!(!form.local_doveadm());
        form.profile.dovecot_execution = "invalid".into();
        assert!(form.validate().is_err());
    }

    #[test]
    fn remote_arguments_are_shell_quoted() {
        assert_eq!(shell_quote("plain-value"), "plain-value");
        assert_eq!(shell_quote("pa ss'word"), "'pa ss'\\''word'");
    }

    #[test]
    fn validation_rejects_zero_ports_with_leading_zeroes() {
        let mut form = dovecot_form();
        for value in ["0", "00", "000"] {
            form.profile.source_port = value.into();
            assert!(form.validate().unwrap_err().contains("Source IMAP port"));
            form.profile.source_port.clear();
            form.profile.destination_port = value.into();
            assert!(
                form.validate()
                    .unwrap_err()
                    .contains("Destination IMAP port")
            );
            form.profile.destination_port.clear();
        }
    }

    #[test]
    fn imap_quoted_values_reject_command_injection_controls() {
        assert_eq!(
            imap_quote("user@example.test").unwrap(),
            "\"user@example.test\""
        );
        assert!(imap_quote("secret\r\na002 NOOP").is_err());
    }

    #[test]
    fn extra_options_preserve_quoted_arguments() {
        assert_eq!(
            parse_shell_words("--foo 'two words' \"three four\"").unwrap(),
            ["--foo", "two words", "three four"]
        );
        assert!(parse_shell_words("--broken '").is_err());
    }

    #[test]
    fn extra_options_cannot_override_preflighted_connection() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.extra_options = "--host1=attacker.example".into();
        assert!(form.validate().unwrap_err().contains("controlled"));
        form.profile.extra_options = "--password2 leaked".into();
        assert!(form.validate().is_err());
        form.profile.extra_options = "--sslargs1 SSL_verify_mode=0".into();
        assert!(form.validate().is_err());
        form.profile.extra_options = "-delete2".into();
        assert!(form.validate().is_err());
        form.profile.extra_options = "--logdir /tmp/elsewhere".into();
        assert!(form.validate().is_err());
    }

    #[test]
    fn extra_options_require_the_safe_engine_allowlist() {
        let mut form = dovecot_form();
        form.profile.extra_options = "--nofoldersizes --timeout=30".into();
        assert!(form.validate().is_ok());
        form.profile.extra_options = "--nofoldersizes --timeout 30".into();
        assert!(form.validate().is_ok());
        form.profile.extra_options = "timeout=30".into();
        assert!(form.validate().unwrap_err().contains("canonical"));
        form.profile.extra_options = "--timeout=30".into();
        assert!(form.validate().is_ok());
        form.profile.extra_options = "--debug --debugimap1 --debugimap2".into();
        assert!(form.validate().is_ok());
        form.profile.extra_options = "--debugimap1=1".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("does not accept a value")
        );
        form.profile.extra_options = "--custom-helper /tmp/helper".into();
        let error = form.validate().unwrap_err();
        assert!(error.contains("safe imapsync option allowlist"));
        form.profile.extra_options = "--pipemess".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("safe imapsync option allowlist")
        );
        form.profile.extra_options = "--timeout".into();
        assert!(form.validate().unwrap_err().contains("requires a value"));
        form.profile.extra_options = "--timeout=fast".into();
        assert!(form.validate().unwrap_err().contains("requires an integer"));
        form.profile.extra_options = "--timeout=0".into();
        assert!(form.validate().unwrap_err().contains("between 1 and 86400"));
        form.profile.extra_options = "--errorsmax=100001".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("between 0 and 100000")
        );
    }

    #[test]
    fn equivalent_extra_option_spellings_share_plan_identity() {
        let mut inline = dovecot_form();
        inline.profile.engine = core::Engine::ImapSync;
        inline.profile.extra_options = "--timeout=030".into();
        let mut separated = inline.clone();
        separated.profile.extra_options = "--timeout 30".into();

        assert!(inline.validate().is_ok());
        assert!(separated.validate().is_ok());
        assert_eq!(inline.args(true), separated.args(true));
        assert_eq!(inline.plan_snapshot(), separated.plan_snapshot());
    }

    #[test]
    fn command_preparation_rejects_unvalidated_extra_options() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.extra_options = "--timeout nope".into();
        let error = match form.prepared_command() {
            Ok(_) => panic!("invalid extra options were prepared"),
            Err(error) => error,
        };
        assert!(error.contains("requires an integer"));
    }

    #[test]
    fn validation_rejects_imap_command_control_characters() {
        let mut form = dovecot_form();
        form.profile.source_user = "user\r\nNOOP".into();
        assert!(form.validate().unwrap_err().contains("control characters"));
        form.profile.source_user = "user".into();
        form.source_password = String::from("secret\nLOGIN injected").into();
        assert!(form.validate().unwrap_err().contains("control characters"));
    }

    #[test]
    fn validation_rejects_invalid_keyring_ids() {
        let mut form = dovecot_form();
        form.profile.source_credential_id = "bad\nentry".into();
        assert!(form.validate().unwrap_err().contains("keyring ID"));
        form.profile.source_credential_id = "x".repeat(257);
        assert!(form.validate().unwrap_err().contains("keyring ID"));
    }

    #[test]
    fn bulk_headers_allow_credentials_to_be_entered_after_import() {
        assert!(
            bulk_import::validate_headers(
                &[
                    "source_host",
                    "source_user",
                    "destination_host",
                    "destination_user"
                ]
                .map(String::from),
            )
            .is_ok()
        );
        assert!(
            bulk_import::validate_headers(
                &["source_host", "source_user", "destination_host"].map(String::from),
            )
            .is_err()
        );
        assert!(
            bulk_import::validate_headers(
                &[
                    "source_host",
                    "source_user",
                    "destination_host",
                    "destination_user",
                    "extra_options",
                ]
                .map(String::from),
            )
            .unwrap_err()
            .contains("cannot contain extra_options")
        );
    }

    #[test]
    fn passwordless_bulk_row_is_importable_but_not_runnable() {
        let mut values = HashMap::new();
        values.insert("source_host".into(), "old.example".into());
        values.insert("source_user".into(), "old@example".into());
        values.insert("destination_host".into(), "new.example".into());
        values.insert("destination_user".into(), "new@example".into());
        let job = bulk_import::job_from_values(values.clone(), &Form::default(), 2).unwrap();
        assert_eq!(job.state, "imported");
        assert_eq!(
            job_state_badge(&job.state, ThemeColors::dark()).0,
            "○ Imported"
        );
        assert!(job.form.source_password.is_empty());
        assert!(job.form.validate().is_err());
    }

    #[test]
    fn bulk_import_preserves_password_whitespace() {
        let mut values = HashMap::new();
        values.insert("source_host".into(), "old.example".into());
        values.insert("source_user".into(), "old@example".into());
        values.insert("source_password".into(), " Secret123 ".into());
        values.insert("destination_host".into(), "new.example".into());
        values.insert("destination_user".into(), "new@example".into());
        values.insert("destination_password".into(), " Destination! ".into());
        let job = bulk_import::job_from_values(values.clone(), &Form::default(), 2).unwrap();
        assert_eq!(job.form.source_password.as_str(), " Secret123 ");
        assert_eq!(job.form.destination_password.as_str(), " Destination! ");
    }

    #[test]
    fn bulk_import_rejects_oversized_files_before_parsing() {
        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-import-limit-{}",
            uuid::Uuid::new_v4()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BULK_IMPORT_BYTES + 1).unwrap();
        let error = bulk_import::validate_bulk_import_file(&path).unwrap_err();
        assert!(error.contains("import file"));
        assert!(error.contains("limit"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn xlsx_container_limits_reject_expansion_before_parsing() {
        assert!(
            bulk_import::validate_workbook_container_limits(
                MAX_BULK_IMPORT_ARCHIVE_ENTRIES,
                MAX_BULK_IMPORT_UNCOMPRESSED_BYTES
            )
            .is_ok()
        );
        let error = bulk_import::validate_workbook_container_limits(
            MAX_BULK_IMPORT_ARCHIVE_ENTRIES + 1,
            MAX_BULK_IMPORT_UNCOMPRESSED_BYTES,
        )
        .unwrap_err();
        assert!(error.contains("entries"));
        let error = bulk_import::validate_workbook_container_limits(
            MAX_BULK_IMPORT_ARCHIVE_ENTRIES,
            MAX_BULK_IMPORT_UNCOMPRESSED_BYTES + 1,
        )
        .unwrap_err();
        assert!(error.contains("expands"));
    }

    #[test]
    fn batch_selection_export_excludes_secret_material() {
        let mut form = Form::default();
        form.profile.source_host = "source.example".into();
        form.profile.source_user = "source@example".into();
        form.profile.destination_host = "destination.example".into();
        form.profile.destination_user = "destination@example".into();
        form.source_password = "source-secret".to_owned().into();
        form.destination_password = "destination-secret".to_owned().into();
        form.profile.source_credential_id = "source-key".into();
        let jobs = vec![BulkJob {
            label: "mailbox".into(),
            form,
            state: "failed".into(),
        }];
        let value = selection_value(&jobs, &HashSet::new(), &[]);
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.contains("source@example"));
        assert!(text.contains("failed"));
        assert!(!text.contains("source-secret"));
        assert!(!text.contains("destination-secret"));
        assert!(!text.contains("source-key"));
        assert!(!text.contains("extra_options"));
    }

    #[test]
    fn corrupt_or_missing_persisted_batch_plan_fails_closed() {
        let missing = match decode_persisted_batch_profile(None, "job-1") {
            Ok(_) => panic!("missing persisted plan must be rejected"),
            Err(error) => error,
        };
        assert!(missing.contains("job-1"));
        assert!(missing.contains("no migration plan"));

        let corrupt = match decode_persisted_batch_profile(Some("not = valid = toml"), "job-2") {
            Ok(_) => panic!("corrupt persisted plan must be rejected"),
            Err(error) => error,
        };
        assert!(corrupt.contains("job-2"));
        assert!(corrupt.contains("is corrupt"));
    }

    #[test]
    fn bulk_import_preserves_per_row_keyring_references() {
        let mut values = HashMap::new();
        values.insert("source_host".into(), "old.example".into());
        values.insert("source_user".into(), "old@example".into());
        values.insert("source_credential_id".into(), "source-alice".into());
        values.insert("destination_host".into(), "new.example".into());
        values.insert("destination_user".into(), "new@example".into());
        values.insert(
            "destination_credential_id".into(),
            "destination-alice".into(),
        );
        let job = bulk_import::job_from_values(values.clone(), &Form::default(), 2).unwrap();
        assert_eq!(job.form.profile.source_credential_id, "source-alice");
        assert_eq!(
            job.form.profile.destination_credential_id,
            "destination-alice"
        );
        assert!(job.form.source_password.is_empty());
        assert!(job.form.destination_password.is_empty());

        let mut base = Form::default();
        base.profile.source_credential_id = "shared-source".into();
        base.profile.destination_credential_id = "shared-destination".into();
        let inherited_values = values
            .into_iter()
            .filter(|(key, _)| key != "source_credential_id" && key != "destination_credential_id")
            .collect();
        let inherited = bulk_import::job_from_values(inherited_values, &base, 3).unwrap();
        assert_eq!(inherited.form.profile.source_credential_id, "shared-source");
        assert_eq!(
            inherited.form.profile.destination_credential_id,
            "shared-destination"
        );
    }

    #[test]
    fn bulk_keyring_apply_fills_only_missing_source_references() {
        let mut with_password = BulkJob {
            label: "password".into(),
            form: Form::default(),
            state: "Ready".into(),
        };
        with_password.form.source_password = String::from("already-present").into();
        let mut with_reference = BulkJob {
            label: "reference".into(),
            form: Form::default(),
            state: "Ready".into(),
        };
        with_reference.form.profile.source_credential_id = "existing".into();
        let empty = BulkJob {
            label: "empty".into(),
            form: Form::default(),
            state: "Ready".into(),
        };
        let mut jobs = vec![with_password, with_reference, empty];
        assert_eq!(apply_keyring_id(&mut jobs, "shared-source", true), 1);
        assert!(jobs[0].form.profile.source_credential_id.is_empty());
        assert_eq!(jobs[1].form.profile.source_credential_id, "existing");
        assert_eq!(jobs[2].form.profile.source_credential_id, "shared-source");
    }

    #[test]
    fn verification_report_write_is_atomic_and_private() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-report-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("report.md");
        write_private_atomic(&path, "report body").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "report body");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ledger_restore_validates_copy_and_preserves_previous_state() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-restore-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let source = directory.join("backup.db");
        let destination = directory.join("state.db");
        let source_store = core::StateStore::in_memory().unwrap();
        source_store
            .create_project("restore source", "source", "destination")
            .unwrap();
        source_store.backup_to(&source).unwrap();

        assert!(restore_ledger(&source, &destination).unwrap().is_none());
        core::StateStore::open_readonly(&destination).unwrap();
        std::fs::remove_file(&destination).unwrap();
        let orphaned_sidecar = PathBuf::from(format!("{}-wal", destination.display()));
        std::fs::write(&orphaned_sidecar, b"orphaned sqlite sidecar").unwrap();
        assert!(restore_ledger(&source, &destination).is_err());
        std::fs::remove_file(orphaned_sidecar).unwrap();
        assert!(restore_ledger(&source, &destination).unwrap().is_none());
        std::fs::remove_file(&destination).unwrap();

        let project = core::StateStore::open(&source)
            .unwrap()
            .latest_project()
            .unwrap()
            .unwrap();
        let source_connection = rusqlite::Connection::open(&source).unwrap();
        source_connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        source_connection
            .execute(
                "UPDATE projects SET name=?1 WHERE id=?2",
                rusqlite::params!["WAL-visible restore", project.id],
            )
            .unwrap();
        let source_sidecar = PathBuf::from(format!("{}-wal", source.display()));
        assert!(source_sidecar.exists());
        let error = restore_ledger(&source, &destination).unwrap_err();
        assert!(error.contains("live SQLite database with sidecar"));
        source_connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(source_connection);
        assert!(!source_sidecar.exists());
        assert!(restore_ledger(&source, &destination).unwrap().is_none());
        assert_eq!(
            core::StateStore::open_readonly(&destination)
                .unwrap()
                .latest_project()
                .unwrap()
                .unwrap()
                .name,
            "WAL-visible restore"
        );
        let clean_source = directory.join("standalone-source.db");
        core::StateStore::open(&source)
            .unwrap()
            .backup_to(&clean_source)
            .unwrap();
        core::StateStore::in_memory()
            .unwrap()
            .backup_to(&destination.with_extension("replacement.db"))
            .unwrap();
        for suffix in ["-wal", "-shm"] {
            std::fs::write(
                format!("{}{}", destination.display(), suffix),
                b"old sqlite sidecar",
            )
            .unwrap();
        }
        let previous = restore_ledger(&clean_source, &destination)
            .unwrap()
            .unwrap();
        for suffix in ["-wal", "-shm"] {
            assert!(std::path::Path::new(&format!("{}{}", previous.display(), suffix)).exists());
            assert!(
                !std::path::Path::new(&format!("{}{}", destination.display(), suffix)).exists()
            );
        }
        core::StateStore::open_readonly(&previous).unwrap();
        core::StateStore::open_readonly(&destination).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn batch_retry_classifier_excludes_authentication_failures() {
        assert!(is_transient_batch_error("connection reset by peer"));
        assert!(is_transient_batch_error("operation timed out"));
        assert!(is_transient_batch_error("server returned 429 rate limit"));
        assert!(!is_transient_batch_error("mailbox is full: OVERQUOTA"));
        assert!(!is_transient_batch_error("IMAP authentication failed"));
        assert!(!is_transient_batch_error(
            "invalid destination configuration"
        ));
    }

    #[test]
    fn capacity_retries_back_off_longer_than_transport_retries() {
        assert_eq!(
            classify_failure("too many connections"),
            FailureClass::Capacity
        );
        assert_eq!(
            transient_retry_delay("connection reset by peer", 0),
            Duration::from_secs(1)
        );
        assert_eq!(
            transient_retry_delay("server busy", 0),
            Duration::from_secs(5)
        );
        assert_eq!(
            transient_retry_delay("server busy", 99),
            Duration::from_secs(120)
        );
        assert_eq!(
            classified_failure_detail("too many requests"),
            "[attention_reason=capacity_limited] [class=capacity] too many requests"
        );
    }

    #[test]
    fn batch_retry_scopes_select_only_the_intended_durable_states() {
        let states = [
            "ready",
            "failed",
            "attention",
            "delta_required",
            "verification_difference",
            "completed",
            "verified",
            "verified_with_exceptions",
        ];
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::Unresolved.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["ready", "failed", "delta_required", "completed",]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::FailedAttention.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["failed", "attention"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::DeltaRequired.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["delta_required"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::VerificationDifference.includes(state))
                .copied()
                .collect::<Vec<_>>(),
            vec!["verification_difference"]
        );
        assert_eq!(
            states
                .iter()
                .filter(|state| BulkRetryScope::All.includes(state))
                .count(),
            states.len()
        );
    }

    #[test]
    fn failure_taxonomy_keeps_operator_actions_distinct() {
        assert_eq!(
            classify_failure("cancelled by operator before launch"),
            FailureClass::Cancellation
        );
        assert_eq!(
            classify_failure("verification evidence is incomplete"),
            FailureClass::Verification
        );
        assert_eq!(
            classify_failure("AUTHENTICATIONFAILED"),
            FailureClass::Authentication
        );
        assert_eq!(classify_failure("OVERQUOTA"), FailureClass::Quota);
        assert_eq!(
            classify_failure("connection reset by peer"),
            FailureClass::Transport
        );
        assert_eq!(
            classify_failure("message too large for destination"),
            FailureClass::Message
        );
        assert_eq!(
            classify_failure("unknown option --bad"),
            FailureClass::Configuration
        );
        assert_eq!(
            classified_failure_detail("OVERQUOTA"),
            "[attention_reason=capacity_limited] [class=quota] OVERQUOTA"
        );
        assert_eq!(
            classify_failure(
                "[attention_reason=transport_failed] [class=transport] authentication failed"
            ),
            FailureClass::Transport
        );
        assert_eq!(
            classify_failure("[attention_reason=unknown] [class=not-a-class] quota exceeded"),
            FailureClass::Quota
        );
    }

    #[test]
    fn successful_live_status_never_overclaims_missing_evidence() {
        assert_eq!(
            successful_run_status(false, false, Some("verified")),
            "Migration completed and verified"
        );
        assert_eq!(
            successful_run_status(false, false, Some("delta_required")),
            "Migration completed; final delta or review required"
        );
        assert_eq!(
            successful_run_status(false, false, None),
            "Migration completed; verification requires operator review"
        );
        assert_eq!(
            successful_run_status(false, true, None),
            "Batch transfer completed; review per-mailbox verification results"
        );
    }

    #[test]
    fn bulk_rejects_duplicate_destination_mailboxes() {
        let mut first = Form::default();
        first.profile.destination_host = "mail.example".into();
        first.profile.destination_user = "user@example".into();
        let mut second = first.clone();
        second.profile.source_user = "different@example".into();
        let jobs = vec![
            BulkJob {
                label: "first".into(),
                form: first,
                state: "Ready".into(),
            },
            BulkJob {
                label: "second".into(),
                form: second,
                state: "Ready".into(),
            },
        ];
        assert!(duplicate_destination(&jobs).unwrap().is_some());
    }

    #[test]
    fn duplicate_destination_detection_normalizes_explicit_default_port() {
        let mut first = Form::default();
        first.profile.destination_host = "mail.example".into();
        first.profile.destination_user = "user@example".into();
        let mut second = first.clone();
        second.profile.destination_host = "mail.example:993".into();
        let jobs = vec![
            BulkJob {
                label: "first".into(),
                form: first,
                state: "Ready".into(),
            },
            BulkJob {
                label: "second".into(),
                form: second,
                state: "Ready".into(),
            },
        ];
        assert!(duplicate_destination(&jobs).unwrap().is_some());
    }

    #[test]
    fn duplicate_destination_identity_uses_transport_and_explicit_port() {
        let mut first = Form::default();
        first.profile.destination_host = "mail.example".into();
        first.profile.destination_user = "user@example".into();
        first.profile.destination_tls = "starttls".into();
        let mut second = first.clone();
        second.profile.destination_port = "1993".into();
        let jobs = vec![
            BulkJob {
                label: "first".into(),
                form: first,
                state: "Ready".into(),
            },
            BulkJob {
                label: "second".into(),
                form: second,
                state: "Ready".into(),
            },
        ];
        assert!(duplicate_destination(&jobs).unwrap().is_none());
        assert_eq!(
            canonical_destination_identity(&jobs[0].form.profile).unwrap(),
            "endpoint:mail.example:143:user@example"
        );
    }

    #[test]
    fn duplicate_destination_identity_fails_closed_on_malformed_endpoint() {
        let mut form = Form::default();
        form.profile.destination_host = "mail.example:not-a-port".into();
        form.profile.destination_user = "user@example".into();
        let jobs = vec![BulkJob {
            label: "invalid".into(),
            form,
            state: "Ready".into(),
        }];
        assert!(duplicate_destination(&jobs).is_err());
    }

    #[test]
    fn duplicate_destination_identity_canonicalizes_hosts_but_preserves_mailbox_case() {
        let mut first = Form::default();
        first.profile.destination_host = "MAIL.EXAMPLE.".into();
        first.profile.destination_user = "User@example".into();
        let mut second = first.clone();
        second.profile.destination_host = "mail.example".into();
        second.profile.destination_user = "user@example".into();
        assert_ne!(
            canonical_destination_identity(&first.profile).unwrap(),
            canonical_destination_identity(&second.profile).unwrap()
        );
        assert_eq!(
            canonical_destination_identity(&first.profile).unwrap(),
            "endpoint:mail.example:993:User@example"
        );
    }

    #[test]
    fn edited_batch_identity_cannot_reuse_old_durable_queue() {
        let stored = vec![core::MailboxJob {
            id: "job-1".into(),
            source_mailbox: "alice@example.com".into(),
            destination_mailbox: "alice@example.net".into(),
            state: "ready".into(),
            config: Some("engine = 'imapsync'".into()),
        }];
        let same = vec![(
            "alice@example.com".into(),
            "alice@example.net".into(),
            "engine = 'imapsync'".into(),
        )];
        let edited = vec![(
            "bob@example.com".into(),
            "bob@example.net".into(),
            "engine = 'imapsync'".into(),
        )];

        assert!(matches_queue(&stored, &same));
        assert!(!matches_queue(&stored, &edited));
    }

    #[test]
    fn project_health_counts_group_durable_mailbox_states() {
        let jobs = vec![
            core::MailboxJob {
                id: "one".into(),
                source_mailbox: "one".into(),
                destination_mailbox: "one".into(),
                state: "verified".into(),
                config: None,
            },
            core::MailboxJob {
                id: "two".into(),
                source_mailbox: "two".into(),
                destination_mailbox: "two".into(),
                state: "attention".into(),
                config: None,
            },
            core::MailboxJob {
                id: "three".into(),
                source_mailbox: "three".into(),
                destination_mailbox: "three".into(),
                state: "verified".into(),
                config: None,
            },
        ];
        let counts = project_health_state_counts(&jobs);
        assert_eq!(counts.get("verified"), Some(&2));
        assert_eq!(counts.get("attention"), Some(&1));
        assert!(!needs_operator_review("verified"));
        assert!(needs_operator_review("verification_difference"));
    }

    #[test]
    fn active_run_project_takes_precedence_over_loaded_projects() {
        assert_eq!(
            preferred_project_id(
                Some("active-batch"),
                Some("selected"),
                Some("single"),
                Some("batch"),
            ),
            Some("active-batch")
        );
        assert_eq!(
            preferred_project_id(None, Some("selected"), Some("single"), Some("batch")),
            Some("selected")
        );
        assert_eq!(
            preferred_project_id(None, None, Some("single"), Some("batch")),
            Some("batch")
        );
    }

    #[test]
    fn active_run_context_rejects_foreign_process_events() {
        let context = ActiveRunContext {
            run_id: "parent".into(),
            project_id: "project".into(),
            job_id: None,
            batch_job_ids: vec!["job-a".into(), "job-b".into()],
            batch_child_run_ids: vec!["child-a".into(), "child-b".into()],
            batch_child_indices: HashMap::from([("child-a".into(), 0), ("child-b".into(), 1)]),
            batch_plan_fingerprints: vec!["plan-a".into(), "plan-b".into()],
            kind: RunKind::Batch,
            dry_run: true,
            engine: core::Engine::ImapSync,
            plan_fingerprint: String::new(),
            credential_fingerprint: String::new(),
        };
        assert!(context.owns_process("child-a", "job-a"));
        assert!(context.owns_process("child-b", "job-b"));
        assert!(context.owns_line("child-a", "job-a"));
        assert!(context.owns_line("child-b", "job-b"));
        assert!(context.owns_batch_child("parent", "child-a", "job-a"));
        assert!(!context.owns_process("child-a", "job-b"));
        assert!(!context.owns_line("child-a", "job-b"));
        assert!(!context.owns_process("foreign-child", "job-a"));
        assert!(!context.owns_line("foreign-child", "job-a"));
        assert!(!context.owns_process("parent", "job-a"));
        assert!(!context.owns_batch_child("other-parent", "child-a", "job-a"));
        assert!(!context.owns_batch_child("parent", "child-a", "job-b"));
    }

    #[test]
    fn subprocess_output_reader_survives_invalid_utf8() {
        let bytes = b"first\n\xff\xfe\nlast\n";
        let lines = process::read_lossy_lines(std::io::Cursor::new(bytes));
        assert_eq!(lines, ["first", "��", "last"]);
    }

    #[test]
    fn visible_output_retention_is_bounded_without_shifting() {
        let mut output = BoundedLineBuffer::new();
        for index in 0..=MAX_VISIBLE_OUTPUT_LINES {
            push_visible_output(&mut output, index.to_string());
        }

        assert_eq!(output.len(), MAX_VISIBLE_OUTPUT_LINES);
        assert_eq!(output.front().map(String::as_str), Some("1"));
        assert_eq!(
            output.back().and_then(|line| line.parse::<usize>().ok()),
            Some(MAX_VISIBLE_OUTPUT_LINES)
        );
    }

    #[test]
    fn diagnostic_buffers_truncate_utf8_and_bound_bytes() {
        let mut output = BoundedLineBuffer::new();
        push_visible_output(&mut output, "é".repeat(MAX_DIAGNOSTIC_LINE_BYTES + 1));
        assert!(output.front().unwrap().len() <= MAX_DIAGNOSTIC_LINE_BYTES);
        assert!(
            output
                .front()
                .unwrap()
                .is_char_boundary(output.front().unwrap().len())
        );

        let tail = Mutex::new(BoundedLineBuffer::new());
        for _ in 0..100 {
            record_process_tail(&tail, &"x".repeat(MAX_DIAGNOSTIC_LINE_BYTES));
        }
        let tail = tail.lock().unwrap();
        assert!(tail.bytes() <= MAX_PROCESS_TAIL_BYTES);
        assert_eq!(tail.bytes(), tail.iter().map(String::len).sum::<usize>());
        assert!(tail.len() <= MAX_PROCESS_TAIL_LINES);
    }

    #[cfg(unix)]
    #[test]
    fn live_dovecot_exit_code_two_is_a_delta_outcome() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::ProcessStarted(_, _, _, _, _, _, _, reply) = event {
                    let _ = reply.send(Ok(()));
                }
            }
        });
        let cancel = AtomicBool::new(false);
        let args = vec!["-c".into(), "exit 2".into()];
        let outcome = run_streaming(
            "/bin/sh",
            &args,
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            true,
        )
        .unwrap();
        drop(tx);
        acknowledger.join().unwrap();
        assert_eq!(outcome.outcome, StreamOutcome::DeltaRequired);
    }

    #[cfg(unix)]
    #[test]
    fn streaming_captures_bounded_imapsync_evidence_without_full_log() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::ProcessStarted(_, _, _, _, _, _, _, reply) = event {
                    let _ = reply.send(Ok(()));
                }
            }
        });
        let cancel = AtomicBool::new(false);
        let args = vec![
            "-c".into(),
            "printf '%s\\n' 'Host1 Nb folders: 2 folders' 'Host2 Nb folders: 2 folders' 'Host1 Nb messages: 7 messages' 'Host2 Nb messages: 7 messages' 'Host1 Total size: 100 bytes' 'Host2 Total size: 100 bytes' 'The sync looks good, all 7 identified messages in host1 are on host2.' 'Detected 0 errors'".into(),
        ];
        let result = run_streaming(
            "/bin/sh",
            &args,
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            false,
        )
        .unwrap();
        drop(tx);
        acknowledger.join().unwrap();
        assert_eq!(result.outcome, StreamOutcome::Completed);
        let evidence = result.imapsync_evidence.unwrap();
        assert_eq!(evidence.source_messages, 7);
        assert_eq!(evidence.destination_messages, 7);
        assert!(evidence.authoritative);
    }

    #[cfg(unix)]
    #[test]
    fn dry_dovecot_exit_code_two_is_not_a_delta_outcome() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                if let Event::ProcessStarted(_, _, _, _, _, _, _, reply) = event {
                    let _ = reply.send(Ok(()));
                }
            }
        });
        let cancel = AtomicBool::new(false);
        let args = vec!["-c".into(), "exit 2".into()];
        let outcome = run_streaming(
            "/bin/sh",
            &args,
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            false,
        );
        drop(tx);
        acknowledger.join().unwrap();
        assert!(outcome.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_process_event_channel_cancels_child_before_returning() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        drop(rx);
        let cancel = AtomicBool::new(false);
        let outcome = run_streaming(
            "/bin/sh",
            &["-c".into(), "sleep 30".into()],
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            false,
        );

        let error = outcome.unwrap_err();
        assert!(error.contains("event channel disconnected"));
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[cfg(unix)]
    #[test]
    fn rejected_process_registration_cancels_child_before_returning() {
        let (tx, rx) = mpsc::sync_channel(MAX_PENDING_EVENTS);
        let acknowledger = thread::spawn(move || {
            if let Ok(Event::ProcessStarted(_, _, _, _, _, _, _, reply)) = rx.recv() {
                let _ = reply.send(Err("synthetic durable registration rejection".into()));
            }
        });
        let cancel = AtomicBool::new(false);
        let outcome = run_streaming(
            "/bin/sh",
            &["-c".into(), "sleep 30".into()],
            &[],
            &tx,
            "test-run",
            "test-job",
            "",
            &cancel,
            &[],
            Duration::from_secs(5),
            false,
        );
        drop(tx);
        acknowledger.join().unwrap();
        assert!(
            outcome
                .unwrap_err()
                .contains("process registration failed; child cancelled")
        );
    }

    #[test]
    fn cleanup_guard_removes_secret_directory_on_scope_exit() {
        let directory = create_secret_directory().unwrap();
        {
            let _guard = CleanupGuard::new(vec![directory.clone()]);
            assert!(directory.is_dir());
        }
        assert!(!directory.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recorded_process_identity_rejects_start_time_mismatch() {
        let process = core::ActiveProcess {
            run_id: "run".into(),
            job_id: "job".into(),
            pid: std::process::id(),
            start_ticks: Some(0),
            process_group: Some(std::process::id()),
            session_id: Some(std::process::id()),
            executable: "test".into(),
        };
        assert!(!recorded_process_matches(&process));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recorded_macos_process_identity_can_be_validated_and_terminated() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let (start_ticks, process_group, session_id) = process_identity(pid).unwrap();
        let process = core::ActiveProcess {
            run_id: "run-macos-identity".into(),
            job_id: "job-macos-identity".into(),
            pid,
            start_ticks: Some(start_ticks),
            process_group: Some(process_group),
            session_id: Some(session_id),
            executable: "sh".into(),
        };
        assert!(recorded_process_matches(&process));
        terminate_recorded_process_group(&process);
        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    #[test]
    fn imap_preflight_requires_tagged_ok_responses() {
        assert!(imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 oK done",
            "a001"
        ));
        assert!(!imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 okay done",
            "a001"
        ));
        assert!(!imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 NO denied",
            "a001"
        ));
        assert!(!imap_command_succeeded(
            "* CAPABILITY IMAP4rev1\na001 BAD denied",
            "a001"
        ));
    }

    #[test]
    fn removing_secret_options_removes_values_starting_with_dashes() {
        let mut args = vec![
            "--password1".into(),
            "--looks-like-an-option".into(),
            "--host".into(),
            "mail".into(),
        ];
        remove_option(&mut args, "--password1");
        assert_eq!(args, ["--host", "mail"]);
    }

    #[test]
    fn endpoint_parser_handles_ports_and_ipv6() {
        assert_eq!(
            endpoint::parts("mail.example:8143", 993).unwrap(),
            ("mail.example".into(), 8143)
        );
        assert_eq!(
            endpoint::parts("[2001:db8::1]:993", 143).unwrap(),
            ("2001:db8::1".into(), 993)
        );
        assert!(endpoint::parts("mail.example:0", 993).is_err());
        assert!(endpoint::parts("[2001:db8::1]garbage", 993).is_err());
    }

    #[test]
    fn command_endpoint_helpers_never_reuse_invalid_input() {
        assert_eq!(
            command_endpoint_parts("mail.example:0", 993),
            ("<invalid-endpoint>".into(), 0)
        );
        assert_eq!(command_port("000", 993), 0);
        assert_eq!(command_port("", 993), 993);
    }

    #[test]
    fn validation_rejects_malformed_embedded_endpoint_ports() {
        let mut form = dovecot_form();
        form.profile.source_host = "source.example:not-a-port".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("Source IMAP host is not a valid endpoint")
        );
        form.profile.source_host = "source.example".into();
        form.profile.destination_host = "destination.example:bad".into();
        assert!(
            form.validate()
                .unwrap_err()
                .contains("Destination IMAP host is not a valid endpoint")
        );
    }

    #[test]
    fn probe_endpoint_preserves_explicit_ipv6_and_port() {
        assert_eq!(
            endpoint_for_probe("[2001:db8::1]", "993").unwrap(),
            "[2001:db8::1]:993"
        );
        assert_eq!(
            endpoint_for_probe("mail.example", "").unwrap(),
            "mail.example"
        );
        assert!(endpoint_for_probe("mail.example", "0").is_err());
    }

    #[test]
    fn imap_default_port_matches_transport_mode() {
        assert_eq!(default_imap_port("imaps"), 993);
        assert_eq!(default_imap_port("starttls"), 143);
        assert_eq!(default_imap_port("plain"), 143);
        assert_eq!(Form::default().profile.destination_tls, "imaps");
    }

    #[test]
    fn imapsync_destination_transport_and_port_are_typed() {
        let mut form = Form::default();
        form.profile.source_host = "source.example".into();
        form.profile.source_user = "source-user".into();
        form.profile.destination_host = "destination.example".into();
        form.profile.destination_user = "destination-user".into();
        form.source_password = String::from("source-secret").into();
        form.destination_password = String::from("destination-secret").into();
        form.profile.source_tls = "starttls".into();
        form.profile.destination_tls = "starttls".into();
        let args = form.args(true);
        assert!(args.contains(&"--tls1".into()));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs1", "SSL_verify_mode=1"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--port2", "143"]));
        assert!(args.contains(&"--tls2".into()));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs2", "SSL_verify_mode=1"])
        );
        assert!(!args.iter().any(|arg| arg.starts_with("--tlsargs")));
        assert!(!args.iter().any(|arg| arg == "--ssl2"));
        form.profile.destination_port = "8143".into();
        let args = form.args(true);
        assert!(args.windows(2).any(|pair| pair == ["--port2", "8143"]));
    }

    #[test]
    fn secret_runtime_isolated_below_xdg_runtime_directory() {
        assert_eq!(
            credentials::secret_runtime_base_from(Some(PathBuf::from("/run/user/1000"))),
            PathBuf::from("/run/user/1000/mailswiftsync")
        );
        let fallback = credentials::secret_runtime_base_from(Some(PathBuf::from("")));
        let temp_dir = std::env::temp_dir();
        assert_eq!(fallback.parent(), Some(temp_dir.as_path()));
        #[cfg(unix)]
        assert!(
            fallback
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("mailswiftsync-runtime-"))
        );
        #[cfg(not(unix))]
        assert_eq!(
            fallback.file_name().and_then(|name| name.to_str()),
            Some("mailswiftsync-runtime")
        );
    }

    #[test]
    fn state_lock_prevents_two_instances_and_releases_on_drop() {
        let state_path =
            std::env::temp_dir().join(format!("mailswiftsync-lock-{}.db", uuid::Uuid::new_v4()));
        let first = acquire_instance_lock(&state_path).unwrap();
        assert!(
            acquire_instance_lock(&state_path)
                .unwrap_err()
                .contains("project database")
        );
        drop(first);
        let second = acquire_instance_lock(&state_path).unwrap();
        drop(second);
        let _ = std::fs::remove_file(state_path.with_extension("lock"));
    }

    #[cfg(unix)]
    #[test]
    fn recorded_process_group_termination_stops_orphaned_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        assert!(child.try_wait().unwrap().is_none());

        terminate_process_group_by_pid(pid);

        let status = child.wait().unwrap();
        assert!(!status.success());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_reaps_matching_process_before_recovering_mailbox() {
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; sleep 30"]);
        configure_process_group(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let (start_ticks, process_group, session_id) = process_identity(pid).unwrap();
        let process = core::ActiveProcess {
            run_id: "run-startup-recovery".into(),
            job_id: String::new(),
            pid,
            start_ticks: Some(start_ticks),
            process_group: Some(process_group),
            session_id: Some(session_id),
            executable: "sh".into(),
        };
        let db = core::StateStore::in_memory().unwrap();
        let project = db.create_project("test", "source", "destination").unwrap();
        let job = db
            .add_mailbox(&project.id, "source", "destination")
            .unwrap();
        let mut process = process;
        process.job_id = job.clone();
        db.begin_run(&project.id, &job, &process.run_id, "test")
            .unwrap();
        db.register_process(&process).unwrap();

        assert!(recorded_process_matches(&process));
        terminate_recorded_process_group(&process);
        assert_eq!(db.recover_abandoned_jobs().unwrap(), 1);
        assert_eq!(
            db.mailbox_state(&job).unwrap().as_deref(),
            Some("attention")
        );
        assert!(db.active_processes().unwrap().is_empty());
        assert_eq!(
            db.run_status(&process.run_id).unwrap().as_deref(),
            Some("abandoned")
        );
        assert!(!child.wait().unwrap().success());
        // The retry is only attempted after the matching process has been
        // reaped and recovery has changed the mailbox out of `running`.
        db.begin_run(&project.id, &job, "run-after-reap", "test")
            .unwrap();
        db.finish_run_for_mailbox(
            &project.id,
            &job,
            "run-after-reap",
            "cancelled",
            "cancelled",
            "test retry cleanup",
        )
        .unwrap();
    }

    #[test]
    fn recommended_action_prioritizes_interrupted_work() {
        assert_eq!(
            recommended_next_action(core::Phase::Preflight, true, 2, false),
            "Review Attention items before starting another migration."
        );
        assert_eq!(
            recommended_next_action(core::Phase::Preflight, true, 0, true),
            "A migration is running — monitor Activity or use Stop migration if you need to halt it."
        );
    }

    #[test]
    fn lifecycle_cannot_advance_when_terminal_durability_is_uncertain() {
        assert!(terminal_phase_advance_allowed(true, true, false));
        assert!(!terminal_phase_advance_allowed(true, false, false));
        assert!(!terminal_phase_advance_allowed(true, true, true));
        assert!(!terminal_phase_advance_allowed(false, true, false));
    }

    #[test]
    fn recommended_action_describes_phase_without_synthetic_readiness() {
        assert_eq!(
            recommended_next_action(core::Phase::Discovery, false, 0, false),
            "Create the project, then run a dry preflight against a test mailbox."
        );
        assert_eq!(
            recommended_next_action(core::Phase::Verification, true, 0, false),
            "Review evidence for each mailbox and export the verification report."
        );
    }

    #[test]
    fn dovecot_plain_tls_maps_to_dovecot_no() {
        let mut form = dovecot_form();
        form.profile.source_tls = "plain".into();
        let (_, args) = form.command(true);
        assert!(args.iter().any(|arg| arg == "imapc_ssl=no"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "ssl_client_require_valid_cert=yes")
        );
    }

    #[test]
    fn dovecot_encrypted_source_forces_certificate_validation() {
        let mut form = dovecot_form();
        form.profile.source_tls = "starttls".into();
        form.profile.dovecot_config = "/etc/dovecot/custom.conf".into();
        form.profile.source_ca_bundle = "/etc/company-ca.pem".into();
        let (_, args) = form.command(true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ssl_client_require_valid_cert=yes"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", "ssl_client_ca_file=/etc/company-ca.pem"])
        );
    }

    #[test]
    fn imapsync_trust_bundle_is_explicit_and_verification_stays_enabled() {
        let mut form = Form::default();
        form.profile.source_ca_bundle = "/etc/company ca.pem".into();
        form.profile.destination_ca_bundle = "/opt/customer trust/ca.pem".into();
        let args = form.args(true);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs1", "SSL_verify_mode=1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs1", "SSL_ca_file=/etc/company ca.pem"])
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("SSL_verify_mode=1 SSL_ca_file="))
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sslargs2", "SSL_verify_mode=1"])
        );
        assert!(
            args.windows(2)
                .any(|pair| { pair == ["--sslargs2", "SSL_ca_file=/opt/customer trust/ca.pem"] })
        );
    }

    #[test]
    fn dovecot_rejects_unenforced_certificate_pins() {
        let mut form = dovecot_form();
        form.profile.source_certificate_pin_sha256 = "ab".repeat(32);
        let error = form.validate_internal(false).unwrap_err();
        assert!(error.contains("Certificate pinning is not currently supported"));

        let mut form = dovecot_form();
        form.profile.destination_certificate_pin_sha256 = "cd".repeat(32);
        let error = form.validate_internal(false).unwrap_err();
        assert!(error.contains("Certificate pinning is not currently supported"));
    }

    #[test]
    fn trust_settings_change_the_preflight_fingerprint() {
        let form = Form::default();
        let original = form.plan_fingerprint();
        let mut changed = form.clone();
        changed.profile.source_ca_bundle = "/etc/company-ca.pem".into();
        assert_ne!(original, changed.plan_fingerprint());
        changed.profile.source_ca_bundle.clear();
        changed.profile.destination_certificate_pin_sha256 = "ab".repeat(32);
        assert_ne!(original, changed.plan_fingerprint());
    }

    #[test]
    fn plan_identity_binds_executable_and_trust_bundle_contents() {
        let executable =
            std::env::temp_dir().join(format!("mailswiftsync-plan-executable-{}", Uuid::new_v4()));
        let ca_bundle =
            std::env::temp_dir().join(format!("mailswiftsync-plan-ca-{}", Uuid::new_v4()));
        std::fs::write(&executable, b"engine version one").unwrap();
        std::fs::write(&ca_bundle, b"-----BEGIN CERTIFICATE-----\none").unwrap();

        let mut form = Form::default();
        form.profile.imapsync_path = executable.to_string_lossy().into_owned();
        form.profile.source_ca_bundle = ca_bundle.to_string_lossy().into_owned();
        let original = form.plan_fingerprint();
        let snapshot = form.plan_snapshot();
        assert!(snapshot.contains("execution_executable_sha256 = \"sha256:"));
        assert!(snapshot.contains("source_ca_bundle_sha256 = \"sha256:"));

        std::fs::write(&executable, b"engine version two").unwrap();
        assert_ne!(original, form.plan_fingerprint());
        let executable_changed = form.plan_fingerprint();

        std::fs::write(&ca_bundle, b"-----BEGIN CERTIFICATE-----\ntwo").unwrap();
        assert_ne!(executable_changed, form.plan_fingerprint());

        let _ = std::fs::remove_file(executable);
        let _ = std::fs::remove_file(ca_bundle);
    }

    #[test]
    fn durable_preflight_rejects_same_path_runtime_replacements() {
        let executable = std::env::temp_dir().join(format!(
            "mailswiftsync-durable-plan-executable-{}",
            Uuid::new_v4()
        ));
        let ca_bundle =
            std::env::temp_dir().join(format!("mailswiftsync-durable-plan-ca-{}", Uuid::new_v4()));
        std::fs::write(&executable, b"engine version one").unwrap();
        std::fs::write(&ca_bundle, b"trust bundle one").unwrap();

        let mut form = Form::default();
        form.profile.imapsync_path = executable.to_string_lossy().into_owned();
        form.profile.source_ca_bundle = ca_bundle.to_string_lossy().into_owned();
        let planned_digest = plan_fingerprint_digest(&form.plan_fingerprint());

        let store = core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("runtime-identity", "source.example", "destination.example")
            .unwrap();
        let job = store
            .add_mailbox(&project.id, "source@example", "destination@example")
            .unwrap();
        store.set_preflight_plan(&job, &planned_digest).unwrap();

        std::fs::write(&executable, b"engine version two").unwrap();
        std::fs::write(&ca_bundle, b"trust bundle two").unwrap();
        let current_digest = plan_fingerprint_digest(&form.plan_fingerprint());
        assert_ne!(planned_digest, current_digest);
        assert_ne!(
            store.preflight_plan(&job).unwrap().as_deref(),
            Some(current_digest.as_str())
        );

        let _ = std::fs::remove_file(executable);
        let _ = std::fs::remove_file(ca_bundle);
    }

    #[test]
    fn plain_source_requires_explicit_live_transport_ack_and_binds_plan() {
        let mut form = dovecot_form();
        form.profile.source_tls = "plain".into();
        assert!(form.requires_insecure_transport_ack());
        assert!(form.validate().is_err());
        let without_ack = form.plan_fingerprint();
        form.profile.allow_insecure_source_transport = true;
        assert!(!form.requires_insecure_transport_ack());
        assert!(form.validate().is_ok());
        assert_ne!(without_ack, form.plan_fingerprint());
    }

    #[test]
    fn imapsync_plain_source_disables_implicit_ssl_and_starttls() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.source_tls = "plain".into();
        let args = engine::imapsync_args(&form.profile, "source", "destination", true, true, 1);

        assert!(args.iter().any(|arg| arg == "--nossl1"));
        assert!(args.iter().any(|arg| arg == "--notls1"));
        assert!(!args.iter().any(|arg| arg == "--ssl1"));
        assert!(!args.iter().any(|arg| arg == "--tls1"));
    }

    #[test]
    fn imapsync_oauth_mode_uses_xoauth2_without_bearer_token_in_preview() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.source_auth = "oauth2".into();
        form.profile.destination_auth = "oauth2".into();
        let args = engine::imapsync_args(
            &form.profile,
            "source-access-token",
            "destination-access-token",
            true,
            true,
            1,
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--authmech1", "XOAUTH2"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--authmech2", "XOAUTH2"])
        );
        assert!(args.iter().any(|arg| arg == "--oauthaccesstoken1"));
        assert!(args.iter().any(|arg| arg == "--oauthaccesstoken2"));
        assert!(!args.iter().any(|arg| arg.contains("access-token")));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--password1" || arg == "--password2")
        );
    }

    #[test]
    fn imapsync_argument_builder_never_materializes_runtime_credentials() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        let args = engine::imapsync_args(
            &form.profile,
            "source-secret",
            "destination-secret",
            false,
            false,
            1,
        );
        assert!(!args.iter().any(|arg| arg.contains("source-secret")));
        assert!(!args.iter().any(|arg| arg.contains("destination-secret")));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--password1", "••••••••"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--password2", "••••••••"])
        );
    }

    #[test]
    fn imapsync_oauth_runtime_uses_private_token_files() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.source_auth = "oauth2".into();
        form.profile.destination_auth = "oauth2".into();
        form.source_password = String::from("source-access-token").into();
        form.destination_password = String::from("destination-access-token").into();
        let prepared = form.prepared_command().unwrap();
        let source_index = prepared
            .args
            .iter()
            .position(|arg| arg == "--oauthaccesstoken1")
            .unwrap();
        let destination_index = prepared
            .args
            .iter()
            .position(|arg| arg == "--oauthaccesstoken2")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&prepared.args[source_index + 1]).unwrap(),
            "source-access-token"
        );
        assert_eq!(
            std::fs::read_to_string(&prepared.args[destination_index + 1]).unwrap(),
            "destination-access-token"
        );
        assert!(!prepared.args.iter().any(|arg| arg.contains("access-token")));
    }

    #[test]
    fn xoauth2_payload_uses_rfc_7628_shape() {
        let payload =
            BASE64_STANDARD.decode(xoauth2_payload("user@example.test", "token").as_bytes());
        assert_eq!(
            payload.unwrap(),
            b"user=user@example.test\x01auth=Bearer token\x01\x01"
        );
    }

    #[test]
    fn imapsync_runtime_plan_uses_ephemeral_passfiles() {
        let mut form = dovecot_form();
        form.profile.engine = core::Engine::ImapSync;
        let prepared = form.prepared_command().unwrap();
        assert!(!prepared.args.contains(&"--password1".into()));
        assert!(!prepared.args.contains(&"--password2".into()));
        let source_index = prepared
            .args
            .iter()
            .position(|arg| arg == "--passfile1")
            .unwrap();
        let destination_index = prepared
            .args
            .iter()
            .position(|arg| arg == "--passfile2")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&prepared.args[source_index + 1]).unwrap(),
            "secret"
        );
        assert_eq!(
            std::fs::read_to_string(&prepared.args[destination_index + 1]).unwrap(),
            "unused"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&prepared.args[source_index + 1])
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(prepared.env.is_empty());
        assert!(!prepared.args.iter().any(|arg| arg == "secret"));
        assert!(prepared.args.iter().any(|arg| arg == "--ssl1"));
        assert!(prepared.args.iter().any(|arg| arg == "--ssl2"));
        assert!(
            prepared
                .args
                .windows(2)
                .any(|pair| { pair == ["--sslargs1", "SSL_verify_mode=1"] })
        );
        assert!(
            prepared
                .args
                .windows(2)
                .any(|pair| { pair == ["--sslargs2", "SSL_verify_mode=1"] })
        );
        cleanup_paths(&prepared.cleanup);
        assert!(!std::path::Path::new(&prepared.args[source_index + 1]).exists());
    }

    #[test]
    fn imapsync_summary_becomes_durable_evidence_input() {
        let lines = [
            "Host1 Nb folders: 3 folders".into(),
            "Host2 Nb folders: 3 folders".into(),
            "Host1 Nb messages: 42 messages".into(),
            "Host2 Nb messages: 42 messages".into(),
            "Host1 Total size: 1000 bytes".into(),
            "Host2 Total size: 1000 bytes".into(),
            "The sync looks good, all 42 identified messages in host1 are on host2.".into(),
            "Detected 0 errors".into(),
        ];
        let evidence = verification::parse_imapsync_evidence(&lines).unwrap();
        assert_eq!(evidence.confidence_percent(), 100);
        assert_eq!(evidence.source_messages, 42);
    }

    #[test]
    fn incomplete_imapsync_summary_is_not_evidence() {
        assert!(verification::parse_imapsync_evidence(&["Detected 0 errors".into()]).is_none());
    }

    #[test]
    fn imapsync_parser_ignores_unrelated_detected_lines() {
        let lines = [
            "Host1 Nb folders: 1 folders".into(),
            "Host2 Nb folders: 1 folders".into(),
            "Host1 Nb messages: 2 messages".into(),
            "Host2 Nb messages: 2 messages".into(),
            "Host1 Total size: 100 bytes".into(),
            "Host2 Total size: 100 bytes".into(),
            "Detected 17 folders during namespace discovery".into(),
            "The sync looks good, all 2 identified messages in host1 are on host2.".into(),
            "Detected 0 errors".into(),
        ];
        let evidence = verification::parse_imapsync_evidence(&lines).unwrap();
        assert_eq!(evidence.failed_messages, 0);
    }

    #[test]
    fn dovecot_status_aggregates_mailbox_evidence() {
        let source = vec![
            "INBOX messages=10 vsize=100".into(),
            "Archive messages=2 vsize=50".into(),
        ];
        let destination = vec![
            "INBOX messages=10 vsize=100".into(),
            "Archive messages=2 vsize=50".into(),
        ];
        let evidence = verification::parse_dovecot_evidence(&source, &destination).unwrap();
        assert_eq!(evidence.source_folders, 2);
        assert_eq!(evidence.source_messages, 12);
        assert_eq!(evidence.confidence_percent(), 85);
    }

    #[test]
    fn live_auth_proof_requires_matching_plan_and_credentials() {
        let proof = LiveAuthProof {
            plan_fingerprint: "plan-a".into(),
            credential_fingerprint: "credentials-a".into(),
        };

        assert!(proof.matches("plan-a", "credentials-a"));
        assert!(!proof.matches("plan-b", "credentials-a"));
        assert!(!proof.matches("plan-a", "credentials-b"));
    }

    #[test]
    fn fresh_imap_authentication_applies_to_encrypted_transports() {
        let form = dovecot_form();
        assert!(!fresh_imap_authentication_applies(&form));

        let mut form = Form::default();
        form.profile.source_tls = "starttls".into();
        form.profile.destination_tls = "imaps".into();
        form.dry_run = false;
        assert!(fresh_imap_authentication_applies(&form));
        form.profile.destination_tls = "starttls".into();
        assert!(fresh_imap_authentication_applies(&form));
        form.profile.source_tls = "plain".into();
        assert!(!fresh_imap_authentication_applies(&form));
    }

    #[test]
    fn ui_scale_cycles_through_readable_operator_presets() {
        assert_eq!(next_ui_scale(0.90), 1.00);
        assert_eq!(next_ui_scale(1.10), 1.25);
        assert_eq!(next_ui_scale(1.50), 0.90);
    }

    #[test]
    fn semantic_theme_text_colors_meet_normal_text_contrast_target() {
        for colors in [ThemeColors::dark(), ThemeColors::light()] {
            for foreground in [
                colors.text_primary,
                colors.text_secondary,
                colors.info,
                colors.success,
                colors.warning,
                colors.danger,
                colors.link,
            ] {
                assert!(
                    contrast_ratio(foreground, colors.background) >= 4.5,
                    "foreground {:?} does not meet contrast target",
                    foreground
                );
            }
        }
    }

    #[test]
    fn password_reveal_is_not_allowed_when_controls_are_locked() {
        assert!(password_reveal_allowed(true, true));
        assert!(!password_reveal_allowed(false, true));
        assert!(!password_reveal_allowed(false, false));
    }

    #[test]
    fn displayed_batch_states_map_to_durable_retry_keys() {
        assert_eq!(display_state_key("Failed"), "failed");
        assert_eq!(display_state_key("Delta required"), "delta_required");
        assert_eq!(
            display_state_key("Verification difference"),
            "verification_difference"
        );
    }

    #[test]
    fn headless_batch_success_requires_verified_terminal_states() {
        assert!(is_verified_terminal_state("verified"));
        assert!(is_verified_terminal_state("verified_with_exceptions"));
        assert!(!is_verified_terminal_state("ready"));
        assert!(!is_verified_terminal_state("completed"));
        assert!(!is_verified_terminal_state("delta_required"));
    }

    #[test]
    fn run_status_severity_is_typed_and_independent_of_display_text() {
        assert_eq!(
            successful_run_severity(false, false, Some("verified")),
            StatusSeverity::Success
        );
        assert_eq!(
            successful_run_severity(false, false, Some("delta_required")),
            StatusSeverity::Warning
        );
        assert_eq!(
            successful_run_severity(false, true, None),
            StatusSeverity::Warning
        );
        assert_eq!(
            successful_run_severity(true, false, None),
            StatusSeverity::Success
        );
    }

    #[test]
    fn mailbox_state_badges_are_semantic_and_not_uniform() {
        assert_eq!(
            job_state_badge("verified", ThemeColors::dark()).0,
            "✓ Verified"
        );
        assert_eq!(job_state_badge("failed", ThemeColors::dark()).0, "× Failed");
        assert_ne!(
            job_state_badge("verified", ThemeColors::dark()).1,
            job_state_badge("failed", ThemeColors::dark()).1
        );
        assert_eq!(
            job_state_badge("retrying", ThemeColors::dark()).0,
            "↻ Retrying"
        );
    }

    #[test]
    fn headless_status_is_secret_free_and_reports_durable_mailboxes() {
        let directory =
            std::env::temp_dir().join(format!("mailswiftsync-headless-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let state = directory.join("state.db");
        let db = core::StateStore::open(&state).unwrap();
        let project = db
            .create_project_with_mailbox(
                "headless-status",
                "source.example",
                "destination.example",
                "source-user",
                "destination-user",
            )
            .unwrap()
            .0;
        drop(db);

        // Read-only status must remain available while a controller owns the
        // exclusive application lock.
        let _writer_lock = acquire_instance_lock(&state).unwrap();
        let status = headless_status(&state, Some(&project.id)).unwrap();
        assert_eq!(status.schema_version, core::CURRENT_SCHEMA_VERSION);
        assert_eq!(status.projects.len(), 1);
        assert_eq!(status.projects[0].mailboxes.len(), 1);
        assert_eq!(status.projects[0].mailboxes[0].state, "queued");
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("password"));
        let summary = headless::headless_status_summary(&state, Some(&project.id)).unwrap();
        assert_eq!(summary.projects[0].mailbox_state_counts.total, 1);
        assert_eq!(summary.projects[0].mailbox_state_counts.ready, 0);
        let summary_json = serde_json::to_string(&summary).unwrap();
        assert!(!summary_json.contains("mailboxes"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn durable_state_path_never_falls_back_to_temporary_storage() {
        assert_eq!(
            persistent_state_path_from(
                Some(OsString::from("/var/lib/mailswiftsync/state.db")),
                None,
            )
            .unwrap(),
            PathBuf::from("/var/lib/mailswiftsync/state.db")
        );
        assert_eq!(
            persistent_state_path_from(None, Some(PathBuf::from("/home/operator/.local/share")))
                .unwrap(),
            PathBuf::from("/home/operator/.local/share/mailswiftsync/state.db")
        );
        let missing_directory = persistent_state_path_from(None, None).unwrap_err();
        assert!(missing_directory.contains("durable state directory"));
        let empty_override = persistent_state_path_from(Some(OsString::new()), None).unwrap_err();
        assert!(empty_override.contains("set but empty"));
    }
}
