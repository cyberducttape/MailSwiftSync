# Automation contract

The CLI is suitable for automation when callers treat its output as a
versioned contract rather than scraping human-readable text.

## Status JSON

Use:

```bash
mailswiftsync status /path/to/state.db project-123 --summary
```

The three status commands emit the versioned status automation contract:

| Command | `format` | `format_version` | Golden fixture |
|---|---|---|---|
| `mailswiftsync status <state.db> [project]` | `mailswiftsync-status` | `1` | [`status.v1.json`](../tests/fixtures/automation/status.v1.json) |
| `mailswiftsync status <state.db> [project] --summary` | `mailswiftsync-status-summary` | `1` | [`status-summary.v1.json`](../tests/fixtures/automation/status-summary.v1.json) |
| `mailswiftsync fleet-status <directory>` | `mailswiftsync-fleet-status` | `1` | [`fleet-status.v1.json`](../tests/fixtures/automation/fleet-status.v1.json) |

The standalone `mailswiftsync reverify` command also emits a versioned JSON
result (`mailswiftsync.reverify`, version `1`). Its `verification_status`,
`verification_level`, `message_coverage`, and `flag_verification` fields must
be consumed together; `verification_outcome` alone is not an unqualified
assurance claim. This result is an evidence artifact rather than a status
projection and therefore is not included in the status golden-fixture set.

`mailswiftsync reverify-batch` emits `mailswiftsync.reverify_batch`, version
`1`, with `total`, `completed`, `failed`, and a `results` array. Each result
contains a mailbox ID, a `completed` or `failed` status, and either the normal
`reverify` artifact or a bounded error string. Callers must treat any nonzero
`failed` count as incomplete evidence; the batch envelope is not a global
point-in-time snapshot.

`mailswiftsync risk <messages> <folders> <bytes>` emits the versioned
`mailswiftsync-pre-migration-risk` format, version `1`. Its readiness score is
explicitly `scale_only` and `advisory_only`; `not_evaluated` lists provider
qualification, authentication, TLS, destination capacity, verification
eligibility, worker disk capacity, maintenance window, and organization
policy. A `ready` result means only that none of the scale-only warnings
triggered. It never clears or substitutes for those unevaluated gates.

The JSON contains project identity, durable lifecycle state, mailbox state
counts, attention reasons, and bounded project summaries. Each fixture lists
every field path with its allowed JSON types; `[]` marks array elements and
`{}` marks the values of a map with data-dependent keys (for example
`attention_reason_counts`). The output is credential-free but still
customer-sensitive.

Each project in `status --summary` (and therefore in `fleet-status`) carries
an `operations` object of durable counters for monitoring and alerting:
`transfer_attempts`, `retried_attempts`, `attempt_outcomes` (by durable
outcome: `completed`, `delta_required`, `failed:<class>` such as
`failed:capacity` for provider throttling or connection limits and
`failed:transport` for network failures, or `unfinished` for running or
interrupted attempts), and `verification_outcomes` (current outcome per
mailbox with evidence). Mailbox state counts and `attention_reason_counts`
give the recovery backlog. Alert on rates between successive polls rather than
on absolute totals.

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
The `mailswiftsync-post-migration-report` artifact uses format version `2`:
`report.total_skipped` is `null` when the ledger has no trustworthy message-
level count, and `report.total_skipped_availability` is `unavailable`. A
numeric value, including zero, is emitted only when a count was explicitly
provided. Consumers of version `1` must update before accepting version `2`.

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
