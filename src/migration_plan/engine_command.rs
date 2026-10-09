//! Building imapsync and Dovecot engine commands from a validated plan.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DovecotConfigDialect {
    Legacy23,
    Modern24,
}

pub(super) fn dovecot_config_dialect(version: &str) -> Result<DovecotConfigDialect, String> {
    let version_token = version.split_whitespace().next().unwrap_or_default();
    let mut components = version_token.split('.');
    let major = components
        .next()
        .and_then(|component| component.parse::<u32>().ok());
    let minor = components
        .next()
        .and_then(|component| component.parse::<u32>().ok());
    match (major, minor) {
        (Some(2), Some(3)) => Ok(DovecotConfigDialect::Legacy23),
        (Some(2), Some(4)) => Ok(DovecotConfigDialect::Modern24),
        _ => Err(format!(
            "unsupported or unrecognized Dovecot version {version:?}; native migration currently supports Dovecot 2.3 and 2.4"
        )),
    }
}

pub(super) fn detect_dovecot_config_dialect(
    doveadm_path: &str,
) -> Result<DovecotConfigDialect, String> {
    let version = crate::runner::probe_engine_version(doveadm_path)
        .or_else(|| {
            let configured = resolve_executable_path(doveadm_path)?;
            let path_doveadm = resolve_executable_path("doveadm")?;
            (configured == path_doveadm)
                .then(|| crate::runner::probe_engine_version("dovecot"))
                .flatten()
        })
        .ok_or_else(|| {
        "could not determine the configured doveadm version; refusing to guess its mail-location configuration syntax".to_owned()
        })?;
    dovecot_config_dialect(&version)
}

pub(super) fn resolve_executable_path(executable: &str) -> Option<PathBuf> {
    crate::plan_identity::resolve_executable(executable)
}

pub(super) fn append_imapc_mail_settings(args: &mut Vec<String>, dialect: DovecotConfigDialect) {
    args.extend(["-o".into()]);
    match dialect {
        DovecotConfigDialect::Legacy23 => args.push("mail_location=imapc:".into()),
        DovecotConfigDialect::Modern24 => {
            args.push("mail_driver=imapc".into());
            args.extend(["-o".into(), "mail_path=".into()]);
        }
    }
}

pub(crate) fn effective_destination_tls(mode: &str) -> &str {
    if mode == "starttls" {
        "starttls"
    } else {
        "imaps"
    }
}

pub(crate) fn default_imap_port(tls_mode: &str) -> u16 {
    match tls_mode {
        "starttls" | "plain" => 143,
        _ => 993,
    }
}

pub(super) fn is_microsoft_365_endpoint(configured_endpoint: &str, tls_mode: &str) -> bool {
    let Ok((host, _)) = endpoint::parts(configured_endpoint, default_imap_port(tls_mode)) else {
        // Endpoint syntax is validated separately and will produce its own
        // actionable error. Do not attempt provider identification on invalid input.
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["outlook.office365.com", "exchange.microsoft.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

pub(crate) fn dovecot_ssl_mode(mode: &str) -> &str {
    match mode {
        "plain" => "no",
        other => other,
    }
}

pub(crate) fn append_dovecot_source_tls_policy(
    args: &mut Vec<String>,
    mode: &str,
    ca_bundle: &str,
) {
    if mode != "plain" {
        args.extend(["-o".into(), "ssl_client_require_valid_cert=yes".into()]);
        if !ca_bundle.trim().is_empty() {
            args.extend([
                "-o".into(),
                format!("ssl_client_ca_file={}", ca_bundle.trim()),
            ]);
        }
    }
}

pub(super) fn dovecot_config_path(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| "Dovecot config and secret paths must be valid UTF-8".to_owned())?;
    if value.chars().any(char::is_whitespace) || value.contains(['\n', '\r', '"']) {
        return Err("Dovecot config and secret paths cannot contain whitespace or quotes".into());
    }
    Ok(value.to_owned())
}

pub(super) fn resolve_dovecot_base_config(configured: &str) -> Result<PathBuf, String> {
    if !configured.trim().is_empty() {
        let path = PathBuf::from(configured);
        if path.is_file() {
            return path.canonicalize().map_err(|error| {
                format!("could not resolve the configured Dovecot config file: {error}")
            });
        }
        return Err("the configured Dovecot config file does not exist or is not a file".into());
    }

    [
        "/etc/dovecot/dovecot.conf",
        "/usr/local/etc/dovecot/dovecot.conf",
        "/opt/homebrew/etc/dovecot/dovecot.conf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .ok_or_else(|| {
        String::from(
            "could not locate the default Dovecot config; set the Dovecot Config path explicitly",
        )
    })?
    .canonicalize()
    .map_err(|error| format!("could not resolve the Dovecot config file: {error}"))
}

pub(super) fn write_dovecot_runtime_config(
    directory: &Path,
    configured_base: &str,
    password_file: &Path,
) -> Result<PathBuf, String> {
    let base = resolve_dovecot_base_config(configured_base)?;
    let base = dovecot_config_path(&base)?;
    let password = dovecot_config_path(password_file)?;
    let runtime_config = directory.join("mailswiftsync-doveadm.conf");
    let _runtime_config_path = dovecot_config_path(&runtime_config)?;
    let contents = format!("!include {base}\nimapc_password = <{password}\n");
    write_secret_file(&runtime_config, &contents)
        .map_err(|error| format!("could not prepare private Dovecot runtime config: {error}"))?;
    Ok(runtime_config)
}

/// An immutable plan that has passed the full admission validator. Engine
/// invocation preparation is intentionally available only through this type.
pub(crate) struct ValidatedPlan<'a> {
    pub(super) form: &'a Form,
}

impl ValidatedPlan<'_> {
    #[cfg(test)]
    pub(crate) fn prepared_command(&self) -> Result<PreparedCommand, String> {
        self.prepare_command_with_dialect(1, None, Some(DovecotConfigDialect::Modern24))
    }

    pub(crate) fn prepared_command_with_throttle_divisor_and_checkpoint(
        &self,
        throttle_divisor: usize,
        checkpoint: Option<&str>,
    ) -> Result<PreparedCommand, String> {
        let form = self.form;
        let dovecot_dialect = if form.engine() == core::Engine::Dovecot {
            Some(detect_dovecot_config_dialect(&form.profile.doveadm_path)?)
        } else {
            None
        };
        self.prepare_command_with_dialect(throttle_divisor, checkpoint, dovecot_dialect)
    }

    fn prepare_command_with_dialect(
        &self,
        throttle_divisor: usize,
        checkpoint: Option<&str>,
        dovecot_dialect: Option<DovecotConfigDialect>,
    ) -> Result<PreparedCommand, String> {
        let form = self.form;
        if form.engine() == core::Engine::Dovecot {
            let dovecot_dialect = dovecot_dialect.ok_or_else(|| {
                "Dovecot command preparation requires a detected configuration dialect".to_owned()
            })?;
            let secret_dir = create_secret_directory()?;
            let source_file = secret_dir.join("source.secret");
            let runtime_config = write_secret_file(&source_file, form.source_password.as_str())
                .map_err(|error| format!("could not prepare source credential file: {error}"))
                .and_then(|()| {
                    write_dovecot_runtime_config(
                        &secret_dir,
                        &form.profile.dovecot_config,
                        &source_file,
                    )
                });
            let runtime_config = match runtime_config {
                Ok(path) => path,
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&secret_dir);
                    return Err(error);
                }
            };
            let config_path = runtime_config.to_string_lossy();
            let (executable, args) = form.command_with_checkpoint_and_mode_and_config(
                false,
                checkpoint,
                form.dry_run,
                Some(&config_path),
                dovecot_dialect,
            )?;
            let verification = if form.dry_run {
                Vec::new()
            } else {
                form.dovecot_verification_commands_with_config(
                    false,
                    Some(&config_path),
                    dovecot_dialect,
                )
            };
            return Ok(PreparedCommand {
                executable,
                args,
                cleanup: vec![secret_dir],
                env: Vec::new(),
                verification,
            });
        }
        let mut args = engine::imapsync_args(&form.profile, form.dry_run, throttle_divisor)?;
        let secret_dir = create_secret_directory()?;
        let source_file = secret_dir.join("source.secret");
        let destination_file = secret_dir.join("destination.secret");
        if let Err(error) = write_secret_file(&source_file, form.source_password.as_str())
            .and_then(|_| write_secret_file(&destination_file, form.destination_password.as_str()))
        {
            let _ = std::fs::remove_dir_all(&secret_dir);
            return Err(format!(
                "Could not prepare temporary credential files: {error}"
            ));
        }
        if auth_method_is_oauth(&form.profile.source_auth) {
            args.extend([
                "--oauthaccesstoken1".into(),
                source_file.to_string_lossy().into_owned(),
            ]);
            if let Err(error) =
                append_oauth_refresh_command(&mut args, 1, form, &secret_dir, &source_file)
            {
                let _ = std::fs::remove_dir_all(&secret_dir);
                return Err(error);
            }
        } else {
            args.extend([
                "--passfile1".into(),
                source_file.to_string_lossy().into_owned(),
            ]);
        }
        if auth_method_is_oauth(&form.profile.destination_auth) {
            args.extend([
                "--oauthaccesstoken2".into(),
                destination_file.to_string_lossy().into_owned(),
            ]);
            if let Err(error) =
                append_oauth_refresh_command(&mut args, 2, form, &secret_dir, &destination_file)
            {
                let _ = std::fs::remove_dir_all(&secret_dir);
                return Err(error);
            }
        } else {
            args.extend([
                "--passfile2".into(),
                destination_file.to_string_lossy().into_owned(),
            ]);
        }
        Ok(PreparedCommand {
            executable: form.profile.imapsync_path.clone(),
            args,
            cleanup: vec![secret_dir],
            env: Vec::new(),
            verification: Vec::new(),
        })
    }
}

/// Give qualified imapsync a protected refresh configuration and token path.
/// imapsync invokes this command before each OAuth authentication, including
/// reconnects, and rereads the token file. Secrets stay in owner-only files;
/// the command line contains only paths. The per-run refresh configuration is
/// intentionally separate from the OS keyring so a rotated provider token is
/// serialized by the atomic file update without exposing it to argv.
fn append_oauth_refresh_command(
    args: &mut Vec<String>,
    side: u8,
    form: &Form,
    secret_dir: &Path,
    token_file: &Path,
) -> Result<(), String> {
    let refresh_id = if side == 1 {
        form.profile.source_oauth_refresh_credential_id.trim()
    } else {
        form.profile.destination_oauth_refresh_credential_id.trim()
    };
    if refresh_id.is_empty() {
        return Ok(());
    }
    #[cfg(windows)]
    return Err(
        "automatic OAuth refresh is not enabled on Windows until imapsync refresh-command quoting is qualified for the Windows command shell".to_owned(),
    );
    let config = form
        .load_oauth_refresh_config(side == 1)?
        .ok_or_else(|| {
            format!(
                "automatic OAuth refresh is configured for side {side}, but its keyring configuration is unavailable"
            )
        })?;
    let config_file = secret_dir.join(format!("oauth-refresh-{side}.json"));
    write_private_atomic(
        &config_file,
        &crate::oauth_refresh::encode_refresh_config(&config),
    )
    .map_err(|error| format!("could not prepare OAuth refresh configuration: {error}"))?;
    let executable = std::env::current_exe()
        .map_err(|error| format!("could not locate MailSwiftSync OAuth refresh helper: {error}"))?;
    let command = format!(
        "{} oauth-access-token {} {} --persist-keyring-id {}",
        shell_quote(&executable.to_string_lossy()),
        shell_quote(&config_file.to_string_lossy()),
        shell_quote(&token_file.to_string_lossy()),
        shell_quote(refresh_id),
    );
    args.extend([format!("--oauthrefreshcmd{side}"), command]);
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(crate) struct PreparedCommand {
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
    pub(crate) cleanup: Vec<PathBuf>,
    pub(crate) env: Vec<(String, credentials::SecretString)>,
    pub(crate) verification: Vec<(String, Vec<String>)>,
}

impl Form {
    #[cfg(test)]
    pub(crate) fn args(&self, redact: bool) -> Vec<String> {
        self.args_with_throttle_divisor(redact, 1)
    }

    #[cfg(test)]
    pub(crate) fn args_with_throttle_divisor(
        &self,
        redact: bool,
        throttle_divisor: usize,
    ) -> Vec<String> {
        self.args_with_throttle_divisor_and_mode(redact, throttle_divisor, self.dry_run)
            .expect("test form uses valid extra options")
    }

    pub(crate) fn args_with_throttle_divisor_and_mode(
        &self,
        redact: bool,
        throttle_divisor: usize,
        dry_run: bool,
    ) -> Result<Vec<String>, String> {
        let _ = redact;
        engine::imapsync_preview_args(&self.profile, dry_run, throttle_divisor)
    }

    /// Build the exact secret-free command shown by the execution-plan
    /// preview. The imapsync branch returns the same validated canonical
    /// argument vector that the runtime command builder is permitted to use;
    /// it must never fall back to silently omitting an invalid expert option.
    pub(crate) fn preview_command(&self) -> Result<(String, Vec<String>), String> {
        self.validate_internal(false)?;
        if self.engine() == core::Engine::Dovecot {
            return self.command(true);
        }
        Ok((
            self.profile.imapsync_path.clone(),
            engine::imapsync_preview_args(&self.profile, self.dry_run, 1)?,
        ))
    }

    pub(crate) fn engine(&self) -> core::Engine {
        // The desktop cannot safely infer the destination's mail stack from a
        // hostname or a locally installed executable; see effective_engine.
        self.profile.effective_engine()
    }

    pub(crate) fn command(&self, redact: bool) -> Result<(String, Vec<String>), String> {
        self.command_with_checkpoint(redact, None)
    }

    pub(crate) fn command_with_checkpoint(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
    ) -> Result<(String, Vec<String>), String> {
        self.command_with_checkpoint_and_mode(redact, checkpoint, self.dry_run)
    }

    pub(crate) fn command_with_checkpoint_and_mode(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
        dry_run: bool,
    ) -> Result<(String, Vec<String>), String> {
        self.command_with_checkpoint_and_mode_and_config(
            redact,
            checkpoint,
            dry_run,
            None,
            DovecotConfigDialect::Modern24,
        )
    }

    pub(super) fn command_with_checkpoint_and_mode_and_config(
        &self,
        redact: bool,
        checkpoint: Option<&str>,
        dry_run: bool,
        runtime_config: Option<&str>,
        dovecot_dialect: DovecotConfigDialect,
    ) -> Result<(String, Vec<String>), String> {
        if self.engine() != core::Engine::Dovecot {
            return Ok((
                self.profile.imapsync_path.clone(),
                self.args_with_throttle_divisor_and_mode(redact, 1, dry_run)?,
            ));
        }
        let source_default_port = default_imap_port(&self.profile.source_tls);
        let (source_host, endpoint_port) =
            command_endpoint_parts(&self.profile.source_host, source_default_port);
        let source_port = command_port(&self.profile.source_port, endpoint_port);
        // No `-k`: doveadm keeps only its import_environment defaults. The
        // source password reaches doveadm through the private runtime config,
        // never through the environment.
        let mut args = Vec::new();
        if let Some(config) = runtime_config {
            args.extend(["-c".into(), config.to_owned()]);
        } else if !self.profile.dovecot_config.trim().is_empty() {
            args.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        args.extend([
            "-o".into(),
            format!("imapc_host={source_host}"),
            "-o".into(),
            format!("imapc_ssl={}", dovecot_ssl_mode(&self.profile.source_tls)),
            "-o".into(),
            format!("imapc_user={}", self.profile.source_user),
        ]);
        append_dovecot_source_tls_policy(
            &mut args,
            &self.profile.source_tls,
            &self.profile.source_ca_bundle,
        );
        if !self.profile.source_port.trim().is_empty() {
            args.extend([
                "-o".into(),
                format!("imapc_port={}", self.profile.source_port.trim()),
            ]);
        } else {
            args.extend(["-o".into(), format!("imapc_port={source_port}")]);
        }
        if dry_run {
            append_imapc_mail_settings(&mut args, dovecot_dialect);
            args.extend([
                "mailbox".into(),
                "list".into(),
                "-u".into(),
                self.profile.source_user.clone(),
            ]);
        } else {
            let preservation_sync = self.profile.dovecot_strategy.uses_preservation_sync();
            // `-l`, `-s`, `-1`, and `-u` are dsync subcommand options, not
            // global doveadm options. Put the sync/backup subcommand before
            // them or Dovecot 2.3 rejects the command as invalid.
            args.push(if preservation_sync { "sync" } else { "backup" }.into());
            args.extend(["-l".into(), DOVECOT_SYNC_LOCK_WAIT_SECONDS.to_string()]);
            // Dovecot prints a new state string when -s is supplied. An
            // empty state requests an initial stateful pass; a prior
            // committed checkpoint makes later passes incremental.
            args.extend([
                "-s".into(),
                core::dovecot_checkpoint_state(checkpoint.unwrap_or_default()).to_owned(),
            ]);
            if preservation_sync {
                args.push("-1".into());
            }
            args.extend([
                "-Ru".into(),
                self.profile.destination_user.clone(),
                "imapc:".into(),
            ]);
        }
        Ok((self.profile.doveadm_path.clone(), args))
    }

    #[cfg(test)]
    pub(crate) fn local_doveadm(&self) -> bool {
        true
    }

    #[cfg(test)]
    pub(crate) fn dovecot_verification_commands(&self, redact: bool) -> Vec<(String, Vec<String>)> {
        self.dovecot_verification_commands_with_config(redact, None, DovecotConfigDialect::Modern24)
    }

    pub(super) fn dovecot_verification_commands_with_config(
        &self,
        redact: bool,
        runtime_config: Option<&str>,
        dovecot_dialect: DovecotConfigDialect,
    ) -> Vec<(String, Vec<String>)> {
        if self.engine() != core::Engine::Dovecot {
            return Vec::new();
        }
        let _ = redact;
        let source_default_port = default_imap_port(&self.profile.source_tls);
        let (source_host, endpoint_port) =
            command_endpoint_parts(&self.profile.source_host, source_default_port);
        let source_port = command_port(&self.profile.source_port, endpoint_port);
        let mut source = Vec::new();
        if let Some(config) = runtime_config {
            source.extend(["-c".into(), config.to_owned()]);
        } else if !self.profile.dovecot_config.trim().is_empty() {
            source.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        source.extend([
            "-o".into(),
            format!("imapc_host={source_host}"),
            "-o".into(),
            format!("imapc_ssl={}", dovecot_ssl_mode(&self.profile.source_tls)),
            "-o".into(),
            format!("imapc_user={}", self.profile.source_user),
        ]);
        // These are global `doveadm -o` settings, so they must all precede
        // the `mailbox status` subcommand (the same rule as sync/backup).
        append_dovecot_source_tls_policy(
            &mut source,
            &self.profile.source_tls,
            &self.profile.source_ca_bundle,
        );
        if !self.profile.source_port.trim().is_empty() {
            source.extend([
                "-o".into(),
                format!("imapc_port={}", self.profile.source_port.trim()),
            ]);
        } else {
            source.extend(["-o".into(), format!("imapc_port={source_port}")]);
        }
        append_imapc_mail_settings(&mut source, dovecot_dialect);
        source.extend([
            "mailbox".into(),
            "status".into(),
            "-u".into(),
            self.profile.source_user.clone(),
            "messages vsize uidvalidity".into(),
            "*".into(),
        ]);
        let mut destination = Vec::new();
        if let Some(config) = runtime_config {
            destination.extend(["-c".into(), config.to_owned()]);
        } else if !self.profile.dovecot_config.trim().is_empty() {
            destination.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        destination.extend([
            "mailbox".into(),
            "status".into(),
            "-u".into(),
            self.profile.destination_user.clone(),
            "messages vsize uidvalidity".into(),
            "*".into(),
        ]);
        vec![
            (self.profile.doveadm_path.clone(), source),
            (self.profile.doveadm_path.clone(), destination),
        ]
    }

    pub(crate) fn dovecot_destination_preflight_commands(&self) -> Vec<(String, Vec<String>)> {
        if self.engine() != core::Engine::Dovecot {
            return Vec::new();
        }
        let mut user = Vec::new();
        if !self.profile.dovecot_config.trim().is_empty() {
            user.extend(["-c".into(), self.profile.dovecot_config.clone()]);
        }
        user.extend(["user".into(), self.profile.destination_user.clone()]);
        // Do not open or enumerate the destination mailboxes before the first
        // dsync. Dovecot warns that this can alter INBOX GUID/UIDVALIDITY and
        // cause the initial backup to fail (or reconcile the wrong state).
        // The userdb lookup validates the local destination identity without
        // touching the mailbox store.
        vec![(self.profile.doveadm_path.clone(), user)]
    }
}
