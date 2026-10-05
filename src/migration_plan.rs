mod credential_identity;
mod profile;
pub(crate) use profile::DestinationMutationPolicy;
mod engine_command;
mod fingerprint;
mod keyring_credentials;
mod saved_profile;
mod validation;
pub(crate) use engine_command::*;
pub(crate) use keyring_credentials::*;
pub(crate) use saved_profile::*;
#[cfg(test)]
use validation::validate_certificate_pin;

use crate::atomic_artifact::write_private_atomic;
use crate::command::remove_option;
use crate::imap_probe::{command_endpoint_parts, command_port};
use crate::{
    DOVECOT_SYNC_LOCK_WAIT_SECONDS, core, create_secret_directory,
    credentials::{self, SecretString},
    endpoint, engine,
    plan_identity::{
        configured_file_content_identity, executable_content_identity, snapshot_sha256,
    },
    write_secret_file,
};
use keyring::Entry;
pub(crate) use profile::{
    DovecotMigrationStrategy, FolderMappingRule, MAX_BATCH_CONCURRENCY,
    MAX_BATCH_PROCESS_STARTS_PER_SECOND, Profile, RunPlanSnapshot, RunProfileSnapshot,
    auth_method_is_oauth, completeness, default_auth_method, default_destination_tls,
    default_doveadm_path, default_migration_timeout_hours, default_source_tls,
    effective_batch_concurrency, effective_batch_process_starts_per_second,
};
use sha2::{Digest, Sha256};
use std::{
    io::{Error, ErrorKind, Read},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub(crate) struct Form {
    pub(crate) profile: Profile,
    pub(crate) source_password: SecretString,
    pub(crate) destination_password: SecretString,
    pub(crate) dry_run: bool,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            profile: Profile {
                name: "New migration".into(),
                imapsync_path: "imapsync".into(),
                source_tls: default_source_tls(),
                source_auth: default_auth_method(),
                destination_tls: default_destination_tls(),
                destination_auth: default_auth_method(),
                doveadm_path: default_doveadm_path(),
                migration_timeout_hours: default_migration_timeout_hours(),
                automap: false,
                sync_internaldates: true,
                ..Default::default()
            },
            source_password: SecretString::default(),
            destination_password: SecretString::default(),
            dry_run: true,
        }
    }
}

impl Form {
    pub(crate) const KEYRING_SERVICE: &'static str = "com.mailswiftsync.mailbox";
    /// Deliberately a distinct keyring service from `KEYRING_SERVICE`, even
    /// though an operator may reuse the same ID string for both entries: a
    /// refresh configuration carries a client secret and refresh token, not
    /// an access token, and must never be returned by a plain password load.
    pub(crate) const OAUTH_REFRESH_KEYRING_SERVICE: &'static str =
        "com.mailswiftsync.oauth-refresh";

    /// Clone the non-secret migration defaults for a bulk row.
    ///
    /// Imported rows must provide their own credentials (or credential IDs),
    /// so copying the base form's secret fields only creates unnecessary
    /// transient secret material.
    pub(crate) fn clone_without_credentials(&self) -> Self {
        Self {
            profile: self.profile.clone(),
            source_password: SecretString::default(),
            destination_password: SecretString::default(),
            dry_run: self.dry_run,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DovecotConfigDialect, Form, MAX_PROFILE_BYTES, OAuthRefreshOutcome, PROFILE_FORMAT,
        PROFILE_FORMAT_VERSION, Profile, SavedProfile, decode_report_run_snapshot,
        decode_saved_profile, dovecot_config_dialect, persist_rotated_refresh_config_with_retry,
        validate_certificate_pin,
    };
    use crate::SecretString;
    use crate::oauth_refresh::OAuthRefreshConfig;
    use std::path::Path;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn dovecot_mail_location_syntax_tracks_supported_major_minor_versions() {
        assert_eq!(
            dovecot_config_dialect("2.3.19.1 (9b53102964)").unwrap(),
            DovecotConfigDialect::Legacy23
        );
        assert_eq!(
            dovecot_config_dialect("2.4.0 (abc123)").unwrap(),
            DovecotConfigDialect::Modern24
        );
        assert!(dovecot_config_dialect("not-a-version").is_err());
        assert!(dovecot_config_dialect("2.5.0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn dovecot_config_dialect_is_probed_from_the_selected_doveadm() {
        use super::detect_dovecot_config_dialect;
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "mailswiftsync-doveadm-version-{}-{}.sh",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, "#!/bin/sh\nprintf '2.3.19.1 (fixture)\\n'\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            detect_dovecot_config_dialect(&path.to_string_lossy()).unwrap(),
            DovecotConfigDialect::Legacy23
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn dovecot_source_commands_use_the_matching_mail_location_dialect() {
        let mut form = Form::default();
        form.profile.engine = crate::core::Engine::Dovecot;
        form.dry_run = true;
        for (dialect, expected, unexpected) in [
            (
                DovecotConfigDialect::Legacy23,
                "mail_location=imapc:",
                "mail_driver=imapc",
            ),
            (
                DovecotConfigDialect::Modern24,
                "mail_driver=imapc",
                "mail_location=imapc:",
            ),
        ] {
            let (_, args) = form
                .command_with_checkpoint_and_mode_and_config(true, None, true, None, dialect)
                .unwrap();
            assert!(args.iter().any(|argument| argument == expected));
            assert!(!args.iter().any(|argument| argument == unexpected));
            let verification = form.dovecot_verification_commands_with_config(true, None, dialect);
            assert!(
                verification[0]
                    .1
                    .iter()
                    .any(|argument| argument == expected)
            );
            assert!(
                !verification[0]
                    .1
                    .iter()
                    .any(|argument| argument == unexpected)
            );
            assert!(verification.iter().all(|(_, arguments)| {
                arguments
                    .iter()
                    .any(|argument| argument == "messages vsize uidvalidity")
            }));
        }
    }

    #[test]
    fn legacy_remote_dovecot_profiles_fail_closed() {
        let result = decode_saved_profile(
            Path::new("profile.toml"),
            "dovecot_execution = \"ssh\"\ndovecot_ssh_user = \"migration\"\n",
        );
        let error = match result {
            Ok(_) => panic!("legacy remote profile should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("unsupported remote Dovecot execution"));

        let result = decode_saved_profile(
            Path::new("profile.toml"),
            "dovecot_execution = \"automatic\"\ndestination_host = \"mail.example\"\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn saved_profile_envelope_round_trips_and_rejects_unknown_versions() {
        let profile = Profile::default();
        let encoded = toml::to_string_pretty(&SavedProfile {
            format: PROFILE_FORMAT,
            format_version: PROFILE_FORMAT_VERSION,
            profile: &profile,
        })
        .unwrap();
        assert!(encoded.contains("format = \"mailswiftsync-profile\""));
        assert!(encoded.contains("format_version = 1"));
        let decoded = decode_saved_profile(Path::new("profile.toml"), &encoded).unwrap();
        assert_eq!(decoded.name, profile.name);

        let error = match decode_saved_profile(
            Path::new("profile.toml"),
            "format = \"mailswiftsync-profile\"\nformat_version = 2\n\n[profile]\nname = \"future\"\n",
        ) {
            Ok(_) => panic!("future profile version should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("unsupported format version 2"));

        let error = match decode_saved_profile(
            Path::new("profile.toml"),
            "format = \"other-profile\"\nformat_version = 1\n\n[profile]\n",
        ) {
            Ok(_) => panic!("unknown profile format should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("unknown or invalid format identifier"));
    }

    #[test]
    fn bare_legacy_saved_profile_remains_readable() {
        let profile = Profile {
            name: "legacy".into(),
            ..Default::default()
        };
        let legacy_toml = toml::to_string_pretty(&profile).unwrap();
        let decoded = decode_saved_profile(Path::new("profile.toml"), &legacy_toml).unwrap();
        assert_eq!(decoded.name, "legacy");
    }

    #[test]
    fn report_snapshot_decode_allows_empty_legacy_snapshots() {
        assert!(decode_report_run_snapshot("  \n").unwrap().is_none());
        match decode_report_run_snapshot("not a run snapshot") {
            Ok(_) => panic!("malformed report snapshot was accepted"),
            Err(error) => assert!(error.contains("plan snapshot is corrupt")),
        }
        let oversized = "x".repeat(MAX_PROFILE_BYTES as usize + 1);
        let error = decode_report_run_snapshot(&oversized)
            .err()
            .expect("oversized report snapshot must be rejected");
        assert!(error.contains("exceeds"));
    }

    /// Run snapshots record the destination mutation policy; snapshots
    /// written before the field existed still report the derived policy.
    #[test]
    fn run_snapshots_record_and_derive_the_destination_policy() {
        let mut form = Form::default();
        form.profile.engine = crate::core::Engine::Dovecot;
        form.profile.dovecot_strategy =
            crate::migration_plan::DovecotMigrationStrategy::InitialMirror;
        let snapshot = form.plan_snapshot();
        assert!(
            snapshot
                .contains("destination_mutation_policy = \"mirror_may_remove_destination_state\""),
            "{snapshot}"
        );
        let legacy = snapshot
            .lines()
            .filter(|line| {
                !line.starts_with("destination_mutation_policy")
                    && !line.starts_with("source_rate_tenant")
                    && !line.starts_with("destination_rate_tenant")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let decoded = decode_report_run_snapshot(&legacy).unwrap().unwrap();
        assert!(decoded.profile.source_rate_tenant.is_empty());
        assert!(decoded.profile.destination_rate_tenant.is_empty());
        assert_eq!(
            decoded.profile.destination_mutation_policy(),
            "mirror_may_remove_destination_state"
        );
    }

    #[test]
    fn certificate_pins_require_sha256_hex() {
        assert!(validate_certificate_pin(&"ab".repeat(32), "source").is_ok());
        assert!(validate_certificate_pin(&"AB".repeat(32), "source").is_ok());
        assert!(validate_certificate_pin("", "source").is_ok());
        assert!(validate_certificate_pin("not-a-pin", "source").is_err());
        assert!(validate_certificate_pin(&"g".repeat(64), "source").is_err());
    }

    #[test]
    fn credential_free_form_clone_preserves_plan_defaults() {
        let base = Form {
            source_password: "source-secret".into(),
            destination_password: "destination-secret".into(),
            dry_run: false,
            ..Form::default()
        };

        let clone = base.clone_without_credentials();

        assert_eq!(clone.profile.name, base.profile.name);
        assert_eq!(clone.profile.source_tls, base.profile.source_tls);
        assert_eq!(clone.profile.destination_tls, base.profile.destination_tls);
        assert!(!clone.dry_run);
        assert!(clone.source_password.is_empty());
        assert!(clone.destination_password.is_empty());
    }

    #[test]
    fn profile_rust_default_matches_serde_runtime_defaults() {
        let profile = crate::migration_plan::profile::Profile::default();
        assert_eq!(profile.batch_concurrency, 2);
        assert!(!profile.automap);
        assert!(profile.sync_internaldates);
        assert_eq!(profile.source_tls, "imaps");
        assert_eq!(profile.destination_tls, "imaps");
        assert_eq!(profile.source_auth, "password");
        assert_eq!(profile.destination_auth, "password");
    }

    #[test]
    fn rate_tenant_scope_is_bounded_and_bound_to_plan_fingerprint() {
        let mut form = Form::default();
        form.profile.source_host = "imap.source.example".into();
        form.profile.source_user = "user@source.example".into();
        form.profile.destination_host = "imap.destination.example".into();
        form.profile.destination_user = "user@destination.example".into();
        assert!(form.validate_internal(false).is_ok());
        let original = form.plan_fingerprint();
        form.profile.source_rate_tenant = "source-tenant-guid".into();
        assert_ne!(original, form.plan_fingerprint());
        assert!(form.validate_internal(false).is_ok());

        form.profile.source_rate_tenant = "x".repeat(257);
        assert!(
            form.validate_internal(false)
                .unwrap_err()
                .contains("Source provider tenant scope")
        );
        form.profile.source_rate_tenant = " ".repeat(257);
        assert!(
            form.validate_internal(false)
                .unwrap_err()
                .contains("Source provider tenant scope")
        );
        form.profile.source_rate_tenant = "tenant\nforged".into();
        assert!(
            form.validate_internal(false)
                .unwrap_err()
                .contains("Source provider tenant scope")
        );
    }

    #[test]
    fn microsoft_365_imap_rejects_password_authentication() {
        let mut form = Form::default();
        form.profile.source_host = "outlook.office365.com".into();
        form.profile.source_user = "user@example.com".into();
        form.profile.destination_host = "imap.example.com".into();
        form.profile.destination_user = "user@example.com".into();
        form.profile.source_auth = "password".into();
        let error = form.validate_internal(false).unwrap_err();
        assert!(error.contains("requires OAuth 2.0 / Modern Authentication"));
    }

    #[test]
    fn microsoft_365_admission_normalizes_host_port_case_and_trailing_dot() {
        for host in [
            "outlook.office365.com:993",
            "OUTLOOK.OFFICE365.COM.:993",
            "mail.outlook.office365.com:993",
            "exchange.microsoft.com",
            "autodiscover.exchange.microsoft.com:993",
        ] {
            let mut form = Form::default();
            form.profile.source_host = host.into();
            form.profile.source_user = "user@example.com".into();
            form.profile.destination_host = "imap.example.com".into();
            form.profile.destination_user = "user@example.com".into();
            form.profile.source_auth = "password".into();

            let error = form.validate_internal(false).unwrap_err();
            assert!(
                error.contains("requires OAuth 2.0 / Modern Authentication"),
                "expected Microsoft 365 policy rejection for {host:?}, got: {error}"
            );
        }
    }

    #[test]
    fn microsoft_365_admission_does_not_match_deceptive_domain_suffixes() {
        let mut form = Form::default();
        form.profile.source_host = "exchange.microsoft.com.attacker.example".into();
        form.profile.source_user = "user@example.com".into();
        form.profile.destination_host = "imap.example.com".into();
        form.profile.destination_user = "user@example.com".into();
        form.profile.source_auth = "password".into();

        assert!(form.validate_internal(false).is_ok());
    }

    #[test]
    fn microsoft_365_admission_applies_to_destination_with_embedded_port() {
        let mut form = Form::default();
        form.profile.source_host = "imap.example.com".into();
        form.profile.source_user = "user@example.com".into();
        form.profile.destination_host = "outlook.office365.com:993".into();
        form.profile.destination_user = "user@example.com".into();
        form.profile.destination_auth = "password".into();

        let error = form.validate_internal(false).unwrap_err();
        assert!(error.contains("Destination Microsoft 365 IMAP requires OAuth"));
    }

    #[test]
    fn oauth_refresh_is_a_no_op_for_password_authentication() {
        let mut form = Form::default();
        form.profile.source_auth = "password".into();
        form.profile.source_oauth_refresh_credential_id = "some-id".into();
        assert!(matches!(
            form.refresh_oauth_access_token(true).unwrap(),
            OAuthRefreshOutcome::NotConfigured
        ));
    }

    #[test]
    fn oauth_refresh_is_a_no_op_without_a_configured_refresh_credential_id() {
        let mut form = Form::default();
        form.profile.source_auth = "oauth2".into();
        form.profile.source_oauth_refresh_credential_id = "   ".into();
        assert!(matches!(
            form.refresh_oauth_access_token(true).unwrap(),
            OAuthRefreshOutcome::NotConfigured
        ));
    }

    #[test]
    fn rotated_refresh_persistence_retries_an_injected_first_keyring_failure() {
        let config = OAuthRefreshConfig {
            token_endpoint: "https://oauth.example.test/token".into(),
            client_id: "client".into(),
            client_secret: SecretString::default(),
            refresh_token: SecretString::from("rotated-refresh-token"),
        };
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&attempts);
        persist_rotated_refresh_config_with_retry(&config, |_config| {
            let attempt = observed.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Err("injected keyring write failure".into())
            } else {
                Ok(())
            }
        })
        .expect("a transient keyring failure should be retried");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
