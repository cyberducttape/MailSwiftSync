//! Durable profile data and stable defaults.
//!
//! Keeping the serialized profile separate from credentials and command
//! preparation makes it possible to audit what is configuration identity and
//! what is ephemeral execution material.

use crate::core;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(crate) struct RunPlanSnapshot {
    pub(crate) dry_run: bool,
    pub(crate) profile: RunProfileSnapshot,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RunProfileSnapshot {
    pub(crate) name: String,
    pub(crate) source_host: String,
    pub(crate) source_port: String,
    pub(crate) source_tls: String,
    pub(crate) source_ca_bundle: String,
    pub(crate) source_certificate_pin_sha256: String,
    pub(crate) allow_insecure_source_transport: bool,
    pub(crate) source_user: String,
    #[serde(default = "default_auth_method")]
    pub(crate) source_auth: String,
    pub(crate) source_credential_id: String,
    #[serde(default)]
    pub(crate) source_oauth_refresh_credential_id: String,
    pub(crate) destination_host: String,
    pub(crate) destination_user: String,
    #[serde(default = "default_auth_method")]
    pub(crate) destination_auth: String,
    pub(crate) destination_credential_id: String,
    #[serde(default)]
    pub(crate) destination_oauth_refresh_credential_id: String,
    pub(crate) destination_port: String,
    pub(crate) destination_tls: String,
    pub(crate) destination_ca_bundle: String,
    pub(crate) destination_certificate_pin_sha256: String,
    pub(crate) imapsync_path: String,
    pub(crate) engine: core::Engine,
    pub(crate) doveadm_path: String,
    pub(crate) dovecot_config: String,
    pub(crate) batch_concurrency: usize,
    pub(crate) batch_retry_count: usize,
    pub(crate) max_messages_per_second: u32,
    pub(crate) max_bytes_per_second: u64,
    pub(crate) migration_timeout_hours: u64,
    pub(crate) automap: bool,
    pub(crate) addheader: bool,
    pub(crate) justfolders: bool,
    #[serde(default = "default_sync_internaldates")]
    pub(crate) sync_internaldates: bool,
    pub(crate) useuid: bool,
    pub(crate) usecache: bool,
    pub(crate) fastio1: bool,
    pub(crate) fastio2: bool,
    pub(crate) allowsizemismatch: bool,
    #[serde(default)]
    pub(crate) dovecot_strategy: DovecotMigrationStrategy,
    pub(crate) delete2: bool,
    pub(crate) extra_options_sha256: String,
    pub(crate) dovecot_checkpoint_sha256: Option<String>,
    #[serde(default)]
    pub(crate) execution_executable_sha256: String,
    #[serde(default)]
    pub(crate) source_ca_bundle_sha256: String,
    #[serde(default)]
    pub(crate) destination_ca_bundle_sha256: String,
    #[serde(default)]
    pub(crate) dovecot_config_sha256: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DovecotMigrationStrategy {
    InitialMirror,
    IncrementalMirror,
    #[default]
    FinalPreservationPass,
    DestinationAlreadyActive,
}

impl DovecotMigrationStrategy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::InitialMirror => "Initial mirror",
            Self::IncrementalMirror => "Incremental mirror",
            Self::FinalPreservationPass => "Final preservation pass",
            Self::DestinationAlreadyActive => "Destination already active",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::InitialMirror => {
                "doveadm backup: mirror source mail to the destination; destination-only changes may be replaced."
            }
            Self::IncrementalMirror => {
                "doveadm backup with the durable checkpoint: repeat an initial mirror before cutover."
            }
            Self::FinalPreservationPass => {
                "doveadm sync -1: preserve destination-side changes for the final cutover pass."
            }
            Self::DestinationAlreadyActive => {
                "Advanced preservation mode using doveadm sync -1; review merge behavior and Dovecot load carefully."
            }
        }
    }

    pub(crate) fn uses_preservation_sync(self) -> bool {
        matches!(
            self,
            Self::FinalPreservationPass | Self::DestinationAlreadyActive
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Profile {
    pub(crate) name: String,
    pub(crate) source_host: String,
    #[serde(default)]
    pub(crate) source_port: String,
    #[serde(default = "default_source_tls")]
    pub(crate) source_tls: String,
    #[serde(default)]
    pub(crate) source_ca_bundle: String,
    #[serde(default)]
    pub(crate) source_certificate_pin_sha256: String,
    #[serde(default)]
    pub(crate) allow_insecure_source_transport: bool,
    pub(crate) source_user: String,
    #[serde(default = "default_auth_method")]
    pub(crate) source_auth: String,
    #[serde(default)]
    pub(crate) source_credential_id: String,
    #[serde(default)]
    pub(crate) source_oauth_refresh_credential_id: String,
    pub(crate) destination_host: String,
    pub(crate) destination_user: String,
    #[serde(default = "default_auth_method")]
    pub(crate) destination_auth: String,
    #[serde(default)]
    pub(crate) destination_credential_id: String,
    #[serde(default)]
    pub(crate) destination_oauth_refresh_credential_id: String,
    #[serde(default)]
    pub(crate) destination_port: String,
    #[serde(default = "default_destination_tls")]
    pub(crate) destination_tls: String,
    #[serde(default)]
    pub(crate) destination_ca_bundle: String,
    #[serde(default)]
    pub(crate) destination_certificate_pin_sha256: String,
    pub(crate) imapsync_path: String,
    #[serde(default)]
    pub(crate) engine: core::Engine,
    #[serde(default = "default_doveadm_path")]
    pub(crate) doveadm_path: String,
    #[serde(default)]
    pub(crate) dovecot_config: String,
    #[serde(default = "default_batch_concurrency")]
    pub(crate) batch_concurrency: usize,
    #[serde(default)]
    pub(crate) batch_retry_count: usize,
    #[serde(default)]
    pub(crate) max_messages_per_second: u32,
    #[serde(default)]
    pub(crate) max_bytes_per_second: u64,
    #[serde(default = "default_migration_timeout_hours")]
    pub(crate) migration_timeout_hours: u64,
    pub(crate) automap: bool,
    pub(crate) addheader: bool,
    pub(crate) justfolders: bool,
    #[serde(default = "default_sync_internaldates")]
    pub(crate) sync_internaldates: bool,
    pub(crate) useuid: bool,
    pub(crate) usecache: bool,
    pub(crate) fastio1: bool,
    pub(crate) fastio2: bool,
    pub(crate) allowsizemismatch: bool,
    #[serde(default)]
    pub(crate) dovecot_strategy: DovecotMigrationStrategy,
    pub(crate) delete2: bool,
    pub(crate) extra_options: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: String::new(),
            source_host: String::new(),
            source_port: String::new(),
            source_tls: default_source_tls(),
            source_ca_bundle: String::new(),
            source_certificate_pin_sha256: String::new(),
            allow_insecure_source_transport: false,
            source_user: String::new(),
            source_auth: default_auth_method(),
            source_credential_id: String::new(),
            source_oauth_refresh_credential_id: String::new(),
            destination_host: String::new(),
            destination_user: String::new(),
            destination_auth: default_auth_method(),
            destination_credential_id: String::new(),
            destination_oauth_refresh_credential_id: String::new(),
            destination_port: String::new(),
            destination_tls: default_destination_tls(),
            destination_ca_bundle: String::new(),
            destination_certificate_pin_sha256: String::new(),
            imapsync_path: "imapsync".into(),
            engine: core::Engine::default(),
            doveadm_path: default_doveadm_path(),
            dovecot_config: String::new(),
            batch_concurrency: default_batch_concurrency(),
            batch_retry_count: 0,
            max_messages_per_second: 0,
            max_bytes_per_second: 0,
            migration_timeout_hours: default_migration_timeout_hours(),
            automap: true,
            addheader: false,
            justfolders: false,
            sync_internaldates: default_sync_internaldates(),
            useuid: false,
            usecache: false,
            fastio1: false,
            fastio2: false,
            allowsizemismatch: false,
            dovecot_strategy: DovecotMigrationStrategy::default(),
            delete2: false,
            extra_options: String::new(),
        }
    }
}

pub(crate) fn default_doveadm_path() -> String {
    "doveadm".into()
}
pub(crate) fn default_batch_concurrency() -> usize {
    2
}
pub(crate) fn default_migration_timeout_hours() -> u64 {
    24
}
pub(crate) fn default_source_tls() -> String {
    "imaps".into()
}
pub(crate) fn default_auth_method() -> String {
    "password".into()
}
pub(crate) fn auth_method_is_oauth(method: &str) -> bool {
    method == "oauth2"
}
pub(crate) fn default_destination_tls() -> String {
    "imaps".into()
}

pub(crate) fn default_sync_internaldates() -> bool {
    true
}

pub(crate) fn completeness(profile: &Profile) -> (usize, usize) {
    let passed = [
        !profile.source_host.trim().is_empty(),
        !profile.destination_host.trim().is_empty(),
        !profile.source_user.trim().is_empty(),
        !profile.destination_user.trim().is_empty(),
    ]
    .into_iter()
    .filter(|ok| *ok)
    .count();
    (passed, 4)
}
