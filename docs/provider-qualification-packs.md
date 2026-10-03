# Provider qualification packs

A qualification pack is the immutable, directed record for one provider pair.
It is generated from exactly one passing dry pilot, live pilot, and recovery
record that share the same qualification bundle, MailSwiftSync commit, engine,
and binary identities.

Build one after the phase evidence has been generated:

```bash
python3 scripts/build-provider-qualification-pack.py \
  "$MAILSWIFTSYNC_EVIDENCE_DIR" gmail microsoft365 \
  google-workspace-to-microsoft365.pack.json
```

The pack contains phase run IDs, proof and dataset digests, scenario coverage,
aggregate counters, tested binary identities, and explicit limitations. It does
not contain endpoints, mailbox names, credentials, or message content. The
`evidence_digest` binds the phase summaries to the pack.

The output shape is defined by
`tests/provider-evidence/pack-schema.json`; downstream tooling should validate
the pack against that schema before publishing it.

The builder is deliberately stricter than a generic support claim: missing
phases, duplicate phases, mixed binaries, mixed commits, failed results, or
missing required scenarios are refused. Pack generation does not replace
`scripts/verify-evidence-gate.sh --release`; the release gate remains the
authoritative check over the retained evidence bundle and customer proofs.

No pack is bundled for a provider pair until real automated qualification jobs
produce the required evidence. A generic fixture or a successful small smoke
test must not be converted into a hosted-provider qualification pack.
