# Automation contract

The CLI is suitable for automation when callers treat its output as a
versioned contract rather than scraping human-readable text.

## Status JSON

Use:

```bash
mailswiftsync status /path/to/state.db project-123 --summary
```

The JSON contains `schema_version`, project identity, durable lifecycle state,
mailbox state counts, attention reasons, and bounded project summaries. Callers
must reject an unknown `schema_version`, treat missing fields as unavailable,
and use durable state—not process exit text—to decide whether work is complete.
The output is credential-free but still customer-sensitive.

## Exit and state rules

- A successful process exit means only that the requested command completed.
- A migration is complete only when the durable project state says so.
- Independent verification evidence and its coverage level must be checked
  before claiming mailbox fidelity.
- `attention`, `failed`, `cancelled`, `verification_difference`, and
  `verification_limit_exceeded` require operator handling; automation must not
  silently retry them as success.
- Unknown or future fields must be ignored, but unknown schema versions must
  fail closed until the adapter is updated.

## Integration guidance

For Bash/systemd/cron/Ansible, persist the command version, state path, exit
code, JSON output digest, and run ID. Use the signed webhook/outbox contract
for event delivery and ticket automation; do not parse GUI output or engine
transcripts. Pin the release artifact and engine identity in the automation
environment, and retain the status/evidence artifact with the change record.

This contract does not yet guarantee a stable major-version compatibility
window. A production API commitment requires a published schema compatibility
policy, golden JSON fixtures, and CI checks that reject accidental breaking
changes.
