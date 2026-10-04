mod atomic_artifact;
mod bootstrap;
mod branding;
mod bulk_import;
mod cli;
mod command;
mod completions;
mod controller;
mod core;
mod credentials;
mod diagnostic_log;
mod doctor;
mod endpoint;
mod engine;
mod engine_install;
mod extra_options;
mod headless;
mod imap_probe;
mod imap_protocol;
mod maintenance_window;
mod migrate_audit;
mod migration_plan;
mod oauth;
mod oauth_authorize;
mod oauth_onboarding;
mod oauth_redirect;
mod oauth_refresh;
mod organization_policy;
mod output;
mod plan_identity;
mod process;
mod progress;
mod provider;
mod reports;
mod runner;
mod storage;
mod storage_paths;
mod ui;
mod verification;
mod webhook;
#[cfg(windows)]
mod windows_private;

#[cfg(test)]
use atomic_artifact::write_private_atomic;
#[cfg(test)]
use command::{parse_shell_words, remove_option};
#[cfg(test)]
use controller::batch_admission::canonical_destination_identity;
#[cfg(test)]
use controller::batch_admission::duplicate_destination;
#[cfg(test)]
use controller::batch_admission::selection_value;
#[cfg(test)]
use controller::batch_admission::{durable_batch_profile_config, validate_batch_throttle};
use controller::failure::{FailureClass, terminal_phase_advance_allowed};
#[cfg(test)]
use controller::failure::{classified_failure_detail, classify_failure};
#[cfg(test)]
use controller::failure::{
    is_transient_batch_error, should_retry_batch_error, transient_retry_delay,
};
use controller::{
    ActiveRunContext, BatchExecutionMode, BulkRetryScope, CapabilityProbeResult, LiveAuthProof,
    PendingDbEvent, RunKind, SingleRunAdmission, SingleRunWorkerSpec, SingleStartContext,
    SingleStartDecision, admit_single_run, batch_mailbox_state, capability_probe_result_matches,
    decode_persisted_batch_profile, durable_single_identity_matches, finish_batch_child,
    is_verified_terminal_state, persist_pending_events, process_event_is_current,
    run_line_is_current, single_start_decision, spawn_single_run_worker,
};
pub(crate) use controller::{Event, StreamOutcome};
#[cfg(test)]
use credentials::CleanupGuard;
use credentials::{
    SecretString, cleanup_paths, cleanup_reconciled_secret_directories,
    cleanup_stale_secret_directories, create_secret_directory, secret_runtime_base,
    write_secret_file,
};
pub(crate) use diagnostic_log::DiagnosticLogger;
use headless::export_support_bundle;
#[cfg(test)]
use headless::headless_status;
use output::BoundedLineBuffer;
#[cfg(test)]
use process::ProcessLaunchLimiter;
#[cfg(all(test, unix))]
use process::terminate_process_group_by_pid;
use process::{
    InstanceLock, acquire_instance_lock, recorded_process_is_gone, recorded_process_matches,
    terminate_recorded_process_group,
};
#[cfg(all(test, unix))]
use process::{configure_process_group, process_identity};
use provider::ProviderPreset;
#[cfg(all(test, unix))]
use runner::{RunContext, run_streaming};
#[cfg(test)]
use runner::{dovecot_state_candidate, record_process_tail};
#[cfg(all(test, unix))]
use std::process::Command;
pub(crate) use ui::App;

use eframe::egui;
#[cfg(test)]
use imap_probe::{
    command_endpoint_parts, command_port, endpoint_for_probe, fresh_imap_authentication_applies,
    imap_command_succeeded, imap_quote,
};
#[cfg(test)]
use migration_plan::FolderMappingRule;
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
    AppearancePreferences, ThemeColors, ThemeKind, UiLanguage, WorkspaceSnapshot, WorkspaceView,
    markdown_escape, needs_operator_review, preferred_project_id, project_health_state_counts,
    push_visible_output, successful_run_severity, successful_run_status, truncate_utf8,
};
use ui::{StatusMessage, StatusSeverity};
#[cfg(test)]
use ui::{contrast_ratio, next_ui_scale, password_reveal_allowed};

const MAX_VISIBLE_OUTPUT_LINES: usize = 10_000;
const MAX_VISIBLE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROCESS_TAIL_LINES: usize = 200;
const MAX_PROCESS_TAIL_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;
pub const MAX_BULK_IMPORT_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_BULK_IMPORT_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_BULK_IMPORT_ARCHIVE_ENTRIES: usize = 4_096;
pub const MAX_BULK_IMPORT_ROWS: usize = 100_000;
pub const MAX_BULK_IMPORT_COLUMNS: usize = 64;
pub const MAX_BULK_IMPORT_CELL_BYTES: usize = 64 * 1024;
pub const MAX_BULK_IMPORT_WORKSHEET_CELLS: usize = 1_000_000;
pub const MAX_BULK_IMPORT_SHARED_STRINGS: usize = 1_000_000;
pub const MAX_BULK_IMPORT_SHARED_STRING_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_BULK_IMPORT_STYLES_BYTES: u64 = 8 * 1024 * 1024;
const DOVECOT_SYNC_LOCK_WAIT_SECONDS: u64 = 300;
const MAX_PENDING_EVENTS: usize = 4_096;
const MAX_ACTIVITY_HISTORY_ROWS: u32 = 250;
// Keep one unusually noisy worker from monopolising an egui frame.
const MAX_EVENTS_PER_FRAME: usize = 250;
const DEFAULT_UI_SCALE: f32 = 1.10;
const MIN_UI_SCALE: f32 = 0.90;
const MAX_UI_SCALE: f32 = 2.00;

#[cfg(test)]
pub(crate) fn is_github_hosted_runner(environment: Option<&str>) -> bool {
    environment == Some("github-hosted")
}

#[cfg(test)]
use bulk_import::BulkJob;
use bulk_import::{BulkImportResult, PendingSheetImport};

fn main() -> eframe::Result<()> {
    cli::run()
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
