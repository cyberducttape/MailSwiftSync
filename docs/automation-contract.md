# Automation contract

The CLI is suitable for automation when callers treat its output as a
versioned contract rather than scraping human-readable text.

## Status JSON

Use:

```bash
mailswiftsync status /path/to/state.db project-123 --summary
```

Three commands emit the versioned automation contract:

| Command | `format` | `format_version` | Golden fixture |
|---|---|---|---|
| `mailswiftsync status <state.db> [project]` | `mailswiftsync-status` | `1` | [`status.v1.json`](../tests/fixtures/automation/status.v1.json) |
| `mailswiftsync status <state.db> [project] --summary` | `mailswiftsync-status-summary` | `1` | [`status-summary.v1.json`](../tests/fixtures/automation/status-summary.v1.json) |
| `mailswiftsync fleet-status <directory>` | `mailswiftsync-fleet-status` | `1` | [`fleet-status.v1.json`](../tests/fixtures/automation/fleet-status.v1.json) |

The JSON contains project identity, durable lifecycle state, mailbox state
counts, attention reasons, and bounded project summaries. Each fixture lists
every field path with its allowed JSON types; `[]` marks array elements and
`{}` marks the values of a map with data-dependent keys (for example
`attention_reason_counts`). The output is credential-free but still
customer-sensitive.

`schema_version` is the SQLite ledger schema version. It changes whenever the
storage schema migrates and is **not** a compatibility signal: dispatch on
`format` and `format_version` instead.

### Compatibility policy

- Callers must check `format` and reject an unknown `format_version`.
- Within one `format_version`, changes are additive only: new fields may
  appear, and callers must ignore fields they do not recognize.
- Removing, renaming, or changing the type of a field, or changing the meaning
  of an existing value, requires a new `format_version` and a new fixture
  file. The previous fixture stays in the repository as the record of the
  older contract.
- Enumerated values such as mailbox `state`, project `phase`, and
  `attention_reason` may gain new members within a version. Treat an unknown
  member as requiring operator review, never as success.
- The CI test `automation_outputs_match_their_versioned_golden_contracts`
  fails when an output drops or retypes a declared field, or emits a field the
  fixture does not declare, so every contract change is reviewed explicitly.

Reports, proofs, certificates, and support bundles carry their own `format`
markers and are artifact formats, not part of this automation contract.

## Exit and state rules

- A successful process exit means only that the requested command completed.
- A migration is complete only when the durable project state says so.
- Independent verification evidence and its coverage level must be checked
  before claiming mailbox fidelity.
- `attention`, `failed`, `cancelled`, `verification_difference`, and
  `verification_limit_exceeded` require operator handling; automation must not
  silently retry them as success.
- Unknown or future fields must be ignored, but an unknown `format_version`
  must fail closed until the adapter is updated.

## Integration guidance

For Bash/systemd/cron/Ansible, persist the command version, state path, exit
code, JSON output digest, and run ID. Use the signed webhook/outbox contract
for event delivery and ticket automation; do not parse GUI output or engine
transcripts. Pin the release artifact and engine identity in the automation
environment, and retain the status/evidence artifact with the change record.

The compatibility window covers the three status outputs above. GUI output,
engine transcripts, and human-readable CLI text are not contracts and may
change in any release.
