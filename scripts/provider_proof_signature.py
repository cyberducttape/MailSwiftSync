"""Verification of trusted Ed25519 signatures on provider qualification proofs."""

import json

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey


def verify_proof_signature(proof: dict, trusted_public_key_hex: str) -> None:
    """Raise ValueError unless proof is signed by the pinned Ed25519 key."""
    signature = proof.get("proof_signature")
    if not isinstance(signature, dict) or signature.get("algorithm") != "Ed25519":
        raise ValueError("proof has no Ed25519 signature")
    public_key_hex = signature.get("public_key")
    signature_hex = signature.get("signature")
    if not isinstance(public_key_hex, str) or public_key_hex.lower() != trusted_public_key_hex.lower():
        raise ValueError("signer does not match the pinned qualification key")
    if not isinstance(signature_hex, str):
        raise ValueError("signature is missing")

    payload = dict(proof)
    payload_signature = dict(signature)
    payload_signature.pop("signature", None)
    payload["proof_signature"] = payload_signature
    canonical = json.dumps(payload, separators=(",", ":"), ensure_ascii=False, sort_keys=True).encode("utf-8")
    try:
        Ed25519PublicKey.from_public_bytes(bytes.fromhex(public_key_hex)).verify(
            bytes.fromhex(signature_hex), canonical
        )
    except (InvalidSignature, TypeError, ValueError) as error:
        raise ValueError("Ed25519 proof signature is invalid") from error
