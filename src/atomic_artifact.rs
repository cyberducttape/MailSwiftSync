//! Private, crash-safe writes for exported artifacts.
//!
//! Durability boundary: a successful return means the new artifact and its
//! directory entry were flushed to storage. A crash before the rename leaves
//! the previous artifact; a crash after it leaves the new one; neither is
//! ever partial. Nothing in the ledger records that an artifact was
//! published: every caller writes an export, report, or setting it can
//! regenerate, or (OAuth refresh state) keeps its own recovery file, so a
//! lost publication is redone rather than reconciled. A crash can leave a
//! private temporary file behind; the next write of a sibling artifact
//! removes temporaries older than `STALE_TEMPORARY_AGE`. On network
//! filesystems the rename and directory flush are only as durable as the
//! server makes them.

use crate::credentials::restrict_file_permissions;
use std::path::Path;
use std::time::Duration;

/// A temporary this old belongs to a writer that died: live writes finish
/// in seconds, and leaving it would keep a private copy of the content.
const STALE_TEMPORARY_AGE: Duration = Duration::from_secs(60 * 60);
const TEMPORARY_MARKER: &str = ".tmp-";

/// Where a test simulates a crash; see `crash_points` in the tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrashPoint {
    AfterWrite,
    BeforeReplace,
    BeforeDirectorySync,
}

#[cfg(test)]
thread_local! {
    static CRASH_AT: std::cell::Cell<Option<CrashPoint>> = const { std::cell::Cell::new(None) };
}

/// Stop as a killed process would: no cleanup runs.
#[cfg(test)]
fn crash_point(point: CrashPoint) {
    if CRASH_AT.with(|slot| slot.get()) == Some(point) {
        panic!("simulated crash at {point:?}");
    }
}

/// Write an artifact through a private temporary file, flush it to storage,
/// then atomically replace the destination. A failed write never leaves a
/// partially written destination artifact behind. Handles Windows and Unix.
pub(crate) fn write_private_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    write_private_atomic_with(path, |file| file.write_all(content.as_bytes()))
}

/// As `write_private_atomic`, but the caller streams the content into the
/// private temporary file, so large artifacts never need to be held in memory.
/// The destination is replaced only after `write` succeeds and is synced.
pub(crate) fn write_private_atomic_with(
    path: &Path,
    write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "artifact path has no parent directory",
        )
    })?;
    // Another user who can write the folder could replace the artifact
    // after it is written, so exports require an owner-only folder.
    crate::credentials::ensure_private_directory(parent).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "export folder {} is not private ({error}); choose a folder only you can write, or run `chmod go-w` on it",
                parent.display()
            ),
        )
    })?;
    remove_stale_temporaries(path);
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&temporary)?;
        let mut buffered = std::io::BufWriter::new(file);
        write(&mut buffered)?;
        let file = buffered.into_inner().map_err(|error| error.into_error())?;
        #[cfg(test)]
        crash_point(CrashPoint::AfterWrite);
        file.sync_all()?;
        restrict_file_permissions(&temporary)?;
        // Windows ReplaceFileW/MoveFileExW can reject a source file that is
        // still open, even though POSIX rename permits it. Flush and close
        // the completed temporary artifact before handing it to the replace
        // operation.
        drop(file);
        #[cfg(test)]
        crash_point(CrashPoint::BeforeReplace);
        atomic_replace(&temporary, path)?;
        #[cfg(test)]
        crash_point(CrashPoint::BeforeDirectorySync);
        // ReplaceFileW preserves security attributes from the old destination.
        // Re-apply the private ACL/mode to the final path so an old permissive
        // destination cannot weaken the newly written artifact.
        restrict_file_permissions(path)?;
        sync_directory(path.parent())
    })();
    cleanup_temporary_after_failure(result, &temporary, |path| std::fs::remove_file(path))
}

/// Remove temporaries a crashed writer left beside `path`'s artifact. They
/// are named `<stem>.tmp-<uuid>`; only old ones are removed, so a writer
/// still running is never disturbed. Best effort: a temporary that cannot
/// be removed is retried on the next write.
fn remove_stale_temporaries(path: &Path) {
    let (Some(parent), Some(stem)) = (path.parent(), path.file_stem()) else {
        return;
    };
    let prefix = format!("{}{TEMPORARY_MARKER}", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_temporary = name
            .strip_prefix(&prefix)
            .is_some_and(|rest| uuid::Uuid::parse_str(rest).is_ok());
        let stale = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_TEMPORARY_AGE);
        if is_temporary && stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn cleanup_temporary_after_failure(
    result: std::io::Result<()>,
    temporary: &Path,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let Err(primary_error) = result else {
        return result;
    };
    match remove(temporary) {
        Ok(()) => Err(primary_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(primary_error),
        Err(cleanup_error) => Err(std::io::Error::new(
            primary_error.kind(),
            format!(
                "{primary_error}; could not remove temporary artifact {}: {cleanup_error}",
                temporary.display()
            ),
        )),
    }
}

/// Atomically install the source file at the destination.
/// On Windows, creation and replacement use distinct same-volume APIs.
#[cfg(windows)]
fn atomic_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
    };

    let src_wide: Vec<u16> = std::ffi::OsStr::new(src.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_wide: Vec<u16> = std::ffi::OsStr::new(dst.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    retry_windows_sharing_violation(|| match std::fs::metadata(dst) {
        Ok(_) => {
            // SAFETY: the UTF-16 vectors are NUL-terminated, remain alive for
            // the call, and the Windows API does not retain either pointer.
            let replaced = unsafe {
                ReplaceFileW(
                    dst_wide.as_ptr(),
                    src_wide.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if replaced == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // SAFETY: both NUL-terminated path buffers remain alive through
            // MoveFileExW, and WRITE_THROUGH requests durable rename handling.
            let moved = unsafe {
                MoveFileExW(src_wide.as_ptr(), dst_wide.as_ptr(), MOVEFILE_WRITE_THROUGH)
            };
            if moved == 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
        Err(error) => Err(error),
    })
}

#[cfg(any(windows, test))]
fn retry_windows_sharing_violation(
    mut operation: impl FnMut() -> std::io::Result<()>,
) -> std::io::Result<()> {
    const ATTEMPTS: usize = 5;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

    for attempt in 0..ATTEMPTS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(error.raw_os_error(), Some(32 | 33)) && attempt + 1 < ATTEMPTS =>
            {
                std::thread::sleep(RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("the bounded retry loop always returns")
}

#[cfg(unix)]
fn atomic_replace(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::rename(src, dst)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn windows_replace_retries_only_transient_sharing_conflicts() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let attempts = AtomicUsize::new(0);
        retry_windows_sharing_violation(|| {
            if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(std::io::Error::from_raw_os_error(32))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 3);

        let attempts = AtomicUsize::new(0);
        let error = retry_windows_sharing_violation(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::from_raw_os_error(5))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn shared_export_folder_error_names_the_folder_and_the_fix() {
        use std::os::unix::fs::PermissionsExt;
        let folder = std::env::temp_dir().join(format!("mss-export-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o775)).unwrap();
        let error = write_private_atomic(&folder.join("proof.json"), "{}").unwrap_err();
        let message = error.to_string();
        assert!(message.contains(&folder.display().to_string()), "{message}");
        assert!(message.contains("chmod go-w"), "{message}");
        assert!(!folder.join("proof.json").exists());
        fs::remove_dir(&folder).unwrap();
    }

    #[test]
    fn atomic_write_creates_new_file() -> std::io::Result<()> {
        let temp_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("mailswiftsync-artifact-{}", uuid::Uuid::new_v4()));
        crate::credentials::ensure_private_directory(&temp_dir)?;
        let path = temp_dir.join(format!("mailswiftsync-test-{}.txt", uuid::Uuid::new_v4()));
        let result = (|| {
            write_private_atomic(&path, "hello")?;
            let content = fs::read_to_string(&path)?;
            assert_eq!(content, "hello");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&temp_dir);
        result
    }

    #[test]
    fn atomic_write_overwrites_existing_file_multiple_times() -> std::io::Result<()> {
        #[cfg(windows)]
        if crate::is_github_hosted_runner(std::env::var("RUNNER_ENVIRONMENT").ok().as_deref()) {
            eprintln!("⊘ Skipping: GitHub-hosted Windows runner does not permit ACL modifications");
            return Ok(());
        }

        let temp_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("mailswiftsync-artifact-{}", uuid::Uuid::new_v4()));
        crate::credentials::ensure_private_directory(&temp_dir)?;
        let path = temp_dir.join(format!("mailswiftsync-test-{}.txt", uuid::Uuid::new_v4()));

        let result = (|| {
            for i in 0..100 {
                let content = format!("iteration-{}", i);
                write_private_atomic(&path, &content)?;
                let read_content = fs::read_to_string(&path)?;
                assert_eq!(read_content, content, "Failed at iteration {}", i);
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(&temp_dir);
        result
    }

    fn private_test_directory(label: &str) -> std::io::Result<std::path::PathBuf> {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-artifact-{label}-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&directory)?;
        Ok(directory)
    }

    fn temporaries(directory: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
        Ok(fs::read_dir(directory)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().contains(TEMPORARY_MARKER))
            .collect())
    }

    /// A crash at each handoff leaves either the complete old artifact or
    /// the complete new one, and its leftover temporary is removed once it
    /// is stale.
    #[test]
    fn a_crash_at_every_publication_step_leaves_a_whole_artifact() -> std::io::Result<()> {
        for (point, survivor) in [
            (CrashPoint::AfterWrite, "old report"),
            (CrashPoint::BeforeReplace, "old report"),
            (CrashPoint::BeforeDirectorySync, "new report"),
        ] {
            let directory = private_test_directory("crash")?;
            let path = directory.join("report.json");
            write_private_atomic(&path, "old report")?;

            CRASH_AT.with(|slot| slot.set(Some(point)));
            let crashed = std::panic::catch_unwind(|| write_private_atomic(&path, "new report"));
            CRASH_AT.with(|slot| slot.set(None));
            assert!(crashed.is_err(), "{point:?}: the crash was not reached");
            assert_eq!(fs::read_to_string(&path)?, survivor, "{point:?}");

            let leftovers = temporaries(&directory)?;
            let expected = usize::from(point != CrashPoint::BeforeDirectorySync);
            assert_eq!(leftovers.len(), expected, "{point:?}: {leftovers:?}");
            // A fresh leftover may belong to a running writer and is kept.
            write_private_atomic(&directory.join("other.json"), "x")?;
            write_private_atomic(&path, "next report")?;
            assert_eq!(temporaries(&directory)?.len(), expected, "{point:?}");
            // Once stale, the next write removes it.
            for leftover in &leftovers {
                fs::File::options()
                    .write(true)
                    .open(leftover)?
                    .set_modified(std::time::SystemTime::now() - STALE_TEMPORARY_AGE * 2)?;
            }
            write_private_atomic(&path, "final report")?;
            assert!(temporaries(&directory)?.is_empty(), "{point:?}");
            assert_eq!(fs::read_to_string(&path)?, "final report");
            fs::remove_dir_all(&directory)?;
        }
        Ok(())
    }

    #[test]
    fn only_this_artifacts_stale_temporaries_are_removed() -> std::io::Result<()> {
        let directory = private_test_directory("sweep")?;
        let old = std::time::SystemTime::now() - STALE_TEMPORARY_AGE * 2;
        let make = |name: &str| -> std::io::Result<std::path::PathBuf> {
            let path = directory.join(name);
            fs::write(&path, "x")?;
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(old)?;
            Ok(path)
        };
        let unrelated = make(&format!("other{TEMPORARY_MARKER}{}", uuid::Uuid::new_v4()))?;
        let not_ours = make("report.tmp-not-a-uuid")?;
        let user_file = make("report.json.bak")?;
        let stale = make(&format!("report{TEMPORARY_MARKER}{}", uuid::Uuid::new_v4()))?;

        write_private_atomic(&directory.join("report.json"), "content")?;

        assert!(!stale.exists());
        assert!(unrelated.exists() && not_ours.exists() && user_file.exists());
        fs::remove_dir_all(&directory)?;
        Ok(())
    }

    #[test]
    fn atomic_write_cleanup_on_error() -> std::io::Result<()> {
        let temp_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-artifact-error-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&temp_dir)?;
        let path = temp_dir.join("destination");
        fs::create_dir(&path)?;

        // The trusted parent is valid, but the destination is a directory, so
        // replacement must fail after the temporary file is created.
        let result = write_private_atomic(&path, "hello");
        assert!(result.is_err());
        assert!(path.is_dir());
        assert_eq!(fs::read_dir(&temp_dir)?.count(), 1);
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    #[test]
    fn atomic_write_reports_temporary_cleanup_failure() -> std::io::Result<()> {
        let temp_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "mailswiftsync-artifact-cleanup-{}",
                uuid::Uuid::new_v4()
            ));
        crate::credentials::ensure_private_directory(&temp_dir)?;
        let temporary = temp_dir.join("stuck-temporary");
        fs::create_dir(&temporary)?;

        let error = cleanup_temporary_after_failure(
            Err(std::io::Error::other("injected write failure")),
            &temporary,
            |path| fs::remove_file(path),
        )
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("injected write failure"));
        assert!(message.contains("could not remove temporary artifact"));
        assert!(message.contains(&temporary.display().to_string()));
        assert!(temporary.is_dir());
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_rejects_a_symlinked_parent() -> std::io::Result<()> {
        use std::os::unix::fs::symlink;

        let suffix = uuid::Uuid::new_v4();
        let victim = std::env::temp_dir().join(format!("mailswiftsync-artifact-victim-{suffix}"));
        let link = std::env::temp_dir().join(format!("mailswiftsync-artifact-link-{suffix}"));
        fs::create_dir(&victim)?;
        symlink(&victim, &link)?;
        let result = write_private_atomic(&link.join("report.json"), "secret");
        assert!(result.is_err());
        assert!(victim.read_dir()?.next().is_none());
        fs::remove_file(link)?;
        fs::remove_dir(victim)?;
        Ok(())
    }
}
