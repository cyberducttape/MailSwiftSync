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
result (`mailswiftsync.reverify`, version `4`). It selects the latest completed
live transfer as its baseline by default; `--baseline-run RUN_ID` can select a
specific completed transfer for an intentional historical audit. The result's
`based_on_transfer_run_id` records the exact parent transfer in either case.
An explicitly selected run must belong to the mailbox, be a completed transfer,
and have an eligible immutable live plan snapshot. Its `verification_status`,
`verification_level`, `message_coverage`, `flag_verification`, and
`folder_observations` fields must be consumed together; `verification_outcome`
alone is not an unqualified assurance claim. Folder observations contain
project-scoped folder digests, UID snapshots, UTC scan intervals, and separate
source/destination revalidation timestamps, not raw folder names. This result
is an evidence artifact rather than a status projection and therefore is not
included in the status golden-fixture set.

`mailswiftsync reverify-batch` emits `mailswiftsync.reverify_batch`, version
`2`, with `total`, `completed`, `failed`, and a `results` array. Each result
contains a mailbox ID, a `completed` or `failed` status, and either the normal
`reverify` artifact or a bounded error string. Callers must treat any nonzero
`failed` count as incomplete evidence; the batch envelope is not a global
point-in-time snapshot.

`mailswiftsync wave <state.db> list <project-id>` emits
`mailswiftsync-wave-list`, version `2`. `verified` counts only mailboxes with
clean verification; `accepted_exceptions` counts mailboxes whose verification
differences were explicitly accepted; and `completed` is their sum for wave
workflow progress. A `complete_with_exceptions` wave status is not equivalent
to clean verification. Neither `complete` nor `complete_with_exceptions`
establishes cutover readiness; the cutover workflow and its separate approvals
remain authoritative. Consumers must check the format and version before
using this output.

`mailswiftsync risk <source-messages> <source-folders> <source-bytes> [--state <state.db>]` emits the versioned
`mailswiftsync-pre-migration-risk` format, version `5`. The inventory inputs
describe the source endpoint only. The report exposes that scope and an
equal-size source-plus-destination body-hash byte projection; destination size
and actual fetched RFC822 bytes remain unknown, so this projection is advisory
and does not replace runtime budget enforcement. Its score is
explicitly `scale_only` and `advisory_only`; `not_evaluated` lists provider
qualification, authentication, TLS, destination capacity, verification
eligibility, worker disk capacity, maintenance window, and organization
policy. `scale_assessment: "scale_clear"` means only that none of the
scale-only warnings triggered. It is deliberately not named `ready` and never
clears or substitutes for those unevaluated gates. Version 2 removed the
ambiguous version-1 `estimated_readiness: "ready"` value, and version 3
clarified one-endpoint size scope and added the combined-byte projection.
Version 4 adds low/high `estimated_verification_stage_and_journal_bytes_min`
and `_max` projections using an equal-count two-endpoint assumption and the
observed 518–808 peak stage-directory bytes per staged record across current
synthetic mismatch-heavy through missing-ID profiles. The directory total
includes the sampled rollback-journal peak. This is a workload-specific
measured range, not a capacity bound; extra destination messages or different
mailbox shapes can increase actual storage. `verification_stage_capacity_status`
therefore remains `unknown_not_measured` unless `--state` is supplied. With
`--state`, the command read-only checks currently free bytes on the filesystem
that contains the durable verification-stage directory; it neither opens nor
creates the ledger or stage directory. Version 5 adds
`verification_stage_filesystem_available_bytes` and permits
`verification_stage_capacity_status` values `below_observed_projection_with_reserve`,
`within_observed_projection_not_a_guarantee`, and `projection_unavailable`.
The comparison uses the synthetic high projection plus the runtime free-space
reserve; it does not reserve space, include destination-only messages, or
establish a safe upper bound. Available space can change immediately after the
observation. Consumers must handle the new fields and enum before accepting
this report.

The JSON contains project identity, durable lifecycle state, mailbox state
counts, attention reasons, and bounded project summaries. Each fixture lists
every field path with its allowed JSON types; `[]` marks array elements and
`{}` marks the values of a map with data-dependent keys (for example
`attention_reason_counts`). The output is credential-free but still
customer-sensitive.

Each project in `status --summary` (and therefore in `fleet-status`) carries
a `hold` object (`held_by`, `reason`, `held_at`) while an operator hold stops
new mailbox launches, or `null` otherwise, and an `operations` object of durable counters for monitoring and alerting:
`transfer_attempts`, `retried_attempts`, `attempt_outcomes` (by durable
outcome: `completed`, `delta_required`, `failed:<class>` such as
`failed:capacity` for provider throttling or connection limits and
`failed:transport` for network failures, or `unfinished` for running or
interrupted attempts), and `verification_outcomes` (current outcome per
mailbox with evidence). `unfinished_attempt_messages_copied` and
`unfinished_attempt_bytes_copied` sum the last durable progress checkpoint
for each attempt that lacks a terminal outcome. These snapshots can be stale
and retries can repeat work; they are neither unique mailbox coverage nor a
throughput measure. `verification_method_outcomes` provides the same
current evidence counts as `method:outcome` keys (for example,
`metadata_reconciliation:exact_metadata_match` versus
`native_dovecot:exact_metadata_match`) so aggregate totals are not mistaken for
per-message reconciliation. `verification_message_findings` carries current
missing, extra, modified, and probable-match counts from each mailbox's latest
evidence. Mailbox state counts and `attention_reason_counts` give the recovery
backlog. Alert on rates between successive polls rather than on absolute
totals.

`schema_version` is the SQLite ledger schema version. It changes whenever the
storage schema migrates and is **not** a compatibility signal: dispatch on
`format` and `format_version` instead.

### SRE operational coverage and limits

The versioned status JSON is the supported machine-readable polling surface;
it is not a Prometheus exposition endpoint. Its current fields map to the
five operator questions as follows:

| Operator question | Durable status evidence | Limitation |
|---|---|---|
| Which migrations are active? | `aggregate_mailbox_state_counts.running` and bounded `active_processes` records (`run_id`, `job_id`, PID and executable identity); per-project summaries also include mailbox-state counts | `active_processes_truncated` must be checked. A durable `running` row and a process record are distinct facts; validate ownership before acting on a PID |
| Which are making useful progress? | Durable transfer-attempt/checkpoint history; the metrics snapshot also includes unfinished-attempt copied-message and byte gauges | Gauges are last committed checkpoints, can be stale, and may count retry work more than once. They do not prove recent progress or a transfer rate; compare successive snapshots |
| Which are blocked or retrying? | `operations.retried_attempts`, `attempt_outcomes` (including `failed:capacity`), mailbox states, and `attention_reason_counts` | `retried_attempts` is cumulative, not a current retry gauge. The status contract does not expose a complete retry timer queue or live provider cooldown state |
| Which have unverified or inconsistent results? | `verification_outcomes`, `verification_method_outcomes`, mailbox states, and durable evidence/reverify outputs | Consumers must retain proof method and outcome separately; transfer completion is not verification |
| Which require human intervention? | `aggregate_mailbox_state_counts.needs_review` and `attention_reason_counts`; detailed status provides bounded mailbox rows | Detail is bounded/truncated. Use the durable ledger and evidence workflow for case-level investigation |

`mailswiftsync metrics <state.db>` emits a one-shot Prometheus text snapshot
from one local ledger. It is a bounded projection, not a distributed exporter;
`serve` exposes the same families summed across every ledger under a
directory (see below). Each status and metrics projection reads its ledger
inside one SQLite read transaction, so separate gauges in the same response
reflect one committed ledger snapshot. A fleet response snapshots each ledger
independently, not all ledgers atomically. The command refuses to emit output
if its project or process projection is truncated. It emits no project,
mailbox, run, customer, or endpoint labels. The metric contract is:

| Metric | Type | Meaning and limits |
|---|---|---|
| `mailswiftsync_mailbox_jobs{state}` | Gauge | Current durable job counts for `total`, `queued`, `ready`, `running`, `verified`, and `needs_review`. `running` includes claimed work; `verified` includes explicitly accepted exceptions. |
| `mailswiftsync_projects_on_hold` | Gauge | Projects whose operator hold currently stops new mailbox launches. |
| `mailswiftsync_registered_engine_processes` | Gauge | Durable process registrations, not a process-liveness check. Validate process ownership before acting on a PID. |
| `mailswiftsync_transfer_attempts_total` | Counter | Cumulative transfer attempts recorded in the projected ledger. |
| `mailswiftsync_transfer_retries_total` | Counter | Cumulative attempts after the first attempt of a pass. |
| `mailswiftsync_unfinished_attempt_messages_copied` | Gauge | Latest durable copied-message snapshots summed across attempts without terminal outcomes. Retries can repeat work, so this is not unique mailbox coverage. |
| `mailswiftsync_unfinished_attempt_bytes_copied` | Gauge | Byte counterpart, also a persisted snapshot rather than a transfer rate or lifetime total. |
| `mailswiftsync_transfer_attempt_outcomes{outcome}` | Gauge | Current durable counts by terminal or unfinished outcome. An attempt can move from `unfinished` to a terminal label, so this is not monotonic and must not be modeled as a counter. Unknown values collapse to `other`; `failed:capacity` is a capacity category, not a provider-throttle event counter. |
| `mailswiftsync_verification_mailboxes{method,outcome}` | Gauge | Current persisted mailbox evidence counts, not message counts. Labels use fixed allowlists; unknown values collapse to `other`. |
| `mailswiftsync_verification_message_findings{kind}` | Gauge | Current latest-evidence counts by `missing`, `extra`, `modified`, or `probable`; probable matches are not confirmed differences. |

Progress gauges reflect the last committed checkpoint for attempts still
lacking a terminal outcome; they can be stale after a crash and do not prove
recent activity. Compare successive snapshots to determine whether these
gauges are moving. These metrics do not include process-local transfer rates,
adaptive cooldown state, reader-drain timeout totals, SQLite transaction
latency, or host CPU/RSS/disk. Do not infer process liveness from ledger
counters. `fleet-status` aggregates local ledgers; it is not distributed worker
discovery or a central control plane. The durable webhook outbox can deliver
lifecycle/status events, but does not replace polling for current gauges or
provide centralized operator identity.

For local monitoring, either invoke the command through a restricted
Prometheus textfile-collector/task integration, or scrape `serve`.

`mailswiftsync serve <directory> --token-file <file> [--operators-file <file>]`
is a long-running listener over the same projections:

| Endpoint | Authentication | Body |
|---|---|---|
| `GET /healthz` | none | `ok`; says only that the process is accepting connections |
| `GET /v1/fleet` | `Authorization: Bearer <token>` | the `fleet-status` v1 document, byte-for-byte the CLI shape |
| `GET /metrics` | `Authorization: Bearer <token>` | the metric families above summed across readable ledgers, plus `mailswiftsync_fleet_ledgers{state="readable"\|"unreadable"}` and `mailswiftsync_fleet_snapshot_timestamp_seconds` |
| `GET /v1/projects/<project-id>/waves` | read or operator token | the `mailswiftsync-wave-list` v2 document that `wave <state> list` prints |
| `POST /v1/waves/<wave-id>/approve` | operator token | `{"format":"mailswiftsync-wave-approval","format_version":1,"wave_id","name","approved_by"}` |
| `POST /v1/projects/<project-id>/hold?reason=<text>` | operator token | `{"format":"mailswiftsync-project-hold","format_version":1,"project_id","state":"held","hold":{"held_by","reason","held_at"}}` |
| `POST /v1/projects/<project-id>/resume` | operator token | the same document with `"state":"resumed"` and the hold that was lifted |

Responses come from one scan per refresh interval (`--refresh-seconds`,
5–3600, default 30), not one scan per request; every response carries
`X-MailSwiftSync-Snapshot-Generated-At` with the scan's Unix time. A scan that
fails, or a `/metrics` projection that any ledger truncates, is answered with
503 instead of stale or partial data. Errors from the wave and hold
endpoints are `{"error": "..."}` with 400 (malformed ID, or a hold without a
reason), 403 (read token used for a control), 404 (no ledger holds the ID),
409 (already approved, already held or not held, ledger busy with
`Retry-After`, older ledger schema, or the ID found in more than one
ledger), or 413 (a request body was sent).

For JSON automation, continue polling `status --summary` (or
`fleet-status` for explicitly isolated local ledgers), check `format` and
`format_version`, and calculate rates from successive snapshots only for
monotonically cumulative counters.

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
The `mailswiftsync-customer-proof` artifact uses format version `6`. Per-mailbox
`assurance_status` distinguishes `aggregate_match`, `metadata_match`,
`bounded_content_match`, `engine_confirmed`, `with_exceptions`, and `incomplete`.
The summary reports `mailboxes_by_assurance` and does not emit a single
exact/not-exact mailbox count: either would collapse distinct evidence levels.
A status of `aggregate_match` means only the observed totals agree, while
`bounded_content_match` is limited to the explicitly covered body hashes.
`mailboxes_with_durable_accepted_result` is a workflow-state count, not a
message-fidelity guarantee. Consumers of customer-proof version `2` must
update before accepting version `3`. The provider-evidence generator accepts
only known customer-proof versions and requires `assurance_status:
engine_confirmed` for version 3 and later qualification evidence. Version 4
adds per-run endpoint scan intervals and explicitly states that the source is
not revalidated after destination scanning; separate endpoint scans are not a
synchronized account-wide snapshot. Consumers of version 3 must update before
accepting version 4. Version 5 records a post-destination source folder-list
and per-folder cursor recheck, including timestamps and whether tracked
UIDVALIDITY, UIDNEXT, EXISTS, and HIGHESTMODSEQ values remained unchanged on
the source. This is not an atomic snapshot and does not prove continued
quiescence. Consumers of version 4 must update before accepting version 5.
Version 6 adds post-scan destination cursor revalidation evidence alongside
the source recheck; the two checks are sequential, not atomic, and do not
prove continued quiescence. For completed live transfer runs, the
`verification_consistency` object is derived from the folder cursors attached
to the verified transfer attempt; standalone verification runs use their own
folder observations. `verification_observation_source.kind` identifies which
record supplied those observations (`transfer_attempt`,
`standalone_verification_run`, or `not_recorded`). A `not_recorded` observation
is not evidence that either endpoint was unchanged.
Consumers of version 5 must update before accepting version 6. Each evidence
row also carries `proof_tier` (`aggregate`, `metadata`, or bounded `content`)
separately from `proof_result`; none implies cutover readiness.
`cutover_readiness.status` remains `not_established`: the operator
quiescence barrier is not machine-enforced and the two servers do not provide
an atomic account-wide snapshot.
The `mailswiftsync-support-bundle` artifact uses format version `2`: project,
mailbox-job, and run identifiers are type-separated pseudonyms derived with a
fresh per-bundle salt, preserving joins inside one bundle without correlating
identifiers across separately exported bundles. Consumers of version `1` must
update before accepting version `2`.
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
- Reopening a completed project invalidates its current verified mailbox states
  and requires fresh terminal evidence before it can complete again. Reopening
  a project managed by a completed cutover workflow is refused until a new
  cutover epoch can be durably represented; the prior external confirmation is
  never reused.
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
