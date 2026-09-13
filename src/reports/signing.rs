use crate::{
    canonical_signed_proof_payload, hex_decode, hex_encode, plan_snapshot_sha256,
    with_proof_digest, write_private_atomic,
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

#[cfg(not(unix))]
fn require_private_key_permissions(_: &Path) -> Result<(), String> {
    Ok(())
}

pub(crate) fn sign_file(
    path: &Path,
    signing_key_path: &Path,
    key_id: &str,
) -> Result<String, String> {
    require_private_key_permissions(signing_key_path)?;
    let key_bytes = std::fs::read(signing_key_path)
        .map_err(|error| format!("could not read signing key: {error}"))?;
    let key_pair = Ed25519KeyPair::from_pkcs8(&key_bytes)
        .map_err(|_| "signing key is not a supported Ed25519 PKCS#8 key".to_owned())?;
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("Invalid migration proof JSON: {error}"))?;
    let mut value = with_proof_digest(value)?;
    let payload = canonical_signed_proof_payload(&value)?;
    let signature = key_pair.sign(payload.as_bytes());
    value
        .as_object_mut()
        .expect("proof object checked by with_proof_digest")
        .insert(
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
    let object = value
        .as_object_mut()
        .ok_or("Migration proof must be a JSON object.")?;
    if !matches!(
        object.get("format").and_then(serde_json::Value::as_str),
        Some("mailswiftsync-project-report" | "mailswiftsync-customer-proof")
    ) {
        return Err("Unsupported migration proof format.".into());
    }
    let signature = object.get("proof_signature").cloned();
    let expected = object
        .remove("proof_digest")
        .and_then(|digest| digest.as_str().map(str::to_owned))
        .ok_or("Migration proof is missing proof_digest.")?;
    let mut digest_value = value.clone();
    digest_value
        .as_object_mut()
        .expect("proof object checked above")
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
        .expect("proof object checked above")
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
            "Migration proof verified: {actual}; Ed25519 signature valid for key {key_id}; {trust_message}"
        ));
    }
    Ok(format!(
        "Migration proof digest verified (unsigned integrity-only artifact): {actual}"
    ))
}
