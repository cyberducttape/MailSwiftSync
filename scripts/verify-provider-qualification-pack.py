#!/usr/bin/env python3
"""Validate the schema and canonical digest of a qualification candidate pack."""

from __future__ import annotations

import hashlib
import hmac
import json
import math
import os
import re
import stat
import sys
from pathlib import Path


MAX_PACK_BYTES = 16 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
DEFAULT_SCHEMA = Path(__file__).resolve().parents[1] / "tests" / "provider-evidence" / "pack-schema.json"


def reject_duplicate_keys(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def canonical_json(value: object) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def reject_nonfinite_constant(value: str):
    raise ValueError(f"non-finite JSON number is not permitted: {value}")


def parse_finite_float(value: str) -> float:
    parsed = float(value)
    significand = value.lower().split("e", 1)[0]
    if not math.isfinite(parsed) or (
        parsed == 0.0 and any(digit in "123456789" for digit in significand)
    ):
        raise ValueError(f"JSON number is outside the supported finite range: {value}")
    return parsed


def parse_pack(path: Path) -> dict:
    if path.is_symlink():
        raise ValueError("pack must not be a symlink")
    try:
        descriptor = os.open(
            path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        )
        try:
            metadata = os.fstat(descriptor)
            if not stat.S_ISREG(metadata.st_mode):
                raise ValueError("pack must be a regular file")
            if metadata.st_size > MAX_PACK_BYTES:
                raise ValueError(f"pack exceeds the {MAX_PACK_BYTES}-byte validation limit")
            with os.fdopen(descriptor, "rb") as handle:
                descriptor = -1
                raw = handle.read(MAX_PACK_BYTES + 1)
            if len(raw) > MAX_PACK_BYTES:
                raise ValueError(f"pack exceeds the {MAX_PACK_BYTES}-byte validation limit")
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=reject_nonfinite_constant,
            parse_float=parse_finite_float,
        )
    except (OSError, UnicodeError, ValueError) as error:
        raise ValueError(f"could not read pack JSON: {error}") from error
    if not isinstance(value, dict):
        raise ValueError("pack root must be an object")
    return value


def verify_pack(path: Path, schema_path: Path = DEFAULT_SCHEMA) -> dict:
    value = parse_pack(path)
    digest = value.get("evidence_digest")
    if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
        raise ValueError("evidence_digest must be a lowercase SHA-256 digest")
    digest_payload = {key: item for key, item in value.items() if key != "evidence_digest"}
    expected = hashlib.sha256(canonical_json(digest_payload)).hexdigest()
    if not hmac.compare_digest(digest, expected):
        raise ValueError("evidence_digest does not match the canonical pack contents")

    try:
        import jsonschema
    except ImportError as error:
        raise ValueError(
            "jsonschema is required; install scripts/requirements-provider-qualification.txt with pip --require-hashes"
        ) from error
    try:
        schema = json.loads(
            schema_path.read_text(encoding="utf-8"),
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=reject_nonfinite_constant,
            parse_float=parse_finite_float,
        )
        jsonschema.Draft202012Validator.check_schema(schema)
        jsonschema.Draft202012Validator(schema).validate(value)
    except (
        OSError,
        UnicodeError,
        ValueError,
        jsonschema.SchemaError,
        jsonschema.ValidationError,
    ) as error:
        raise ValueError(f"pack schema validation failed: {error}") from error
    return value


def main() -> int:
    if len(sys.argv) not in (2, 4) or (len(sys.argv) == 4 and sys.argv[2] != "--schema"):
        print(
            "Usage: verify-provider-qualification-pack.py <pack.json> [--schema <schema.json>]",
            file=sys.stderr,
        )
        return 2
    path = Path(sys.argv[1])
    schema_path = Path(sys.argv[3]) if len(sys.argv) == 4 else DEFAULT_SCHEMA
    try:
        pack = verify_pack(path, schema_path)
    except (OSError, ValueError) as error:
        print(f"Qualification pack rejected: {error}", file=sys.stderr)
        return 1
    print(
        f"Valid schema-v{pack['schema_version']} qualification candidate for "
        f"{pack['provider_pair']} (not release-qualified)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
