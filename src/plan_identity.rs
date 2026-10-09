//! Stable identities used by preflight admission and exported evidence.
//!
//! This module deliberately contains no UI or controller state.  File
//! contents, rather than paths alone, are included where a plan depends on a
//! local executable or trust/configuration artifact.

use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::UNIX_EPOCH,
};

const MAX_EXECUTABLE_IDENTITY_CACHE: usize = 64;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExecutableFileKey {
    path: PathBuf,
    size: u64,
    modified_nanos: Option<u128>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_nanos: i64,
}

static EXECUTABLE_IDENTITY_CACHE: OnceLock<Mutex<HashMap<ExecutableFileKey, String>>> =
    OnceLock::new();

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

pub(crate) fn resolve_executable(executable: &str) -> Option<PathBuf> {
    let executable = Path::new(executable.trim());
    if executable.is_absolute() || executable.components().count() > 1 {
        return executable
            .is_file()
            .then(|| is_executable_file(executable))
            .flatten()
            .and_then(|()| std::fs::canonicalize(executable).ok());
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(executable);
        if candidate.is_file() && is_executable_file(&candidate).is_some() {
            return std::fs::canonicalize(candidate).ok();
        }
        #[cfg(windows)]
        if executable.extension().is_none() {
            let candidate = directory.join(format!("{}.exe", executable.display()));
            if candidate.is_file() && is_executable_file(&candidate).is_some() {
                return std::fs::canonicalize(candidate).ok();
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> Option<()> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        (metadata.permissions().mode() & 0o111 != 0).then_some(())
    }
    #[cfg(not(unix))]
    {
        metadata.is_file().then_some(())
    }
}

fn file_content_identity_uncached(path: &Path) -> String {
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

fn executable_file_key(path: &Path) -> Option<ExecutableFileKey> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    Some(ExecutableFileKey {
        path: path.to_path_buf(),
        size: metadata.len(),
        modified_nanos,
        #[cfg(unix)]
        device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
        #[cfg(unix)]
        changed_nanos: {
            use std::os::unix::fs::MetadataExt;
            metadata
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(metadata.ctime_nsec())
        },
    })
}

fn cached_executable_content_identity(path: &Path) -> String {
    let Some(key) = executable_file_key(path) else {
        return file_content_identity_uncached(path);
    };
    let cache = EXECUTABLE_IDENTITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(identity) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .cloned()
    {
        return identity;
    }
    let identity = file_content_identity_uncached(path);
    if identity.starts_with("sha256:") {
        let mut entries = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if entries.len() >= MAX_EXECUTABLE_IDENTITY_CACHE {
            entries.clear();
        }
        entries.insert(key, identity.clone());
    }
    identity
}

pub(crate) fn configured_file_content_identity(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        "none".into()
    } else {
        file_content_identity_uncached(Path::new(path))
    }
}

pub(crate) fn executable_content_identity(executable: &str) -> String {
    resolve_executable(executable)
        .as_deref()
        .map(cached_executable_content_identity)
        .or_else(|| {
            let path = Path::new(executable.trim());
            path.is_file().then(|| file_content_identity_uncached(path))
        })
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
    // Do not use the admission cache here: this is the final TOCTOU check
    // immediately before spawn and must read the current file contents.
    let actual_identity = file_content_identity_uncached(&path);
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        let expected = executable_content_identity(&path.to_string_lossy());
        assert!(revalidate_executable(&path.to_string_lossy(), &expected).is_ok());
        std::fs::write(&path, b"engine-b").unwrap();
        let error = revalidate_executable(&path.to_string_lossy(), &expected).unwrap_err();
        assert!(error.contains("identity changed"), "{error}");
        let _ = std::fs::remove_file(path);
    }
}
