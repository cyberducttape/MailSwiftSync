//! Private, crash-safe writes for exported artifacts.

use crate::credentials::restrict_file_permissions;
use std::path::Path;

/// Write an artifact through a private temporary file, flush it to storage,
/// then atomically replace the destination. A failed write never leaves a
/// partially written destination artifact behind. Handles Windows and Unix.
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
        atomic_replace(&temporary, path)?;
        // ReplaceFileW preserves security attributes from the old destination.
        // Re-apply the private ACL/mode to the final path so an old permissive
        // destination cannot weaken the newly written artifact.
        restrict_file_permissions(path)?;
        sync_directory(path.parent())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
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

    match std::fs::metadata(dst) {
        Ok(_) => unsafe {
            if ReplaceFileW(
                dst_wide.as_ptr(),
                src_wide.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => unsafe {
            if MoveFileExW(src_wide.as_ptr(), dst_wide.as_ptr(), MOVEFILE_WRITE_THROUGH) == 0 {
                return Err(std::io::Error::last_os_error());
            }
        },
        Err(error) => return Err(error),
    }
    Ok(())
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
    fn atomic_write_creates_new_file() -> std::io::Result<()> {
        let temp_dir = std::env::temp_dir();
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
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn atomic_write_overwrites_existing_file_multiple_times() -> std::io::Result<()> {
        let temp_dir = std::env::temp_dir();
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
        let _ = fs::remove_file(&path);
        result
    }

    #[test]
    fn atomic_write_cleanup_on_error() -> std::io::Result<()> {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!("subdir-{}/test.txt", uuid::Uuid::new_v4()));

        // Directory doesn't exist, should fail
        let result = write_private_atomic(&path, "hello");
        assert!(result.is_err());
        Ok(())
    }
}
