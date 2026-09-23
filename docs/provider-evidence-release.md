# Release evidence hand-off

Provider evidence is not part of the source commit that it certifies. The
release commit is immutable; adding an evidence JSON containing that commit's
SHA would necessarily create a different commit.

The release gate therefore has two inputs:

- `tests/provider-evidence/policy.json` and `schema.json`, checked into the
  release source as the policy contract.
- An evidence bundle generated or downloaded after checkout, supplied through
  `MAILSWIFTSYNC_EVIDENCE_DIR`.

The generated evidence records must still contain the exact release SHA in
`mailswiftsync_commit`. The release workflow supplies that value through
`MAILSWIFTSYNC_RELEASE_COMMIT`, and `scripts/verify-evidence-gate.sh --release`
fails closed if any record differs.

Example hand-off for a release candidate:

```bash
export MAILSWIFTSYNC_RELEASE_COMMIT="$(git rev-parse HEAD)"
export MAILSWIFTSYNC_EVIDENCE_DIR="$RUNNER_TEMP/mailswiftsync-release-evidence"
export MAILSWIFTSYNC_POLICY_FILE="$PWD/tests/provider-evidence/policy.json"
export MAILSWIFTSYNC_SCHEMA_FILE="$PWD/tests/provider-evidence/schema.json"

# Populate the external directory with generated evidence and its referenced
# customer proofs, using --mailswiftsync-commit "$MAILSWIFTSYNC_RELEASE_COMMIT".
scripts/verify-evidence-gate.sh --release
```

This keeps the release gate strict while removing the impossible requirement
that a Git commit contain a self-hash. An evidence bundle remains auditable as
a separate release input and must be retained with the release artifacts.
