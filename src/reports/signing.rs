use crate::atomic_artifact::write_private_atomic_with;
use crate::reports::{
    canonical::{HashWriter, ProofDocument, Sink},
    integrity::{MAX_PROOF_BYTES, hex_decode, hex_encode},
};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use std::{io::Read, path::Path};

const MAX_SIGNING_KEY_BYTES: u64 = 64 * 1024;

/// Payloads up to this size keep the original pure-Ed25519 signature over
/// the canonical bytes, which external verifiers already accept. Larger
/// payloads are signed as `Ed25519-SHA256`: Ed25519 over the SHA-256 digest
/// of the same canonical bytes, so neither side needs the payload in memory.
const MAX_PURE_ED25519_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;
const PURE_ED25519: &str = "Ed25519";
const PREHASHED_ED25519: &str = "Ed25519-SHA256";

#[cfg(test)]
thread_local! {
    static PURE_SIGNATURE_LIMIT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Largest payload signed with pure Ed25519; tests lower it to exercise the
/// prehashed form without building a 32 MiB proof.
fn pure_signature_limit() -> u64 {
    #[cfg(test)]
    if let Some(limit) = PURE_SIGNATURE_LIMIT.with(std::cell::Cell::get) {
        return limit;
    }
    MAX_PURE_ED25519_PAYLOAD_BYTES
}

fn read_proof(path: &Path) -> Result<ProofDocument, String> {
    ProofDocument::read(path, MAX_PROOF_BYTES)
}

fn string_field(document: &ProofDocument, key: &str) -> Result<Option<String>, String> {
    Ok(document
        .value(key)?
        .and_then(|value| value.as_str().map(str::to_owned)))
}

/// The canonical bytes a signature covers: the document with the signature
/// object minus its `signature` value.
fn signed_payload_replacement(signature: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut unsigned = signature.clone();
    unsigned
        .as_object_mut()
        .ok_or("Migration proof signature must be an object.")?
        .remove("signature");
    serde_json::to_vec(&unsigned).map_err(|error| error.to_string())
}

/// The message handed to Ed25519 for a signature algorithm.
fn signature_message(
    document: &ProofDocument,
    algorithm: &str,
    payload_replacement: &[u8],
) -> Result<Vec<u8>, String> {
    let replace = Some(("proof_signature", payload_replacement));
    match algorithm {
        PURE_ED25519 => {
            let length = document
                .write_canonical(&mut Sink, &[], replace)
                .map_err(|error| error.to_string())?;
            if length > MAX_PURE_ED25519_PAYLOAD_BYTES {
                return Err(format!(
                    "Migration proof payload exceeds the {MAX_PURE_ED25519_PAYLOAD_BYTES}-byte limit for an {PURE_ED25519} signature."
                ));
            }
            let mut payload = Vec::with_capacity(length as usize);
            document
                .write_canonical(&mut payload, &[], replace)
                .map_err(|error| error.to_string())?;
            Ok(payload)
        }
        PREHASHED_ED25519 => {
            let mut hasher = HashWriter::default();
            document
                .write_canonical(&mut hasher, &[], replace)
                .map_err(|error| error.to_string())?;
            Ok(sha2::Digest::finalize(hasher.0).to_vec())
        }
        _ => Err("Unsupported migration proof signature algorithm.".into()),
    }
}

#[cfg(unix)]
fn read_private_signing_key(path: &Path) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("could not open signing key: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect signing key: {error}"))?;
    if !metadata.is_file() {
        return Err("signing key must be a regular file".into());
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("signing key must be owned by the effective user".into());
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err("signing key must have mode 0600".into());
    }
    if metadata.nlink() != 1 {
        return Err("signing key must not be hard-linked".into());
    }
    if metadata.len() > MAX_SIGNING_KEY_BYTES {
        return Err(format!(
            "signing key exceeds the {MAX_SIGNING_KEY_BYTES}-byte limit"
        ));
    }
    let mut key_bytes = zeroize::Zeroizing::new(Vec::with_capacity(
        metadata.len().min(MAX_SIGNING_KEY_BYTES) as usize,
    ));
    file.by_ref()
        .take(MAX_SIGNING_KEY_BYTES + 1)
        .read_to_end(&mut key_bytes)
        .map_err(|error| format!("could not read signing key: {error}"))?;
    if key_bytes.len() as u64 > MAX_SIGNING_KEY_BYTES {
        return Err(format!(
            "signing key exceeds the {MAX_SIGNING_KEY_BYTES}-byte limit"
        ));
    }
    Ok(key_bytes)
}

#[cfg(windows)]
fn require_private_key_permissions(path: &Path) -> Result<(), String> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Authorization::GetNamedSecurityInfoW,
        Security::Authorization::SE_FILE_OBJECT,
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CreateWellKnownSid, DACL_SECURITY_INFORMATION,
            EqualSid, GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
            OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
            SECURITY_MAX_SID_SIZE, WinCreatorOwnerRightsSid, WinLocalSystemSid,
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
    let mut dacl: *mut ACL = ptr::null_mut();
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
        let mut owner_rights_sid = [0_u8; SECURITY_MAX_SID_SIZE as usize];
        let mut owner_rights_sid_size = owner_rights_sid.len() as u32;
        let owner_rights_sid = owner_rights_sid.as_mut_ptr() as PSID;
        let valid_owner_rights_sid = unsafe {
            CreateWellKnownSid(
                WinCreatorOwnerRightsSid,
                ptr::null_mut(),
                owner_rights_sid,
                &mut owner_rights_sid_size,
            )
        };
        if valid_owner_rights_sid == 0 {
            return Err("could not construct the Owner Rights SID".into());
        }

        let mut owner_seen = false;
        let mut system_seen = false;
        let ace_count = unsafe { (*dacl).AceCount };
        for index in 0..u32::from(ace_count) {
            let mut ace_pointer = ptr::null_mut();
            if unsafe { GetAce(dacl, index, &mut ace_pointer) } == 0 || ace_pointer.is_null() {
                return Err("could not inspect a signing key DACL entry".into());
            }
            let header = unsafe { &*(ace_pointer as *const ACE_HEADER) };
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
                return Err("signing key DACL contains an unsupported or inherited ACE".into());
            }
            let ace = unsafe { &*(ace_pointer as *const ACCESS_ALLOWED_ACE) };
            let sid = (&ace.SidStart as *const u32).cast_mut().cast();
            if unsafe { EqualSid(sid, owner) } != 0
                || unsafe { EqualSid(sid, owner_rights_sid) } != 0
            {
                owner_seen = true;
            } else if unsafe { EqualSid(sid, local_system_sid) } != 0 {
                system_seen = true;
            } else {
                return Err("signing key DACL grants access to an unapproved identity".into());
            }
        }
        if ace_count != 2 || !owner_seen || !system_seen {
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
    sign_file_to(path, path, signing_key_path, key_id, false)
}

/// Sign a proof into a separate destination. The certificate command uses
/// this to keep an unsigned intermediate out of the final output path.
pub(crate) fn sign_file_to(
    path: &Path,
    output_path: &Path,
    signing_key_path: &Path,
    key_id: &str,
    certificate: bool,
) -> Result<String, String> {
    #[cfg(unix)]
    let key_bytes = read_private_signing_key(signing_key_path)?;
    #[cfg(not(unix))]
    let key_bytes = {
        require_private_key_permissions(signing_key_path)?;
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut file = options
            .open(signing_key_path)
            .map_err(|error| format!("could not open signing key: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("could not inspect signing key: {error}"))?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("signing key path must not refer to a symlink or reparse point".into());
        }
        if !metadata.is_file() {
            return Err("signing key path must refer to a regular file".into());
        }
        if metadata.len() > MAX_SIGNING_KEY_BYTES {
            return Err(format!(
                "signing key exceeds the {MAX_SIGNING_KEY_BYTES}-byte limit"
            ));
        }
        let mut key_bytes = zeroize::Zeroizing::new(Vec::with_capacity(
            metadata.len().min(MAX_SIGNING_KEY_BYTES) as usize,
        ));
        std::io::Read::by_ref(&mut file)
            .take(MAX_SIGNING_KEY_BYTES + 1)
            .read_to_end(&mut key_bytes)
            .map_err(|error| format!("could not read signing key: {error}"))?;
        if key_bytes.len() as u64 > MAX_SIGNING_KEY_BYTES {
            return Err(format!(
                "signing key exceeds the {MAX_SIGNING_KEY_BYTES}-byte limit"
            ));
        }
        key_bytes
    };
    let key_pair = Ed25519KeyPair::from_pkcs8(&key_bytes)
        .map_err(|_| "signing key is not a supported Ed25519 PKCS#8 key".to_owned())?;
    let mut document = read_proof(path)?;
    // Re-digest exactly as `with_proof_digest`: a prior signature is metadata,
    // not part of the signed report.
    document.remove("proof_signature");
    document.set("proof_digest", &document.digest()?.into());
    if certificate {
        if string_field(&document, "format")?.as_deref() != Some("mailswiftsync-customer-proof") {
            return Err("Migration certificate requires a customer-proof artifact.".into());
        }
        let completion_status = document
            .value("completion_claim")?
            .and_then(|claim| claim.get("status")?.as_str().map(str::to_owned));
        if completion_status.as_deref() != Some("durably_complete") {
            return Err("Migration certificate requires durable project completion.".into());
        }
        document.set("artifact_role", &"signed_migration_certificate".into());
        document.set(
            "certificate",
            &serde_json::json!({
                "type": "authenticated_durable_migration_evidence",
                "issued_at": chrono::Utc::now().to_rfc3339(),
                "independent_message_level_attestation": false,
                "note": "This certificate authenticates the exported durable ledger evidence and signer. It does not replace per-message reconciliation or provider-independent audit review."
            }),
        );
        document.set("proof_digest", &document.digest()?.into());
    }
    let public_key = hex_encode(key_pair.public_key().as_ref());
    let unsigned_signature = |algorithm: &str| {
        serde_json::json!({
            "algorithm": algorithm,
            "key_id": key_id,
            "public_key": public_key,
            "signature": "",
        })
    };
    // Keep the widely verifiable pure signature whenever the payload fits.
    let pure_payload_length = document
        .write_canonical(
            &mut Sink,
            &[],
            Some((
                "proof_signature",
                &signed_payload_replacement(&unsigned_signature(PURE_ED25519))?,
            )),
        )
        .map_err(|error| error.to_string())?;
    let algorithm = if pure_payload_length <= pure_signature_limit() {
        PURE_ED25519
    } else {
        PREHASHED_ED25519
    };
    let mut signature_value = unsigned_signature(algorithm);
    let message = signature_message(
        &document,
        algorithm,
        &signed_payload_replacement(&signature_value)?,
    )?;
    let signature = key_pair.sign(&message);
    drop(message);
    signature_value["signature"] = serde_json::Value::String(hex_encode(signature.as_ref()));
    document.set("proof_signature", &signature_value);
    write_private_atomic_with(output_path, |file| {
        document.write_canonical(file, &[], None).map(drop)
    })
    .map_err(|error| error.to_string())?;
    if certificate {
        Ok(format!(
            "Created signed migration certificate with key {key_id}"
        ))
    } else {
        Ok(format!("Signed migration proof with key {key_id}"))
    }
}

pub(crate) fn verify_file(path: &Path, trusted_public_key: Option<&str>) -> Result<String, String> {
    verify_document(&read_proof(path)?, trusted_public_key)
}

fn verify_document(
    document: &ProofDocument,
    trusted_public_key: Option<&str>,
) -> Result<String, String> {
    let format = string_field(document, "format")?.ok_or("Migration proof is missing format.")?;
    if !matches!(
        format.as_str(),
        "mailswiftsync-project-report"
            | "mailswiftsync-customer-proof"
            | "mailswiftsync-migration-assurance"
    ) {
        return Err("Unsupported migration proof format.".into());
    }
    let expected = string_field(document, "proof_digest")?
        .ok_or("Migration proof is missing proof_digest.")?;
    let actual = document.digest()?;
    if expected != actual {
        return Err(format!(
            "Migration proof digest mismatch: expected {expected}, calculated {actual}."
        ));
    }
    if let Some(signature_value) = document.value("proof_signature")? {
        let signature_object = signature_value
            .as_object()
            .ok_or("Migration proof signature must be an object.")?;
        let algorithm = signature_object
            .get("algorithm")
            .and_then(serde_json::Value::as_str)
            .filter(|algorithm| matches!(*algorithm, PURE_ED25519 | PREHASHED_ED25519))
            .ok_or("Unsupported migration proof signature algorithm.")?;
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
        let message = signature_message(
            document,
            algorithm,
            &signed_payload_replacement(&signature_value)?,
        )?;
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(&message, &signature)
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
            "Migration proof integrity verified: {actual}; {algorithm} signature valid for key {key_id}; {trust_message}; this validates the {format} artifact and signer, not independent migration completion"
        ));
    }
    // A pinned signer asks "was this signed by that key?". An unsigned
    // artifact must fail that question: otherwise stripping the signature
    // and recomputing the plain digest would pass verification.
    if trusted_public_key.is_some() {
        return Err(
            "Migration proof is unsigned, but a trusted public key was required; refusing to treat a checksum as signer verification."
                .into(),
        );
    }
    Ok(format!(
        "Internal checksum verified: {actual}; artifact authenticity is not established; a modified report can be re-digested. This validates {format} corruption resistance only, not signer identity or independent migration completion"
    ))
}

/// Verify both the cryptographic proof and the certificate role. A valid
/// customer proof is not automatically a migration certificate.
pub(crate) fn verify_certificate_file(
    path: &Path,
    trusted_public_key: Option<&str>,
) -> Result<String, String> {
    let document = read_proof(path)?;
    if string_field(&document, "format")?.as_deref() != Some("mailswiftsync-customer-proof") {
        return Err("Migration certificate must use the customer-proof format.".into());
    }
    if string_field(&document, "artifact_role")?.as_deref() != Some("signed_migration_certificate")
    {
        return Err("Artifact is not a signed migration certificate.".into());
    }
    if document
        .value("certificate")?
        .and_then(|certificate| certificate.get("type")?.as_str().map(str::to_owned))
        .as_deref()
        != Some("authenticated_durable_migration_evidence")
    {
        return Err("Migration certificate metadata is missing or unsupported.".into());
    }
    if document
        .value("completion_claim")?
        .and_then(|claim| claim.get("status")?.as_str().map(str::to_owned))
        .as_deref()
        != Some("durably_complete")
    {
        return Err("Migration certificate is not marked durably complete.".into());
    }
    if !document.contains("proof_signature") {
        return Err("Migration certificate is unsigned; an Ed25519 signature is required.".into());
    }
    verify_document(&document, trusted_public_key)
}

#[cfg(all(test, unix))]
mod tests {
    use super::read_private_signing_key;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn signing_key_reader_rejects_symlinks_before_reading() {
        let suffix = uuid::Uuid::new_v4();
        let target = std::env::temp_dir().join(format!("mailswiftsync-signing-target-{suffix}"));
        let link = std::env::temp_dir().join(format!("mailswiftsync-signing-link-{suffix}"));
        fs::write(&target, b"not-a-key").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &link).unwrap();

        let error = read_private_signing_key(&link).unwrap_err();
        assert!(error.contains("open signing key"));

        fs::remove_file(link).unwrap();
        fs::remove_file(target).unwrap();
    }

    #[test]
    fn signing_key_reader_rejects_hard_links() {
        let suffix = uuid::Uuid::new_v4();
        let target = std::env::temp_dir().join(format!("mailswiftsync-signing-target-{suffix}"));
        let link = std::env::temp_dir().join(format!("mailswiftsync-signing-hardlink-{suffix}"));
        fs::write(&target, b"not-a-key").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&target, &link).unwrap();

        let error = read_private_signing_key(&target).unwrap_err();
        assert!(error.contains("hard-linked"));

        fs::remove_file(link).unwrap();
        fs::remove_file(target).unwrap();
    }
}

#[cfg(test)]
mod prehashed_signature_tests {
    use super::{PREHASHED_ED25519, PURE_ED25519, PURE_SIGNATURE_LIMIT, sign_file, verify_file};
    use ring::signature::KeyPair;

    fn signing_fixture() -> (std::path::PathBuf, std::path::PathBuf, String) {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let proof_path = directory.join("proof.json");
        let proof = crate::reports::integrity::with_proof_digest(serde_json::json!({
            "format": "mailswiftsync-project-report",
            "project": {"name": "Large migration"},
            "mailboxes": (0..50).map(|index| serde_json::json!({"id": index})).collect::<Vec<_>>(),
        }))
        .unwrap();
        std::fs::write(&proof_path, serde_json::to_string_pretty(&proof).unwrap()).unwrap();
        let key = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .unwrap();
        let key_path = directory.join("signing-key.pk8");
        let mut key_file = crate::credentials::open_secret_file(&key_path).unwrap();
        std::io::Write::write_all(&mut key_file, key.as_ref()).unwrap();
        drop(key_file);
        let public_key = crate::reports::integrity::hex_encode(
            ring::signature::Ed25519KeyPair::from_pkcs8(key.as_ref())
                .unwrap()
                .public_key()
                .as_ref(),
        );
        (proof_path, key_path, public_key)
    }

    #[test]
    fn payloads_above_the_pure_limit_are_signed_over_their_digest() {
        let (proof_path, key_path, public_key) = signing_fixture();
        let directory = proof_path.parent().unwrap().to_owned();
        let _cleanup = crate::credentials::CleanupGuard::new(vec![directory]);

        PURE_SIGNATURE_LIMIT.with(|limit| limit.set(Some(0)));
        let signed = sign_file(&proof_path, &key_path, "large-key");
        PURE_SIGNATURE_LIMIT.with(|limit| limit.set(None));
        if let Err(error) = signed {
            // Hosted Windows runners cannot always satisfy the key ACL check.
            if cfg!(windows) {
                return;
            }
            panic!("{error}");
        }
        let certificate: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&proof_path).unwrap()).unwrap();
        assert_eq!(
            certificate["proof_signature"]["algorithm"],
            PREHASHED_ED25519
        );
        let message = verify_file(&proof_path, Some(&public_key)).unwrap();
        assert!(
            message.contains("Ed25519-SHA256 signature valid"),
            "{message}"
        );

        let mut tampered = certificate.clone();
        tampered["mailboxes"][7]["id"] = 700.into();
        std::fs::write(&proof_path, serde_json::to_string(&tampered).unwrap()).unwrap();
        assert!(verify_file(&proof_path, None).is_err());

        // Re-signing a normal-sized proof returns to the pure algorithm.
        std::fs::write(&proof_path, serde_json::to_string(&certificate).unwrap()).unwrap();
        sign_file(&proof_path, &key_path, "small-key").unwrap();
        let resigned: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&proof_path).unwrap()).unwrap();
        assert_eq!(resigned["proof_signature"]["algorithm"], PURE_ED25519);
        verify_file(&proof_path, Some(&public_key)).unwrap();
    }
}
