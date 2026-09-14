//! Durable state-path selection and verified SQLite ledger restoration.

use crate::credentials::restrict_file_permissions;
use crate::{core, restrict_directory_permissions};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(crate) fn persistent_state_path() -> Result<PathBuf, String> {
    persistent_state_path_from(
        std::env::var_os("MAILSWIFTSYNC_STATE_PATH"),
        dirs_next::data_local_dir(),
    )
}

pub(crate) fn persistent_state_path_from(
    state_override: Option<OsString>,
    data_directory: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if let Some(path) = state_override {
        let path = PathBuf::from(path);
        if path.as_os_str().is_empty() {
            return Err("MAILSWIFTSYNC_STATE_PATH is set but empty.".into());
        }
        return Ok(path);
    }
    let data_directory = data_directory.ok_or_else(|| {
        "Cannot determine a durable state directory; set MAILSWIFTSYNC_STATE_PATH to an explicit ledger path.".to_owned()
    })?;
    Ok(data_directory.join("mailswiftsync/state.db"))
}

/// Restore a verified SQLite ledger without ever replacing the destination
/// with an unchecked or partially copied file. An existing destination is
/// retained as a uniquely named rollback artifact.
pub(crate) fn restore_ledger(backup: &Path, destination: &Path) -> Result<Option<PathBuf>, String> {
    if backup == destination {
        return Err("Restore source and destination must be different files.".into());
    }
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", backup.display(), suffix));
        if sidecar.exists() {
            return Err(format!(
                "restore source has a SQLite sidecar {}; use the backup command to create a standalone snapshot before restoring",
                sidecar.display()
            ));
        }
    }
    core::StateStore::open_readonly(backup)
        .map_err(|error| format!("restore source is not a valid current ledger: {error}"))?;
    let parent = destination
        .parent()
        .ok_or("Restore destination has no parent directory.")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create restore directory: {error}"))?;
    restrict_directory_permissions(parent)
        .map_err(|error| format!("could not secure restore directory: {error}"))?;
    if !destination.exists() {
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
            if sidecar.exists() {
                return Err(format!(
                    "restore destination has an orphaned SQLite sidecar {}; remove or recover it before restoring",
                    sidecar.display()
                ));
            }
        }
    }
    let temporary = parent.join(format!(
        ".{}.restore-{}.tmp",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state.db"),
        uuid::Uuid::new_v4()
    ));
    std::fs::copy(backup, &temporary)
        .map_err(|error| format!("could not copy restore source: {error}"))?;
    if let Err(error) = restrict_file_permissions(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not secure restored ledger: {error}"));
    }
    std::fs::File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("could not flush restored ledger: {error}"))?;
    if let Err(error) = core::StateStore::open_readonly(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!(
            "copied restore ledger failed integrity/schema validation: {error}"
        ));
    }
    let previous = if destination.exists() {
        let path = parent.join(format!(
            "{}.pre-restore-{}.db",
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state"),
            uuid::Uuid::new_v4()
        ));
        if let Err(error) = std::fs::rename(destination, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("could not preserve existing destination: {error}"));
        }
        let mut moved_sidecars = Vec::new();
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
            if !sidecar.exists() {
                continue;
            }
            let previous_sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
            if let Err(error) = std::fs::rename(&sidecar, &previous_sidecar) {
                for (original, preserved) in moved_sidecars.into_iter().rev() {
                    let _ = std::fs::rename(preserved, original);
                }
                let _ = std::fs::rename(&path, destination);
                let _ = std::fs::remove_file(&temporary);
                return Err(format!(
                    "could not preserve SQLite sidecar {sidecar:?}: {error}"
                ));
            }
            moved_sidecars.push((sidecar, previous_sidecar));
        }
        Some(path)
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&temporary, destination) {
        if let Some(previous) = &previous {
            let _ = std::fs::rename(previous, destination);
            for suffix in ["-wal", "-shm"] {
                let preserved = PathBuf::from(format!("{}{}", previous.display(), suffix));
                let original = PathBuf::from(format!("{}{}", destination.display(), suffix));
                let _ = std::fs::rename(preserved, original);
            }
        }
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("could not install restored ledger: {error}"));
    }
    restrict_file_permissions(destination).map_err(|error| {
        format!("restored ledger was installed but could not be secured: {error}")
    })?;
    sync_parent_directory(parent).map_err(|error| {
        format!("restored ledger was installed but could not be flushed: {error}")
    })?;
    Ok(previous)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_: &Path) -> std::io::Result<()> {
    Ok(())
}
