//! Durable state-path selection and verified SQLite ledger restoration.

use crate::core;
use crate::credentials::restrict_file_permissions;
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
                "restore source is a live SQLite database with sidecar {}; run the backup command first to create a standalone snapshot",
                sidecar.display()
            ));
        }
    }
    let parent = destination
        .parent()
        .ok_or("Restore destination has no parent directory.")?;

    // Security boundary: only restrict permissions on directories we create.
    // If the parent already exists, verify it is writable; do not chmod system directories.
    match std::fs::symlink_metadata(parent) {
        Ok(_) => {
            // Establish the no-follow ownership and permission boundary
            // before probing writability; do not chmod system directories.
            crate::credentials::verify_private_directory(parent)
                .and_then(|_| crate::credentials::verify_directory_writable(parent))
                .map_err(|e| format!("Restore directory exists but is not writable: {e}"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Parent does not exist. Create it and restrict permissions.
            crate::credentials::ensure_private_directory(parent)
                .map_err(|error| format!("could not secure restore directory: {error}"))?;
        }
        Err(e) => return Err(format!("Could not access restore directory parent: {e}")),
    }
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
    if let Err(error) = core::StateStore::snapshot_to(backup, &temporary) {
        return Err(format_restore_preinstall_failure(
            format!("could not snapshot restore source: {error}"),
            Ok(()),
            cleanup_restore_temporary(&temporary),
        ));
    }
    if let Err(error) = restrict_file_permissions(&temporary) {
        return Err(format_restore_preinstall_failure(
            format!("could not secure restored ledger: {error}"),
            Ok(()),
            cleanup_restore_temporary(&temporary),
        ));
    }
    if let Err(error) = std::fs::OpenOptions::new()
        .write(true)
        .open(&temporary)
        .and_then(|file| file.sync_all())
    {
        return Err(format_restore_preinstall_failure(
            format!("could not flush restored ledger: {error}"),
            Ok(()),
            cleanup_restore_temporary(&temporary),
        ));
    }
    if let Err(error) = core::StateStore::open_readonly(&temporary) {
        return Err(format_restore_preinstall_failure(
            format!("copied restore ledger failed integrity/schema validation: {error}"),
            Ok(()),
            cleanup_restore_temporary(&temporary),
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
            return Err(format_restore_preinstall_failure(
                format!("could not preserve existing destination: {error}"),
                Ok(()),
                cleanup_restore_temporary(&temporary),
            ));
        }
        let mut moved_sidecars = Vec::new();
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
            if !sidecar.exists() {
                continue;
            }
            let previous_sidecar = PathBuf::from(format!("{}{}", path.display(), suffix));
            if let Err(error) = std::fs::rename(&sidecar, &previous_sidecar) {
                let rollback = restore_preserved_destination(&path, destination, &moved_sidecars);
                let cleanup = cleanup_restore_temporary(&temporary);
                return Err(format_restore_preinstall_failure(
                    format!("could not preserve SQLite sidecar {sidecar:?}: {error}"),
                    rollback,
                    cleanup,
                ));
            }
            moved_sidecars.push((sidecar, previous_sidecar));
        }
        Some((path, moved_sidecars))
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&temporary, destination) {
        let rollback = previous.as_ref().map_or(Ok(()), |(path, sidecars)| {
            restore_preserved_destination(path, destination, sidecars)
        });
        let cleanup = cleanup_restore_temporary(&temporary);
        return Err(format_restore_preinstall_failure(
            format!("could not install restored ledger: {error}"),
            rollback,
            cleanup,
        ));
    }
    if let Err(error) = restrict_file_permissions(destination) {
        let rollback = rollback_restore_installation(destination, previous.as_ref());
        return Err(format_restore_failure("secure", error, rollback));
    }
    if let Err(error) = sync_parent_directory(parent) {
        let rollback = rollback_restore_installation(destination, previous.as_ref());
        return Err(format_restore_failure("flush", error, rollback));
    }
    Ok(previous.map(|(path, _)| path))
}

fn restore_preserved_destination(
    previous: &Path,
    destination: &Path,
    sidecars: &[(PathBuf, PathBuf)],
) -> Result<(), String> {
    restore_preserved_destination_with(previous, destination, sidecars, |from, to| {
        std::fs::rename(from, to)
    })
}

fn restore_preserved_destination_with(
    previous: &Path,
    destination: &Path,
    sidecars: &[(PathBuf, PathBuf)],
    mut rename: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    if let Err(error) = rename(previous, destination) {
        failures.push(format!(
            "could not restore previous ledger {previous:?} to {destination:?}: {error}"
        ));
    }
    for (original, preserved) in sidecars.iter().rev() {
        if let Err(error) = rename(preserved, original) {
            failures.push(format!(
                "could not restore previous SQLite sidecar {preserved:?} to {original:?}: {error}"
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn cleanup_restore_temporary(path: &Path) -> Result<(), String> {
    let mut failures = Vec::new();
    for candidate in std::iter::once(path.to_path_buf()).chain(
        ["-wal", "-shm"].map(|suffix| PathBuf::from(format!("{}{}", path.display(), suffix))),
    ) {
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!(
                "could not remove restore temporary {candidate:?}: {error}"
            )),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn format_restore_preinstall_failure(
    primary: String,
    rollback: Result<(), String>,
    cleanup: Result<(), String>,
) -> String {
    let mut message = primary;
    if let Err(error) = rollback {
        message.push_str(&format!("; rollback also failed: {error}"));
    }
    if let Err(error) = cleanup {
        message.push_str(&format!("; temporary cleanup also failed: {error}"));
    }
    message
}

fn rollback_restore_installation(
    destination: &Path,
    previous: Option<&(PathBuf, Vec<(PathBuf, PathBuf)>)>,
) -> Result<(), String> {
    remove_installed_sidecars(destination)?;
    std::fs::remove_file(destination)
        .map_err(|error| format!("could not remove installed ledger: {error}"))?;
    let Some((previous, sidecars)) = previous else {
        return Ok(());
    };
    restore_preserved_destination(previous, destination, sidecars)
}

fn remove_installed_sidecars(destination: &Path) -> Result<(), String> {
    remove_installed_sidecars_with(destination, |path| std::fs::remove_file(path))
}

fn remove_installed_sidecars_with(
    destination: &Path,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", destination.display(), suffix));
        match remove(&sidecar) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!(
                "could not remove installed SQLite sidecar {sidecar:?}: {error}"
            )),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn format_restore_failure(
    operation: &str,
    error: std::io::Error,
    rollback: Result<(), String>,
) -> String {
    match rollback {
        Ok(()) => {
            format!("could not {operation} restored ledger; previous state was restored: {error}")
        }
        Err(rollback_error) => format!(
            "could not {operation} restored ledger ({error}); rollback also failed: {rollback_error}"
        ),
    }
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_rollback_attempts_every_rename_and_reports_each_failure() {
        let previous = Path::new("previous.db");
        let destination = Path::new("state.db");
        let sidecars = vec![
            (
                PathBuf::from("state.db-wal"),
                PathBuf::from("previous.db-wal"),
            ),
            (
                PathBuf::from("state.db-shm"),
                PathBuf::from("previous.db-shm"),
            ),
        ];
        let mut attempts = Vec::new();

        let error =
            restore_preserved_destination_with(previous, destination, &sidecars, |from, to| {
                attempts.push((from.to_path_buf(), to.to_path_buf()));
                if from == Path::new("previous.db-wal") {
                    Err(std::io::Error::other("injected sidecar rollback failure"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

        assert_eq!(attempts.len(), 3, "all rollback renames must be attempted");
        assert!(error.contains("previous.db-wal"));
        assert!(error.contains("injected sidecar rollback failure"));
    }

    #[test]
    fn restore_preinstall_failure_reports_rollback_and_cleanup_errors() {
        let message = format_restore_preinstall_failure(
            "install failed".to_owned(),
            Err("rollback failed".to_owned()),
            Err("cleanup failed".to_owned()),
        );

        assert!(message.contains("install failed"));
        assert!(message.contains("rollback also failed: rollback failed"));
        assert!(message.contains("temporary cleanup also failed: cleanup failed"));
    }

    #[test]
    fn installed_sidecar_cleanup_attempts_both_files_and_reports_failure() {
        let mut attempts = Vec::new();
        let error = remove_installed_sidecars_with(Path::new("state.db"), |path| {
            attempts.push(path.to_path_buf());
            if path == Path::new("state.db-wal") {
                Err(std::io::Error::other("injected sidecar cleanup failure"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();

        assert_eq!(attempts.len(), 2, "both sidecar removals must be attempted");
        assert!(error.contains("state.db-wal"));
        assert!(error.contains("injected sidecar cleanup failure"));
    }
}
