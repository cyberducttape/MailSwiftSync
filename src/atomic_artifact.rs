//! Private, crash-safe writes for exported artifacts.

use crate::credentials::restrict_file_permissions;
use std::path::Path;

/// Write an artifact through a private temporary file, flush it to storage,
/// then atomically rename it into place. A failed write never leaves a
/// partially written destination artifact behind.
pub(crate) fn write_private_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        std::io::Write::write_all(&mut file, content.as_bytes())?;
        file.sync_all()?;
        restrict_file_permissions(&temporary)?;
        std::fs::rename(&temporary, path)?;
        sync_directory(path.parent())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_directory(path: Option<&Path>) -> std::io::Result<()> {
    if let Some(path) = path {
        std::fs::File::open(path)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_: Option<&Path>) -> std::io::Result<()> {
    Ok(())
}
