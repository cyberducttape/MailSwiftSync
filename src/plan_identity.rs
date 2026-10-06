//! Stable identities used by preflight admission and exported evidence.
//!
//! This module deliberately contains no UI or controller state.  File
//! contents, rather than paths alone, are included where a plan depends on a
//! local executable or trust/configuration artifact.

use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) fn snapshot_sha256(snapshot: &str) -> String {
    let digest = Sha256::digest(snapshot.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>()
}

pub(crate) fn fingerprint_digest(fingerprint: &str) -> String {
    snapshot_sha256(fingerprint)
}

fn resolve_executable(executable: &str) -> Option<PathBuf> {
    let executable = Path::new(executable.trim());
    if executable.is_absolute() || executable.components().count() > 1 {
        return executable
            .is_file()
            .then(|| std::fs::canonicalize(executable).ok())
            .flatten();
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(executable);
        if candidate.is_file() {
            return std::fs::canonicalize(candidate).ok();
        }
        #[cfg(windows)]
        if executable.extension().is_none() {
            let candidate = directory.join(format!("{}.exe", executable.display()));
            if candidate.is_file() {
                return std::fs::canonicalize(candidate).ok();
            }
        }
    }
    None
}

fn file_content_identity(path: &Path) -> String {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return format!("unavailable:{:?}", error.kind()),
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(error) => return format!("unavailable:{:?}", error.kind()),
        };
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    format!("sha256:{}", hex)
}

pub(crate) fn configured_file_content_identity(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        "none".into()
    } else {
        file_content_identity(Path::new(path))
    }
}

pub(crate) fn executable_content_identity(executable: &str) -> String {
    resolve_executable(executable)
        .as_deref()
        .map(file_content_identity)
        .unwrap_or_else(|| "unresolved".into())
}

/// Re-resolve and re-hash an executable immediately before launch. Returning
/// the canonical path also prevents a PATH lookup or symlink traversal during
/// `Command::spawn` from selecting a different file than the one checked.
pub(crate) fn revalidate_executable(
    executable: &str,
    expected_identity: &str,
) -> Result<PathBuf, String> {
    let path = resolve_executable(executable)
        .ok_or_else(|| format!("could not resolve executable {executable:?} before launch"))?;
    let actual_identity = file_content_identity(&path);
    if !expected_identity.starts_with("sha256:") || !actual_identity.starts_with("sha256:") {
        return Err(format!(
            "executable identity is unavailable before launch: expected {expected_identity}, found {actual_identity}"
        ));
    }
    if actual_identity != expected_identity {
        return Err(format!(
            "executable identity changed before launch: expected {expected_identity}, found {actual_identity}"
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_revalidation_rejects_replacement_before_launch() {
        let path =
            std::env::temp_dir().join(format!("mailswiftsync-executable-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"engine-a").unwrap();
        let expected = executable_content_identity(&path.to_string_lossy());
        assert!(revalidate_executable(&path.to_string_lossy(), &expected).is_ok());
        std::fs::write(&path, b"engine-b").unwrap();
        let error = revalidate_executable(&path.to_string_lossy(), &expected).unwrap_err();
        assert!(error.contains("identity changed"), "{error}");
        let _ = std::fs::remove_file(path);
    }
}
