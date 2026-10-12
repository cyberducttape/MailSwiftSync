# Provider qualification evidence packs

The builder emits an immutable, directed *candidate evidence pack* for one
provider pair. It requires exactly one unqualified-pass dry pilot, live pilot,
and recovery record; live and recovery records must have high-confidence message
verification, meet policy volume thresholds, and share the same qualification
bundle, dataset, fixture, MailSwiftSync commit, engine, and binary identities.
Its status is `qualification_candidate`, never `qualified`.

Build a candidate after phase evidence has been generated:

```bash
python3 scripts/build-provider-qualification-pack.py \
  "$MAILSWIFTSYNC_EVIDENCE_DIR" gmail microsoft365 \
  google-workspace-to-microsoft365.pack.json
```

The pack contains phase run IDs, proof and dataset digests, scenario coverage,
aggregate counters, tested binary identities, and explicit limitations. It does
not contain endpoints, mailbox names, credentials, message content, or free-text
known-issue details. Output is private and non-overwriting. The `evidence_digest`
is SHA-256 over canonical JSON of the complete pack except the digest field
itself, so edits to the top-level summary or phase records are detectable. It
is not a signature or proof that the evidence was independently authenticated.

The output shape is defined by
`tests/provider-evidence/pack-schema.json`; downstream tooling should validate
the candidate against that schema before further handling. The current
breaking candidate/status and provenance contract is schema version 2; version
1 packs are not interchangeable with it.

The builder refuses missing or duplicate phases, mixed identities or datasets,
exceptions, aggregate-only verification, insufficient message-level coverage,
and missing required scenarios. Pack generation does not replace
`scripts/verify-evidence-gate.sh --release`; that gate remains authoritative
over the retained evidence bundle, signatures, policy observations, and release
commit. A candidate pack must not be presented as provider qualification until
the release gate passes and the final pack is signed by the trusted qualification
signer.

No candidate is bundled for a provider pair until real automated qualification
jobs produce the required evidence. A generic fixture or successful small
smoke test must not be converted into a hosted-provider qualification claim.
