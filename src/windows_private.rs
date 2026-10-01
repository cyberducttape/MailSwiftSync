//! Handle-based Windows checks for private state boundaries.
//!
//! Unix callers rely on `O_NOFOLLOW` descriptors, `st_uid`, mode bits, and
//! device/inode identity. The Windows equivalents here open the final path
//! component itself (never a reparse-point target), inspect the owner SID and
//! DACL of that handle, apply protected DACLs through the handle, and read the
//! stable `FILE_ID_INFO` identity. No decision is made from a second pathname
//! lookup that another account could redirect.

use std::{
    fs, io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_SUCCESS, GENERIC_ALL, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION, GetAce,
        GetLengthSid, GetSecurityDescriptorDacl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_MAX_SID_SIZE,
        TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser, WELL_KNOWN_SID_TYPE,
        WinBuiltinAdministratorsSid, WinCreatorOwnerRightsSid, WinCreatorOwnerSid,
        WinLocalSystemSid,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx, READ_CONTROL,
        ReOpenFile, WRITE_DAC, WRITE_OWNER,
    },
    System::{
        SystemServices::{
            ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, ACCESS_DENIED_CALLBACK_ACE_TYPE,
            ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE, ACCESS_DENIED_OBJECT_ACE_TYPE, MAXIMUM_ALLOWED,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

/// Stable identity of an open file: volume serial number and 128-bit file ID.
pub(crate) type FileIdentity = (u64, [u8; 16]);

/// Rights that let a principal replace, plant, rename, delete, or re-ACL
/// objects. Holding any of them on a private boundary defeats its purpose.
/// For directories `FILE_WRITE_DATA`/`FILE_APPEND_DATA` are add-file and
/// add-subdirectory.
const WRITE_LIKE_RIGHTS: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL
    | MAXIMUM_ALLOWED;

const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// Open the final path component itself without following a symlink,
/// junction, or other reparse point, and require the expected object kind.
pub(crate) fn open_no_follow(path: &Path, directory: bool, access: u32) -> io::Result<fs::File> {
    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if directory {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    let file = fs::OpenOptions::new()
        .access_mode(access | FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(flags)
        .open(path)?;
    let attributes = file.metadata()?.file_attributes();
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private path must not be a symlink, junction, or other reparse point",
        ));
    }
    if (attributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory {
        return Err(io::Error::new(
            if directory {
                io::ErrorKind::NotADirectory
            } else {
                io::ErrorKind::InvalidInput
            },
            if directory {
                "private path is not a directory"
            } else {
                "private path is not a regular file"
            },
        ));
    }
    Ok(file)
}

/// Verify an opened directory is a private boundary: owned by this user or a
/// trusted principal, and no other account may write, delete, or re-ACL it or
/// (through inheritable entries) the objects later created inside it.
/// BUILTIN\Administrators and LocalSystem are trusted, as root is on Unix;
/// user-profile folders such as `%LOCALAPPDATA%` are often owned by them.
pub(crate) fn verify_private_handle(file: &fs::File, label: &str) -> io::Result<()> {
    let trusted = TrustedSids::current()?;
    let descriptor = SecurityDescriptor::of(file)?;
    trusted.require_owner(descriptor.owner, label, true)?;
    let dacl = descriptor.dacl;
    if dacl.is_null() {
        return Err(denied(format!(
            "{label} has a NULL DACL that grants everyone full access"
        )));
    }
    let ace_count = unsafe { (*dacl).AceCount };
    for index in 0..u32::from(ace_count) {
        let mut ace_pointer = ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut ace_pointer) } == 0 || ace_pointer.is_null() {
            return Err(io::Error::last_os_error());
        }
        let header = unsafe { &*(ace_pointer as *const ACE_HEADER) };
        let ace_type = u32::from(header.AceType);
        if ace_type == ACCESS_ALLOWED_ACE_TYPE {
            let ace = unsafe { &*(ace_pointer as *const ACCESS_ALLOWED_ACE) };
            let sid = (&ace.SidStart as *const u32).cast_mut().cast();
            if ace.Mask & WRITE_LIKE_RIGHTS != 0 && !trusted.trusts(sid) {
                return Err(denied(format!(
                    "{label} grants write or delete access to another account; \
                     move it to a folder only you can modify"
                )));
            }
        } else if !matches!(
            ace_type,
            ACCESS_DENIED_ACE_TYPE
                | ACCESS_DENIED_OBJECT_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_ACE_TYPE
                | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE
        ) {
            // Deny entries only narrow access. Any other allow-style entry
            // (object, callback, compound) is not evaluated, so fail closed.
            return Err(denied(format!(
                "{label} has an access-control entry type {ace_type} that cannot be verified"
            )));
        }
    }
    Ok(())
}

/// Require that this user (token user or default owner) owns the opened
/// object. Callers then grant access through `Owner Rights`, so a SYSTEM- or
/// Administrators-owned object is refused here: re-ACLing it would lock out a
/// non-elevated user.
pub(crate) fn verify_owner(file: &fs::File, label: &str) -> io::Result<()> {
    let trusted = TrustedSids::current()?;
    let descriptor = SecurityDescriptor::of(file)?;
    trusted.require_owner(descriptor.owner, label, false)
}

/// Replace the DACL of an open object with the protected owner+SYSTEM DACL.
/// The handle must have been opened with `WRITE_DAC`.
pub(crate) fn apply_private_dacl(file: &fs::File, directory: bool) -> io::Result<()> {
    with_private_dacl(directory, |dacl| {
        let error = unsafe {
            SetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if error == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error as i32))
        }
    })
}

/// Make an already-open regular file owner-only without a path lookup: the
/// owner is checked on the handle, which is then reopened with `WRITE_DAC`.
pub(crate) fn restrict_open_file(file: &fs::File) -> io::Result<()> {
    verify_owner(file, "private file")?;
    let reopened = unsafe {
        ReOpenFile(
            file.as_raw_handle() as HANDLE,
            READ_CONTROL | WRITE_DAC,
            SHARE_ALL,
            0,
        )
    };
    if reopened == INVALID_HANDLE_VALUE || reopened.is_null() {
        return Err(io::Error::last_os_error());
    }
    let reopened = fs::File::from(unsafe { OwnedHandle::from_raw_handle(reopened as _) });
    apply_private_dacl(&reopened, false)
}

/// Return the stable identity of an open file.
pub(crate) fn handle_identity(file: &fs::File) -> io::Result<FileIdentity> {
    let handle = file.as_raw_handle() as HANDLE;
    let mut info = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0
    {
        return Ok((info.VolumeSerialNumber, info.FileId.Identifier));
    }
    // FILE_ID_INFO is unavailable on some older or non-NTFS volumes; the
    // 64-bit index is unique per volume there.
    let mut fallback: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut fallback) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut identifier = [0_u8; 16];
    identifier[..4].copy_from_slice(&fallback.nFileIndexLow.to_le_bytes());
    identifier[4..8].copy_from_slice(&fallback.nFileIndexHigh.to_le_bytes());
    Ok((u64::from(fallback.dwVolumeSerialNumber), identifier))
}

/// Return the identity of the regular file at `path`, refusing reparse points.
pub(crate) fn path_identity(path: &Path) -> io::Result<FileIdentity> {
    handle_identity(&open_no_follow(path, false, 0)?)
}

/// Build the protected owner+SYSTEM DACL used for secrets, locks, stages, and
/// the durable ledger, and pass it to `apply`.
pub(crate) fn with_private_dacl<T>(
    directory: bool,
    apply: impl FnOnce(*mut ACL) -> io::Result<T>,
) -> io::Result<T> {
    // The protected DACL prevents inherited Users/Administrators access from
    // silently widening the boundary on a permissive parent directory.
    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    with_sddl_dacl(sddl, apply)
}

fn with_sddl_dacl<T>(sddl: &str, apply: impl FnOnce(*mut ACL) -> io::Result<T>) -> io::Result<T> {
    let sddl_wide = sddl
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = ptr::null_mut();
        if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
            == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(io::Error::last_os_error());
        }
        apply(dacl)
    })();
    unsafe {
        LocalFree(descriptor as HLOCAL);
    }
    result
}

fn denied(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// Owner SID and DACL read from an open handle; frees the descriptor on drop.
struct SecurityDescriptor {
    owner: PSID,
    dacl: *mut ACL,
    descriptor: PSECURITY_DESCRIPTOR,
}

impl SecurityDescriptor {
    fn of(file: &fs::File) -> io::Result<Self> {
        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self {
            owner,
            dacl,
            descriptor,
        })
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.descriptor as HLOCAL);
        }
    }
}

/// SIDs allowed to own or write a private boundary.
struct TrustedSids {
    /// The token user and the token's default owner. Elevated administrators
    /// create objects owned by BUILTIN\Administrators, not by their user SID.
    owners: [Vec<u8>; 2],
    writers: Vec<Vec<u8>>,
}

impl TrustedSids {
    fn current() -> io::Result<Self> {
        let owners = [token_sid(TokenUser)?, token_sid(TokenOwner)?];
        let mut writers = owners.to_vec();
        for kind in [
            WinLocalSystemSid,
            WinBuiltinAdministratorsSid,
            // Owner Rights resolves to the current owner, verified above.
            WinCreatorOwnerRightsSid,
            // CREATOR OWNER entries are templates that resolve to whoever
            // creates a child object; they grant nothing to other accounts.
            WinCreatorOwnerSid,
        ] {
            writers.push(well_known_sid(kind)?);
        }
        Ok(Self { owners, writers })
    }

    fn require_owner(&self, owner: PSID, label: &str, allow_privileged: bool) -> io::Result<()> {
        if owner.is_null() {
            return Err(denied(format!("{label} has no owner")));
        }
        let owner_bytes = sid_bytes(owner);
        // writers[2..4] are LocalSystem and BUILTIN\Administrators.
        let privileged = &self.writers[2..4];
        if self.owners.iter().any(|sid| sid == owner_bytes)
            || (allow_privileged && privileged.iter().any(|sid| sid == owner_bytes))
        {
            return Ok(());
        }
        Err(denied(format!(
            "{label} is owned by another account ({})",
            sid_string(owner)
        )))
    }

    fn trusts(&self, sid: PSID) -> bool {
        let sid = sid_bytes(sid);
        self.writers.iter().any(|trusted| trusted == sid)
    }
}

/// Render a SID as `S-1-...` for operator-facing errors.
fn sid_string(sid: PSID) -> String {
    let mut wide: *mut u16 = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 || wide.is_null() {
        return "unknown SID".into();
    }
    let length = (0..)
        .take_while(|&index| unsafe { *wide.add(index) } != 0)
        .count();
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(wide, length) });
    unsafe {
        LocalFree(wide as HLOCAL);
    }
    text
}

fn sid_bytes<'a>(sid: PSID) -> &'a [u8] {
    // SIDs are self-describing binary structures; GetLengthSid reads their
    // sub-authority count. Equal SIDs have identical canonical bytes.
    unsafe { std::slice::from_raw_parts(sid as *const u8, GetLengthSid(sid) as usize) }
}

fn token_sid(class: i32) -> io::Result<Vec<u8>> {
    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token as _) };
    let mut length = 0_u32;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            class,
            ptr::null_mut(),
            0,
            &mut length,
        )
    };
    // u64 storage keeps the TOKEN_* structure and embedded SID aligned.
    let mut buffer = vec![0_u64; (length as usize).div_ceil(8).max(1)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle() as HANDLE,
            class,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid = if class == TokenUser {
        unsafe { (*(buffer.as_ptr() as *const TOKEN_USER)).User.Sid }
    } else {
        unsafe { (*(buffer.as_ptr() as *const TOKEN_OWNER)).Owner }
    };
    if sid.is_null() {
        return Err(io::Error::other("process token has no SID for this class"));
    }
    Ok(sid_bytes(sid).to_vec())
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0_u8; SECURITY_MAX_SID_SIZE as usize];
    let mut size = buffer.len() as u32;
    if unsafe { CreateWellKnownSid(kind, ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(size as usize);
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;

    /// Install an arbitrary SDDL DACL by path, as a hostile setup step.
    fn set_dacl(path: &Path, sddl: &str) {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let protected = if sddl.starts_with("D:P") {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            0
        };
        with_sddl_dacl(sddl, |dacl| {
            let error = unsafe {
                SetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | protected,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    dacl,
                    ptr::null(),
                )
            };
            assert_eq!(error, ERROR_SUCCESS, "could not set test DACL {sddl}");
            Ok(())
        })
        .unwrap();
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("mss-acl-{name}-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        // Restore an owner-controlled DACL so removal cannot be blocked.
        set_dacl(path, "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)");
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn hardened_runtime_directory_is_accepted() {
        let base = scratch("hardened");
        let private = base.join("private");
        crate::credentials::ensure_private_directory(&private).unwrap();
        crate::credentials::verify_private_directory(&private).unwrap();
        cleanup(&base);
    }

    #[test]
    fn directory_writable_by_users_is_rejected() {
        let directory = scratch("users-write");
        // FILE_ADD_FILE (0x2) alone is enough to plant a replacement file.
        set_dacl(
            &directory,
            "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;;0x2;;;BU)",
        );
        let error = crate::credentials::verify_private_directory(&directory).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
        cleanup(&directory);
    }

    #[test]
    fn inherited_permissive_acl_is_rejected() {
        let parent = scratch("inherited");
        // Inherit-only: grants nothing on the parent itself, but every child
        // created later inherits Users full control.
        set_dacl(
            &parent,
            "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICIIO;FA;;;BU)",
        );
        assert!(crate::credentials::verify_private_directory(&parent).is_err());
        let child = parent.join("child");
        fs::create_dir(&child).unwrap();
        assert!(crate::credentials::verify_private_directory(&child).is_err());
        assert!(crate::credentials::ensure_private_directory(&parent.join("new")).is_err());
        cleanup(&parent);
    }

    #[test]
    fn directory_junction_is_rejected() {
        let base = scratch("junction");
        let target = base.join("target");
        crate::credentials::ensure_private_directory(&target).unwrap();
        let link = base.join("link");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
        let error = crate::credentials::verify_private_directory(&link).unwrap_err();
        assert!(error.to_string().contains("reparse point"), "{error}");
        assert!(crate::credentials::ensure_private_directory(&link.join("sub")).is_err());
        let _ = fs::remove_dir(&link);
        cleanup(&base);
    }

    #[test]
    fn file_identity_changes_when_the_path_is_replaced() {
        let base = scratch("identity");
        let path = base.join("state.db");
        let replacement = base.join("replacement.db");
        fs::write(&path, b"original").unwrap();
        fs::write(&replacement, b"replacement").unwrap();
        let before = path_identity(&path).unwrap();
        assert_eq!(
            before,
            handle_identity(&fs::File::open(&path).unwrap()).unwrap()
        );
        fs::rename(&replacement, &path).unwrap();
        assert_ne!(before, path_identity(&path).unwrap());
        cleanup(&base);
    }
}
