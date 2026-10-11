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
use ring::rand::SecureRandom;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub(crate) fn oauth_refresh_credential_digest(keyring_id: &str) -> String {
    // codeql[rust/weak-sensitive-data-hashing] keyring_id is a non-secret OS-keyring label, not a password or token; this digest is only a stable local lock/recovery filename and is not used for authentication.
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
) -> Result<String, String> {
    let encoded = zeroize::Zeroizing::new(crate::oauth_refresh::encode_refresh_config(config));
    let key = oauth_refresh_fingerprint_key()?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key[..]);
    Ok(digest_hex(
        ring::hmac::sign(&key, encoded.as_bytes()).as_ref(),
    ))
}

#[cfg(test)]
fn oauth_refresh_fingerprint_key() -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    Ok(zeroize::Zeroizing::new([0x5a; 32]))
}

#[cfg(not(test))]
fn oauth_refresh_fingerprint_key() -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    let directory = dirs_next::data_local_dir()
        .ok_or_else(|| "could not determine private OAuth fingerprint storage".to_owned())?
        .join("mailswiftsync");
    crate::credentials::ensure_private_directory(&directory)
        .map_err(|error| format!("could not secure OAuth fingerprint storage: {error}"))?;
    oauth_refresh_fingerprint_key_at(&directory.join("oauth-refresh-fingerprint.key"))
}

fn oauth_refresh_fingerprint_key_at(
    path: &std::path::Path,
) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "OAuth fingerprint key has no parent directory".to_owned())?;
    crate::credentials::ensure_private_directory(parent)
        .map_err(|error| format!("could not secure OAuth fingerprint directory: {error}"))?;
    let _lock = crate::process::acquire_named_lock_with_timeout(
        &path.with_extension("lock"),
        "another process is initializing OAuth fingerprint protection",
        std::time::Duration::from_secs(5),
    )?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let encoded = read_secret_file(path)?;
            if encoded.as_str().len() != 64 {
                return Err("OAuth fingerprint key has an invalid length".into());
            }
            let mut key = zeroize::Zeroizing::new([0_u8; 32]);
            for (index, pair) in encoded.as_str().as_bytes().chunks_exact(2).enumerate() {
                let pair = std::str::from_utf8(pair)
                    .map_err(|_| "OAuth fingerprint key is not valid hexadecimal".to_owned())?;
                key[index] = u8::from_str_radix(pair, 16)
                    .map_err(|_| "OAuth fingerprint key is not valid hexadecimal".to_owned())?;
            }
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = zeroize::Zeroizing::new([0_u8; 32]);
            ring::rand::SystemRandom::new()
                .fill(&mut key[..])
                .map_err(|_| {
                    "operating-system random source failed for OAuth fingerprint key".to_owned()
                })?;
            let encoded = zeroize::Zeroizing::new(digest_hex(&key[..]));
            crate::atomic_artifact::write_private_atomic(path, &encoded).map_err(|error| {
                format!("could not persist private OAuth fingerprint key: {error}")
            })?;
            Ok(key)
        }
        Err(error) => Err(format!("could not inspect OAuth fingerprint key: {error}")),
    }
}

fn oauth_refresh_legacy_sha256(config: &crate::oauth_refresh::OAuthRefreshConfig) -> String {
    let encoded = zeroize::Zeroizing::new(crate::oauth_refresh::encode_refresh_config(config));
    // codeql[rust/weak-sensitive-data-hashing] Only verifies legacy v1 recovery records.
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
    previous_config_fingerprint: PreviousOAuthConfigFingerprint,
    pub(crate) rotated_config: crate::oauth_refresh::OAuthRefreshConfig,
}

enum PreviousOAuthConfigFingerprint {
    LegacySha256(String),
    HmacSha256(String),
}

pub(crate) fn encode_oauth_refresh_recovery(
    previous: &crate::oauth_refresh::OAuthRefreshConfig,
    rotated: &crate::oauth_refresh::OAuthRefreshConfig,
) -> Result<zeroize::Zeroizing<String>, String> {
    #[derive(serde::Serialize)]
    struct RecoveryRecord<'a> {
        format_version: u8,
        previous_config_hmac_sha256: String,
        rotated_config: &'a str,
    }
    let rotated = crate::oauth_refresh::encode_refresh_config(rotated);
    Ok(zeroize::Zeroizing::new(
        serde_json::to_string(&RecoveryRecord {
            format_version: 2,
            previous_config_hmac_sha256: oauth_refresh_config_digest(previous)?,
            rotated_config: rotated.as_str(),
        })
        .map_err(|error| format!("could not encode OAuth recovery record: {error}"))?,
    ))
}

pub(crate) fn read_pending_oauth_refresh_recovery(
    path: &std::path::Path,
) -> Result<Option<PendingOAuthRefreshRecovery>, String> {
    #[derive(serde::Deserialize)]
    struct RecoveryRecord {
        format_version: u8,
        #[serde(default)]
        previous_config_sha256: Option<String>,
        #[serde(default)]
        previous_config_hmac_sha256: Option<String>,
        rotated_config: String,
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let stored = read_secret_file(path).map_err(|error| {
                format!("could not read protected OAuth recovery config: {error}")
            })?;
            let record: RecoveryRecord = serde_json::from_str(stored.as_str())
                .map_err(|error| format!("protected OAuth recovery record is invalid: {error}"))?;
            let previous_config_fingerprint = match record.format_version {
                1 => PreviousOAuthConfigFingerprint::LegacySha256(
                    record.previous_config_sha256.ok_or_else(|| {
                        "legacy OAuth recovery record is missing its config fingerprint".to_owned()
                    })?,
                ),
                2 => PreviousOAuthConfigFingerprint::HmacSha256(
                    record.previous_config_hmac_sha256.ok_or_else(|| {
                        "OAuth recovery record is missing its keyed config fingerprint".to_owned()
                    })?,
                ),
                _ => {
                    return Err("protected OAuth recovery record has an unsupported version".into());
                }
            };
            let rotated = zeroize::Zeroizing::new(record.rotated_config);
            let rotated_config = crate::oauth_refresh::decode_refresh_config(&rotated)?;
            Ok(Some(PendingOAuthRefreshRecovery {
                previous_config_fingerprint,
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
    if oauth_refresh_config_digest(&verified)? != oauth_refresh_config_digest(config)? {
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
    let (current_matches_previous, current_matches_rotated) =
        match &recovery.previous_config_fingerprint {
            PreviousOAuthConfigFingerprint::HmacSha256(previous) => {
                let current = oauth_refresh_config_digest(current_keyring_config)?;
                let rotated = oauth_refresh_config_digest(&recovery.rotated_config)?;
                (current == *previous, current == rotated)
            }
            PreviousOAuthConfigFingerprint::LegacySha256(previous) => {
                let current = oauth_refresh_legacy_sha256(current_keyring_config);
                let rotated = oauth_refresh_legacy_sha256(&recovery.rotated_config);
                (current == *previous, current == rotated)
            }
        };
    if current_matches_previous {
        crate::migration_plan::persist_rotated_refresh_config_with_retry(
            &recovery.rotated_config,
            |config| persist(config),
        )
        .map_err(|error| {
            format!(
                "could not restore the pending rotated OAuth credential to the keyring after bounded retries; protected recovery remains for the next attempt: {error}"
            )
        })?;
    } else if !current_matches_rotated {
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
    let recovery = encode_oauth_refresh_recovery(previous_config, config)?;
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

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    #[test]
    fn persisted_fingerprint_key_is_private_stable_and_rejects_corruption() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-oauth-fingerprint-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.join("fingerprint.key");
        let first = oauth_refresh_fingerprint_key_at(&path).unwrap();
        let second = oauth_refresh_fingerprint_key_at(&path).unwrap();
        assert_eq!(&first[..], &second[..]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let corrupt = directory.join("corrupt.key");
        crate::atomic_artifact::write_private_atomic(&corrupt, "not-a-key").unwrap();
        assert!(oauth_refresh_fingerprint_key_at(&corrupt).is_err());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn new_recovery_records_use_keyed_fingerprints_and_keep_v1_compatibility() {
        let previous = crate::oauth_refresh::OAuthRefreshConfig {
            token_endpoint: "https://oauth.example.test/token".into(),
            client_id: "client".into(),
            client_secret: crate::credentials::SecretString::from("client-secret"),
            refresh_token: crate::credentials::SecretString::from("old-refresh-token"),
        };
        let rotated = crate::oauth_refresh::OAuthRefreshConfig {
            refresh_token: crate::credentials::SecretString::from("rotated-refresh-token"),
            ..previous.clone()
        };
        let encoded_previous =
            zeroize::Zeroizing::new(crate::oauth_refresh::encode_refresh_config(&previous));
        let hmac = oauth_refresh_config_digest(&previous).unwrap();
        assert_ne!(
            hmac,
            digest_hex(&Sha256::digest(encoded_previous.as_bytes()))
        );
        let recovery = encode_oauth_refresh_recovery(&previous, &rotated).unwrap();
        let value: serde_json::Value = serde_json::from_str(&recovery).unwrap();
        assert_eq!(value["format_version"], 2);
        assert!(value["previous_config_hmac_sha256"].is_string());
        assert!(value.get("previous_config_sha256").is_none());

        let legacy_rotated = crate::oauth_refresh::encode_refresh_config(&rotated);
        let legacy = serde_json::json!({
            "format_version": 1,
            "previous_config_sha256": oauth_refresh_legacy_sha256(&previous),
            "rotated_config": legacy_rotated.as_str(),
        });
        assert_eq!(legacy["format_version"], 1);
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-oauth-legacy-recovery-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let recovery_path = directory.join("legacy.json");
        crate::atomic_artifact::write_private_atomic(
            &recovery_path,
            &serde_json::to_string(&legacy).unwrap(),
        )
        .unwrap();
        let recovered = recover_pending_oauth_refresh(&recovery_path, &previous, |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(recovered.refresh_token.as_str(), "rotated-refresh-token");
        assert!(!recovery_path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }
}
