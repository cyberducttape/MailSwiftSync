//! Filesystem identity and privacy checks for the SQLite ledger boundary.

use super::*;

#[cfg(unix)]
pub(super) type DatabaseIdentity = (u64, u64);
#[cfg(windows)]
pub(super) type DatabaseIdentity = crate::windows_private::FileIdentity;
#[cfg(all(not(unix), not(windows)))]
pub(super) type DatabaseIdentity = ();

pub(super) fn database_identity(path: &Path) -> std::io::Result<DatabaseIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "state database path is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        // Volume serial and FILE_ID_INFO from a no-follow handle, so a
        // replaced path is detected as it is by device/inode on Unix.
        crate::windows_private::path_identity(path)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        Ok(())
    }
}

pub(super) fn verify_database_identity(
    path: &Path,
    expected: DatabaseIdentity,
) -> std::io::Result<()> {
    if database_identity(path)? != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "state database path changed while opening",
        ));
    }
    Ok(())
}

pub(super) fn verify_database_parent(path: &Path) -> std::io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "state database path has no parent directory",
        )
    })?;
    crate::credentials::verify_private_directory(parent)?;
    Ok(parent)
}

pub(super) fn create_private_database_file(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    options.open(path).map(drop)
}

pub(super) fn remove_created_database_file(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let sidecar = Path::new(&format!("{}{}", path.display(), suffix)).to_owned();
        let _ = std::fs::remove_file(sidecar);
    }
}
