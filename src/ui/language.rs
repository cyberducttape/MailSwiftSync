//! UI language selection backed by external, stable-key locale catalogs.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum UiLanguage {
    #[default]
    English,
    German,
}

impl UiLanguage {
    pub(crate) fn all() -> &'static [Self] {
        &[Self::English, Self::German]
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::German => "Deutsch (teilweise)",
        }
    }

    pub(crate) fn text(self, source: &'static str) -> &'static str {
        locale_catalog()
            .message(self, source)
            .or_else(|| self.lookup(source))
            .unwrap_or(source)
    }

    /// Translate fixed or runtime text using the external locale catalogs.
    /// Existing callers may pass English source copy; catalogs resolve it to
    /// a stable locale key. New UI code should use `message(key)`.
    pub(crate) fn lookup(self, source: &str) -> Option<&'static str> {
        if self == Self::English {
            return None;
        }
        locale_catalog().translate_source(source)
    }

    /// Resolve a stable locale key, falling back to the English catalog.
    pub(crate) fn message(self, key: &'static str) -> &'static str {
        locale_catalog().message(self, key).unwrap_or(key)
    }
}

#[derive(Deserialize)]
struct LocaleFile {
    meta: LocaleMetadata,
    messages: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct LocaleMetadata {
    language: String,
    display_name: String,
}

struct LocaleCatalog {
    source_keys: std::collections::HashMap<String, String>,
    english: std::collections::HashMap<String, String>,
    german: std::collections::HashMap<String, String>,
}

impl LocaleCatalog {
    fn load() -> Self {
        let english: LocaleFile = toml::from_str(include_str!("../../locales/en.toml"))
            .expect("built-in English locale catalog must be valid");
        let german: LocaleFile = toml::from_str(include_str!("../../locales/de.toml"))
            .expect("built-in German locale catalog must be valid");
        assert_eq!(english.meta.language, "en");
        assert_eq!(german.meta.language, "de");
        assert_eq!(english.meta.display_name, "English");
        assert_eq!(german.meta.display_name, "Deutsch (teilweise)");
        let source_keys = english
            .messages
            .iter()
            .map(|(key, value)| (value.clone(), key.clone()))
            .collect();
        Self {
            source_keys,
            english: english.messages,
            german: german.messages,
        }
    }

    fn translate_source(&'static self, source: &str) -> Option<&'static str> {
        let key = self.source_keys.get(source)?;
        self.german.get(key).map(String::as_str)
    }

    fn message(&'static self, language: UiLanguage, key: &str) -> Option<&'static str> {
        let catalog = match language {
            UiLanguage::English => &self.english,
            UiLanguage::German => &self.german,
        };
        catalog
            .get(key)
            .or_else(|| self.english.get(key))
            .map(String::as_str)
    }
}

fn locale_catalog() -> &'static LocaleCatalog {
    use std::sync::OnceLock;
    static CATALOG: OnceLock<LocaleCatalog> = OnceLock::new();
    CATALOG.get_or_init(LocaleCatalog::load)
}

#[cfg(test)]
mod tests {
    use super::UiLanguage;

    #[test]
    fn language_catalog_keeps_english_default_and_german_shell_labels() {
        assert_eq!(UiLanguage::English.text("Overview"), "Overview");
        assert_eq!(UiLanguage::German.text("Overview"), "Übersicht");
        assert_eq!(UiLanguage::English.text("ui.overview"), "Overview");
        assert_eq!(UiLanguage::German.text("ui.overview"), "Übersicht");
        assert_eq!(
            UiLanguage::German.text("mailbox.destination-destructive-warning"),
            "Nachrichten, die nur im Ziel vorhanden sind, können während der Live-Migration entfernt werden."
        );
        assert_eq!(UiLanguage::German.label(), "Deutsch (teilweise)");
        assert_eq!(
            UiLanguage::German.text("untranslated technical detail"),
            "untranslated technical detail"
        );
    }

    #[test]
    fn destructive_and_safety_paths_are_translated() {
        for key in [
            "Confirm live migration",
            "Migration still running",
            "This desktop owns the migration controller. Closing the window will interrupt the run and require recovery review; migrations cannot yet continue after the desktop exits.",
            "To stop safely, use Stop migration in Activity and wait for the run to finish before closing.",
            "Keep window open",
            "Go to Activity",
            "I understand — start migration",
            "Confirm live batch migration",
            "I understand — start batch",
            "Sample of selected mailboxes:",
            "View all selected",
            "Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.",
            "Confirmation stale: concurrency, scope, or settings changed while dialog was open. Review the queue and try again.",
            "Review selected ({})",
            "{} selected",
            "{} ready for pilot",
            "{} require operator review",
            "{} selected rows are unavailable in the loaded queue.",
            "Migration behavior",
            "Recorded verification inventory",
            "{} folders · {} messages · {}",
            "Recorded plan snapshot digest: {}",
            "View complete assessment",
            "No mailboxes selected.",
            "DESTRUCTIVE: destination deletion enabled",
            "Select one or more rows to enable actions.",
            "Operator action",
            "Inspect durable run detail",
            "Clear mailbox queue?",
            "Advanced migration options",
            "Review verification",
            "PROVIDER QUALIFICATION",
            "UNQUALIFIED",
            "COMMUNITY / UNQUALIFIED PATH",
            "The generic IMAP migration path is available, but this provider pair has not passed the MailSwiftSync production qualification suite.",
            "Last qualification: none · no live provider evidence is currently bundled.",
            "Qualification requires tested authentication, folder inventory and mapping, independent verification, interruption recovery, throttling recovery, and large-mailbox coverage.",
            "PRE-MIGRATION SIMULATION",
            "SCOPE",
            "RISKS",
            "Plan preview only — estimates are shown only when supported by observed data.",
            "SOURCE",
            "DESTINATION",
            "ENGINE",
            "MAPPING",
            "AUTH",
            "ESTIMATED SCALE",
            "PROPOSED EXECUTION",
            "Read-only; source messages are not deleted by default",
            "No destination writes are intended during preflight",
            "resolved from PATH",
            "Folder inventory: {} source · {} destination",
            "Message count and data volume are not known yet",
            "Authenticated in the latest readiness check",
            "Not yet verified",
            "standard-folder automapping enabled",
            "standard-folder automapping disabled",
            "namespace prefix/delimiter warning",
            "namespace mapping not yet assessed",
            "personal namespaces match",
            "shared or other-user namespaces detected",
            "version checked at readiness",
            "engine version not yet checked",
            "One mailbox plan template",
            "{} selected of {} queued mailbox(es)",
            "High: destination quota is currently exhausted",
            "Medium: namespace or shared-folder behavior needs review",
            "Readiness pending: authenticate both endpoints",
            "Inventory pending: mailbox scale is unknown",
            "No risk is established yet; provider-specific review is still required",
            "OAuth 2.0",
            "Password / app password",
            "Configured authentication",
            "Preflight → small pilot → seed → catch-up → final delta → independent verification",
            "Provider readiness runbook",
            "Read-only operational guidance. Preflight and live admission remain authoritative.",
            "Guidance version: {}",
            "Why:",
            "Success:",
            "Known provider issues",
            "! Source transport is cleartext by explicit configuration",
            "✓ Encrypted source transport with certificate verification",
            "Plain IMAP can expose the source password and mailbox data in transit.",
            "I understand and explicitly allow cleartext source transport",
            "PROCESS OWNERSHIP REVIEW REQUIRED",
            "I confirmed no unverified migration process remains",
            "HISTORICAL PROJECT · READ ONLY",
            "Start a new migration",
            "Run authenticated readiness probe",
            "Create project from plan",
            "No preflight assessment has been recorded for the current plan.",
            "Reopen project",
            "Passwords are redacted. This is an argument list for review, not a shell command to paste.",
            "Fix the advanced options above before this plan can run.",
            "Could not create project: {error}",
            "Preflight input is invalid: {error}",
            "Source readiness probe blocked: {error}",
            "Destination readiness probe blocked: {error}",
            "Live authentication probe blocked: {error}",
            "⚠ Execution plan validation failed: {error}",
            "These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.",
            "Delete destination messages missing from source  (--delete2)",
            "Use only for an intentionally exact backup after a tested preflight. This can remove destination mail.",
            "Enable bounded body-content verification (forensic)",
            "Downloads and hashes message bodies from both accounts. It is opt-in, bounded, and requires a stable metadata-preserving plan.",
            "Body proof is resource-intensive. The run will stop rather than exceed either byte bound; successful evidence is labeled BodyHash.",
            "Maximum body bytes per message",
            "Maximum body bytes per verification",
            "The Extra imapsync options field accepts only the documented safe tuning allowlist. Debug output is unavailable here because it may contain customer-sensitive protocol data. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.",
            "Selected mailbox scope remains explicit while this drawer is open.",
            "Choose migration engine",
            "How should this migration run?",
            "Select the execution engine that fits the destination. MailSwiftSync owns planning, safety gates, orchestration, and verification; the selected engine owns message transfer.",
            "Native Dovecot execution is local-only until a secret-safe broker is implemented.",
            "Dry mode only lists the destination mailbox. Native Dovecot uses the selected migration strategy; backup and sync -1 have different merge behavior.",
            "Engine and execution settings are locked while a migration is running.",
            "Continue to migration plan",
            "Choose worksheet",
            "Select the migration worksheet",
            "{} contains {} worksheet(s). Choose the sheet with the mailbox headers.",
            "The selected worksheet is parsed and validated in the background. Other worksheets are not imported.",
            "Import selected worksheet",
            "Workspace",
            "No project selected",
            "Task center",
            "Migration running",
            "No active migration",
            "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.",
            "Importing {} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.",
            "• {}: {} → {}",
            "Export verification report…",
            "Customer proof is ready: the durable project is Complete and no mailbox requires review.",
            "Customer proof remains gated until the durable project is Complete, every mailbox is verified, and the state view is current.",
            "Assurance",
            "Attention required: this mailbox is not currently safe to close.",
            "Transfer and inventory evidence support this mailbox's current state.",
            "Assurance is incomplete; review the missing facts before proceeding.",
            "Destination reachable",
            "Inventory reconciled",
            "Message-level evidence",
            "Verification authority",
            "The transfer finished, but no mailbox-level evidence has been captured yet.",
            "Open migration plan  →",
            "Review preflight  →",
            "Review migration plan  →",
            "Open Activity  →",
            "Review mailboxes  →",
            "Open customer proof  →",
            "Review pilot activity  →",
            "Open mailbox actions  →",
            "Run final delta  →",
            "Open verification  →",
            "Review batch mailboxes  →",
            "Refresh preflight assessment",
            "Preflight is the default",
            "Saved profiles exclude passwords",
            "Source mail is read-only by default",
            "{} shown · {} total need review",
            "{}/{} configuration items complete",
            "Process review acknowledged; execution gates are available again.",
            "Could not clear reviewed process identities: {error}",
            "Readiness observations expired because the migration plan changed; run discovery again.",
            "Project reopened for documented review",
            "Could not reopen project: {error}",
            "{} visible on page · showing {}–{} of {}",
            "MIGRATION LIFECYCLE",
            "ATTENTION REQUIRED",
            "A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved.",
            "Elapsed {}",
            "{} visible of {} loaded",
            "Selected {} mailbox row(s) for focused review.",
            "Imported {} mailbox rows. Review them and run preflight before migration.",
            "Choose the worksheet containing the migration rows before importing.",
            "A mailbox file is already being imported.",
            "Importing {} in the background…",
            "Importing the selected worksheet in the background…",
            "Queue cleared; its durable batch association was discarded.",
            "Errors and attention",
            "Running",
            "Completed",
            "Execution completed without a durable run context",
            "Migration result requires durable storage; retrying terminal commit",
            "Migration result requires durability review",
            "Migration failed: {error}",
            "Preflight completed successfully",
            "Batch transfer completed; review per-mailbox verification results",
            "Migration completed and verified",
            "Migration completed with accepted verification exceptions",
            "Dovecot synchronization completed with changes pending; repeat the final pass until exit code 0",
            "Migration completed; verification found differences requiring review",
            "Migration completed; verification requires operator review",
            "All statuses",
            "No durable runs recorded yet.",
            "Worksheet selection cancelled; no rows were imported.",
            "Create or restore a project to see durable runs.",
            "Showing the newest 250 runs. Export the audit report for complete history.",
            "Run",
            "Mailbox",
            "Stage",
            "Started",
            "Finished",
            "Detail",
            "Accept residual difference",
            "This records an auditable exception; it does not change the underlying evidence or claim exact equality.",
            "Operator",
            "Why is this difference acceptable? Include the change-ticket or customer approval reference.",
            "Accept and mark verified with exceptions",
            "Verification exception recorded durably",
            "Could not accept verification exception: {error}",
            "{} is required.",
            "{} contains an invalid control character.",
            "Saved OAuth credential configured; session token not required.",
            "Saved credential configured; session password not required.",
            "Source credential stored in OS keyring",
            "Source credential loaded",
            "Source credential deleted from OS keyring",
            "Destination credential stored in OS keyring",
            "Destination credential loaded",
            "Destination credential deleted from OS keyring",
            "Mailbox state filter",
            "! imapsync was not found or did not report a version",
            "! imapsync {} is not the qualified version; transfers would be unverified",
            "Check installed engine",
            "Confirming the engine version…",
            "Desktop elevation (pkexec) is unavailable; run `mailswiftsync install-engine` in a terminal.",
            "Download verified.",
            "Downloading…",
            "Downloading… {}%",
            "Downloads from the official imapsync site and installs only if the pinned SHA-256 matches.",
            "Install qualified imapsync {}",
            "Installing… approve the system prompt if one appears.",
            "imapsync {} engine",
            "✓ Installed imapsync {} and selected it for this plan",
            "✓ imapsync {} is qualified",
            "Automatic installation is not available on macOS because imapsync needs Perl modules from CPAN. Install imapsync 2.314 from the official tarball (https://imapsync.lamiral.info/dist2/imapsync-2.314.tgz) following its INSTALL.d/INSTALL.OnMac.txt, or run the pinned MailSwiftSync container image, then enter the imapsync path in MailSwiftSync.",
            "Automatic installation supports Debian/Ubuntu (apt) and Windows. Install imapsync 2.314 from the official distribution (https://imapsync.lamiral.info/dist2/) using your platform's INSTALL.d instructions, or run the pinned MailSwiftSync container image, then enter the imapsync path in MailSwiftSync.",
            "! {} · retry {} in {} · {}",
            "Active transfers",
            "Active workers",
            "Dovecot runs do not report transfer progress; counts below stay empty.",
            "Enter the cutover window to check whether the estimate fits.",
            "Estimated time remaining",
            "Estimating…",
            "Finishes around {}",
            "Latest actionable failure",
            "Likely to overrun · window closes in {}",
            "Mailboxes remaining",
            "Maintenance window",
            "Migration operations",
            "Needs 10 s of transfer data",
            "Needs a few minutes of transfer data",
            "On schedule · window closes in {}",
            "Outside the window",
            "Retries and provider cooldowns",
            "Retries pending: {}",
            "Retry {} in {} · {}",
            "Review mailbox  →",
            "This mailbox",
            "Throughput · 5-min average",
            "Tight · window closes in {}",
            "Transferred",
            "Use HH:MM-HH:MM, optionally @Mon,Tue",
            "Window closes in {}; no estimate yet",
            "Window is open all day",
            "and {} more in flight",
            "{} ago",
            "{} finished · {} waiting",
            "{} messages",
            "{} messages copied",
            "{} messages left",
            "{} msgs/min",
            "{} of ~{}",
            "{}/s · {} ago",
            "⏸ {} · new launches paused for {} after provider throttling",
            "Review attention items ({})  →",
            "Review imported mailboxes ({})  →",
            "✓ Account connected · token refresh stored as {}",
            "✓ Saved sign-in configured · keyring ID {}",
            "Reconnect {} account",
            "Connect {} account",
            "Hide sign-in details",
            "Browser sign-in is built in for Google Workspace and Microsoft 365. For other OAuth providers run `mailswiftsync oauth-authorize`, or paste an access token below.",
            "Use a temporary access token instead",
            "Connect the account or provide an access token before testing it.",
            "Enter the password or a saved keyring ID before testing the account.",
            "Other IMAP server",
            "An OS keyring operation is already in progress.",
            "Waiting for the OS keyring…",
            "The keyring ID changed while the credential was loading; it was not applied.",
            "Credential settings are locked while a migration is running or OAuth refresh is in progress.",
            "This is password storage, not OAuth/Modern Auth. Do not use it as a substitute for provider-specific OAuth setup or unattended secret brokering.",
            "Source OAuth refresh configuration deleted",
            "Destination OAuth refresh configuration deleted",
            "Delete saved credential?",
            "{}: {} · keyring ID: {}",
            "This permanently removes the saved credential from the OS keyring.",
            "This removes only the locally stored OAuth refresh configuration; it does not revoke the provider token.",
            "The profile reference and any credential already loaded in this session are not changed.",
            "This permanently removes the selected item from the OS keyring. The profile reference and any credential already loaded in this session are not changed.",
            "Delete saved credential",
            "saved password / access token",
            "automatic OAuth refresh configuration",
            "Refreshing OAuth token…",
            "Refreshing OAuth token in the background…",
            "OAuth refresh worker stopped before returning a result.",
            "OAuth refresh finished after account settings changed; its access token was discarded. Review the account and refresh again.",
            "{} OAuth access token refreshed (expires in {}s)",
            "{} OAuth access token refreshed",
            "No automatic refresh is configured for the {}",
            "{} OAuth refresh configuration stored in OS keyring",
            "No active project selected",
            "Verified with exceptions",
            "Run a migration to create a durable mailbox evidence record.",
            "The selected mailbox is not present in the cached project snapshot. Refresh the workspace before viewing or exporting its evidence.",
            "Discovery",
            "Seed",
            "Catch-up",
            "Final delta",
            "Complete",
            "A migration is running — monitor Activity or use Stop migration if you need to halt it.",
            "Review Attention items before starting another migration.",
            "Run the dry preflight and review every blocker before going live.",
            "The batch is running — monitor Activity and review any Attention rows before continuing.",
            "Review the imported mailbox rows, then run a dry preflight before any live migration.",
            "✓ Verified with exceptions",
            "✓ Verified",
            "✓ Completed",
            "● Running",
            "○ Queued",
            "◌ Preflight",
            "↻ Retrying",
            "○ Ready",
            "↻ Delta required",
            "≠ Verification difference",
            "! Attention",
            "× Failed",
            "× Cancelled",
            "? Unknown",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German translation: {key}"
            );
        }
    }

    #[test]
    fn destination_mutation_policy_text_is_translated() {
        use crate::migration_plan::DestinationMutationPolicy as Policy;
        for policy in [
            Policy::Additive,
            Policy::MergePreservingDestination,
            Policy::MirrorMayRemoveDestinationState,
            Policy::ExplicitDeleteMissingSourceMessages,
        ] {
            for text in [policy.label(), policy.warning()] {
                assert_ne!(
                    UiLanguage::German.text(text),
                    text,
                    "missing German: {text}"
                );
            }
        }
    }

    #[test]
    fn lookup_translates_fixed_runtime_text_only() {
        assert_eq!(UiLanguage::German.lookup("Idle"), Some("Bereit"));
        assert_eq!(UiLanguage::English.lookup("Idle"), None);
        // Formatted messages carry values and stay untranslated.
        assert_eq!(UiLanguage::German.lookup("Durability error: save"), None);
        assert_eq!(UiLanguage::German.text("Idle"), "Bereit");
    }

    #[test]
    fn batch_health_counts_are_translated() {
        for key in [
            "{} mailbox jobs in scope",
            "{} imported",
            "{} queued",
            "{} preflight",
            "{} ready",
            "{} running",
            "{} verified",
            "{} attention",
            "{} failed",
            "{} delta required",
            "{} unresolved",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German batch-health translation: {key}"
            );
        }
    }

    #[test]
    fn verification_scope_explanations_are_translated() {
        for key in [
            "Evidence includes bounded RFC822 body fingerprints from both accounts; provider-specific qualification remains required.",
            "Evidence labels describe metadata and aggregate reconciliation; message bodies were not compared.",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German verification explanation: {key}"
            );
        }
    }

    #[test]
    fn verification_detail_labels_are_translated() {
        for key in [
            "aggregate_engine",
            "metadata_reconciliation",
            "body_hash",
            "native_dovecot",
            "Exact body match — bounded RFC822 SHA-256 fingerprints compared",
            "Exact metadata match — message bodies not compared",
            "Probable metadata match — message bodies not compared",
            "Ambiguous metadata result — message bodies not compared",
            "Missing messages detected",
            "Changed messages detected",
            "Unexpected messages detected",
            "Verification evidence incomplete",
            "Verification failed",
            "Accepted by {operator} at {time}: {reason}",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German verification detail translation: {key}"
            );
        }
    }

    #[test]
    fn provider_authorization_and_verification_levels_are_translated() {
        for key in [
            "ui.connect-provider-account",
            "ui.oauth-app-registration-required",
            "ui.oauth-authorization-stored",
            "ui.oauth-token-refresh-tested",
            "ui.imap-authentication-verified-for",
            "ui.oauth-code-received-completing-authorization",
            "ui.testing-oauth-token-refresh",
            "ui.verifying-imap-authentication",
            "ui.verification-levels",
            "ui.verification-level-1",
            "ui.verification-level-2",
            "ui.verification-level-3",
            "ui.verification-level-1-description",
            "ui.verification-level-2-description",
            "ui.verification-level-3-description",
            "ui.migration-policy",
            "ui.folder-handling",
            "ui.destination-behavior",
            "ui.level-1-aggregate-evidence",
            "ui.level-2-metadata-reconciliation-plan-dependent",
            "ui.level-3-bounded-content-fingerprints",
            "ui.provider-qualification-and-detailed-simulation",
            "ui.actions-toggle-inspector",
            "ui.close-inspector",
            "ui.additional-themes",
        ] {
            assert_ne!(
                UiLanguage::German.message(key),
                UiLanguage::English.message(key),
                "missing German UI translation: {key}"
            );
        }
    }

    #[test]
    fn dynamic_engine_and_attention_copy_is_translated() {
        for engine in [
            crate::core::Engine::Auto,
            crate::core::Engine::Dovecot,
            crate::core::Engine::ImapSync,
        ] {
            assert_ne!(
                UiLanguage::German.text(engine.label()),
                engine.label(),
                "missing German engine label: {}",
                engine.label()
            );
            assert_ne!(
                UiLanguage::German.text(engine.description()),
                engine.description(),
                "missing German engine description: {}",
                engine.description()
            );
        }
        for strategy in [
            crate::migration_plan::DovecotMigrationStrategy::InitialMirror,
            crate::migration_plan::DovecotMigrationStrategy::IncrementalMirror,
            crate::migration_plan::DovecotMigrationStrategy::FinalPreservationPass,
            crate::migration_plan::DovecotMigrationStrategy::DestinationAlreadyActive,
        ] {
            assert_ne!(
                UiLanguage::German.text(strategy.label()),
                strategy.label(),
                "missing German Dovecot strategy label: {}",
                strategy.label()
            );
            assert_ne!(
                UiLanguage::German.text(strategy.description()),
                strategy.description(),
                "missing German Dovecot strategy description"
            );
        }
        for reason in [
            crate::core::AttentionReason::Interrupted,
            crate::core::AttentionReason::VerificationIncomplete,
            crate::core::AttentionReason::VerificationDifference,
            crate::core::AttentionReason::ProcessIdentityUnverified,
            crate::core::AttentionReason::AuthenticationFailed,
            crate::core::AttentionReason::TransportFailed,
            crate::core::AttentionReason::PolicyBlocked,
            crate::core::AttentionReason::ConfigurationInvalid,
            crate::core::AttentionReason::CapacityLimited,
            crate::core::AttentionReason::MessageRejected,
            crate::core::AttentionReason::Unknown,
        ] {
            assert_ne!(
                UiLanguage::German.text(reason.label()),
                reason.label(),
                "missing German attention label: {}",
                reason.label()
            );
            assert_ne!(
                UiLanguage::German.text(reason.recommended_action()),
                reason.recommended_action(),
                "missing German attention action"
            );
        }
        for interruption in [
            crate::core::recovery_dashboard::InterruptionReason::ProcessTerminated,
            crate::core::recovery_dashboard::InterruptionReason::NetworkTimeout,
            crate::core::recovery_dashboard::InterruptionReason::ProviderThrottled,
        ] {
            for step in
                crate::core::recovery_dashboard::RecoveryPlanner::generate_guidance(interruption)
            {
                assert_ne!(
                    UiLanguage::German.text(step),
                    step,
                    "missing German recovery guidance: {step}"
                );
            }
        }
    }
}
