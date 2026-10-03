# Changelog

All notable changes to MailSwiftSync are documented here. Detailed pre-alpha
development history is preserved in repository history and is not shipped in
operator distribution archives.

## [Unreleased]

- Extracted bounded IMAP LIST parsing, mailbox descriptor construction, and
  folder metadata handling into a dedicated folder-inventory module. Existing
  response-size, literal-size, mailbox-count, retained-inventory, timeout, and
  cancellation bounds remain enforced.
- Extracted adaptive UID FETCH page sizing, compact UID-set encoding, and exact
  requested-versus-parsed page coverage checks into a dedicated fetch-pages
  module; sparse UID ordering and fail-closed coverage tests remain in place.
- Fixed schema-version documentation drift and broadened the documentation gate
  to recognize parenthesized and labeled SQLite schema-version claims.
- Corrected the operator-facing rate-limit summary to describe the current
  hierarchical adaptive process-launch policy instead of implying a fixed
  global-only process-start ceiling; documentation validation now checks that
  scenario against the canonical capability evidence.
- Added a scheduled libFuzzer target for XLSX worksheet-dimension and cell
  reference parsing, exercising the same bounded parser used before workbook
  materialization.
- Extracted shared CSV/XLSX row normalization and added a scheduled fuzzer for
  CSV parsing, normalized headers, required-column policy, row widths, and cell
  byte limits.
- Adaptive process-launch recovery counts successful starts at the spawn
  boundary but orders them by the exact token-admission instant. A slow OS
  spawn therefore cannot make a pre-penalty permit look like a fresh recovery
  launch or prematurely raise a throttled domain.
- The extra-options fuzzer exposed valid inline integer options expanding past
  the canonical argv limit; such plans now fail validation before fingerprinting
  or execution, preserving round-trip canonicalization.
- Windows webhook-outbox tests now tolerate brief OS file-handle release delays
  after closing their SQLite ledgers.
- Destination readiness now retains RFC 7889 `APPENDLIMIT` capability data and
  shows the observed maximum message size in the simulation/capacity view.
  Unsupported provider-specific limits remain explicitly unknown.
- Typed folder rules now also support exact source-folder exclusions. The
  exclusion is emitted as an anchored imapsync regex and the independent
  verifier omits the same source folders before reconciliation.
- Added immutable typed exact folder mappings. Each source-to-destination rule
  is validated, included in the plan snapshot/fingerprint, emitted as
  imapsync `--f1f2`, editable from the advanced plan controls, and reused by
  independent message verification; arbitrary regex transforms remain
  unsupported rather than silently diverging between transfer and proof.
- Provider error classification now enforces execution-boundary provider
  identity: documented Gmail, Microsoft 365, and Dovecot signatures cannot
  be attributed to an unrelated endpoint, while RFC IMAP response codes remain
  provider-neutral. Retry delays, durable failure details, and rate-domain
  cooldowns use the same provider-scoped signal.
- Reconciled capability and controller documentation with the provider-context
  classification implementation; live provider qualification remains explicitly
  separate from code-level provider identity handling.
- Batch state-set selection now uses a durable SQL effective-state projection
  instead of scanning every mailbox row in the UI thread.
- Added a regression assertion covering SQL state-set selection across multiple
  effective mailbox states.
- Added strict provider qualification-pack generation from one passing dry,
  live, and recovery evidence record, bound to one bundle, release commit,
  engine binary, proof set, and explicit limitations. Pack generation remains
  separate from and cannot weaken the release evidence gate.
- Organization policy now supports provider-scoped tenant concurrency ceilings
  with canonical provider aliases and fail-closed key/value validation; global
  concurrency policy remains supported for existing configurations.
- Batch admission now evaluates the same organization-policy snapshot against
  every rebuilt durable row, preventing endpoint or tenant overrides from
  bypassing provider-specific safety ceilings.
- Provider endpoint, tenant, and credential concurrency ceilings now constrain
  their corresponding runtime rate domains independently, while unrelated
  domains can use the global worker pool; policy snapshots flow from admission
  into the scheduler.
- Stable release verification now builds and publishes one provider
  qualification pack per release-required provider pair after the evidence
  gate passes; preview releases continue to refuse unsupported qualification
  claims.
- Closed the debug-scene hard-coded cryptographic-value finding by keeping
  debug credentials empty, and removed detailed UID collections from IMAP
  probe assertion diagnostics so sensitive mailbox data cannot be emitted in
  cleartext failure output.
- Added an approval-gated durable cutover workflow with Seed, Catch-up, Final
  Delta, and Verification stages. Each stage advance requires every mailbox to
  have durable verification evidence; intermediate advances requeue only
  verified rows, and final completion requires an explicit external cutover
  confirmation. A staged workflow now prevents generic polling from silently
  collapsing a cutover into Complete. The `cutover run` command now refuses to
  start before the approved RFC 3339 time or outside the persisted maintenance
  window, and preserves the destructive-destination acknowledgement gate. The
  schema validator also verifies the workflow foreign key and stage constraint.
  Cutover plans are now one-shot records: an existing approved or active plan
  cannot be reset by creating another plan.
- Added a dedicated recovery-state libFuzzer target against the production
  mailbox transition policy; arbitrary wire values cannot become accepted
  state edges merely through parsing.
- Added migration-plan TOML round-trip fuzzing against the production profile
  serializer used by durable plans and plan identity.
- Process launch admission now adapts on the same attributed rate-domain path
  as worker concurrency: a tenant-scoped capacity failure reduces only that
  tenant's launch bucket, while global/provider/credential/mailbox buckets are
  reduced only when those domains are implicated. Independent domains continue
  using the global operator ceiling; scoped rates recover additively.
- Batch launch token acquisition now atomically consumes the global token and
  each penalized provider/tenant/credential/mailbox token on that job's path;
  a cooled-down tenant no longer halves launch throughput for unrelated work.
- Migration simulation now shows separate source and destination capacity
  facts from the latest endpoint readiness probe, including provider-reported
  quota usage/limits, exhausted destination capacity, and an explicit unknown
  state when quota headroom was not observed. Unknown capacity is never shown
  as safe headroom.
- Batch failure details now preserve the provider identity selected at the
  execution boundary, matching the provider-aware retry and rate-domain
  decisions instead of falling back to generic classification in durable
  operator evidence.
- Added `notify-webhook --watch [--poll-seconds=N]`, a continuously running
  durable outbox worker that drains lifecycle events and bounded retries while
  keeping endpoint URLs and authentication material outside SQLite. The
  existing one-shot notifier remains available for scheduled delivery.
- Corrected lifecycle webhook classification: failed, cancelled, and
  attention mailboxes now emit `mailbox.failed`, project-level run completion
  emits `migration.completed`, and only successful mailbox runs emit
  `mailbox.completed`. The durable schema is now version 20 so existing
  ledgers replace the earlier trigger transactionally.
- Added owner-only organization policy enforcement through
  `organization-policy.toml`: administrators can require encrypted transport,
  prohibit destination mutation, require metadata/body verification, and cap
  batch concurrency. The policy is displayed in preflight and rechecked at
  batch admission; malformed or unsafe policy files fail closed.
- Webhook status notifications now carry deterministic event IDs, an event
  type, and an idempotency key. Operators may configure a separate HMAC
  signing secret so receivers can authenticate the JSON envelope; durable
  retry/dead-letter delivery remains a separate outbox feature.
- Webhook notifications now queue credential-free payloads in the durable
  SQLite ledger before delivery. Retries use bounded exponential backoff and
  transition to durable dead-letter state after the attempt limit; endpoint
  URLs and authentication secrets are never stored, and endpoint digests bind
  retries to the operator-selected destination.
- Durable ledger transitions now enqueue lifecycle webhook events in the same
  SQLite transaction: migration started/completed, cutover-ready, mailbox
  completion, verification differences, and accepted verification exceptions.
  Events remain endpoint-unbound until an operator supplies the delivery
  endpoint, avoiding persisted URLs while preventing transition/publish races.
- Added a dedicated `certificate` command that refuses incomplete projects and
  atomically emits an Ed25519-signed migration certificate from the completed,
  redacted customer-proof artifact. The certificate records its authenticated
  durable-ledger scope explicitly and does not overclaim independent
  message-level attestation.
- Live transfer attempts now emit bounded, content-free durable progress
  checkpoints (separate from lossy UI telemetry); checkpoint events use
  reliable bounded delivery, and an undeliverable checkpoint prevents a clean
  attempt result. Recovery shows the latest observed attempt/message/byte
  position while continuing to make clear that external-engine progress is
  not per-message verification proof.
- Recovery and batch workflow views now explain blocker consequence and expose
  remediation routes (reconnect accounts, review policy, inspect evidence, or
  review affected mailbox actions) instead of presenting unresolved counts as
  descriptive-only warnings.
- Mailboxes now foreground the migration lifecycle and recommended next action;
  queue policy controls are grouped under collapsed Operator tools so large
  batch review stays workflow-oriented without removing expert controls.
- Batch execution now carries conservative provider identity from each
  endpoint into hierarchical rate domains and provider-aware retry/backoff
  classification; Gmail, Microsoft 365, Dovecot, and unknown endpoints remain
  distinct without inventing undocumented quota limits.
- Controller retry, cooldown, lifecycle, and durable failure-detail paths now
  cross a typed `MigrationError` boundary after redacted diagnostic parsing;
  raw provider/engine prose remains context only and cannot override the
  resulting policy class. Added coverage for server-requested backoff and
  diagnostic-tail isolation.
- CI now generates an LCOV report and enforces explicit line-coverage floors
  for batch admission, durable schema/state transitions, verification,
  recovery/process ownership, provider failure classification, migration-plan
  validation, and OAuth authorization. This is a safety-critical module gate,
  not an arbitrary whole-project percentage; the report is retained as a CI
  artifact.
- `migrateaudit` now streams snapshot categories into temporary SQLite tables,
  compares duplicate identities with indexed SQL multiplicity joins, and
  computes deterministic digests without materializing both input documents in
  Rust. The 256 MiB per-file limit remains explicit: this is bounded supporting
  tooling, not restartable fleet reconciliation.
- Documented the batch execution boundary accurately: engine transfers remain
  OS-thread/process based with a conservative default of two workers, a
  configurable 1–256 ceiling, and hierarchical provider/tenant/credential
  rate domains. Future fleet scale should prioritize pooled/asynchronous
  probes and reconciliation rather than multiplying engine processes blindly.
- imapsync runtime argument construction is now fail-closed: invalid expert
  options return a named plan-construction error before any argv or secret
  files are prepared, instead of silently dropping the invalid tokens. Added a
  regression test for the partial-command hazard.
- Documentation status is now generated from `capabilities.toml`: release
  feature and provider-qualification matrices, package version, SQLite schema
  version, qualified engine, and review metadata are rendered into
  `CAPABILITY_MANIFEST.md` and `PRODUCTION_STATUS.md`. CI regenerates the
  documents and fails if the committed output is stale, preventing implemented
  recovery/report/provider-intelligence work or schema changes from being
  represented as old roadmap items.
- Guided Google Workspace and Microsoft 365 OAuth onboarding: the sign-in
  panel shows the one-time application registration (console link, steps,
  and the exact scope, redirect, and permission values with copy buttons,
  checked in tests against what the authorizer requests), rejects client IDs
  and tenants that do not have the shape the provider issues before opening
  the browser, and remembers the client ID and tenant of a successful
  sign-in (never secrets) in `oauth-clients.toml` for later accounts.
  MailSwiftSync still ships no OAuth client of its own.
- Failure classification now recognizes RFC 5530 IMAP response codes and
  documented provider responses: Exchange Online throttling with its
  suggested backoff (honored as a floor for retries and rate-domain
  cooldowns) and the IMAP-disabled `User is authenticated but not connected`
  response, Gmail's connection, bandwidth, web-login, and app-password
  responses, and Dovecot's connection limit. Gmail's "Too many simultaneous
  connections" was previously treated as a permanent failure and is now a
  retryable capacity signal. Recognized signals and their next step appear in
  the durable failure detail. These are documented, not live-qualified,
  behaviors.
- Added a Recovery workspace. It groups every mailbox that needs an operator
  decision (attention, failed, cancelled, verification difference, delta
  required) by durable attention reason, shows the recommended action and
  the fail-closed checklist, lists the affected mailboxes with their latest
  run detail and transfer-attempt outcome (keyset-paged), and hands a group
  to the Mailboxes page as an explicit selection or opens evidence review.
  It never starts work itself. Recorded processes whose ownership could not
  be verified are surfaced there as a blocking notice.
- The batch queue is now durable SQLite state instead of an in-memory job
  list. An import is written to the ledger as a batch project
  on a worker thread; the Mailboxes view renders virtual rows from a
  filtered row-ID index and a bounded row cache; search and state filters
  run as SQL over a narrow per-mailbox facts table; and admission and the
  batch scheduler read rows from the ledger, loading each job only when it
  is about to run. A restart restores the queue exactly, including row
  labels. Keyring references applied on the Mailboxes page now update the
  durable plans (and require a new preflight) instead of an in-memory copy.
  Selected rows hidden by the current filter no longer make a confirmed
  live run look stale.
- The ledger now records durable provenance for every live engine attempt
  (ledger version 15): mailbox pass and attempt, pass kind, executable identity,
  the secret-free launched command and its digest, folder scope, source
  range, outcome, delta requirement, engine completion counters, emitted
  resume-state digest, and the verification method and outcome with each
  verified folder's UIDVALIDITY/UIDNEXT/EXISTS snapshot and cursor (folder
  names by digest only). The JSON project report carries it per run and the
  Markdown report adds a Transfer passes table. This is still not a
  per-message transfer checkpoint.
- Batch scheduling no longer ties up workers: mailboxes in a provider
  cooldown or retry backoff are parked in a timer queue, ready mailboxes are
  served round-robin across tenants, and workers only run admitted attempts.
- Provider throttling is attributed to hierarchical rate domains (mailbox,
  credential, tenant, provider, global) with AIMD concurrency, escalating to
  a broader domain only after distinct children are throttled, so one
  tenant's throttling no longer pauses another on the same endpoints.
- The batch process-start limiter now takes its token immediately before
  spawn, so workers released together cannot start engines in a burst.
- Startup no longer panics when neither durable nor temporary state can be
  opened; it shows a fatal screen (or exits 1 headless) with both errors.
- Verification levels and the `install-engine` capability are checked
  against `capabilities.toml` by the documentation validator.

- Redesigned the desktop GUI for a professional finish: a structured header,
  an icon sidebar with a clear selection state, raised rounded cards with
  consistent padding, a filled primary action per section, page headers,
  aligned form rows, side-by-side source and destination cards on the Plan,
  status pills for queue health, a console-style activity log, and a progress
  stepper that replaces the wrapping workflow and lifecycle chips. Overview
  stat cards no longer run off the right edge, and the duplicate current-phase
  card is gone. Retro and high-contrast colour packs keep square corners and
  full-strength borders.
- Arrows, check marks, and CJK mailbox folder names no longer render as empty
  boxes: platform symbol and CJK fonts are loaded as fallbacks behind the
  bundled fonts, each validated with egui's own parser first.
- Fixed unreadable text on saturated selection and stripe fills in retro
  packs (black on Windows 95 navy), and account-card colours that ignored the
  active colour pack. Tests now assert selected-text and stripe contrast for
  every pack.
- Added `mailswiftsync oauth-authorize google|microsoft|custom <keyring-id>`:
  a browser consent flow (authorization code with PKCE S256, random state,
  one-shot loopback redirect) for the operator's registered OAuth
  application. It refuses a mismatched state or a response without a refresh
  token and stores the refresh configuration in the OS-keyring entry that
  automatic refresh already reads. It has not yet been qualified against live
  Google or Microsoft tenants.
- Corrected operator documentation that lagged the implementation: batch
  children run independent message-level verification for encrypted imapsync
  plans, verification staging is durably restartable and snapshot-bound, and
  engines are held behind the internal launcher until process ownership is
  durable. The docs now state that the transfer itself still has no
  per-message checkpoint.
- Added a restart state-machine suite for durable verification staging that
  drives the real fetch path against a scripted IMAP server: unchanged resume,
  append, expunge, expunge plus append with unchanged EXISTS, changed
  UIDVALIDITY, a crash after page insertion but before the cursor commit,
  completed-folder restart, and body-fingerprint partial staging. Removed a
  scenario test whose name claimed interruption recovery but only checked
  arithmetic.
- Durable verification stages are made owner-only through the opened
  descriptor on every open, not only at creation, and existing SQLite
  sidecars are repaired the same way. The stage must be a regular file owned
  by the current user, and the path SQLite reopens must be the descriptor
  that was secured.
- Batch workers now route every controller-generated journal line, terminal
  failure detail persisted to the ledger, and error diagnostic on stderr
  through one per-mailbox redaction boundary. The message-verification failure
  line previously computed a redacted string but displayed the raw error, and
  upstream error text such as OAuth `error_description` was trusted verbatim.
  The GUI also redacts journal lines against the active plan's secrets.
- Restartable message verification no longer combines two mailbox snapshots.
  Stage cursors now persist the folder's SELECT snapshot (UIDVALIDITY,
  UIDNEXT, EXISTS). A resumed scan reuses staged pages only when the server
  still reports that exact snapshot; any difference, such as an expunge plus a
  delivery that leaves EXISTS unchanged, discards the folder's staged rows,
  fingerprints, and cursor and rescans it from zero. Completed folders are no
  longer trusted unconditionally, a folder completes only when its staged rows
  equal the snapshot's EXISTS, and stages written by earlier builds are
  discarded on open.
- `notify-webhook` transport failures no longer echo the request URL, so a
  secret-bearing path supplied through `MAILSWIFTSYNC_WEBHOOK_URL_FILE` stays
  out of stderr and service logs; authentication headers are marked sensitive.
- Headless path options (`--source-secret-file`, `--destination-secret-file`,
  `--diagnostic-log`) are refused when repeated instead of silently using the
  last value, and a missing value names the offending option.
- Fixed a startup stack overflow caused by the overview summary redispatching
  the Overview page recursively; the application shell now remains the sole
  owner of workspace-page dispatch.
- Encrypted imapsync verification now persists fetched-page cursors and opt-in
  body fingerprints in a private SQLite stage, resumes staged pages after a
  controller restart, and binds retained data to a SHA-256 plan identity.
  Successful verification or a changed plan clears the stage; provider-scale
  interruption qualification remains outstanding.
- Verification failures now surface their bounded diagnostic reason in
  operator attention state, making incomplete evidence actionable.
- The doctor command now reports engine-specific qualification requirements:
  it checks the trusted imapsync version for imapsync runs without incorrectly
  requiring imapsync for native Dovecot runs.
- German completion and safety statuses now use the translation catalog, and
  the localization guard covers additional visible UI text patterns.
- The disposable destination storage-fault lab now retries temporary-directory
  cleanup after Dovecot shutdown, preventing a socket-removal race from
  turning successful fault assertions into a CI failure.
- Added a persisted English/German language choice for the desktop interface,
  with German navigation, settings, account setup, overview, activity, and
  verification labels. CLI output and generated reports remain English.
- Support bundles no longer include customer-controlled project names; their
  redaction metadata explicitly records that project names are excluded.
- Added deterministic property-style parser tests for arbitrary IMAP response,
  endpoint, and shell-option inputs, plus UTF-8 quoted LIST-name round trips.
- Split metadata FETCH response parsing into its own IMAP parser module without
  changing the live verifier's call surface.
- Staged message pages are now inserted in canonical mailbox/UID order, making
  duplicate-Message-ID reconciliation independent of randomized HashMap order.
- Added generated differential reconciliation coverage comparing every
  semantic source/destination mismatch field across 48 deterministic cases.
- Aligned the in-memory reconciliation reference with SQLite's deterministic
  wrong-folder candidate selection and expanded staged parity checks to compare
  full source/destination folder and UID evidence.
- Updated the capability manifest to reflect that SQLite-backed streaming
  reconciliation is wired into live verification; large-account qualification
  and externally fault-injected per-message resume qualification remain
  outstanding. Durable verification-page checkpoints are now implemented.
- Microsoft 365 Basic-auth admission now identifies canonical hostnames when
  endpoints include ports or trailing dots, and no longer misclassifies hosts
  that merely contain a Microsoft domain as a substring.
- IMAP LIST quoted mailbox names now preserve UTF-8 bytes while unescaping
  quoted backslashes and quotes, preventing non-ASCII folder names from being
  corrupted during enumeration and verification.
- Documented the native Dovecot initial-backup limitation observed with
  Maildir targets that refuse INBOX replacement; the automated native-engine
  fixture covers mdbox, not Maildir. The native integration lab now also runs
  `sync -1` against an already-populated destination, covering the preservation
  command path that previously lacked product-level regression coverage.
- The native Dovecot integration lab now uses mdbox for its destination
  mailbox store because initial `doveadm backup` may need to replace INBOX,
  which Maildir cannot delete and recreate. Both fixture servers explicitly
  share a hierarchy separator, and messages are verified through
  `doveadm fetch` instead of Maildir filenames.
- Native Dovecot preflight now validates the destination userdb without
  enumerating the target mailbox store before its first sync, avoiding early
  target access that can trigger GUID/UIDVALIDITY conflicts.
- The disposable IMAP integration lab now captures bounded, redacted engine
  diagnostic logs on failure, including output produced before a headless
  command stalls or times out.
- Native Dovecot verification failures now retain the failing command's
  bounded diagnostic tail or report the parsed source/destination inventory
  counts, making incomplete status output actionable instead of generic.
- Native Dovecot source enumeration and verification now select the correct
  mail-location override for Dovecot 2.3 versus 2.4; headless failure output
  also includes the bounded verification reason instead of hiding it behind
  a generic durability message. Status fields use Dovecot's space-delimited
  field-list syntax, so 2.3 accepts the aggregate query.
- Headless durability failures now include the latest bounded persistence
  diagnostic, so integration failures identify the failed ledger operation.
- Late or foreign process-output events are discarded without marking the
  durable migration result as failed; those events are presentation-only.
- A completed single-mailbox project can be reopened for an incremental
  headless pass only with an explicit `--reopen-reason`; reopening verifies
  the prior queue is fully verified and has no active run, and records the
  operator-provided reason in the ledger.
- The Dovecot product-lab artifact now includes its bounded fetched Message-ID
  sample and fixture counts when destination assertions fail.
- Sparse-UID lab fixtures now use zero-padded Maildir filenames so Dovecot's
  lexical scan order matches numeric fixture order before expunging UIDs; the
  retained-message assertion checks UID-associated fixtures 91 and 100.
- The distributed Linux runtime now includes `procps`, which packaged
  imapsync invokes for process inspection; the product integration lab fails
  early if `ps` is missing.
- The packaged-imapsync integration fixture now enables bounded diagnostic
  output and a 30-second engine protocol timeout to locate the dry-run shutdown
  stall before the outer lab watchdog expires; a bounded live thread/process-
  group snapshot is captured after 60 seconds if a product command is still running.
- Integration-only process supervision diagnostics now distinguish a child
  wait stall, blocked stdout/stderr drain, terminal-event delivery, and
  repeated terminal persistence failure without routine product output when
  the opt-in environment switch is unset.
- Successful single and batch preflights now atomically persist engine
  observations with their immutable plan digest while keeping the mailbox
  `ready`; preflight statistics cannot be mistaken for verification.
- Maildir integration assertions now inspect only `cur/` and `new/` messages,
  excluding Dovecot index/cache files from fixture message counts.
- Provider qualification now normalizes durable operator-facing engine labels
  (such as `imapsync fallback`) to canonical evidence engine identifiers.
- The small generic IMAP integration lab now uploads its verified product
  proof without mislabeling it as provider-qualification evidence.
- The disposable destination storage-fault fixture now runs with the
  privileges required to prepare Dovecot-owned test mail storage and locates
  the Dovecot worker without aborting on absent library directories. Controller
  state and runtime files now live in an owner-matched private subdirectory;
  the file-size limit allows server metadata writes while rejecting the target
  oversized-message fixture, and the expected absent-message search is safe
  under strict shell error handling.
- Dovecot native transfers now use a private runtime config that includes the
  destination config and reads the source credential from an owner-only file;
  the password stays out of process arguments and the profile.
- Fixed Dovecot source mailbox verification command ordering so TLS and
  `-o` overrides are passed before the `mailbox status` subcommand.
- Fixed Dovecot live command ordering: dsync-specific `-l`, `-s`, and `-1`
  options now follow `sync`/`backup`, avoiding a Dovecot 2.3 global-option
  parse failure before migration starts.
- The Dovecot integration fixture now selects `initial_mirror` explicitly, so
  its initial transfer exercises `doveadm backup` rather than inheriting the
  final-preservation `sync -1` default.
- Strengthened the archive-layout regression test to inspect the actual
  macOS/Linux/Windows release packaging commands before validating extracted
  archive roots.
- The Dovecot integration fixture now asserts sparse-mailbox ownership and
  owner-write permission before transfer, failing immediately with the actual
  UID and mode instead of timing out later in the migration.
- Expanded `make check` to include documentation-claim fixtures, local Markdown
  link validation, release-channel and archive-layout tests, and the same
  preview compatibility gate used by CI.
- Restored Dovecot service-account ownership of the sparse Maildir after
  `doveadm expunge`, which can create or rewrite root-owned mailbox metadata.
- Staged verifier fallback lookups now index each message by its reconciled
  mailbox, normalized date, and size, avoiding repeated cross-folder scans
  when many folders share common metadata.
- Diagnostic transcripts now request owner-only permissions atomically when
  each file is created on Unix, avoiding a brief umask-dependent exposure
  before permissions are tightened.
- Fixed the sparse-UID Dovecot fixture ownership after creating its Maildir
  tree, so the Dovecot service user can assign UIDs and expunge the fixture
  messages during integration setup.
- Schema validation now parses actual SQLite `CHECK` expressions instead of
  accepting matching text embedded in defaults or comments; writable repair
  and read-only validation use the same strict constraint signature.
- Closed SQLite handles before removing temporary database fixtures, fixing
  Windows test failures caused by platform-specific file locking.
- Fixed integration artifact-directory setup so the host runner applies
  private permissions before handing ownership to the container user.
- The disposable Dovecot integration lab now explicitly runs its fixture
  provisioning as root and creates a matching private runtime directory;
  product state, configuration, secrets, and proof outputs live under that
  private directory, and the distributed image still defaults to its
  unprivileged service account.
- Native Dovecot integration failures now upload the same bounded, redacted
  environment and server/controller diagnostics as the packaged IMAP lab.
- Recovery schema validation now checks the `WHERE` predicates of partial
  unique run indexes, rejecting same-named indexes that do not enforce active
  run ownership; writable recovery backs up and rebuilds malformed indexes.
- Webhook notifications now default to credential-free operational status with
  customer names, endpoint hosts, and active-process details excluded; sending
  customer metadata requires the explicit `--include-customer-metadata` option.
- Removed the verifier-wide dead-code exemption so unused production verifier
  paths are reported by normal compiler and lint checks.
- Batch verification failures now persist an explicit verification-incomplete
  reason, and bounded status summaries/webhooks include aggregate counts by
  durable attention reason in deterministic key order.
- Moved the message-verification test suite into its own module file so the
  production reconciliation implementation is easier to review independently.
- Moved the large core persistence test suite out of `core.rs`; the root now
  serves as a compact module index over database, state, evidence, report, and
  policy responsibilities.
- The disposable IMAP lab now records bounded runtime/dependency diagnostics
  before early exits and uploads environment and server/controller diagnostics
  when a product transfer fails.
- New profiles disable imapsync automapping by default so independent message
  verification can certify live migrations; headless live rejects plans that
  cannot provide required verification evidence.
- Batch migrations now persist verification-incomplete detail with the
  durable terminal result instead of reducing the reason to an output line.
- XLSX early-dimension inspection now uses a bounded streaming XML parser and
  resolves namespace-qualified OOXML element and attribute names by local name.
- The disposable IMAP integration artifact now retains MailSwiftSync command
  output when a smoke run fails, enabling diagnosis without retaining fixture
  credentials or private server keys.
- Startup now immediately removes leftover credential run directories when
  durable process recovery proves there are no surviving or unverified owners;
  ambiguous ownership retains the seven-day cleanup fallback.
- Startup recovery now bounds the number of active process identities it will
  materialize and fails closed on an oversized ledger instead of allocating an
  unbounded recovery vector.
- Durable batch creation and full mailbox/ID loads now fail closed above the
  100,000-row queue limit instead of accepting or materializing an arbitrarily
  large restored ledger.
- The doctor imapsync version probe now uses a five-second process timeout and
  bounded output capture, preventing a malformed executable from hanging or
  exhausting diagnostic memory.
- CI failure annotations no longer interpolate untrusted compiler or test log
  text into GitHub workflow commands; detailed logs remain available as
  artifacts and step summaries.
- Ledger validation now rejects projects whose durable mailbox queues exceed
  the runtime's 100,000-row admission and restore limit.
- CSV imports now validate and parse the same no-follow file handle, with a
  bounded reader that rejects files that grow past the input limit mid-read.
- XLSX imports now validate and parse through one bounded no-follow file
  handle, preventing path replacement between archive checks and parsing.
- XLSX import now validates actual cell coordinates and dense range area before
  Calamine allocates its cell matrix. Legacy XLS import is disabled because its
  parser materializes the matrix before those resource bounds can be enforced.
- Release compatibility and evidence gates now share explicit prerelease
  classification, including numbered alpha tags such as `v0.1.0-alpha.1`.
- Headless batch retry admission now fails closed when durable mailbox state is
  missing, rather than silently omitting those mailbox IDs from selection.
- Recovery schema validation now requires every runtime object to be an actual
  SQLite table, rejecting stamped ledgers that substitute views for tables.
- Durable batch reuse now streams queue comparison and loads only mailbox IDs,
  avoiding materialization of every persisted profile for large imported queues.
- Verification staging now keeps SQLite temporary sort and join data in memory,
  preventing sensitive mailbox metadata from spilling into a process-wide temp
  directory outside the private run lifecycle.
- Ledger validation now bounds both individual and aggregate persisted batch
  profile sizes, preventing oversized queues from causing unbounded startup
  allocations during durable-state restore; the checks use UTF-8 byte length.
- Terminal run details now use the durable event-size bound for both run and
  event records, preventing oversized provider errors from bloating the ledger.
- Batch parent runs now retain a bounded count-and-digest plan summary while
  child runs retain their individual plans, avoiding a duplicated giant parent
  snapshot for large queues.
- Batch project creation now rejects oversized persisted profile data before
  inserting any durable project or mailbox rows.
- Headless batch retry selection now uses chunked durable state reads instead
  of issuing per-mailbox state and attention queries for large queues.
- Live IMAP metadata verification now stages source and destination records in
  a private SQLite database and reconciles them in bounded batches, avoiding
  account-sized Rust message maps while preserving bounded mismatch evidence.
- Verification-stage databases now live inside private per-run directories, so
  normal cleanup and stale-run recovery remove mailbox metadata after a crash.
- XLSX import dimension checks now fail closed when a non-empty worksheet lacks
  an early bounded dimension declaration, preventing unchecked Calamine expansion.
- Removed the obsolete string-based IMAP FETCH parser so metadata tests and live
  behavior share the byte-preserving parser.
- Bound XOAUTH2 continuation reads, error acknowledgements, and tagged results
  to the live verification deadline and operator cancellation signal.
- Added an aggregate memory bound for retained IMAP LIST folder descriptors,
  preventing many individually bounded records from exhausting verifier memory.
- Released transient fetched-page reservations immediately after SQLite staging,
  so large-account admission scales with bounded page memory instead of
  accumulating the former account-sized allowance.
- Kept SQLite reconciliation intermediates inside the private per-run stage
  database instead of process-wide temporary tables, preserving metadata
  permissions and crash cleanup boundaries.
- Capped DNS address results before connection attempts so hostile or malformed
  resolver responses cannot create an unbounded socket-address vector.
- Bounded headless status and support projections of active process identities,
  with explicit truncation reporting; full recovery scans remain unbounded only
  where inspecting every recorded process is required for safety.
- Added an aggregate mailbox-row budget to detailed headless status so many
  projects cannot combine their per-project pages into an oversized response.
- Applied the same aggregate sample budget to support-bundle mailbox exports,
  preventing large multi-project ledgers from producing oversized artifacts.
- Recovery validation now rejects cross-owner links among runs, events, active
  processes, verification acceptances, and evidence records.
- Added SQLite nonnegative constraints for process identity fields and durable
  mismatch numeric metadata, with transactional repair of older ledger tables.
- Classified IMAP folder failures as control, session-fatal, or folder-local
  outcomes so cancellation and unusable connections stop account enumeration
  promptly while recoverable folder errors retain bounded diagnostics.
- Hardened release-bundle verification against absolute, traversal, and
  backslash-containing archive member paths, with tar and ZIP regression cases.
- Release-bundle verification now rejects symlinks and special filesystem
  entries instead of following or validating them as ordinary files.
- External provider-evidence extraction now rejects unsafe or special tar
  members and refuses to overwrite existing evidence files.
- Fixed headless batch completion validation to use chunked durable mailbox
  states instead of a capped full-mailbox status read, avoiding false failures
  for batches larger than the status-page limit.
- Bounded complete project report snapshots so customer-proof and operator
  exports fail closed instead of materializing an unbounded mailbox population;
  larger projects remain available through paged status/report views.
- Removed an unnecessary full clone of the pending single-run mismatch vector
  before terminal persistence, reducing peak memory during large verifications
  while preserving retry-on-durability-failure behavior.
- Added a separate 64 MiB estimated budget for accumulated mismatch detail;
  pathological mismatch-heavy verification now fails closed for operator review
  instead of growing mismatch evidence without a process-level bound.
- Shared the immutable job/run identifiers across in-memory mismatch records,
  removing two repeated heap allocations per detail row during reconciliation
  and bounded mismatch report reads.
- Enforced the mismatch-detail budget inside each reconciliation pass, preventing
  an individual pass from exceeding the bound before its results are merged.
- Expanded the verifier admission estimate to include reconciliation indexes and
  classification sets, so expensive in-memory structures are budgeted before
  they are constructed.
- Transferred pass-level mismatch vectors into the final result with allocation
  reuse, reducing transient peak memory during reconciliation merges.
- Interned live IMAP mailbox names across metadata-page keys, reducing repeated
  folder-string allocations while preserving mailbox-local message identity.
- Removed the duplicate owned UID from live metadata records; coverage now
  validates the canonical mailbox key directly.
- Read-only and writable ledger validation now rejects orphaned foreign-key
  rows, including records imported while SQLite foreign-key enforcement was off.
- Ledger validation now rejects semantically contradictory exact-verification
  outcomes instead of trusting a forged status label over its counters.
- The terminal evidence write boundary now applies the same exact-outcome
  invariant before committing a verification-difference record.
- Explicit exact aggregate-engine evidence now also requires the
  engine-authoritative marker.
- Schema validation now verifies foreign-key actions and match modes, not
  only parent and child column names.
- IMAP folder-failure reporting now retains only a bounded cause sample while
  preserving the total failure count.
- Evidence history now enforces a foreign key from each historical result to
  its recorded run, rejecting orphaned audit claims during recovery.
- Recovery validation now rejects evidence and mismatch rows whose valid run
  belongs to a different mailbox.
- Live source and destination metadata scans now share one fetched-state
  admission budget, preventing each account from independently consuming the
  full allowance before reconciliation begins.
- Reconciliation indexes now borrow interned mailbox names instead of cloning
  a folder string for every message candidate.
- Routed budgeted IMAP authentication and metadata commands through
  cancellation-aware, timeout-retrying writes with short socket I/O slices.

### Documentation and release packaging

- Fixed the broken bulk-migration template link and added a real local
  Markdown-link validator for source documentation and release archives.
- Release staging now includes linked capability metadata and the provider
  operator documentation allowlist, so packaged documentation does not point
  outside the archive.
- Archived scheduler and message-verification design documents, maintainer
  qualification material, CI setup, and historical reports from distribution
  archives.
- Removed the duplicate Native Dovecot entry from the README's stable section
  and added manifest-backed validation for experimental-only status terms.
- Corrected Gmail qualification guidance to use the Admin console for Workspace
  user creation and distinguish Workspace OAuth, personal Gmail OAuth, and
  conditional app-password testing.
- Replaced fixed Google and Microsoft refresh-token lifetime claims with
  provider-linked lifecycle guidance and explicit reauthorization behavior.
- Added SQLite nonnegative constraints for durable counters and made signed
  integer-to-unsigned reads fail closed instead of turning negative values into
  huge evidence counts.
- Migrated existing current-version evidence tables that lacked those SQLite
  constraints, with a pre-repair backup and constraint validation on open,
  read-only, backup, and snapshot paths.
- Extended the current-schema signature to require the evidence-history
  foreign-key relationship used by recovery and reporting.
- Made SQLite snapshots copy the validated, migrated connection so restoring a
  dirty current-version ledger cannot reinstall the unrepaired source file.
- Strengthened the v12 schema signature to verify runtime index columns,
  uniqueness, and partial-index properties rather than names alone.
- Made durable mismatch readers reject negative SQLite values instead of
  silently interpreting malformed unsigned fields as absent.
- Replaced string-matched mailbox stability retries with a typed mutation
  outcome so transport and protocol failures are not retried as mailbox churn.
- Hardened XLSX dimension preflight to inspect every worksheet XML part,
  avoiding assumptions about worksheet-part numbering before Calamine parsing.
- Schema index validation now also verifies each required index is attached to
  its intended runtime table.
- Hardened XLSX dimension parsing against overflowing column references before
  applying worksheet size limits.
- Preserved duplicate IMAP SEARCH UIDs until coverage validation so malformed
  server responses cannot be silently deduplicated.
- Made IMAP LIST read retries observe operator cancellation while waiting on
  transient socket timeouts or nonblocking reads.
- Applied an absolute eight-second connection budget to IMAP greeting reads,
  including slow-drip peers during STARTTLS setup.
- Added an absolute per-command deadline to non-verification tagged IMAP
  responses so preflight/authentication reads cannot be extended by slow drips.
- Applied the same absolute deadline to XOAUTH2 continuation and result reads.
- Made restore permission/flush failures roll back the installed ledger and
  recover the preserved SQLite sidecars when possible.
- Bounded secret-file reads after opening the file, so concurrent growth cannot
  bypass the configured 64 KiB credential-file limit.
- Applied the same bounded signing-key read to the Windows signing path as to
  Unix, preventing platform-specific unbounded key allocations.
- Made Windows signing-key reads open the final path component without
  following reparse points, matching the secret-file safety boundary.
- Switched executable and trust-bundle identity hashing to fixed-size chunks,
  avoiding whole-file allocations during plan validation.
- Bounded current and legacy saved-profile reads to 1 MiB before TOML parsing.
- Bounded migration-proof reads to 32 MiB before signing or verification JSON
  parsing.
- Bounded migration-assurance snapshot inputs to 256 MiB per file before
  in-memory comparison.
- Streamed migration-assurance snapshot hashing instead of allocating a
  second full serialized snapshot-sized buffer.
- Made customer-proof aggregate counters saturating so malformed extreme
  evidence values cannot wrap into misleading totals.
- Bounded diagnostic-log filename components so oversized durable IDs cannot
  create oversized headers or evade per-file rotation limits.
- Bounded trusted extra-option input to 64 KiB and 128 parsed tokens before
  allowlist validation and command generation.
- Bounded branding and appearance preference reads before TOML parsing, so
  oversized local configuration files cannot consume unbounded memory.
- Capped project-browser query results so an all-projects refresh cannot turn
  SQLite's signed LIMIT conversion into an accidental unbounded ledger load.
- Stopped secret-free mailbox summary reads from materializing discarded
  mailbox configuration blobs in support bundles.
- Kept full mailbox configuration out of report snapshots, where report
  rendering never consumes it.
- Bounded report snapshot history reads to the latest acceptance/evidence per
  mailbox and the 20 runs rendered by operator reports.
- Kept headless status and completion checks off the full mailbox configuration
  read path; those consumers only need mailbox identity and state.
- Bounded persisted batch-profile TOML parsing and identity extraction to 1 MiB
  before deserialization.
- Bounded durable report plan-snapshot TOML parsing to 1 MiB before
  deserialization.
- Bounded OAuth refresh-configuration JSON read from the keyring to 64 KiB
  before deserialization.
- Bounded ordinary keyring credentials and OAuth access/refresh tokens to
  64 KiB before retaining them in process memory.
- Aligned report evidence selection with the ledger's captured-time ordering,
  including the deterministic ID tie-breaker.
- Restore failures while flushing a temporary snapshot now remove the
  temporary ledger before returning an error.
- Corrected the active production-status document to identify the full
  `0.1.0-alpha.1` package version used by binaries and reports.
- Added deterministic 10k/100k synthetic verifier coverage at 0%, 10%, and
  100% mismatch rates to guard the indexed reconciliation path at scale.
- Bounded opt-in diagnostic transcripts with buffered checkpoint flushing,
  per-file rotation, and a total diagnostic-directory size cap.
- Applied the pinned cargo-deny advisories, bans, licenses, and sources policy
  to pull-request CI and release publication gates.
- Added the capability-claim validator to the local `make check` path and
  explicitly exclude repository metadata from host-native package archives.
- Updated dependency-audit documentation to reflect pull-request and release
  enforcement, not only the scheduled audit.
- Updated the cargo-deny pin to 0.20.2 so current RustSec CVSS 4.0 advisories
  are parsed before policy checks run.
- Indexed wrong-folder reconciliation by metadata and ordered folder ranges to
  avoid rescanning every folder for each duplicate Message-ID candidate.
- Indexed duplicate Message-ID content and metadata matching with per-group
  queues, avoiding repeated linear scans of large duplicate groups.
- Capped core mailbox pages at 1,000 rows, mailbox status exports at 100,000
  rows, and verification/run-list pages at 1,000 rows before passing limits
  to SQLite.
- Capped detailed headless status mailbox output at 10,000 rows per project
  and added an explicit truncation indicator; summary status remains exact.

## [0.1.0-alpha.1] - 2026-09-25

MailSwiftSync 0.1.0-alpha.1 is a technical preview for controlled operator
pilots. It is not a general-availability or unattended-production release.
See [the release note](docs/release-0.1.0-alpha.md),
[production status](PRODUCTION_STATUS.md), and the
[release-readiness criteria](docs/release-readiness.md) for scope and open
gates.

### Added

- Durable project, mailbox, run, recovery, evidence, and operator-review
  workflows with local SQLite state and secret-free reports.
- Dovecot-native and imapsync transfer engines, with local `doveadm`
  execution and bounded metadata verification for encrypted imapsync runs.
- Cross-platform portable release archives, checksums, SBOMs, and build
  provenance artifacts.
- Capability, compatibility, security, and release-readiness documentation
  backed by automated drift and validation checks.

### Changed

- Hardened v12 schema validation, backup, restore, and read-only recovery;
  malformed or non-MailSwiftSync SQLite files fail closed.
- Reworked message reconciliation to use indexed bookkeeping, prepared
  mismatch persistence, normalized IMAP dates, and bounded evidence detail.
- Added bounded IMAP DNS resolution, connection/read/write deadlines, LIST and
  cancellation budgets, incremental response framing, folded Message-ID
  parsing, and bounded folder failure details.
- Made plaintext-secret import require exactly
  `MAILSWIFTSYNC_ALLOW_PLAINTEXT_SECRETS=1` and tightened bulk-import limits.
- Removed dormant remote-Dovecot SSH configuration and command generation;
  native Dovecot execution is explicitly local-only until a secret broker
  exists. Legacy profiles requesting remote execution fail closed.
- Set the package version and release identity to `0.1.0-alpha.1`; release
  tags must exactly match the Cargo package version.

### Security and integrity

- Credentials remain session-only at the application layer and are delivered
  through private files or local child environments as appropriate.
- Ledger, evidence, process identity, enum, signed-integer, and filesystem
  validation fail closed on malformed persisted state.
- Release and capability documentation gates reject contradictory production,
  GA, and support claims.

### Known limitations

- Hosted-provider live pilots, very-large-mailbox scale evidence, and full
  body-content verification remain release-readiness gates.
- Verification currently materializes bounded metadata state in memory;
  SQLite-backed streaming reconciliation is still required for very large
  migrations.
- Native Dovecot verification remains aggregate-level, and remote Dovecot
  execution is intentionally unsupported.
