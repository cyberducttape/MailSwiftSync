use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub struct CleanupGuard {
    paths: Vec<PathBuf>,
}

impl CleanupGuard {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        cleanup_paths(&self.paths);
    }
}

pub fn create_secret_directory() -> Result<PathBuf, String> {
    let base = secret_runtime_base();
    fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    restrict_directory_permissions(&base).map_err(|error| error.to_string())?;
    let directory = base.join(format!("run-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).map_err(|error| error.to_string())?;
    if let Err(error) = restrict_directory_permissions(&directory) {
        let _ = fs::remove_dir(&directory);
        return Err(error.to_string());
    }
    Ok(directory)
}

pub fn secret_runtime_base() -> PathBuf {
    secret_runtime_base_from(std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

pub fn secret_runtime_base_from(runtime_dir: Option<PathBuf>) -> PathBuf {
    match runtime_dir.filter(|path| !path.as_os_str().is_empty()) {
        Some(path) => path.join("mailswiftsync"),
        None => fallback_secret_runtime_base(),
    }
}

#[cfg(unix)]
fn fallback_secret_runtime_base() -> PathBuf {
    // XDG_RUNTIME_DIR is normally per-user. When it is unavailable, retain
    // that isolation property in the system temporary directory instead of
    // converging all Unix users on one predictable shared pathname.
    let uid = unsafe { libc::geteuid() };
    std::env::temp_dir().join(format!("mailswiftsync-runtime-{uid}"))
}

#[cfg(not(unix))]
fn fallback_secret_runtime_base() -> PathBuf {
    std::env::temp_dir().join("mailswiftsync-runtime")
}

pub fn cleanup_stale_secret_directories(base: &Path) {
    const MAX_SECRET_DIRECTORY_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    cleanup_stale_secret_directories_at(base, SystemTime::now(), MAX_SECRET_DIRECTORY_AGE);
}

fn cleanup_stale_secret_directories_at(base: &Path, now: SystemTime, max_age: Duration) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Do not follow symlinks while deciding what the cleanup routine owns.
        // A link named run-* is not a MailSwiftSync secret directory.
        if !entry.file_name().to_string_lossy().starts_with("run-") || !file_type.is_dir() {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if stale {
            let _ = fs::remove_dir_all(path);
        }
    }
}

pub fn write_secret_file(path: &Path, secret: &str) -> std::io::Result<()> {
    let mut file = open_secret_file(path)?;
    file.write_all(secret.as_bytes())?;
    file.sync_all()?;
    restrict_file_permissions(path)
}

#[cfg(unix)]
pub fn open_secret_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
pub fn open_secret_file(path: &Path) -> std::io::Result<fs::File> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    #[cfg(windows)]
    restrict_file_permissions(path)?;
    Ok(file)
}

pub fn cleanup_paths(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
pub fn restrict_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)
}

#[cfg(windows)]
pub fn restrict_file_permissions(path: &Path) -> std::io::Result<()> {
    restrict_windows_acl(path, false)
}

#[cfg(all(not(unix), not(windows)))]
pub fn restrict_file_permissions(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub fn restrict_directory_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions)
}

#[cfg(windows)]
pub fn restrict_directory_permissions(path: &Path) -> std::io::Result<()> {
    restrict_windows_acl(path, true)
}

#[cfg(all(not(unix), not(windows)))]
pub fn restrict_directory_permissions(_: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn restrict_windows_acl(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{BOOL, ERROR_SUCCESS, HLOCAL, LocalFree},
        Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        },
        Security::{
            ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        },
    };

    // Owner and LocalSystem are the only principals that need access to
    // short-lived secrets, signing keys, locks, and the durable ledger. The
    // protected DACL prevents inherited Users/Administrators access from
    // silently widening the boundary on a permissive parent directory.
    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let sddl_wide = sddl
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut descriptor_size = 0_u32;
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            &mut descriptor_size,
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error());
    }

    let result = (|| {
        let mut dacl_present: BOOL = 0;
        let mut dacl_defaulted: BOOL = 0;
        let mut dacl: *mut ACL = ptr::null_mut();
        let valid_dacl = unsafe {
            GetSecurityDescriptorDacl(
                descriptor,
                &mut dacl_present,
                &mut dacl,
                &mut dacl_defaulted,
            )
        };
        if valid_dacl == 0 || dacl_present == 0 || dacl.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let error = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null_mut(),
            )
        };
        if error != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(error as i32));
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor as HLOCAL);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::cleanup_stale_secret_directories_at;
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    #[cfg(unix)]
    #[test]
    fn stale_cleanup_does_not_follow_run_symlinks() {
        use std::os::unix::fs::symlink;

        let suffix = uuid::Uuid::new_v4();
        let base = std::env::temp_dir().join(format!("mailswiftsync-cleanup-{suffix}"));
        let target = std::env::temp_dir().join(format!("mailswiftsync-target-{suffix}"));
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&target).unwrap();
        let link: PathBuf = base.join("run-linked");
        symlink(&target, &link).unwrap();

        cleanup_stale_secret_directories_at(
            &base,
            SystemTime::now() + Duration::from_secs(8 * 24 * 60 * 60),
            Duration::from_secs(7 * 24 * 60 * 60),
        );

        assert!(link.exists());
        assert!(target.exists());
        fs::remove_file(link).unwrap();
        fs::remove_dir(target).unwrap();
        fs::remove_dir(base).unwrap();
    }
}
