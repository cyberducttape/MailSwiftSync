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
        return executable.is_file().then(|| executable.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(executable);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        if executable.extension().is_none() {
            let candidate = directory.join(format!("{}.exe", executable.display()));
            if candidate.is_file() {
                return Some(candidate);
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
