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

/// A deterministic, engine-owned folder rule. Exact mappings and exclusions
/// are used by both imapsync and the independent verifier; arbitrary regex
/// transforms remain intentionally outside the typed plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FolderMappingRule {
    pub(crate) source: String,
    #[serde(default)]
    pub(crate) destination: String,
    #[serde(default)]
    pub(crate) exclude: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct RunProfileSnapshot {
    #[serde(default = "default_execution_profile")]
    pub(crate) execution_profile: String,
    pub(crate) name: String,
    pub(crate) source_host: String,
    pub(crate) source_port: String,
    pub(crate) source_tls: String,
    pub(crate) source_ca_bundle: String,
    pub(crate) source_certificate_pin_sha256: String,
    pub(crate) allow_insecure_source_transport: bool,
    pub(crate) source_user: String,
    #[serde(default)]
    pub(crate) source_rate_tenant: String,
    #[serde(default = "default_auth_method")]
    pub(crate) source_auth: String,
    pub(crate) source_credential_id: String,
    #[serde(default)]
    pub(crate) source_oauth_refresh_credential_id: String,
    pub(crate) destination_host: String,
    pub(crate) destination_user: String,
    #[serde(default)]
    pub(crate) destination_rate_tenant: String,
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
    #[serde(default = "default_batch_process_starts_per_second")]
    pub(crate) batch_process_starts_per_second: usize,
    pub(crate) max_messages_per_second: u32,
    pub(crate) max_bytes_per_second: u64,
    #[serde(default)]
    pub(crate) body_hash_verification: bool,
    #[serde(default = "default_body_hash_max_bytes")]
    pub(crate) body_hash_max_bytes: u64,
    #[serde(default = "default_body_hash_max_total_bytes")]
    pub(crate) body_hash_max_total_bytes: u64,
    pub(crate) migration_timeout_hours: u64,
    pub(crate) automap: bool,
    #[serde(default)]
    pub(crate) folder_mapping_rules: Vec<FolderMappingRule>,
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
    /// `DestinationMutationPolicy::as_str` of the plan, recorded so audit
    /// artifacts state what the run was allowed to do to the destination.
    /// Empty in snapshots written before the field existed.
    #[serde(default)]
    pub(crate) destination_mutation_policy: String,
}

fn default_execution_profile() -> String {
    "hardened".into()
}

impl RunProfileSnapshot {
    /// The recorded policy, or the policy derived from the recorded engine,
    /// strategy, and options for snapshots that predate the field.
    pub(crate) fn destination_mutation_policy(&self) -> &'static str {
        if let Some(policy) = DestinationMutationPolicy::parse(&self.destination_mutation_policy) {
            return policy.as_str();
        }
        DestinationMutationPolicy::derive(self.engine, self.dovecot_strategy, self.delete2).as_str()
    }
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
                "doveadm backup: mirror source mail to the destination; destination-only changes may be replaced. Dovecot may need to replace INBOX, which some Maildir targets refuse; test the exact target storage before migration."
            }
            Self::IncrementalMirror => {
                "doveadm backup with the durable checkpoint: repeat an initial mirror before cutover. Maildir targets may refuse INBOX replacement; test the exact storage format."
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

/// What a live run may do to state that already exists on the destination.
/// This is the single safety model for review, confirmation, batch plans,
/// reports, and CLI warnings; it is derived from the complete plan (engine,
/// Dovecot strategy, and imapsync options), never from one engine flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DestinationMutationPolicy {
    /// imapsync without `--delete2`: copies and updates messages; never
    /// removes destination-only mail.
    Additive,
    /// `doveadm sync -1`: one-way merge that preserves destination changes.
    MergePreservingDestination,
    /// `doveadm backup`: forces the destination to match the source and may
    /// remove or replace destination-only messages and mailboxes.
    MirrorMayRemoveDestinationState,
    /// imapsync `--delete2`: removes destination messages missing from the
    /// source.
    ExplicitDeleteMissingSourceMessages,
}

impl DestinationMutationPolicy {
    /// Derive the policy from the plan elements that decide it.
    pub(crate) fn derive(
        engine: core::Engine,
        strategy: DovecotMigrationStrategy,
        delete2: bool,
    ) -> Self {
        match engine {
            core::Engine::Dovecot if strategy.uses_preservation_sync() => {
                Self::MergePreservingDestination
            }
            // `doveadm backup` ignores --delete2; the mirror itself removes.
            core::Engine::Dovecot => Self::MirrorMayRemoveDestinationState,
            _ if delete2 => Self::ExplicitDeleteMissingSourceMessages,
            _ => Self::Additive,
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        [
            Self::Additive,
            Self::MergePreservingDestination,
            Self::MirrorMayRemoveDestinationState,
            Self::ExplicitDeleteMissingSourceMessages,
        ]
        .into_iter()
        .find(|policy| policy.as_str() == value)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Additive => "additive",
            Self::MergePreservingDestination => "merge_preserving_destination",
            Self::MirrorMayRemoveDestinationState => "mirror_may_remove_destination_state",
            Self::ExplicitDeleteMissingSourceMessages => "explicit_delete_missing_source_messages",
        }
    }

    /// Whether a live run can remove or replace mail or mailboxes that exist
    /// only on the destination. Such plans need explicit acknowledgement.
    pub(crate) fn may_remove_destination_state(self) -> bool {
        matches!(
            self,
            Self::MirrorMayRemoveDestinationState | Self::ExplicitDeleteMissingSourceMessages
        )
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Additive => "Additive copy",
            Self::MergePreservingDestination => "Merge preserving destination",
            Self::MirrorMayRemoveDestinationState => "Destination mirror",
            Self::ExplicitDeleteMissingSourceMessages => "Delete destination-only messages",
        }
    }

    /// One sentence describing the effect on existing destination state.
    pub(crate) fn warning(self) -> &'static str {
        match self {
            Self::Additive => "Additive copy — destination-only messages and mailboxes are kept.",
            Self::MergePreservingDestination => {
                "Merge preserving destination — destination-side changes are kept; review merge conflicts."
            }
            Self::MirrorMayRemoveDestinationState => {
                "Destination mirror mode — destination-only messages/mailboxes may be removed or replaced."
            }
            Self::ExplicitDeleteMissingSourceMessages => {
                "Destination deletion (--delete2) — destination messages missing from the source will be removed."
            }
        }
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
    /// Explicit provider tenant/account scope for adaptive batch scheduling.
    /// Empty falls back to the mailbox's email domain as a best-effort key.
    #[serde(default)]
    pub(crate) source_rate_tenant: String,
    #[serde(default = "default_auth_method")]
    pub(crate) source_auth: String,
    #[serde(default)]
    pub(crate) source_credential_id: String,
    #[serde(default)]
    pub(crate) source_oauth_refresh_credential_id: String,
    pub(crate) destination_host: String,
    pub(crate) destination_user: String,
    #[serde(default)]
    pub(crate) destination_rate_tenant: String,
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
    #[serde(default = "default_batch_process_starts_per_second")]
    pub(crate) batch_process_starts_per_second: usize,
    #[serde(default)]
    pub(crate) max_messages_per_second: u32,
    #[serde(default)]
    pub(crate) max_bytes_per_second: u64,
    /// Opt-in forensic verification mode. Disabled by default because it
    /// downloads every message body from both accounts.
    #[serde(default)]
    pub(crate) body_hash_verification: bool,
    #[serde(default = "default_body_hash_max_bytes")]
    pub(crate) body_hash_max_bytes: u64,
    #[serde(default = "default_body_hash_max_total_bytes")]
    pub(crate) body_hash_max_total_bytes: u64,
    #[serde(default = "default_migration_timeout_hours")]
    pub(crate) migration_timeout_hours: u64,
    pub(crate) automap: bool,
    #[serde(default)]
    pub(crate) folder_mapping_rules: Vec<FolderMappingRule>,
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

impl Profile {
    /// The engine a run actually uses. Auto is an explicit conservative
    /// default that resolves to imapsync, not environment detection.
    pub(crate) fn effective_engine(&self) -> core::Engine {
        match self.engine {
            core::Engine::Auto => core::Engine::ImapSync,
            selected => selected,
        }
    }

    /// The destination mutation policy of a live run of this plan.
    pub(crate) fn destination_mutation_policy(&self) -> DestinationMutationPolicy {
        DestinationMutationPolicy::derive(
            self.effective_engine(),
            self.dovecot_strategy,
            self.delete2,
        )
    }
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
            source_rate_tenant: String::new(),
            source_auth: default_auth_method(),
            source_credential_id: String::new(),
            source_oauth_refresh_credential_id: String::new(),
            destination_host: String::new(),
            destination_user: String::new(),
            destination_rate_tenant: String::new(),
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
            batch_process_starts_per_second: default_batch_process_starts_per_second(),
            max_messages_per_second: 0,
            max_bytes_per_second: 0,
            body_hash_verification: false,
            body_hash_max_bytes: default_body_hash_max_bytes(),
            body_hash_max_total_bytes: default_body_hash_max_total_bytes(),
            migration_timeout_hours: default_migration_timeout_hours(),
            // Keep the default plan eligible for independent metadata
            // verification. imapsync's automapping cannot currently be
            // replayed from an immutable mapping snapshot.
            automap: false,
            folder_mapping_rules: Vec::new(),
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
/// Normal desktop-control-plane ceiling. Higher values require a separately
/// qualified fleet runner rather than inviting users to multiply IMAP engines
/// from the GUI.
pub(crate) const MAX_BATCH_CONCURRENCY: usize = 32;
pub(crate) const MAX_BATCH_PROCESS_STARTS_PER_SECOND: usize = 100;
pub(crate) fn default_batch_process_starts_per_second() -> usize {
    2
}
pub(crate) fn effective_batch_concurrency(value: usize) -> usize {
    value.clamp(1, MAX_BATCH_CONCURRENCY)
}
pub(crate) fn effective_batch_process_starts_per_second(value: usize) -> usize {
    value.clamp(1, MAX_BATCH_PROCESS_STARTS_PER_SECOND)
}
pub(crate) fn default_migration_timeout_hours() -> u64 {
    24
}
pub(crate) fn default_body_hash_max_bytes() -> u64 {
    8 * 1024 * 1024
}
pub(crate) fn default_body_hash_max_total_bytes() -> u64 {
    512 * 1024 * 1024
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

#[cfg(test)]
mod destination_mutation_policy_tests {
    use super::{
        DestinationMutationPolicy as Policy, DovecotMigrationStrategy as Strategy, Profile,
    };
    use crate::core::Engine;

    fn profile(engine: Engine, strategy: Strategy, delete2: bool) -> Profile {
        Profile {
            engine,
            dovecot_strategy: strategy,
            delete2,
            ..Profile::default()
        }
    }

    /// The reviewer's case: `doveadm backup` with delete2 off must still be
    /// treated as able to remove destination-only state.
    #[test]
    fn dovecot_mirror_is_destructive_without_delete2() {
        for strategy in [Strategy::InitialMirror, Strategy::IncrementalMirror] {
            let policy = profile(Engine::Dovecot, strategy, false).destination_mutation_policy();
            assert_eq!(policy, Policy::MirrorMayRemoveDestinationState);
            assert!(policy.may_remove_destination_state());
        }
    }

    #[test]
    fn preservation_sync_and_additive_imapsync_keep_destination_state() {
        for strategy in [
            Strategy::FinalPreservationPass,
            Strategy::DestinationAlreadyActive,
        ] {
            // delete2 is an imapsync flag; doveadm sync -1 never uses it.
            let policy = profile(Engine::Dovecot, strategy, true).destination_mutation_policy();
            assert_eq!(policy, Policy::MergePreservingDestination);
            assert!(!policy.may_remove_destination_state());
        }
        for engine in [Engine::ImapSync, Engine::Auto] {
            let policy =
                profile(engine, Strategy::InitialMirror, false).destination_mutation_policy();
            assert_eq!(policy, Policy::Additive);
        }
    }

    #[test]
    fn imapsync_delete2_is_explicit_deletion() {
        for engine in [Engine::ImapSync, Engine::Auto] {
            let policy = profile(engine, Strategy::default(), true).destination_mutation_policy();
            assert_eq!(policy, Policy::ExplicitDeleteMissingSourceMessages);
            assert!(policy.may_remove_destination_state());
        }
    }

    #[test]
    fn policy_names_round_trip() {
        for policy in [
            Policy::Additive,
            Policy::MergePreservingDestination,
            Policy::MirrorMayRemoveDestinationState,
            Policy::ExplicitDeleteMissingSourceMessages,
        ] {
            assert_eq!(Policy::parse(policy.as_str()), Some(policy));
        }
        assert_eq!(Policy::parse(""), None);
    }
}
