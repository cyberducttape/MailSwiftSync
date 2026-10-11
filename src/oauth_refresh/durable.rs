//! Durable, cross-process coordination for rotating OAuth refresh tokens.
//!
//! A provider that rotates refresh tokens consumes the old token on every
//! exchange. Every exchange for one keyring credential, whether made by a
//! batch launch in this process or by an imapsync reconnect callback in a
//! helper process, therefore holds one per-credential file lock while it
//! reads the latest token, exchanges it, and persists the replacement. A
//! rotated token that could not reach the keyring is kept in an owner-only
//! recovery file and replayed by the next exchange for that credential.

use crate::credentials::read_secret_file;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub(crate) fn oauth_refresh_credential_digest(keyring_id: &str) -> String {
    digest_hex(&Sha256::digest(keyring_id.as_bytes()))
}

pub(crate) fn digest_hex(digest: &[u8]) -> String {
    let mut file_name = String::with_capacity(digest.len() * 2 + 4);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(file_name, "{byte:02x}");
    }
    file_name
}

pub(crate) fn oauth_refresh_config_digest(
    config: &crate::oauth_refresh::OAuthRefreshConfig,
) -> String {
    let encoded = crate::oauth_refresh::encode_refresh_config(config);
    digest_hex(&Sha256::digest(encoded.as_bytes()))
}

pub(crate) fn oauth_refresh_lock_path(
    lock_directory: &std::path::Path,
    keyring_id: &str,
) -> PathBuf {
    lock_directory.join(format!(
        "{}.lock",
        oauth_refresh_credential_digest(keyring_id)
    ))
}

pub(crate) fn oauth_refresh_recovery_path(
    recovery_directory: &std::path::Path,
    keyring_id: &str,
) -> PathBuf {
    recovery_directory.join(format!(
        "{}.json",
        oauth_refresh_credential_digest(keyring_id)
    ))
}

pub(crate) fn oauth_refresh_file_recovery_path(
    config_path: &std::path::Path,
) -> Result<PathBuf, String> {
    let file_name = config_path
        .file_name()
        .ok_or_else(|| "OAuth refresh config path has no file name".to_owned())?;
    let mut recovery_name = file_name.to_os_string();
    recovery_name.push(".oauth-refresh-recovery");
    Ok(config_path.with_file_name(recovery_name))
}

pub(crate) fn oauth_refresh_recovery_directory() -> Result<PathBuf, String> {
    let data_directory = dirs_next::data_local_dir()
        .ok_or_else(|| "could not determine a private directory for OAuth recovery".to_owned())?;
    let recovery_directory = data_directory.join("mailswiftsync/oauth-refresh-recovery");
    crate::credentials::ensure_private_directory(&recovery_directory)
        .map_err(|error| format!("could not secure OAuth refresh recovery directory: {error}"))?;
    Ok(recovery_directory)
}

pub(crate) fn acquire_oauth_refresh_lock_in(
    lock_directory: &std::path::Path,
    keyring_id: &str,
    timeout: std::time::Duration,
) -> Result<crate::process::InstanceLock, String> {
    crate::process::acquire_named_lock_with_timeout(
        &oauth_refresh_lock_path(lock_directory, keyring_id),
        "another migration is refreshing this OAuth credential; retry after it finishes",
        timeout,
    )
}

/// How long an exchange waits for another holder of the same credential. One
/// exchange is a single bounded HTTPS request, so this covers a queue of
/// concurrent launches without letting a wedged holder stall a run forever.
pub(crate) const OAUTH_REFRESH_LOCK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(35);

/// Take the per-credential lock shared by every process that exchanges the
/// refresh token stored under `keyring_id`.
pub(crate) fn acquire_oauth_refresh_lock(
    keyring_id: &str,
) -> Result<crate::process::InstanceLock, String> {
    let data_directory = dirs_next::data_local_dir().ok_or_else(|| {
        "could not determine a private directory for OAuth refresh coordination".to_owned()
    })?;
    let lock_directory = data_directory.join("mailswiftsync/oauth-refresh-locks");
    crate::credentials::ensure_private_directory(&lock_directory).map_err(|error| {
        format!("could not secure OAuth refresh coordination directory: {error}")
    })?;
    acquire_oauth_refresh_lock_in(&lock_directory, keyring_id, OAUTH_REFRESH_LOCK_TIMEOUT)
        .map_err(|error| format!("could not coordinate OAuth refresh: {error}"))
}

pub(crate) struct PendingOAuthRefreshRecovery {
    pub(crate) previous_config_sha256: String,
    pub(crate) rotated_config: crate::oauth_refresh::OAuthRefreshConfig,
}

pub(crate) fn encode_oauth_refresh_recovery(
    previous: &crate::oauth_refresh::OAuthRefreshConfig,
    rotated: &crate::oauth_refresh::OAuthRefreshConfig,
) -> zeroize::Zeroizing<String> {
    #[derive(serde::Serialize)]
    struct RecoveryRecord<'a> {
        format_version: u8,
        previous_config_sha256: String,
        rotated_config: &'a str,
    }
    let rotated = crate::oauth_refresh::encode_refresh_config(rotated);
    zeroize::Zeroizing::new(
        serde_json::to_string(&RecoveryRecord {
            format_version: 1,
            previous_config_sha256: oauth_refresh_config_digest(previous),
            rotated_config: rotated.as_str(),
        })
        .expect("OAuth recovery record is serializable"),
    )
}

pub(crate) fn read_pending_oauth_refresh_recovery(
    path: &std::path::Path,
) -> Result<Option<PendingOAuthRefreshRecovery>, String> {
    #[derive(serde::Deserialize)]
    struct RecoveryRecord {
        format_version: u8,
        previous_config_sha256: String,
        rotated_config: String,
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let stored = read_secret_file(path).map_err(|error| {
                format!("could not read protected OAuth recovery config: {error}")
            })?;
            let record: RecoveryRecord = serde_json::from_str(stored.as_str())
                .map_err(|error| format!("protected OAuth recovery record is invalid: {error}"))?;
            if record.format_version != 1 {
                return Err("protected OAuth recovery record has an unsupported version".into());
            }
            let rotated = zeroize::Zeroizing::new(record.rotated_config);
            let rotated_config = crate::oauth_refresh::decode_refresh_config(&rotated)?;
            Ok(Some(PendingOAuthRefreshRecovery {
                previous_config_sha256: record.previous_config_sha256,
                rotated_config,
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "could not inspect protected OAuth recovery config: {error}"
        )),
    }
}

pub(crate) fn persist_and_verify_keyring_refresh_config(
    entry: &keyring::Entry,
    config: &crate::oauth_refresh::OAuthRefreshConfig,
) -> Result<(), String> {
    let encoded = crate::oauth_refresh::encode_refresh_config(config);
    entry.set_password(&encoded).map_err(|error| {
        format!("OS keyring could not store the rotated OAuth refresh config: {error}")
    })?;
    let stored = entry
        .get_password()
        .map_err(|error| format!("could not verify OAuth refresh config in OS keyring: {error}"))?;
    let verified = crate::oauth_refresh::decode_refresh_config(&stored)
        .map_err(|error| format!("OS keyring returned an invalid OAuth refresh config: {error}"))?;
    if oauth_refresh_config_digest(&verified) != oauth_refresh_config_digest(config) {
        return Err("OS keyring returned a different OAuth refresh configuration".to_owned());
    }
    Ok(())
}

pub(crate) fn clear_oauth_refresh_recovery(path: &std::path::Path) -> Result<(), String> {
    let mut cleanup = crate::credentials::CleanupGuard::new(vec![path.to_owned()]);
    cleanup
        .cleanup_now()
        .map_err(|error| format!("could not remove recovered OAuth configuration: {error}"))
}

pub(crate) fn recover_pending_oauth_refresh(
    path: &std::path::Path,
    current_keyring_config: &crate::oauth_refresh::OAuthRefreshConfig,
    mut persist: impl FnMut(&crate::oauth_refresh::OAuthRefreshConfig) -> Result<(), String>,
) -> Result<Option<crate::oauth_refresh::OAuthRefreshConfig>, String> {
    let Some(recovery) = read_pending_oauth_refresh_recovery(path)? else {
        return Ok(None);
    };
    let current_digest = oauth_refresh_config_digest(current_keyring_config);
    let rotated_digest = oauth_refresh_config_digest(&recovery.rotated_config);
    if current_digest == recovery.previous_config_sha256 {
        crate::migration_plan::persist_rotated_refresh_config_with_retry(
            &recovery.rotated_config,
            |config| persist(config),
        )
        .map_err(|error| {
            format!(
                "could not restore the pending rotated OAuth credential to the keyring after bounded retries; protected recovery remains for the next attempt: {error}"
            )
        })?;
    } else if current_digest != rotated_digest {
        return Err(
            "the OAuth keyring entry changed since a rotation recovery was saved; refusing to overwrite it, and retaining the protected recovery file for operator review".into(),
        );
    }
    clear_oauth_refresh_recovery(path)?;
    Ok(Some(recovery.rotated_config))
}

pub(crate) fn persist_rotated_oauth_refresh(
    recovery_path: &std::path::Path,
    run_config_path: Option<&std::path::Path>,
    previous_config: &crate::oauth_refresh::OAuthRefreshConfig,
    config: &crate::oauth_refresh::OAuthRefreshConfig,
    mut persist: impl FnMut(&crate::oauth_refresh::OAuthRefreshConfig) -> Result<(), String>,
) -> Result<(), String> {
    let encoded = crate::oauth_refresh::encode_refresh_config(config);
    let recovery = encode_oauth_refresh_recovery(previous_config, config);
    crate::atomic_artifact::write_private_atomic(recovery_path, &recovery).map_err(|error| {
        format!(
            "provider rotated the OAuth refresh token but protected durable recovery could not be written: {error}"
        )
    })?;
    if let Some(run_config_path) = run_config_path {
        crate::atomic_artifact::write_private_atomic(run_config_path, &encoded).map_err(
            |error| {
                format!(
                    "provider rotated the OAuth refresh token; durable recovery was saved but the per-run config could not be updated: {error}"
                )
            },
        )?;
    }
    crate::migration_plan::persist_rotated_refresh_config_with_retry(config, |config| {
        persist(config)
    })
    .map_err(|error| {
        format!(
            "could not persist the rotated OAuth config to the OS keyring after bounded retries; protected recovery remains for the next attempt: {error}"
        )
    })?;
    clear_oauth_refresh_recovery(recovery_path)
}
