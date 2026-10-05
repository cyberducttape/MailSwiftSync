//! The versioned saved-profile file format and report plan snapshots.

use super::*;

pub(super) const MAX_PROFILE_BYTES: u64 = 1024 * 1024;

pub(super) const PROFILE_FORMAT: &str = "mailswiftsync-profile";

pub(super) const PROFILE_FORMAT_VERSION: i64 = 1;

#[derive(serde::Serialize)]
pub(super) struct SavedProfile<'a> {
    pub(super) format: &'static str,
    pub(super) format_version: i64,
    pub(super) profile: &'a Profile,
}

pub(super) fn read_profile_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_PROFILE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("profile exceeds the {MAX_PROFILE_BYTES}-byte limit"),
        ));
    }
    let mut text = String::new();
    file.by_ref()
        .take(MAX_PROFILE_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_PROFILE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("profile exceeds the {MAX_PROFILE_BYTES}-byte limit"),
        ));
    }
    Ok(text)
}

pub(crate) fn decode_report_run_snapshot(
    snapshot: &str,
) -> Result<Option<RunPlanSnapshot>, String> {
    if snapshot.trim().is_empty() {
        return Ok(None);
    }
    if snapshot.len() as u64 > MAX_PROFILE_BYTES {
        return Err(format!(
            "The evidence run plan snapshot exceeds the {MAX_PROFILE_BYTES}-byte limit"
        ));
    }
    toml::from_str(snapshot)
        .map(Some)
        .map_err(|error| format!("The evidence run plan snapshot is corrupt: {error}"))
}

pub(super) fn decode_saved_profile(path: &Path, text: &str) -> Result<Profile, String> {
    let raw: toml::Value = toml::from_str(text)
        .map_err(|error| format!("could not decode saved profile {}: {error}", path.display()))?;
    let profile_value = if raw.get("format").is_some() || raw.get("format_version").is_some() {
        let format = raw.get("format").and_then(toml::Value::as_str);
        if format != Some(PROFILE_FORMAT) {
            return Err(format!(
                "saved profile {} has an unknown or invalid format identifier",
                path.display()
            ));
        }
        let version = raw.get("format_version").and_then(toml::Value::as_integer);
        match version {
            Some(PROFILE_FORMAT_VERSION) => raw.get("profile").ok_or_else(|| {
                format!(
                    "saved profile {} is missing its profile table",
                    path.display()
                )
            })?,
            Some(version) => {
                return Err(format!(
                    "saved profile {} uses unsupported format version {version}",
                    path.display()
                ));
            }
            None => {
                return Err(format!(
                    "saved profile {} has a missing or invalid format version",
                    path.display()
                ));
            }
        }
    } else {
        // Version zero is the historical bare-Profile TOML format. Keep this
        // explicit migration path so existing installations remain readable.
        &raw
    };
    let remote_execution = profile_value
        .get("dovecot_execution")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("ssh"));
    let automatic_remote_execution = profile_value
        .get("dovecot_execution")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("automatic")
                && !["localhost", "127.0.0.1", "::1"].contains(
                    &profile_value
                        .get("destination_host")
                        .and_then(toml::Value::as_str)
                        .unwrap_or_default()
                        .trim(),
                )
        });
    let remote_user = profile_value
        .get("dovecot_ssh_user")
        .and_then(toml::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if remote_execution || automatic_remote_execution || remote_user {
        return Err(format!(
            "saved profile {} requests unsupported remote Dovecot execution; use local doveadm or imapsync",
            path.display()
        ));
    }
    profile_value
        .clone()
        .try_into()
        .map_err(|error| format!("could not decode saved profile {}: {error}", path.display()))
}

impl Form {
    pub(crate) fn path() -> Result<std::path::PathBuf, String> {
        dirs_next::config_dir()
            .map(|directory| directory.join("mailswiftsync/profile.toml"))
            .ok_or_else(|| {
                "Cannot determine a durable configuration directory; repair the OS profile or use an explicit state path.".to_owned()
            })
    }

    pub(crate) fn load() -> Result<Self, String> {
        let mut form = Self::default();
        let path = Self::path()?;
        let legacy = path
            .parent()
            .and_then(|parent| parent.parent())
            .map(|directory| directory.join("sourcecraft-imapsync/profile.toml"))
            .ok_or_else(|| "Saved profile path has no configuration directory.".to_owned())?;
        let text = match read_profile_file(&path) {
            Ok(text) => Some((path, text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match read_profile_file(&legacy) {
                    Ok(text) => Some((legacy, text)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(format!("could not read legacy profile: {error}")),
                }
            }
            Err(error) => return Err(format!("could not read profile: {error}")),
        };
        if let Some((path, text)) = text {
            form.profile = decode_saved_profile(&path, &text)?;
        }
        Ok(form)
    }

    #[allow(dead_code)]
    pub(crate) fn save(&self) -> Result<(), String> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            credentials::ensure_private_directory(parent).map_err(|e| e.to_string())?;
        }
        let content = toml::to_string_pretty(&SavedProfile {
            format: PROFILE_FORMAT,
            format_version: PROFILE_FORMAT_VERSION,
            profile: &self.profile,
        })
        .map_err(|e| e.to_string())?;
        write_private_atomic(&path, &content).map_err(|e| e.to_string())
    }
}
