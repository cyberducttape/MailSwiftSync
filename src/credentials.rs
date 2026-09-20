use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use zeroize::Zeroizing;

/// Owned secret material. The only string access exposed to callers is
/// borrowed, so passing a credential through a worker does not silently turn
/// it into an ordinary `String` allocation.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct SecretString(Zeroizing<String>);

impl SecretString {
    pub(crate) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn as_mut_string(&mut self) -> &mut String {
        &mut self.0
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString(REDACTED)")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self::new(value.to_owned())
    }
}

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

/// Read an operator-provided headless credential without retaining it in an
/// ordinary owned string after the file read completes.
pub fn read_secret_file(path: &Path) -> Result<SecretString, String> {
    const MAX_SECRET_FILE_BYTES: u64 = 64 * 1024;
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("could not open secret file: {error}"))?
    };
    #[cfg(not(unix))]
    let mut file = open_secret_read_handle(path)?;

    // Inspect the already-open handle so validation and reading refer to the
    // same file. On Unix this also avoids following a symlink at open time.
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect secret file: {error}"))?;
    if !metadata.is_file() {
        return Err("secret-file path must refer to a regular file".into());
    }
    if metadata.len() > MAX_SECRET_FILE_BYTES {
        return Err(format!(
            "secret file exceeds the {MAX_SECRET_FILE_BYTES}-byte limit"
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("secret file must be owner-only (0600 or stricter)".into());
        }
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("secret file must be owned by the effective user".into());
        }
        if metadata.nlink() != 1 {
            return Err("secret file must not be hard-linked".into());
        }
    }
    let mut contents = Vec::with_capacity(metadata.len().min(MAX_SECRET_FILE_BYTES) as usize);
    file.read_to_end(&mut contents)
        .map_err(|error| format!("could not read secret file: {error}"))?;
    if contents.ends_with(b"\r\n") {
        contents.truncate(contents.len() - 2);
    } else if contents.ends_with(b"\n") || contents.ends_with(b"\r") {
        contents.truncate(contents.len() - 1);
    }
    let contents = String::from_utf8(contents)
        .map_err(|_| "secret file must contain valid UTF-8 text".to_owned())?;
    Ok(SecretString::new(contents))
}

#[cfg(windows)]
fn open_secret_read_handle(path: &Path) -> Result<fs::File, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    let file = OpenOptions::new()
        .read(true)
        // Open the final path component itself so a reparse point cannot be
        // substituted between validation and the read. A regular file is
        // required below; this flag makes that check apply to the opened
        // object rather than to a second path lookup.
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| format!("could not open secret file: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect secret file: {error}"))?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("secret-file path must not refer to a symlink or reparse point".into());
    }
    verify_windows_secret_acl(&file)?;
    Ok(file)
}

#[cfg(windows)]
fn verify_windows_secret_acl(file: &fs::File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
            EqualSid, GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
            SECURITY_MAX_SID_SIZE, WinCreatorOwnerRightsSid, WinLocalSystemSid,
        },
        System::SystemServices::ACCESS_ALLOWED_ACE_TYPE,
    };

    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as _,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(format!(
            "could not inspect secret-file DACL (Windows error {status})"
        ));
    }
    let result = (|| {
        if owner.is_null() || dacl.is_null() || descriptor.is_null() {
            return Err("secret file must have an explicit owner and DACL".into());
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        let valid_control =
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) };
        if valid_control == 0 || control & SE_DACL_PROTECTED == 0 {
            return Err("secret-file DACL must be protected from inheritance".into());
        }
        let mut dacl_present = 0;
        let mut dacl_defaulted = 0;
        let mut checked_dacl = ptr::null_mut();
        let valid_dacl = unsafe {
            GetSecurityDescriptorDacl(
                descriptor,
                &mut dacl_present,
                &mut checked_dacl,
                &mut dacl_defaulted,
            )
        };
        if valid_dacl == 0 || dacl_present == 0 || checked_dacl.is_null() || checked_dacl != dacl {
            return Err("secret file must have a present, stable DACL".into());
        }

        let mut local_system_sid = [0_u8; SECURITY_MAX_SID_SIZE as usize];
        let mut local_system_sid_size = local_system_sid.len() as u32;
        let local_system_sid = local_system_sid.as_mut_ptr() as PSID;
        if unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                ptr::null_mut(),
                local_system_sid,
                &mut local_system_sid_size,
            )
        } == 0
        {
            return Err("could not construct the LocalSystem SID".into());
        }
        let mut owner_rights_sid = [0_u8; SECURITY_MAX_SID_SIZE as usize];
        let mut owner_rights_sid_size = owner_rights_sid.len() as u32;
        let owner_rights_sid = owner_rights_sid.as_mut_ptr() as PSID;
        if unsafe {
            CreateWellKnownSid(
                WinCreatorOwnerRightsSid,
                ptr::null_mut(),
                owner_rights_sid,
                &mut owner_rights_sid_size,
            )
        } == 0
        {
            return Err("could not construct the Owner Rights SID".into());
        }

        let ace_count = unsafe { (*dacl).AceCount };
        let mut owner_rights_seen = false;
        let mut system_seen = false;
        for index in 0..u32::from(ace_count) {
            let mut ace_pointer = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut ace_pointer) } == 0 || ace_pointer.is_null() {
                return Err("could not inspect a secret-file DACL entry".into());
            }
            let header = unsafe { &*(ace_pointer as *const ACE_HEADER) };
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
                return Err("secret-file DACL contains an unsupported or inherited ACE".into());
            }
            let ace = unsafe { &*(ace_pointer as *const ACCESS_ALLOWED_ACE) };
            let sid = (&ace.SidStart as *const u32).cast_mut().cast();
            if unsafe { EqualSid(sid, owner_rights_sid) } != 0 {
                owner_rights_seen = true;
            } else if unsafe { EqualSid(sid, local_system_sid) } != 0 {
                system_seen = true;
            } else {
                return Err("secret-file DACL grants access to an unapproved identity".into());
            }
        }
        if ace_count != 2 || !owner_rights_seen || !system_seen {
            return Err("secret-file DACL must contain only Owner Rights and LocalSystem".into());
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor as HLOCAL);
    }
    result
}

#[cfg(all(not(unix), not(windows)))]
fn open_secret_read_handle(path: &Path) -> Result<fs::File, String> {
    fs::File::open(path).map_err(|error| format!("could not open secret file: {error}"))
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

/// Securely verify that a directory is writable.
/// Uses a UUID-based temporary file with create_new semantics and Unix-specific
/// safety flags (O_NOFOLLOW) to prevent symlink and race-condition attacks.
pub fn verify_directory_writable(dir: &Path) -> std::io::Result<()> {
    let test_file = dir.join(format!(".mailswiftsync-write-test-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&test_file)?;
        drop(file);
        Ok(())
    })();
    let _ = fs::remove_file(&test_file);
    result
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::cleanup_stale_secret_directories_at;
    use super::{SecretString, read_secret_file};
    use std::fs;
    #[cfg(unix)]
    use std::{
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    #[test]
    fn secret_debug_output_never_contains_material() {
        let secret = SecretString::from("customer-password");
        let debug = format!("{secret:?}");
        assert_eq!(debug, "SecretString(REDACTED)");
        assert!(!debug.contains("customer-password"));
    }

    #[test]
    fn secret_file_reader_strips_one_terminal_line_ending() {
        let path =
            std::env::temp_dir().join(format!("mailswiftsync-secret-{}", uuid::Uuid::new_v4()));
        super::write_secret_file(&path, "operator-secret\n").unwrap();
        let secret = read_secret_file(&path).unwrap();
        assert_eq!(secret.as_str(), "operator-secret");
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_reader_rejects_symlinks() {
        use std::os::unix::fs::symlink;
        let suffix = uuid::Uuid::new_v4();
        let target = std::env::temp_dir().join(format!("mailswiftsync-secret-target-{suffix}"));
        let link = std::env::temp_dir().join(format!("mailswiftsync-secret-link-{suffix}"));
        super::write_secret_file(&target, "operator-secret").unwrap();
        symlink(&target, &link).unwrap();
        let error = read_secret_file(&link).unwrap_err();
        assert!(error.contains("secret file"));
        fs::remove_file(link).unwrap();
        fs::remove_file(target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_reader_rejects_hard_links() {
        use std::fs::hard_link;
        let suffix = uuid::Uuid::new_v4();
        let target = std::env::temp_dir().join(format!("mailswiftsync-secret-target-{suffix}"));
        let link = std::env::temp_dir().join(format!("mailswiftsync-secret-hardlink-{suffix}"));
        super::write_secret_file(&target, "operator-secret").unwrap();
        hard_link(&target, &link).unwrap();
        let error = read_secret_file(&link).unwrap_err();
        assert!(error.contains("hard-linked"));
        fs::remove_file(link).unwrap();
        fs::remove_file(target).unwrap();
    }

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

    #[cfg(windows)]
    #[test]
    fn windows_private_acl_is_applied_to_runtime_secret_material() {
        let base = std::env::temp_dir().join(format!("mailswiftsync-acl-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        super::restrict_directory_permissions(&base).unwrap();
        let secret = base.join("secret");
        super::write_secret_file(&secret, "test-secret").unwrap();
        // The ACL calls are the assertion: failure means the runtime would
        // otherwise silently inherit a broader Windows DACL. Native CI runs
        // this test on Windows; the secret is removed even if later checks
        // fail.
        assert_eq!(fs::read_to_string(&secret).unwrap(), "test-secret");
        assert_eq!(read_secret_file(&secret).unwrap().as_str(), "test-secret");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn verify_directory_writable_succeeds_for_temp_dir() {
        let temp_dir = std::env::temp_dir();
        let result = super::verify_directory_writable(&temp_dir);
        assert!(result.is_ok(), "Failed to verify temp directory is writable: {:?}", result);
    }

    #[test]
    fn verify_directory_writable_fails_for_nonexistent_dir() {
        let nonexistent = std::env::temp_dir().join(format!("nonexistent-{}", uuid::Uuid::new_v4()));
        let result = super::verify_directory_writable(&nonexistent);
        assert!(result.is_err(), "Should fail for nonexistent directory");
    }
}
