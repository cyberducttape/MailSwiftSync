import json
import sys
import unittest
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat


ROOT = Path(__file__).parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from provider_proof_signature import verify_proof_signature  # noqa: E402


class ProviderProofSignatureTests(unittest.TestCase):
    def setUp(self):
        self.private_key = Ed25519PrivateKey.generate()
        self.public_key_hex = self.private_key.public_key().public_bytes(
            Encoding.Raw, PublicFormat.Raw
        ).hex()
        self.proof = {
            "format": "mailswiftsync-customer-proof",
            "proof_digest": "a" * 64,
            "proof_signature": {
                "algorithm": "Ed25519",
                "key_id": "qualification-test",
                "public_key": self.public_key_hex,
                "signature": "",
            },
        }
        payload_object = json.loads(json.dumps(self.proof))
        payload_object["proof_signature"].pop("signature")
        payload = json.dumps(
            payload_object, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        )
        self.proof["proof_signature"]["signature"] = self.private_key.sign(
            payload.encode("utf-8")
        ).hex()

    def test_accepts_signature_from_pinned_key(self):
        verify_proof_signature(self.proof, self.public_key_hex)

    def test_rejects_signature_from_unpinned_key(self):
        with self.assertRaisesRegex(ValueError, "pinned qualification key"):
            verify_proof_signature(self.proof, "00" * 32)

    def test_rejects_tampered_payload(self):
        tampered = dict(self.proof)
        tampered["completion_claim"] = {"status": "durably_complete"}
        with self.assertRaisesRegex(ValueError, "signature is invalid"):
            verify_proof_signature(tampered, self.public_key_hex)


if __name__ == "__main__":
    unittest.main()
