use crate::atomic_artifact::write_private_atomic;
use crate::{
    plan_identity::snapshot_sha256 as plan_snapshot_sha256,
    reports::integrity::{
        canonical_signed_proof_payload, hex_decode, hex_encode, with_proof_digest,
    },
};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use std::path::Path;

#[cfg(unix)]
fn require_private_key_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|error| format!("could not inspect signing key: {error}"))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err("signing key must be owner-only (0600 or stricter)".into());
    }
    Ok(())
}

#[cfg(windows)]
fn require_private_key_permissions(path: &Path) -> Result<(), String> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Authorization::SE_FILE_OBJECT,
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
            EqualSid, GetAce, GetNamedSecurityInfoW, GetSecurityDescriptorControl,
            GetSecurityDescriptorDacl, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
            SE_DACL_PROTECTED, SECURITY_MAX_SID_SIZE, WinLocalSystemSid,
        },
        System::SystemServices::ACCESS_ALLOWED_ACE_TYPE,
    };

    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not inspect signing key: {error}"))?;
    if !metadata.is_file() {
        return Err("signing key path must refer to a regular file".into());
    }
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut owner: PSID = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
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
            "could not inspect signing key DACL (Windows error {status})"
        ));
    }
    let result = (|| {
        if owner.is_null() || dacl.is_null() {
            return Err("signing key must have an explicit owner and DACL".into());
        }
        let mut control = 0_u16;
        let mut revision = 0_u32;
        let valid_control =
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) };
        if valid_control == 0 || control & SE_DACL_PROTECTED == 0 {
            return Err("signing key DACL must be protected from inheritance".into());
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
        if valid_dacl == 0 || dacl_present == 0 || checked_dacl.is_null() {
            return Err("signing key must have a present DACL".into());
        }
        if checked_dacl != dacl {
            return Err("signing key DACL changed while it was being inspected".into());
        }

        let mut local_system_sid = [0_u8; SECURITY_MAX_SID_SIZE as usize];
        let mut local_system_sid_size = local_system_sid.len() as u32;
        let local_system_sid = local_system_sid.as_mut_ptr() as PSID;
        let valid_system_sid = unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                ptr::null_mut(),
                local_system_sid,
                &mut local_system_sid_size,
            )
        };
        if valid_system_sid == 0 {
            return Err("could not construct the LocalSystem SID".into());
        }

        let mut owner_seen = false;
        let mut system_seen = false;
        for index in 0..u32::from((*dacl).AceCount) {
            let mut ace_pointer = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut ace_pointer) } == 0 || ace_pointer.is_null() {
                return Err("could not inspect a signing key DACL entry".into());
            }
            let header = unsafe { &*(ace_pointer as *const ACE_HEADER) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
                return Err("signing key DACL contains an unsupported or inherited ACE".into());
            }
            let ace = unsafe { &*(ace_pointer as *const ACCESS_ALLOWED_ACE) };
            let sid = (&ace.SidStart as *const u32).cast_mut().cast();
            if unsafe { EqualSid(sid, owner) } != 0 {
                owner_seen = true;
            } else if unsafe { EqualSid(sid, local_system_sid) } != 0 {
                system_seen = true;
            } else {
                return Err("signing key DACL grants access to an unapproved identity".into());
            }
        }
        if (*dacl).AceCount != 2 || !owner_seen || !system_seen {
            return Err("signing key DACL must contain only owner and LocalSystem entries".into());
        }
        Ok(())
    })();
    unsafe {
        LocalFree(descriptor as HLOCAL);
    }
    result
}

#[cfg(all(not(unix), not(windows)))]
fn require_private_key_permissions(_: &Path) -> Result<(), String> {
    Ok(())
}

pub(crate) fn sign_file(
    path: &Path,
    signing_key_path: &Path,
    key_id: &str,
) -> Result<String, String> {
    require_private_key_permissions(signing_key_path)?;
    let key_bytes = zeroize::Zeroizing::new(
        std::fs::read(signing_key_path)
            .map_err(|error| format!("could not read signing key: {error}"))?,
    );
    let key_pair = Ed25519KeyPair::from_pkcs8(&key_bytes)
        .map_err(|_| "signing key is not a supported Ed25519 PKCS#8 key".to_owned())?;
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("Invalid migration proof JSON: {error}"))?;
    let mut value = with_proof_digest(value)?;
    let payload = canonical_signed_proof_payload(&value)?;
    let signature = key_pair.sign(payload.as_bytes());
    let object = value
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?;
    object.insert(
        "proof_signature".into(),
        serde_json::json!({
            "algorithm": "Ed25519",
            "key_id": key_id,
            "public_key": hex_encode(key_pair.public_key().as_ref()),
            "signature": hex_encode(signature.as_ref()),
        }),
    );
    let output = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    write_private_atomic(path, &output).map_err(|error| error.to_string())?;
    Ok(format!("Signed migration proof with key {key_id}"))
}

pub(crate) fn verify_file(path: &Path, trusted_public_key: Option<&str>) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("Invalid migration proof JSON: {error}"))?;
    let format = value
        .get("format")
        .and_then(serde_json::Value::as_str)
        .ok_or("Migration proof is missing format.")?
        .to_owned();
    if !matches!(
        format.as_str(),
        "mailswiftsync-project-report" | "mailswiftsync-customer-proof"
    ) {
        return Err("Unsupported migration proof format.".into());
    }
    let signature = value.get("proof_signature").cloned();
    let expected = value
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .remove("proof_digest")
        .and_then(|digest| digest.as_str().map(str::to_owned))
        .ok_or("Migration proof is missing proof_digest.")?;
    let mut digest_value = value.clone();
    digest_value
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .remove("proof_signature");
    let canonical = serde_json::to_string(&digest_value).map_err(|error| error.to_string())?;
    let actual = plan_snapshot_sha256(&canonical);
    if expected != actual {
        return Err(format!(
            "Migration proof digest mismatch: expected {expected}, calculated {actual}."
        ));
    }
    value
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?
        .insert("proof_digest".into(), serde_json::Value::String(expected));
    if let Some(signature_value) = signature {
        let signature_object = signature_value
            .as_object()
            .ok_or("Migration proof signature must be an object.")?;
        if signature_object
            .get("algorithm")
            .and_then(serde_json::Value::as_str)
            != Some("Ed25519")
        {
            return Err("Unsupported migration proof signature algorithm.".into());
        }
        let public_key = hex_decode::<32>(
            signature_object
                .get("public_key")
                .and_then(serde_json::Value::as_str)
                .ok_or("Migration proof signature is missing public_key.")?,
        )?;
        let trust_pinned = trusted_public_key.is_some();
        if let Some(trusted) = trusted_public_key {
            let trusted = hex_decode::<32>(trusted.trim())?;
            if trusted != public_key {
                return Err("Migration proof signer does not match the trusted public key.".into());
            }
        }
        let signature = hex_decode::<64>(
            signature_object
                .get("signature")
                .and_then(serde_json::Value::as_str)
                .ok_or("Migration proof signature is missing signature.")?,
        )?;
        let payload = canonical_signed_proof_payload(&value)?;
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(payload.as_bytes(), &signature)
            .map_err(|_| "Migration proof signature verification failed.".to_owned())?;
        let key_id = signature_object
            .get("key_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unidentified");
        let trust_message = if trust_pinned {
            "trusted public key matched"
        } else {
            "signer identity is not trust-pinned"
        };
        return Ok(format!(
            "Migration proof integrity verified: {actual}; Ed25519 signature valid for key {key_id}; {trust_message}; this validates the {format} artifact and signer, not independent migration completion"
        ));
    }
    Ok(format!(
        "Migration proof integrity verified (unsigned artifact): {actual}; this validates {format} artifact integrity only, not signer identity or independent migration completion"
    ))
}
