//! Plan fingerprints and immutable plan snapshots.

use super::*;

impl Form {
    /// A deterministic, secret-free description of the live execution plan.
    /// It intentionally includes the generated arguments so changing an
    /// option, endpoint, engine, mapping, or credential reference invalidates
    /// an earlier preflight. Password bytes are intentionally excluded.
    pub(crate) fn plan_fingerprint(&self) -> String {
        let (executable, mut args) = match self.command_with_checkpoint_and_mode(true, None, false)
        {
            Ok(command) => command,
            Err(_) => {
                // Keep the rest of the invalid plan identity-sensitive (for
                // example, changing an endpoint must invalidate an in-flight
                // capability probe even while expert options remain invalid).
                // Only the rejected option text is represented by its digest.
                let mut safe_profile = self.profile.clone();
                safe_profile.extra_options.clear();
                let mut args = engine::imapsync_preview_args(&safe_profile, false, 1)
                    .expect("an empty expert-option field is valid");
                args.push("--invalid-extra-options-sha256".into());
                args.push(crate::plan_identity::snapshot_sha256(
                    &self.profile.extra_options,
                ));
                (self.profile.imapsync_path.clone(), args)
            }
        };
        if self.engine() == core::Engine::ImapSync {
            remove_option(&mut args, "--password1");
            remove_option(&mut args, "--password2");
            remove_option(&mut args, "--oauthaccesstoken1");
            remove_option(&mut args, "--oauthaccesstoken2");
        }
        format!(
            "{}\n{}\ncredential-source1={}\ncredential-source2={}\nsource-auth={}\ndestination-auth={}\nsource-rate-tenant={}\ndestination-rate-tenant={}\ninsecure-source-transport-ack={}\nsource-ca-bundle={}\nsource-ca-bundle-sha256={}\ndestination-ca-bundle={}\ndestination-ca-bundle-sha256={}\nsource-certificate-pin={}\ndestination-certificate-pin={}\nexecution-executable-sha256={}\ndovecot-config-sha256={}\nbody-hash-verification={}\nbody-hash-max-bytes={}\nbody-hash-max-total-bytes={}",
            executable,
            args.join("\u{1f}"),
            self.profile.source_credential_id.trim(),
            self.profile.destination_credential_id.trim(),
            self.profile.source_auth,
            self.profile.destination_auth,
            self.profile.source_rate_tenant.trim(),
            self.profile.destination_rate_tenant.trim(),
            self.profile.allow_insecure_source_transport,
            self.profile.source_ca_bundle.trim(),
            configured_file_content_identity(&self.profile.source_ca_bundle),
            self.profile.destination_ca_bundle.trim(),
            configured_file_content_identity(&self.profile.destination_ca_bundle),
            self.profile
                .source_certificate_pin_sha256
                .trim()
                .to_ascii_lowercase(),
            self.profile
                .destination_certificate_pin_sha256
                .trim()
                .to_ascii_lowercase(),
            executable_content_identity(&executable),
            configured_file_content_identity(&self.profile.dovecot_config),
            self.profile.body_hash_verification,
            self.profile.body_hash_max_bytes,
            self.profile.body_hash_max_total_bytes,
        )
    }

    /// Serialize the launch configuration without session passwords or raw
    /// expert-option values. This is persisted with the run so historical
    /// reports do not depend on the currently edited form.
    #[cfg(test)]
    pub(crate) fn plan_snapshot(&self) -> String {
        self.plan_snapshot_with_checkpoint(None)
            .expect("test snapshot serialization")
    }

    /// Serialize the launch configuration and, when applicable, the identity
    /// of the previously committed Dovecot state supplied to this run. The
    /// state token itself stays out of durable reports; its digest is enough
    /// to prove which resume point was selected.
    pub(crate) fn plan_snapshot_with_checkpoint(
        &self,
        checkpoint: Option<&str>,
    ) -> Result<String, String> {
        let canonical_extra_options = crate::extra_options::canonical(&self.profile.extra_options)?;
        let digest = Sha256::digest(canonical_extra_options.join("\u{1f}").as_bytes());
        let extra_options_sha256 = digest
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect::<String>();
        let profile = &self.profile;
        let execution_executable = match self.engine() {
            core::Engine::Dovecot => &profile.doveadm_path,
            core::Engine::ImapSync | core::Engine::Auto => &profile.imapsync_path,
        };
        let snapshot = RunPlanSnapshot {
            dry_run: self.dry_run,
            profile: RunProfileSnapshot {
                execution_profile: match crate::process::engine_execution_profile() {
                    crate::process::EngineExecutionProfile::Hardened => "hardened",
                    crate::process::EngineExecutionProfile::Compatibility => "compatibility",
                }
                .into(),
                name: profile.name.clone(),
                source_host: profile.source_host.clone(),
                source_port: profile.source_port.clone(),
                source_tls: profile.source_tls.clone(),
                source_ca_bundle: profile.source_ca_bundle.clone(),
                source_certificate_pin_sha256: profile.source_certificate_pin_sha256.clone(),
                allow_insecure_source_transport: profile.allow_insecure_source_transport,
                source_user: profile.source_user.clone(),
                source_rate_tenant: profile.source_rate_tenant.clone(),
                source_auth: profile.source_auth.clone(),
                source_credential_id: profile.source_credential_id.clone(),
                source_oauth_refresh_credential_id: profile
                    .source_oauth_refresh_credential_id
                    .clone(),
                destination_host: profile.destination_host.clone(),
                destination_user: profile.destination_user.clone(),
                destination_rate_tenant: profile.destination_rate_tenant.clone(),
                destination_auth: profile.destination_auth.clone(),
                destination_credential_id: profile.destination_credential_id.clone(),
                destination_oauth_refresh_credential_id: profile
                    .destination_oauth_refresh_credential_id
                    .clone(),
                destination_port: profile.destination_port.clone(),
                destination_tls: profile.destination_tls.clone(),
                destination_ca_bundle: profile.destination_ca_bundle.clone(),
                destination_certificate_pin_sha256: profile
                    .destination_certificate_pin_sha256
                    .clone(),
                imapsync_path: profile.imapsync_path.clone(),
                engine: profile.engine,
                doveadm_path: profile.doveadm_path.clone(),
                dovecot_config: profile.dovecot_config.clone(),
                batch_concurrency: profile.batch_concurrency,
                batch_retry_count: profile.batch_retry_count,
                batch_process_starts_per_second: profile.batch_process_starts_per_second,
                max_messages_per_second: profile.max_messages_per_second,
                max_bytes_per_second: profile.max_bytes_per_second,
                body_hash_verification: profile.body_hash_verification,
                body_hash_max_bytes: profile.body_hash_max_bytes,
                body_hash_max_total_bytes: profile.body_hash_max_total_bytes,
                migration_timeout_hours: profile.migration_timeout_hours,
                automap: profile.automap,
                folder_mapping_rules: profile.folder_mapping_rules.clone(),
                addheader: profile.addheader,
                justfolders: profile.justfolders,
                sync_internaldates: profile.sync_internaldates,
                useuid: profile.useuid,
                usecache: profile.usecache,
                fastio1: profile.fastio1,
                fastio2: profile.fastio2,
                allowsizemismatch: profile.allowsizemismatch,
                dovecot_strategy: profile.dovecot_strategy,
                delete2: profile.delete2,
                extra_options_sha256,
                dovecot_checkpoint_sha256: (self.engine() == core::Engine::Dovecot
                    && !self.dry_run)
                    .then(|| checkpoint.map(snapshot_sha256))
                    .flatten(),
                execution_executable_sha256: executable_content_identity(execution_executable),
                source_ca_bundle_sha256: configured_file_content_identity(
                    &profile.source_ca_bundle,
                ),
                destination_ca_bundle_sha256: configured_file_content_identity(
                    &profile.destination_ca_bundle,
                ),
                dovecot_config_sha256: configured_file_content_identity(&profile.dovecot_config),
                destination_mutation_policy: profile
                    .destination_mutation_policy()
                    .as_str()
                    .to_owned(),
            },
        };
        toml::to_string(&snapshot)
            .map_err(|error| format!("could not serialize immutable run plan snapshot: {error}"))
    }
}
