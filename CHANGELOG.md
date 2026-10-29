# Changelog

## Unreleased

- Detect a CSV import's separator by parsing the header with CSV quoting
  rules for each candidate and choosing the one that yields the required
  columns, so a comma inside a quoted header no longer selects the comma for a
  semicolon- or tab-separated file. A header that splits into the required
  columns under two separators is refused as ambiguous.
- Bound the OAuth redirect listener: stop after 16 invalid callbacks (start
  the authorization again), check the flow deadline on every connection
  rather than only when the listener is idle, and bound the whole request
  head read so a client trickling bytes cannot hold the listener past the
  deadline or cancellation. Empty preconnects no longer count as invalid
  callbacks.
- Record each transfer attempt's failure class with the provider of the
  endpoint that failed, the same classification the controller uses for retry
  and terminal state, instead of a provider-blind guess.
- Classify a batch or single-run failure once, at the execution boundary, and
  drive retry eligibility, backoff, rate-domain cooldowns, the log label, and
  the durable detail from that one result. A failure whose retry budget ran
  out no longer loses its controller class marker to the "retry budget
  exhausted" prefix.
- Pin the libFuzzer compiler to the already-qualified `nightly-2026-10-02`.
  Add GitHub's workflow dependency lockfile and a pinned verification workflow;
  lock all direct and transitive Actions dependencies after replacing an
  unreachable `dtolnay/rust-toolchain` SHA with its supported `master` ref and
  explicit Rust 1.92.0 toolchain input.
- Resolve container Debian packages from a dated Bookworm snapshot so the same
  source revision uses a reproducible package set; snapshot advances remain an
  explicit security-update review.
- Enable weekly Dependabot pip-compile updates for the Python dependency sets
  used by provider qualification and evidence validation, including their
  generated hash-locked requirement files.
- Add a weekly and pull-request ZAP baseline scan against a live, isolated
  fleet API instance. The scan uses a generated read token, passive baseline
  rules, a digest-pinned ZAP image, and retains its report as a workflow
  artifact. Require the same-origin cross-origin resource policy and narrowly
  document ZAP dispositions for the API's intentional no-store policy and
  authenticated snapshot timestamp.
- Remove CodeQL-reported test log injection sinks by using fixed assertion
  messages instead of echoing HTTP responses, and generate per-test credential
  fixtures instead of embedding password-like literals.
- Make unmaintained transitive Rust dependencies an explicit deny-by-default
  policy, with narrowly documented exceptions for the three Linux `zbus` 5
  maintenance advisories that have no compatible parent-crate upgrade yet.
  Add an advanced CodeQL workflow with a valid checked-in query config and
  Rust, Python, and Actions analysis on pull requests, pushes, and a weekly
  schedule. Hash-lock the Python qualification and `pip-audit` dependency
  trees, and require hashes in CI and release installs. Replace the fleet
  API's permissive request-line/header splitting with a strict `httparse`
  request-head parser and fuzz the production parser through the existing
  libFuzzer workflow.
- Prevent a continuously eligible older project from monopolizing unattended
  supervision. The supervisor now rotates through eligible projects after
  every batch pass while preserving per-project retry backoff, so recurring
  delta passes cannot indefinitely starve later customer projects.
- Add real-server fault drills to the IMAP integration lab (external review
  finding 5). Besides killing the controller, the lab now SIGKILLs only the
  imapsync engine, crashes the source and the destination Dovecot server
  mid-transfer, and crashes the controller three times before resuming. Every
  drill spreads its messages over three folders and requires the resumed
  destination to hold each source Message-ID exactly once, so a duplicate
  and a lost message can no longer cancel out in a count. The crash-recovery
  matrix records the new scenarios.
- Fix exactly matching mail landing in Attention, unrecorded, whenever the
  destination kept a folder the source does not have, such as a provider's
  own empty folder or a parent imapsync creates for a nested folder
  (`Projects` for `Projects/2026` when the source's `Projects` is not
  selectable). The folder totals now cover the verified scope: excluded
  source folders and empty destination-only folders are left out, while a
  destination-only folder holding mail still counts and its messages are
  reported as extra. Before, the unequal totals contradicted the exact
  verification label and the ledger refused the result. Found by the new
  three-folder interruption drill.
- Fix maintenance-window boundaries (external review findings 1 and 2). A
  weekday now names the day a window opens: `22:00-06:00@Fri` runs Friday
  22:00 to Saturday 06:00. Previously it matched Friday 00:00-06:00 (the
  tail of Thursday night) and stopped at Friday midnight, while the
  Activity page's "window closes in" readout assumed Saturday 06:00.
  Membership and time-to-close now come from the same occurrence, back-to-
  back full-day windows count as one span, and the close instant is
  resolved through the local time zone, so a window across a
  daylight-saving change reports its real length (a skipped close time
  closes when the clock jumps; a repeated one closes at its first
  occurrence). Day names must be exact three-letter abbreviations or full
  names; `MondayXYZ` and `Fr` are refused instead of being read by prefix.
- Enforce the organization policy's `minimum_verification` on the evidence a
  mailbox actually reached, not only on the plan (external review finding
  6). An exact match below the minimum, for example counts-only evidence
  under the default `metadata` policy, now goes to Attention with the reason
  *verification incomplete* instead of Verified, keeping its evidence, so it
  cannot pass for a reconciled mailbox or unlock the customer proof. The
  Plan page now reports the level the plan can really establish: an
  imapsync plan with automap, header mutation, or another setting that
  disables message-level verification showed "Level 2 — message metadata"
  but could only produce counts. It also names the limiting setting and
  warns when policy requires more than the plan can establish. A single
  live run (GUI or `headless live`) now enforces the policy's plan check
  as batch admission already did; before, it was only shown on the
  preflight checklist. A native Dovecot migration therefore needs a
  `compatibility` or `lab` policy with `minimum_verification = "aggregate"`
  (and `allow_destination_deletion = true` for its initial mirror).
- Fix the fleet API's over-capacity 503 being lost on Windows: a refused
  connection is now answered on a helper thread that keeps the socket open
  until the client has read the reply (at most one second), instead of
  closing it with the request unread, which reset the connection.
- Harden the `serve` fleet API for unattended network use:
  - `--tls-cert`/`--tls-key` terminate TLS 1.2+ natively, so a network
    listener no longer needs a reverse proxy; the key file must be
    owner-only, and plaintext requests to the TLS port get no answer.
  - `/readyz` reports readiness separately from `/healthz` liveness: 503
    when the latest scan failed or is older than three refresh intervals,
    with no paths or error text in the unauthenticated body.
  - `/metrics` adds `mailswiftsync_fleet_refresh_failures_total` and
    `mailswiftsync_fleet_last_successful_scan_timestamp_seconds`.
  - Accept errors are logged (at most once a minute) with a short backoff
    instead of being dropped silently.
  - The 8 KiB request-head limit now also applies when the terminator
    arrives in the read that crosses it.
- Add project holds: a durable pause that stops new mailbox launches without
  interrupting transfers already running. Place one with `mailswiftsync
  hold <state.db> <project-id> <operator> <reason>`, from the fleet API
  (`POST /v1/projects/<id>/hold?reason=...` with an operator token), or in
  the GUI under **Activity → Hold new launches…**; lift it with `resume`,
  `POST /v1/projects/<id>/resume`, or the **Resume launches** button in the
  banner every page shows while the project is held. The hold is checked in
  the ledger transaction that claims each mailbox, so it also stops a batch
  that another process already has in progress; new runs, including
  preflights, are refused, and `supervise` skips the project. Mailboxes the
  hold kept from starting keep their state (a pending final catch-up stays
  `delta_required`) and their queued child runs close as cancelled.
  `status --summary` and `fleet-status` gain a `hold` object per project, and
  `/metrics` gains `mailswiftsync_projects_on_hold`. Ledger schema v39 adds
  the `project_holds` table.
- Fix GUI issues found in a full visual audit (dark, light, Windows 95,
  German, 1024×700, and 200% interface size):
  - The keyring, advanced-options, settings, and engine dialogs could run
    past the bottom of the window with no way to reach the lower controls;
    they now grow to the screen edge and scroll.
  - At large interface sizes the project title ran underneath the status
    pill in the header; it now truncates with an ellipsis.
  - A long German detail line in the Overview's command center widened the
    whole page past the window edge; grid values now wrap.
  - Page cards no longer sit underneath the floating scroll bar.
  - "Advanced connection settings" for source and destination now line up
    when the two account cards differ in height.
  - The Mailboxes selection summary read "2 selected · 2 visible", which
    looked like only two rows were on screen; it now reads "2 selected · 2
    of them in this list · 0 hidden by the current filter".
  - Recovery explained every unclassified group as "needs operator review";
    delta-required and cancelled groups now say why they stopped.
  - Recovery guidance, quota-readiness rows, retry and throttling lines, the
    blocker count on Mailboxes, and several status messages were shown in
    English in every language. They are now translated, and the
    localization guard catches `language.text(...)` calls split across
    lines, which is how these slipped through.
  - Secondary text in the Windows 95 theme measured 3.8:1 on the teal page
    background, below WCAG AA; it is now black, and the contrast test now
    checks primary and secondary text on the background and striped rows of
    every theme.
- Let named operators approve migration waves through the `serve` fleet API.
  `--operators-file` names an owner-only file of `<token> <name>` lines;
  `POST /v1/waves/<wave-id>/approve` with an operator's token records
  `<name> (fleet API)` as the approver, so the audit trail names whoever
  holds that token. Approval takes the same instance lock as `wave approve`,
  so it lands between supervised passes (409 with `Retry-After` while the
  ledger is busy), and it is refused for a read-only token, an
  already-approved wave, a ledger on an older schema, or an ID found in more
  than one ledger. `GET /v1/projects/<project-id>/waves` returns the wave
  list for read and operator tokens. Without an operators file `serve`
  stays read-only.
- Add `serve`, a long-running read-only fleet API for monitoring and PSA
  tooling. `mailswiftsync serve <directory> --token-file <file>` scans every
  ledger under the directory on a fixed interval (default 30 seconds) and
  serves the unchanged `fleet-status` v1 JSON at `/v1/fleet` and fleet-wide
  Prometheus metrics at `/metrics`, plus an unauthenticated `/healthz`. Data
  endpoints require a bearer token of at least 32 characters read from an
  owner-only file. The listener binds loopback by default and refuses another
  address unless `--allow-non-loopback` acknowledges that TLS must come from
  a reverse proxy. It exposes no control actions, takes no instance lock, and
  answers a failed scan or truncated projection with 503 rather than stale
  or partial data. Requests are bounded in size, time, and concurrency.
- Add **Queue settings → Verification workers** (`batch_verification_workers`,
  default 0). With it, a batch mailbox whose transfer finished credits the
  transfer to the adaptive limiter, hands its transfer slot back, and runs
  independent verification under a separate bounded pool, so long scans no
  longer hold transfer capacity. The scheduler is woken when a slot is
  released early. The default keeps verification in the transfer slot;
  verification workers are capped at the transfer workers and count toward
  organization `max_concurrency`.
- Add **Suggest mappings from server folder roles** to the Plan page's typed
  folder mappings. It reads the RFC 6154 special-use roles (Sent, Drafts,
  Trash, Junk, Archive) of both accounts, with a name-based fallback for
  servers that advertise none, and proposes explicit mappings such as
  `[Gmail]/Sent Mail` → `Sent Items` for review. Applying them turns off
  automap, so Gmail ↔ Microsoft 365 plans keep their special folders and stay
  certifiable by independent verification. Ambiguous roles, roles missing on
  the destination, and Gmail's All Mail are reported for an operator
  decision instead of being guessed.
- Update the Windows clipboard dependency `clipboard-win` from the yanked
  5.4.1 to 5.4.3 so `cargo deny` accepts the lockfile again.
- Update the transitive `ctutils` dependency (via `digest` and the Linux
  Secret Service keyring backend) to 0.4.4 for RUSTSEC-2026-0385, which
  fixed non-constant-time mask generation and `u128` comparisons.
- Authenticate the independent verifier with SASL `AUTHENTICATE PLAIN` when
  a password-auth user name or password contains non-ASCII characters and
  the server offers `AUTH=PLAIN`. IMAP quoted strings are 7-bit, so `LOGIN`
  with, for example, an umlaut in the password could be rejected after the
  transfer itself had succeeded, leaving the mailbox without verification.
  ASCII credentials keep using `LOGIN`.
- Keep `supervise` serving the whole ledger when one project's batch pass
  fails. A failed pass previously stopped the supervisor; under the shipped
  `Restart=on-failure` unit it restarted onto the same oldest project every
  10 seconds, retrying an unreachable server indefinitely and starving every
  newer project. The failing project now backs off exponentially (up to one
  hour) while other projects continue; a bounded run with failed passes still
  exits nonzero.
- Make the Overview's "Review attention items (n)" prompt for a batch count
  failed and verification-difference rows, matching the command center's
  Needs attention bucket on the same screen (it counted only Attention rows).
- Keep only the newest transfer-progress checkpoint per attempt. Checkpoints
  are written every few seconds and every reader uses only the latest, but
  all were retained: a one-hour transfer left about 580 KB of ledger per
  mailbox (tens of gigabytes for a 100,000-mailbox project). Durable state is
  now about 3.2 KB per mailbox regardless of transfer duration. Existing
  ledgers keep their historical checkpoints.
- Make ledger backups finish while a migration is writing. `backup` (and the
  read-only repair copy used by status, metrics, and the GUI snapshot
  worker) copied 128 pages per SQLite backup step, and SQLite restarts a
  stepped backup whenever another connection writes, so a backup started
  during a migration only completed once writes paused. The copy now reads
  one consistent WAL snapshot in a single step; under the benchmark workload
  it completes in about 0.1 s without delaying writes.
- Add `scripts/benchmark-ledger-workload.sh`: a sustained controller write
  workload with concurrent read-only pollers that gates write latency
  percentiles, SQLITE_BUSY/locked failures, WAL high-water mark, and durable
  bytes per mailbox.
- Name the reason when the ledger refuses a project phase change or reopen
  (for example "reopen requires every mailbox verified and no active runs or
  processes") instead of surfacing SQLite's "Query is not read-only".
- Import semicolon- and tab-separated mailbox CSV files. Excel in German and
  many other European locales saves "CSV" with semicolons, which previously
  parsed as one column and was rejected for missing headers; the separator is
  now detected from the header row.
- Update `rustls-webpki` to 0.103.16 and `webpki-roots` to 1.0.10 for
  RUSTSEC-2026-0382 (certificate verification could accept distrusted
  intermediates) and RUSTSEC-2026-0381 (bundled roots trusted distrusted
  emSign intermediates). They back the verifier's own IMAP TLS connections
  as well as OAuth token and webhook HTTPS requests.
- Stop reporting refused batch claims as durability failures. A child whose
  durable claim is refused by policy (for example a Final Delta
  source-quiescence attestation older than 15 minutes, or a mailbox already
  running) now fails with that reason; previously the refusal set the
  session-wide durability fault, blocked phase advancement, and marked the
  whole batch for durability review. Run-admission refusals are named
  ledger rejections instead of SQLite's "Query is not read-only".
- Emit each `mailswiftsync metrics` family as one contiguous group with its
  HELP and TYPE lines first. Headers were written together before any
  samples, which the Prometheus exposition format and `promtool check
  metrics` reject.

- Stabilize native runtime CI by testing launch-limiter pacing at the permit
  boundary instead of relying on child-process scheduling, and enforce LF
  endings for Markdown so documentation assertions behave consistently on
  Windows. Keep expert `imapsync --useuid/--usecache` settings confined to the
  explicit recovery drill. The clean default-imapsync integration fixture no
  longer includes duplicate Message-IDs that upstream intentionally skips;
  verification still fails closed when such omissions occur. Widen the
  scheduler-level anti-burst test interval so loaded macOS runners do not
  mistake delayed child cleanup for an unpaced release.
- Make unattended supervision select the oldest project with automation-safe
  batch work across the whole ledger, restore that project explicitly, and
  advance to the next eligible project after settlement. This prevents a newer
  idle (or corrupt) project from hiding older queued migrations; add restart and
  multi-project fairness regressions.
- Normalize snapshot file paths without allowing `..` components to escape an
  absolute root, and label simultaneous missing/extra/modified snapshot
  differences as `mixed` in both in-memory and streamed reports.
- Classify supervisor SQLite retry conditions by rusqlite error code instead
  of parsing vendor-dependent error text.
- Carry ledger lock contention as a typed supervisor condition, so permission,
  corruption, and other permanent ownership failures are not retried based on
  diagnostic wording. Align Linux service preflight commands with the unit's
  `/usr/bin/mailswiftsync` executable path and cover every snapshot difference
  status combination.
- Define MSP alert severities, response owners, and polling limitations for
  the existing local metrics/status snapshots; explicitly identify operational
  signals that still require direct review rather than implying coverage.
- Improve desktop review flow: responsive overview and evidence cards,
  blocker-first preflight groups, a guided verification empty state, and
  contextual next steps and side-by-side folder/UID details for message
  differences. Difference tables scroll horizontally on narrow windows;
  verification level descriptions collapse to keep results prominent.
- Put the current migration phase and next action above scheduling details;
  fix verification totals substituting the verified count into every field.
  Recovery read errors now offer retry instead of showing an empty success
  state. Refresh Retro 80s README screenshots from the running application
  using an isolated synthetic fixture and a reproducible capture script.
- Preserve durable verification staging when SQLite refuses to close,
  including during subsequent drop; cover the failure with a live-statement
  SQLITE_BUSY regression test.
- Export durable transfer-attempt outcome counts as a gauge rather than a
  counter: unfinished attempts change label when they reach a terminal outcome,
  so the series is not monotonic and must not carry a Prometheus `_total` name.
- Apply available Debian security upgrades to the runtime image while holding
  the qualified Dovecot package versions, and scan PR/release images with
  checksum-pinned Trivy; retain full CVE reports and scanner/database identity,
  and block fixable HIGH or CRITICAL findings.
- Persist an explicit verification-incomplete reason when a completed live
  transfer emits neither verification evidence nor a diagnostic, so attention
  runs cannot have an empty terminal detail; apply the same rule to batch
  mailboxes and include known plan limitations in their durable detail.
- Refresh the locked Linux D-Bus dependency patch releases. RustSec still
  classifies `zbus_names`, `zvariant`, and `zvariant_derive` as unmaintained
  (RUSTSEC-2026-0371, -0370, and -0372); they remain transitive desktop,
  accessibility, and keyring dependencies, with no compatible replacement
  selected.
- Exercise the LIST fuzz target through the production budgeted streaming
  reader, fixing a fuzz-only compile failure and ensuring the harness covers
  the same bounded parser path used by live verification.
- Rerun process ownership, controller crash recovery, and ledger write/corrupt-
  restore fault drills on the current commit; record the binary digest and
  retain the explicit boundary between local fault fixtures and power-loss or
  live-engine qualification.
- Compile every nightly fuzz target locally and mark intentionally unused
  provider identity helpers in the provider-error fuzz harness, keeping the
  full parser/safety-target build warning-free.
- Request a TLS 1.2-or-newer protocol floor on both encrypted imapsync
  connections, matching the native Rustls probe; make the packaged IMAP lab's
  Dovecot server floor explicit and add a packaged IO::Socket::SSL smoke test
  with forced TLS 1.0/1.1 rejection baselines and a TLS 1.2 success case.
  End-to-end imapsync protocol enforcement and negotiated-protocol evidence
  remain integration qualification gates.
- Record a full durable typical-shape verification benchmark matrix on commit
  `478fe7bbdf37e8aa010b9f1ff36964cc9a2b3ebf` through 1,000,000 messages per
  endpoint. The release CLI reports the full source revision; harness and
  release-binary SHA-256 values and per-size reconciliation/RSS/stage/journal
  metrics are in `docs/verification-envelope.md`. This is synthetic SQLite
  evidence only, not provider, IMAP transfer, concurrent-worker, or final-
  evidence-commit qualification.
- Add a bounded, privacy-preserving one-shot Prometheus text snapshot for
  durable local-ledger job, transfer-attempt, and verification counts. It
  refuses truncated projections and labels no customer-controlled identities;
  document that registered processes are not a liveness check and that
  process-local throughput, host resources, provider throttles, and SQLite
  latency are not exported.
- Add the queued mailbox-job count to status summary and fleet-status JSON as
  an additive field in the existing versioned contract.
- Export last durable progress snapshots for unfinished transfer attempts as
  gauges, explicitly distinguishing retryable attempt work from unique mailbox
  coverage, throughput, and freshness.
- Export current durable verification finding counts by missing, extra,
  modified, and probable-match category; keep lower-confidence probable matches
  distinct from confirmed differences.
- Read status projections from one SQLite transaction snapshot so concurrent
  controller commits cannot mix ledger moments within a response; document
  that fleet snapshots remain independent per ledger.
- Map the five SRE operations questions to the versioned durable status fields,
  and explicitly distinguish current gauges from process-local telemetry,
  cumulative counters, and metrics not yet exported. Document the absence of a
  Prometheus contract and prohibit sensitive high-cardinality labels.
- Separate existing adversarial-security test coverage from outstanding
  release-specific penetration qualification, and state explicitly that the
  webhook/OAuth egress guard does not make generic IMAP endpoints safe for a
  remotely accessible service.
- Document controller state/ownership safeguards and the unqualified areas:
  SQLite successful-write/WAL workload metrics, independent transfer and
  verification budgets, and adaptive optimization for verified throughput.
- Clarify supply-chain evidence boundaries: the final-image SPDX inventory is
  not an OS/Perl vulnerability scan, and the pinned imapsync checksum is not
  upstream publisher provenance. Document the version-update qualification
  sequence and keep these as explicit GA release gates.
- Keep the migration configuration fingerprint independent of secret bytes
  while testing that static-secret rotation changes the process-local preflight
  credential binding; add an exact-image CI check for Docker state-volume
  persistence versus ephemeral runtime files. Clarify that OAuth recovery
  records are owner-only but unencrypted, and that container-volume smoke tests
  do not qualify SQLite recovery, backups, or host power-loss durability.
- Document subprocess reader counters and platform caveats; verify destructive
  sync's default-off/production-policy, confirmation, same-plan preflight, and
  durable-plan boundaries, including the absence of identity-bound approval in
  this local single-owner product.
- Track active subprocess reader threads and cumulative detached-reader/drain
  timeout counts in drain-failure diagnostics; add a repeated inherited-pipe
  regression proving released reader threads exit.
- Replace first-match secret-option removal with validated removal of every
  generated placeholder pair; malformed/duplicate options are flagged and an
  adjacent CLI switch is preserved rather than consumed as a value.
- Expand production qualification scenarios for TLS behavior in both the Rustls
  probe and actual imapsync transfer/reconnect paths, and make the interim
  one-customer-per-OS-security-boundary operating rule explicit for MSP pilots.
- Clarify the Basic/Standard/Forensic evidence contract, exact RFC822 body-hash
  semantics, and partial-coverage limits; expand the still-open hypervisor
  power-cut matrix to include runtime secrets, checkpoints, evidence commits,
  report signing, and explicit recovery outcomes.
- Reject imapsync `--useuid` plus `--usecache` plans under production
  organization policy because an interrupted in-flight APPEND can duplicate a
  message on resume; compatibility and lab policies retain the explicitly
  acknowledged path with independent verification.
- Use a random, zeroized per-logger HMAC key and type-separated pseudonyms for
  diagnostic project, run, mailbox-job, email, and Message-ID identifiers;
  avoid dictionary-recoverable plain hashes and raw run IDs in transcript paths
  and headers.
- Correct the release-readiness verification-resume description and add a
  regression check for tracked-snapshot metadata reuse versus always-refetched
  body proofs.
- Bound diagnostic-writer shutdown when filesystem I/O stalls; optional
  transcript output may be incomplete rather than holding controller shutdown.
- Re-run the complete durable verification reconciliation matrix on the current
  revision through 1,000,000 messages per endpoint; record per-phase I/O, peak
  RSS, stage/journal growth, FLAGS work, and restart timings without presenting
  synthetic SQLite performance as provider or transfer qualification.
- Exercise duplicate-ID, absent-ID, and mismatch-heavy reconciliation through
  1,000,000 records per endpoint on the same current revision; record the
  shape-dependent disk and latency costs while keeping them explicitly separate
  from provider and end-to-end migration qualification.
- Re-run the release-gatekeeper, controller-crash, engine-SIGKILL, bounded
  storage-failure, corrupt-restore, and backup-recovery drills against an
  artifact embedding the current full commit SHA; record its digest while
  keeping power-loss and real filesystem-full qualification explicitly open.
- Inject `SQLITE_FULL` midway through terminal evidence plus 4,000 streamed
  mismatch rows; prove the run/mailbox/evidence transaction rolls back wholly
  and retries successfully after storage capacity is restored.
- Reuse the verifier's preallocated SHA-256 hex encoder for synthetic header
  identities, avoiding per-byte formatting allocations while preserving the
  stable identity encoding.
- Reject migration plans whose source and destination resolve to the same
  normalized IMAP host, effective port, and case-folded username, while
  allowing distinct IMAP services on different ports of one host.
- Normalize IPv4-mapped IPv6 endpoint literals to their IPv4 identity so
  self-copy and destination-collision checks cannot be bypassed by address
  spelling alone.
- Treat the same normalized IMAP host and username as one mailbox identity in
  the provider-smoke self-copy guard regardless of port, while still
  validating the configured ports; cover a 143/993 alias collision.
- Attach the actual completed transfer-attempt verification folder cursors to
  customer-proof run consistency, including scan intervals and successful
  source/destination revalidation times; distinguish these from standalone
  verification runs and report `not_recorded` when no verified observations
  exist.
- Update the transitive `inout` dependency to non-yanked 0.2.4; the prior
  lockfile version caused the configured strict dependency audit to fail.
- Record the exact packaged imapsync and native Dovecot integration evidence,
  including its metadata-versus-aggregate assurance levels and explicit
  cutover/provider-qualification limitations; refresh the evidence with the
  current-head repeat run and validate each exported proof digest.
- Keep secret-shaped diagnostic redaction fixtures out of assertion failure
  messages so a failing test cannot echo the fixture into CI logs.
- Redact bare usernames and account identifiers in diagnostic transcripts,
  while preserving already-pseudonymized mailbox identifiers.
- Align the Fedora RPM install smoke test and service deployment guide with the
  package's nested `docs/distribution` paths, and assert the RPM, workflow, and
  operator guide agree.
- Extend the versioned pre-migration risk report with an optional read-only
  stage-filesystem capacity observation; warn when available headroom is below
  the observed synthetic projection while explicitly avoiding a fit guarantee,
  and refuse symlinked or untrusted stage-directory paths.
- Replace Calamine-based XLSX parsing with a bounded OOXML reader for the
  mailbox-list import subset, remove the yanked ZIP 8.6.0 exception, and add a
  dedicated parser fuzz target. This removes the reported Calamine parser from
  the dependency graph without claiming the upstream report is remediated; the
  custom parser remains subject to adversarial review and fuzz qualification.
- Require `cargo audit --deny yanked` for stable releases and retain that gate
  after removing the yanked dependency.
- Remove the stale `ttf-parser` advisory claim from current release readiness;
  the current lockfile no longer contains that crate after the Wayland
  renderer dependency change.
- Reject reversed XLSX worksheet dimensions and out-of-range explicit row/cell
  references before worksheet materialization, preventing malformed
  coordinates from reaching unchecked arithmetic.
- Reject malformed XLSX boolean-typed cells during raw archive preflight so
  noncanonical values cannot be silently coerced into migration booleans by
  the workbook parser; discover worksheet parts from XML roots rather than
  conventional ZIP paths, including relationship-targeted custom paths, and
  detect UTF-16 XML parts before worksheet materialization.
- Run live provider smoke jobs as the packaged unprivileged UID 10001, including
  ownership of staged secrets and smoke evidence.
- Limit live-provider smoke proof artifact retention to 14 days because the
  reports contain test mailbox identity and operational metadata.
- Restrict the live-provider secret workflow to `main` branch code.
- Require the live-provider environment to be branch-restricted and reviewer-
  approved before it releases credentials to workflow code.
- Terminate and reap the detached recovery-test process group before provider-
  smoke cleanup removes its ledger and credential workspace, including on
  interrupt or termination signals.
- Include the implemented cPanel/Dovecot route in provider-smoke usage output.
- Decouple the native Dovecot integration path from the pinned imapsync
  prerequisite, allowing that path to run and be qualified independently; the
  native Dovecot product flow passed in the isolated Dovecot 2.3.19.1 lab.
- Prefix generated Dovecot 2.4 runtime configurations with the required
  `dovecot_config_version` setting; qualify the native path against Dovecot
  2.4.2 and imapsync 2.314 in the disposable IMAP lab.
- Make the disposable Dovecot fixture work with unprivileged local accounts;
  disabling its login chroot remains an explicit option for already-isolated
  sandboxes only.
- Fuzz the production streaming IMAP LIST response parser, including literal
  mailbox names and trailing extended data, in addition to its field helpers.
- Add a manually dispatched cPanel/Dovecot-to-cPanel/Dovecot smoke route using
  separate disposable source, destination, and recovery mailboxes with
  owner-only staged password files; this is not provider qualification.
- Reject provider smoke configurations that identify the same source and
  destination mailbox, and require the recovery destination to differ from
  both after canonicalizing endpoint hostnames, ports, IPv6 literals, and
  usernames, preventing recovery writes to either protected account.
- Validate IMAP endpoint syntax and safely quote generated profile strings;
  invoke packaged binary paths with spaces as single executable paths.
- Sanitize provider labels when forming smoke-proof artifact paths while
  retaining the original provider identity in proof metadata.
- Add a workload-specific measured-range projection for verification stage
  storage (including sampled rollback-journal peak) to the scale-only risk
  report; it is not a capacity bound, and worker disk capacity remains
  unknown unless separately measured.
- Treat changes in CONDSTORE/HIGHESTMODSEQ availability as verification snapshot
  drift; a newly available watermark without a scan-time baseline now requires
  a fresh verification pass.
- Label exact per-message metadata reconciliation as a metadata match in
  customer and operator evidence, rather than implying that only aggregate
  mailbox totals were compared; body equality remains explicitly unverified.
- Exercise a real SQLite `SQLITE_FULL` failure in durable verification staging;
  assert the page transaction rolls back, the stage survives process-style
  reopen, and staging can resume after capacity is restored.
- Require cutover readiness to reference current final-stage evidence; metadata
  and body evidence now need a completed Verification-phase run, a transfer-pass
  record matching the same method and outcome, and complete durable folder
  scans with source and destination cursor revalidation ordered after the
  opposite endpoint's scan. Readiness event details separately count
  independently revalidated mailboxes and engine/aggregate-only evidence.
- Record current-main, one-million-message-per-endpoint durable verification
  benchmarks for typical, mismatch-heavy, duplicate-ID, and missing-ID
  synthetic shapes, with host, stage/journal, reconciliation, flag, and restart
  measurements; explicitly retain their non-provider-qualified interpretation.
- Allow standalone reverification to select a specific completed transfer
  baseline with `--baseline-run`; include the baseline run ID in the versioned
  JSON result so historical audits retain explicit provenance. Bump the
  reverification and batch-result schema versions.
- Re-emit managed cutover readiness with a new event identity when its prior
  source-quiescence validity window has expired and the operator reconfirms;
  active readiness windows remain deduplicated.
- Preserve preexisting verification-stage databases when schema or identity
  initialization fails; newly created partial stages are cleaned up, while
  existing recovery evidence is retained for inspection.
- Bound unbudgeted STARTTLS handshakes with an absolute deadline, and commit
  each durable verification page together with its progress cursor to avoid an
  extra SQLite synchronization and inconsistent intermediate checkpoint.
- Resume committed durable metadata prefixes after interruption only when a
  fresh SELECT exactly matches UIDVALIDITY, UIDNEXT, EXISTS, and tracked
  HIGHESTMODSEQ; complete folders may be reused, while drifted/untracked
  prefixes and all body fingerprints require fresh fetches.
- Consume bounded literal values in IMAP LIST extension data without treating
  them as mailbox names, including parenthesized extensions whose closing
  delimiters immediately follow literal bytes; reject aggregate extension-
  literal data beyond the inventory limit. Reduce body-fingerprint and FETCH
  protocol-text temporary allocations, and reject a projected metadata-page
  overrun before issuing the metadata UID FETCH or staging its records.
  Interrupted metadata scans resume only from committed prefixes with an
  identical tracked snapshot; otherwise they restart from the first page.
- Apply one absolute deadline across native TLS capability and destination-
  readiness probes, including DNS/TCP setup, STARTTLS, authentication, and
  follow-up IMAP commands; report probe timeouts separately from verification
  resource-limit failures.
- Rerun process-release, controller-crash/recovery, and ledger corruption /
  storage-fault drills on a binary embedding the current full commit SHA; record
  the digest and host details while retaining the explicit non-coverage of
  provider behavior, real filesystem exhaustion, and host power loss.
- Record the newly reported RustSec informational unmaintained notices for the
  transitive `zvariant`, `zbus_names`, and `zvariant_derive` crates. No
  vulnerability or yanked crate was reported; compatible parent-crate migration
  and Linux keyring/accessibility qualification remain open.
- Record that GitHub Dependabot alerts and security updates are disabled for
  this repository despite weekly update configuration and scheduled audit CI;
  enabling and verifying the repository-level controls is an administrator
  release gate.
- Record that GitHub secret scanning is enabled but push protection and
  non-provider-pattern scanning are disabled; distinguish detection from
  push-time prevention and retain the required administrator configuration as
  a release gate.
- Pace provider-facing engine release only after its durable process
  registration acknowledgement, so gatekeepers accumulated during slow
  registration cannot open IMAP sessions in a burst; add a concurrent delayed-
  acknowledgement regression using the Unix test gatekeeper.
- Allow operators to refresh an expired source-quiescence attestation during
  Catch-up, so it can authorize the transition to Final Delta without reopening
  or recreating the managed cutover workflow.
- Add a fresh pre-launch IMAP readiness check that blocks single and batch live
  runs when the destination reports an exhausted INBOX-root quota. Unsupported,
  failed, or incomplete quota observations remain unknown; non-exhausted INBOX
  quota does not imply projected or account-wide capacity.
- Require operators to reconfirm source quiescence after Final Delta before
  admitting final-verification workers or standalone re-verification. Clear
  the prior-stage attestation at the Verification transition and emit
  `migration.cutover_verification_started` only after the new attestation;
  retain the earlier assertion in audit history.
- Publish `migration.cutover_ready` only while the source-quiescence
  attestation is fresh, include its expiry in the event detail, and allow a
  refreshed attestation to publish readiness after verification without a
  redundant mailbox rescan.
- Keep exact UIDVALIDITY, UIDNEXT, message-count, and MODSEQ values out of
  verification revalidation errors; retain the names of changed fields for
  useful diagnosis without logging raw mailbox cursor state.
- Redact structured mailbox and folder values from opt-in diagnostic
  transcripts, alongside credentials, email addresses, and message IDs; retain
  the documented warning that arbitrary engine prose can still expose metadata.
- Return actionable cutover refusal reasons (including missing or stale source-
  quiescence attestations and missing operator confirmation) instead of
  presenting safety-gate refusals as generic SQLite errors.
- Represent plan checks as passed, blocked, or unknown. Show destination
  capacity as unknown unless exhaustion is observed: even a non-exhausted
  INBOX-root quota does not prove that the projected transfer fits or establish
  account-wide headroom. Keep unresolved capacity visible in the readiness
  summary and direct operators to confirm it with the provider.
- Revalidate the freshness of source-quiescence attestations when advancing
  out of Final Delta and before cutover completion. Allow audited refreshes
  during Verification so a long final scan does not force use of stale
  operator confirmation.
- Recheck verification-stage filesystem headroom before materialized SQLite
  reconciliation phases and every 256 persisted mismatch rows, so detected
  low-space conditions stop further stage work while preserving the durable
  stage for recovery. This is still a high-watermark, not a disk reservation
  or an aggregate stage-size bound.
- Serialize cutover approval/attestation/advancement and migration-wave approval
  with the exclusive ledger owner lock. Make cutover/wave status reads use a
  read-only connection, and revalidate the approved cutover stage after the
  headless worker acquires the lock so a stale pre-lock read cannot launch a
  later stage.
- Share one cutover admission validator between the CLI precheck and the
  lock-held headless launch check, with regression coverage for stale stages,
  missing source-quiescence evidence, and future schedules.
- Enforce managed cutover approval, stage/phase pairing, and fresh
  source-quiescence evidence inside single-run, batch-run, and child-claim
  SQLite transactions, so GUI/batch callers cannot bypass the headless gate or
  launch delayed Final Delta workers using an old pause assertion. Generic
  project transitions cannot advance a managed workflow. Allow an audited
  attestation refresh during Final Delta after the operator reconfirms
  quiescence.
- Preserve managed cutover phase when a transfer pass fails; keep the failed
  mailbox and run durable for review without attempting an invalid generic
  Attention transition or misreporting that blocked transition as a ledger
  durability failure.
- Hold the exclusive ledger owner lock for certificate export and its durable
  `proof_ready` event, preventing that state transition from racing an active
  controller.
- Use the caller's IMAP tag in LIST commands during source snapshot
  revalidation; the previous hard-coded tag caused the verifier to ignore
  Dovecot's tagged LIST completion and time out after a successful inventory.
- Emit `migration.final_delta_started` when a managed cutover enters its final
  delta, emit `migration.cutover_verification_started` when that pass enters
  final verification, and delay `migration.cutover_ready` until all final
  Verification-stage evidence is durable or differences are explicitly
  accepted. Include separate clean and exception counts in readiness events;
  generic non-cutover phase changes no longer emit cutover lifecycle events.
  The ledger migration repairs queued legacy notifications and moves readiness
  publication to the final verification commit.
- Keep approved cutover workflows in Verification after exception acceptance;
  only the cutover transition with its external confirmation can mark them
  Complete.
- Invalidate mailbox terminal states when reopening a standalone completed
  project, and refuse reopening completed cutover workflows until repeat
  cutover epochs are modeled.
- Version migration-wave list output as `mailswiftsync-wave-list` v2 and
  distinguish clean verification from accepted exceptions; wave completion no
  longer claims cutover readiness.
- Bound and reject multiline/control-character cutover, migration-wave,
  project-reopen, and verification-exception audit inputs so operator-provided
  values cannot forge additional-looking durable audit events.
- After independent destination scanning, re-LIST the source and read-only
  EXAMINE each scanned folder. Refuse message-level verification if the
  selectable folder set, UIDVALIDITY, UIDNEXT, EXISTS, or a previously tracked
  HIGHESTMODSEQ changed. Persist each matching source revalidation timestamp
  through the stage, transfer-pass ledger, verification ledger, and customer
  proof v5. This narrows the observation gap but does not claim an atomic
  cross-server snapshot or continued quiescence after the check. Migrate the
  ledger to persist the new source revalidation cursor and timestamp.
- Query RFC 2087 quota roots using the mandatory INBOX mailbox name, require a
  successful tagged response with a matching QUOTAROOT, and label the result as
  INBOX-root scope instead of implying account-wide capacity.
- Disable implicit Reqwest proxy discovery for policy-controlled OAuth token
  and webhook requests, so environment proxy variables cannot bypass locally
  resolved-address policy. Version webhook HMAC as v2 and authenticate the
  event ID, type, idempotency key, delivery timestamp, and exact body bytes
  using an unambiguous length-prefixed canonical envelope; document receiver
  replay-window and durable-deduplication responsibilities.
- Bump customer-proof artifacts to v5. In addition to endpoint scan intervals,
  proofs record whether the source folder set and per-folder cursors remained
  unchanged during a post-destination recheck. This is still a sequential,
  non-atomic observation and does not establish continued source quiescence or
  cutover readiness. Preserve fail-closed behavior for unsupported migration
  transformations.
- Recheck source folder inventories and tracked cursors after destination
  revalidation as well as before it, refusing exact evidence if the source
  changes during the destination check. The final source observation narrows
  the consistency window; it is not an atomic snapshot or proof of continued
  mail-flow quiescence.
- Require an audited source-quiescence operator attestation before the final
  delta stage can run or advance. The durable cutover status records who
  attested, when, and the change reference/evidence; it is explicitly a manual
  operational assertion, not automatic IMAP cursor revalidation. The ledger
  also persists post-destination source-cursor revalidation. Older ledgers
  cannot complete a newly gated final delta without this attestation.
- Check available space on the verification-stage filesystem before scanning
  and before each SQLite page write; stop verification below a 512 MiB free
  reserve and report an explicit verification limit. This is a best-effort
  high-watermark, not a universal stage-size estimate or reservation.
- Add the missing cPanel/Dovecot-to-Google Workspace and Fastmail-to-Google
  Workspace routes to the generated qualification summaries, and test that every
  release-required signed-evidence route appears in the capability manifest.
- Add a method-qualified verification outcome breakdown to automation status,
  distinguishing aggregate totals from metadata and bounded-content evidence.
- Skip test-inventory generation and upload when CI is canceled by a newer
  commit, while retaining inventory publication after ordinary failures.
- Clarify that Debian/RPM packages are already shipped with signed checksums,
  while signed package-repository metadata and a stable-release support
  lifecycle remain outstanding; refresh the generated capability review date.
- Update Rustls to 0.23.46 to remediate four newly reported TLS advisories;
  enforce the dependency advisory gate against the refreshed lockfile.
- Make the paused-tenant scheduler regression assert that unrelated work starts
  before the actual cooldown deadline, rather than imposing an arbitrary
  half-cooldown latency cutoff that flakes under loaded CI runners.
- Make the resolver-capacity test synchronize worker startup and cancel blocked
  helpers after all slots are occupied, removing its two-second CI scheduling
  assumption while retaining the slot-exhaustion and cleanup assertions.
- Require a tracked CONDSTORE `HIGHESTMODSEQ` before collecting IMAP FLAGS for
  fidelity evidence. Without it, continue metadata reconciliation but leave
  flag/keyword coverage incomplete so an untracked mutable flag observation
  cannot be represented as stable evidence; remove the ineffective extra FLAGS
  round trips, align both the capability manifest and verification envelope,
  and enforce the documented certification boundary with regression tests.
  Parse MODSEQ only from a syntactically valid untagged OK response code with a
  digits-only value, so unrelated or malformed server text cannot establish
  flag-fidelity coverage.
- Reject malformed quoted escapes, ASCII control octets, and invalid UTF-8 in
  IMAP `LIST` fields, mailbox-name literals, and literal continuations, so
  malformed folder inventories cannot pass endpoint readiness.
- Keep server-derived FETCH parser details out of test assertion output while
  retaining the malformed-record regression predicate.
- Document the LIST scanner's intentional ASCII-control-octet mailbox-name
  restriction as an interoperability boundary; RFC 3501 permits some such
  quoted characters, so affected mailboxes fail closed as unsupported.
- Correct the README's engine-profile instructions: hardened execution is the
  default, while compatibility mode is an explicit opt-in.
- Clarify that OAuth rotation-recovery files are plaintext protected by local
  owner-only permissions, not encrypted, and that live expiry/rotation remains
  unqualified.
- Clarify that imapsync automatic OAuth callback refresh is currently Unix-only;
  Windows refuses callback plans until native command-shell quoting is qualified.
  Align the alpha notes and operator wiki with this platform boundary, and
  align operator runbook wording, and regression-test active OAuth-refresh
  documentation surfaces.
- Distinguish the fixed mailbox-name field from RFC 5258 trailing `LIST`
  extension fields; reject literal-valued extension fields explicitly instead
  of misclassifying their payload as a mailbox name.
- Validate the fixed mailbox and hierarchy-delimiter fields in summary-only
  `LIST` probes so malformed records cannot satisfy endpoint readiness checks.
- Record the packaged engine-side destination write-fault CI run with its exact
  commit, run ID, Dovecot/imapsync versions, and successful assertions; label
  `RLIMIT_FSIZE` as a bounded write-failure simulation rather than a real full-
  filesystem qualification.
- Bump customer-proof artifacts to format v3 and replace ambiguous `exact`/
  `verified` certificate fields with evidence-level-specific assurance statuses
  and summary counts. Aggregate agreement no longer appears as message-level
  exactness; label aggregate evidence explicitly in customer exports. Make the
  provider-evidence generator reject unknown proof versions and require the
  v3 engine-confirmed assurance status for qualification claims.
- Require signed provider-qualification evidence for every hosted-provider
  route in the compatibility plan, including cPanel/Dovecot pairs and
  Fastmail-to-Google Workspace; require 1, 10, 20, 50, and 100 GiB live-pilot
  mailbox evidence per route and align operator guidance with the complete
  size and 100k-message scenarios.
- Keep the compatibility matrix and signed-evidence policy aligned with a
  regression test so a documented route cannot be omitted from GA release gates.
- Fail closed when provider-policy strings contain tab or control characters,
  preventing malformed TSV serialization from changing the release gate's
  interpreted policy; require typed release gates and integer thresholds so
  malformed values cannot silently demote required provider routes. Regress
  partial-policy parsing, delimiter injection, and policy type confusion.
- Extend provider-evidence schema and generation tests for the required
  1/10/20/50/100 GiB mailbox observations, preventing the evidence exporter
  from producing records that the strict schema would reject.
- Require the live provider qualification gate to include a direct-imapsync
  comparison for each required large-mailbox case, bound to the tested engine
  binary, dataset and source/destination baselines; verify retained snapshot
  and run-artifact files against their declared digests; carry measurements
  and artifact references into the qualification-pack digest.
- Bound provider A/B metric values to the unsigned 64-bit evidence domain and
  reject oversized JSON integers consistently in the pack builder, release
  gate, schemas, and regression tests.
- Make the provider evidence gate's `--help` and mode parsing explicit so a
  misspelled or conflicting mode cannot silently run a preview validation.
- Stream SHA-256 hashing of provider qualification artifacts in fixed-size
  chunks so large retained logs and mailbox snapshots do not require a
  same-sized temporary memory allocation.
- Reject duplicate JSON object keys in provider policies, evidence, and
  customer proofs so parser differences cannot change signed qualification
  semantics.
- Bound qualification policy/schema, evidence, and customer-proof JSON inputs
  before parsing, preventing oversized external evidence bundles from causing
  unbounded memory use in release tooling.
- Align the provider-testing guide's release-route list with all seven routes
  in the signed-evidence policy, and regression-test route and auth coverage.
- Qualify the production-status recovery summary so durable run-level state is
  not mistaken for a universal or per-message recovery guarantee.
- Label exact aggregate-engine totals as an aggregate match rather than an
  exact metadata match; reserve the latter wording for per-message metadata
  reconciliation while keeping the persisted outcome code compatible.
- Persist UTC scan-start and scan-completion timestamps per verified folder in
  transfer and standalone reverification provenance, export them in the
  project report and reverify response, and version the project-report JSON
  contract to v3 and the single-mailbox reverify response to v2. Legacy
  provenance remains explicitly timestamp-unknown; timestamps do not imply an
  atomic cross-server snapshot.
- Require a balanced parenthesized IMAP FETCH data list with exactly one
  top-level UID before sequence inventory records can count as exact coverage;
  reject truncated lists, missing delimiters, and duplicate UID items.
- Reject malformed numeric-sequence FETCH records instead of silently
  dropping them before metadata, flags, or body evidence is parsed.
- Document that typed imapsync source-to-destination mappings reject `=` in
  source names while destination names and typed exclusions may contain it.
- Clarify that verification cursors retained after interruption are recovery
  context only; fresh scans are required before staged evidence can be reused.
- Refresh the locked `libc`, `skrifa`, `uuid`, and `zeroize` dependencies and
  pin workflow artifact uploads to `actions/upload-artifact` v7.0.2; validate
  the combined update against the current ZIP 9 / Calamine ZIP 8 lock graph.
- Update the direct ZIP dependency to v9 while retaining the v8 version required
  by Calamine; handle fallible ZIP entry-name decoding explicitly in XLSX
  inspection and engine archive validation.
- Parse the mailbox name and delimiter from their fixed IMAP LIST fields rather
  than assuming the mailbox is the final token; ignore following RFC 5258
  extended data, preserve NIL hierarchy delimiters as absent, and reject
  unsupported zero-length mailbox-name literals explicitly. Validate CRLF
  framing after mailbox-name literals instead of blindly dropping two bytes.
- Add a bounded 5 ms backoff when IMAP and OAuth socket reads, writes, or TLS
  handshakes report `TimedOut`/`WouldBlock`; retain absolute deadlines and
  cancellation checks, and cover immediate `WouldBlock` with a regression test.
- Record the current-main GitHub Actions 10k-message / 1 GiB aggregate-payload
  generic Dovecot run with exact commit, engine and plan digests, resource
  samples, proof digests, and explicit metadata-only evidence limits.
- Reject incomplete or malformed sequence-number FETCH inventory pages,
  including duplicate/missing sequence numbers and UIDs inconsistent with
  sequence order; require UID order to remain monotonic across pages.
- Record a fresh 1M-per-endpoint maximum-envelope durable verification repeat
  for mismatch-heavy and duplicate-ID fixtures, including stage I/O, journal,
  flags, and RSS; retain explicit synthetic-only capacity limitations.
- Record a fresh 100k-per-endpoint durable verification benchmark for typical,
  mismatch-heavy, and duplicate-ID synthetic fixtures, including host and
  stage/journal/RSS observations while keeping provider and capacity claims
  explicitly unqualified.
- Reconcile release-readiness stage-disk guidance with the retained 1M-per-side
  typical, mismatch-heavy, and duplicate-ID measurements; report observed
  workload ranges and the correct rollback-journal denominator instead of a
  fixed per-message multiplier. Add a documentation regression check.
- Bump the pre-migration risk report to format v3, identify supplied mailbox
  facts as source-endpoint inventory, and estimate the shared body-hash byte
  budget using both endpoints under an explicit equal-size assumption. Keep
  destination size and actual fetched bytes labeled unknown; warn at the
  correct half-budget source threshold and fail visibly on projection overflow.
- Bump the pre-migration risk JSON contract to v2 and replace the ambiguous
  `estimated_readiness: "ready"` result with `scale_assessment: "scale_clear"`.
  A warning-free scale-only assessment does not establish migration readiness;
  authentication, TLS, destination capacity, verification eligibility, policy,
  and provider qualification remain unevaluated independent gates.
- Add a credential-free integration check against packaged imapsync 2.314 that
  forces an IMAP disconnect and verifies source and destination OAuth refresh
  callbacks run before XOAUTH2 reauthentication and the engine rereads updated
  token files. Keep real-provider expiry and refresh-token rotation unqualified.
- Correct the live-provider smoke claim: it exchanges OAuth refresh configuration
  before transfer but uses static access-token files, so it does not qualify
  imapsync reconnect refresh or token rotation during a long migration. Keep
  that runtime scenario explicitly open in the provider and recovery test plans.
- Keep automatic OAuth refresh fail-closed on Windows until native imapsync
  command-shell behavior is qualified, and add regression coverage for the
  refusal and POSIX quoting of paths containing spaces, Unicode, apostrophes,
  and shell metacharacters.
- Record the current packaged imapsync and native Dovecot smoke artifacts with
  proof digests and evidence levels; explicitly keep this generic-lab result
  outside provider and scale qualification.
- Assert that packaged imapsync and native Dovecot customer-proof artifacts
  identify the transfer engine actually exercised by the integration lab;
  exclude verification-only runs from that summary and include the engine name
  when a durable record contains only a bare version number.
- Fall back to the engine name in customer-proof summaries when the recorded
  engine version is missing or reported as `unknown`, preventing native-engine
  certificates from exporting an ambiguous top-level engine identity.
- Keep provider-pair evidence packs explicitly marked as qualification
  candidates until the signed release gate passes. Require consistent fixture,
  dataset, provider, and binary identities across phases; reject exceptions,
  aggregate-only evidence, insufficient test volumes, and mismatched endpoint
  totals. Preserve fixture and provider-version provenance in the exported
  pack, omit free-text issue details, and publish output privately without
  overwriting an existing artifact. Bind pack-wide claims and summaries in the
  evidence digest, not only the phase array. Reject unknown environment
  identities and bump the breaking candidate-pack contract to format v2.
- Add an offline qualification-pack verifier for duplicate-key rejection,
  canonical whole-pack digest checking, and strict schema validation; the
  verified status remains candidate, not release-qualified.
- Cancel capture, live-transfer, and doctor pipe drainers when bounded draining
  times out. Revocation releases secret references and readiness polling lets
  reader threads exit even when detached descendants retain inherited pipes;
  late output is discarded. The live-transfer and capture regressions verify
  each test descendant remains alive in a separate session, avoiding a
  process-group cleanup race that could mask the pipe-drainer behavior. Add
  readiness-polling, cancellation, pipe-error, and execution-profile decode
  regressions to cover both reader outcomes and security profiles.
- Version the support bundle as format 2 and replace exported project,
  mailbox-job, and run identifiers with per-bundle-salted, type-separated
  SHA-256 pseudonyms, preserving in-artifact joins without cross-export linkage.
- Serialize process-start admission with the OS spawn so workers delayed after
  taking a launch token cannot later burst together when a provider cooldown
  releases. Revalidate the executable before entering the bounded spawn
  critical section; keep cooldown-release spacing covered by the scheduler
  regression test.
- Refresh the generated capability-review date after rechecking the current
  scale evidence, security findings, and release gates.
- Record the current-main packaged-imapsync 100k-message qualification run,
  its proof and resource measurements, and its unsigned generic-fixture limits.
- Record the current-main 10k-message/1-GiB payload run separately from body
  hashing, including its proof and sampled resource measurements.
- Further restrict the Linux supervisor service from changing system clock,
  kernel logs, hostname, or namespaces, and limit it to the native syscall ABI.
  Keep these restrictions covered by the service-unit contract test and
  systemd's offline security analysis.
- Raise the bounded cross-platform release-build job timeout from 20 to 30
  minutes after the clean x86_64 Linux target compile exceeded the prior CI
  limit; keep the existing per-job timeout rather than disabling the guard.
- Make the provider cooldown reset test use an injected monotonic timestamp
  instead of subtracting elapsed time from `Instant::now()`. Windows can start
  with a monotonic clock origin less than the test's simulated quiet period,
  causing an underflow panic unrelated to the limiter behavior.
- After the packaged imapsync lab showed that a parent-death SIGTERM could let
  the engine finish an in-flight large APPEND after its controller was killed,
  use SIGKILL for Linux crash-only parent-death containment and reject the
  pre-exec parent-death race. Operator cancellation remains graceful. A
  subprocess regression covers an engine that ignores SIGTERM; the post-fix
  packaged Dovecot 2.3.19.1 / imapsync 2.314 CI drill stopped the engine after
  observing 3,145,263 of 67,109,132 bytes in flight, then resumed to exactly
  one verified message. This qualifies the fixture only, not provider or
  host-power-loss behavior.
- Ship the canonical systemd supervisor unit in Debian/RPM documentation and
  harden it with an unprivileged identity, empty capability sets, restricted
  networking, and read-only system paths. CI runs an offline systemd security
  analysis on the canonical unit.
- Add a real-engine 64 MiB APPEND interruption case. The lab waits until the
  destination Maildir contains a partial message before killing the controller,
  then resumes and independently verifies exactly one message. CI observed
  2,481,711 of 67,109,132 bytes before the kill with imapsync 2.314 and Dovecot
  2.3.19.1; the configured byte-rate target is average run pacing, not socket
  shaping.
- Record the real-engine interruption/restore qualification accurately: the
  default imapsync plan resumes a partially transferred fixture from a restored
  ledger and verifies the exact message count; host power-loss and provider
  qualification remain open.
- Show an explicit overall evidence status beside the verification tier and
  message outcome in the verification list and detail view. Partial flag or
  keyword coverage is labeled “Verified with exceptions” rather than appearing
  as an unqualified exact result.
- Remove the unmaintained `ttf-parser` dependency from the Linux Wayland
  decoration path. Keep Adwaita client-side decorations and dynamic Wayland
  loading, while omitting decoration title text; retain the app's X11,
  Wayland, accessibility, and rendering features.
- Apply the IMAP 32-bit UID-domain check to metadata, FLAGS, and body FETCH
  parsing as well as UID enumeration, rejecting zero and out-of-range values
  before they enter verification staging.
- Exercise cross-process ledger-lock recovery by forcibly terminating the
  lock-owning subprocess and verifying another process can acquire ownership.
- Make single-mailbox completion banners state the recorded verification tier:
  metadata-only runs explicitly say message bodies were not compared, bounded
  body-fingerprint runs disclaim full byte-for-byte proof, and aggregate/engine
  evidence disclaims independent per-message verification. A missing method on
  a `verified` result now produces a warning instead of an unqualified success.
- Version pre-migration risk JSON and mark its score as scale-only/advisory;
  enumerate authentication, TLS, provider, capacity, verification, disk,
  maintenance-window, and policy gates that this score does not evaluate.
- Remove unused `MPL-2.0` and `OpenSSL` entries from the dependency-license
  allowlist so future crates require an explicit license review.
- Retain durable verification staging when the evidence commit is rejected
  and the controller falls back to an operator-attention run; cleanup now
  occurs only after the evidence-bearing terminal transaction succeeds.
- Bound `doctor` engine-version output draining after the child exits; a
  detached descendant that inherits stdout/stderr can no longer hang
  diagnostics indefinitely, and incomplete capture is reported as blocked.
- Prune staged rows for folders absent from a fresh endpoint LIST, then
  atomically invalidate each live folder's messages, fingerprints, cursors,
  and PERMANENTFLAGS before refetch. This prevents stale-folder evidence
  without duplicating a mailbox-wide deletion journal on every retry.
- Benchmark durable verification through the same SQLite-staged mismatch path
  used by live runs; the prior harness materialized mismatch details in memory
  and could hit its separate 64 MiB ephemeral-report limit on adversarial data.
- Record a fresh durable verification-scale matrix through 1,000,000 messages
  per endpoint at the synthetic SQLite reconciliation layer, with host profile,
  peak RSS, stage/journal footprint, throughput, and explicit qualification
  limits.
- Reconfirm the 1M-message typical durable-stage benchmark on commit
  `927f04b1cf4baa1519d8dab3a4430ad3bcfa4c3b` with SQLite staging under the
  workstation's ext4/NVMe filesystem: 21 MiB peak RSS and 41,978 reconciled
  messages/s. This is not end-to-end or provider qualification.
- Measure the 1M-per-endpoint duplicate-ID fixture at current commit
  `6f714fb07ea2bb708cc61b69882aef1e2a8ae22a`: 37,558 reconciled messages/s,
  21 MiB peak RSS, and a 1.15 GB peak stage directory on the shared
  ext4/NVMe workstation. Correct the fixture description to match its actual
  nine/ten-message groups and 1% hot ID; this remains synthetic SQLite-only
  evidence, not provider or end-to-end qualification.
- Document service-identity keyring validation and credential rotation,
  revocation, export, crash-residue, and policy-administration procedures;
  clarify that systemd hardening does not isolate the engine from the worker UID.
- Allow `=` in typed folder-mapping destinations; retain the engine-required
  restriction on source names and explain it precisely.
- Version post-migration report artifacts as v2 and encode an unrecorded
  `total_skipped` count as `null` with explicit `unavailable` status; manually
  supplied counts remain numeric and marked available. Use the same v2
  format marker on both durable and explicitly supplied report exports.
- Compare duplicate-message FLAGS with streaming frequency maps, retaining one
  key per distinct canonical flag set instead of one entry per message.
- Enforce fresh folder rescans after verification restarts: live folders are
  atomically invalidated even when UIDVALIDITY, UIDNEXT, and EXISTS are
  unchanged, while rows for deleted/renamed folders are pruned against fresh
  LIST inventory. Body fingerprints are never reused and durable cursors are
  recovery metadata, not reusable verification evidence.
- Keep synthetic header fingerprints out of exact RFC Message-ID reconciliation. Persist RFC-ID and synthetic-candidate counts separately in evidence history and signed/report exports.
- Exclude synthetic header identities from FLAGS and keyword fidelity coverage; only real RFC Message-ID groups establish comparable flag pairs.
- Refresh automatic OAuth credentials on native verifier reconnects and before standalone re-verification, not only before live transfer and imapsync reconnects.
- Upgrade ledger schema to v31 with nonnegative identity-provenance counters; prior ledgers migrate with zero-valued legacy counters.
- Use a private local HMAC key for new OAuth rotation-recovery fingerprints while retaining one-time compatibility with existing version-1 SHA-256 recovery records.
- Document the non-secret input and filename-only purpose of the stable OAuth keyring-ID digest flagged by password-hashing analysis.
- Version customer proof exports as v2 to identify the expanded evidence fields, including identity provenance counts.
- Add a real-engine interruption drill to the IMAP lab: the controller is
  SIGKILLed mid-transfer, the engine must stop with it, and `recover` plus a
  fresh live pass must end with exactly the source messages verified. With
  expert `--useuid --usecache`, the drill shows the in-flight message is
  appended twice and verification flags it; the plan options now explain
  this trade-off. The default-plan recovery is repeated from a restored ledger.

- Run project report, project JSON, customer-proof, and health exports on a
  background worker with its own read-only ledger connection; a fleet-scale
  export no longer freezes the window, and a second export waits for the
  first.

- Stream complete proof exports from a pinned ledger snapshot instead of
  loading every mailbox and run: the 100,000-run export ceiling is gone, and
  a 100,000-mailbox, 300,000-run customer proof (341 MiB) exports in 36 s
  with 15 MiB of memory growth.
- Verify and sign proofs by streaming canonicalization with a private spill
  file; the proof cap rises from 32 MiB (about 14,000 mailboxes) to 1 GiB, and
  a 341 MiB proof verifies with no measurable memory growth. Signed payloads
  above 32 MiB use `Ed25519-SHA256`; smaller ones keep pure `Ed25519`, and the
  qualification verifier script accepts both. Signed output is compact
  canonical JSON.
- Fix per-mailbox verification reports for any project with more than 20
  runs: the evidence run was looked up among only the 20 most recent runs,
  so older mailboxes reported "The evidence refers to a run that is no
  longer available". The report now reads just that mailbox and run.
- Move credential-directory owner locks beside the run directory instead of
  inside it; Windows treats byte-range locks as mandatory, so a locked file
  inside blocked listing, renaming, and removing the directory.

- Protect live credential directories across processes: every `run-*`
  secret directory now holds an owner lock for its lifetime, and startup or
  `recover` cleanup skips directories another live MailSwiftSync process
  still owns. Previously a second instance on a different ledger deleted the
  passfiles and OAuth refresh files of running migrations, breaking their
  mid-run token refresh.
- Serialize in-process OAuth launch refreshes with the same per-credential
  cross-process lock the imapsync reconnect callback uses, and replay a
  callback's durable rotation recovery before exchanging; concurrent batch
  workers sharing a rotating refresh credential no longer spend the same
  token or overwrite each other's rotation.
- Drop a redundant second URI-userinfo pass from diagnostic line redaction.
- Fix Windows-only clippy findings in the OAuth refresh guard, lock
  contention check, and credential tests.
- Add deterministic IMAP session-recovery fault tests proving a fatal connection
  error reconnects and reauthenticates before retrying the failed folder,
  refreshes server capabilities, fails closed on reconnect authentication errors,
  and stops after three consecutive reconnect attempts.
- Harden opt-in diagnostic redaction for hyphenated and camel-case credential
  field names, underscore-prefixed OAuth fields, and quoted JSON-style keys;
  quoted values with escaped quotes are redacted as a whole rather than leaving
  a suffix in the transcript.
- Handle Windows byte-range lock contention as normal lock contention, retry
  transient sharing violations during atomic replacement, and close staged
  artifact handles before replacement. This restores exclusive-ledger and
  rotating OAuth refresh behavior on Windows.
- Make the cross-platform native-test log assertion match the actual workflow
  step structure, so Windows runs validate diagnostics instead of failing on
  an unrelated YAML line-order assumption.
- Serialize live OAuth refresh callbacks across concurrent mailbox processes
  that share a rotating keyring credential; each callback reloads the newest
  refresh configuration while holding the per-credential lock, and retries
  transient keyring writes before failing a rotated-token refresh closed.
  A protected, hashed-ID recovery file preserves a rotated refresh config if
  all keyring writes fail and is reconciled before the next token exchange.
- Explicitly close and remove private migration-assurance SQLite staging data
  before publishing a comparison report; setup and cleanup failures are now
  returned with the comparison error instead of being silently left to drop.
- Check credential-file and directory cleanup before reporting transfer completion;
  failed removals remain guarded for a best-effort retry and are included in run
  failure details without misclassifying local permission errors as provider
  authentication failures. Crash-recovery cleanup now reports inaccessible or
  retained secret directories instead of silently ignoring them; cleanup fault
  injection is deterministic across Windows and Unix test runners. Startup
  explains when cleanup is deliberately skipped because process ownership is
  unavailable, with regression coverage for the non-durable startup path. Failed
  command preparation also checks credential cleanup for both migration engines.
- Preserve complete restore-rollback diagnostics: failures returning the prior
  ledger or SQLite sidecars are no longer silently discarded, and all rollback
  steps are attempted before reporting the restore failure.
- Surface atomic-export temporary-file cleanup failures alongside the original
  write error, so retained private artifacts are visible for operator cleanup.
- Report cleanup failures for rejected or interrupted pinned-engine downloads,
  keeping an unverified partial artifact from being silently abandoned.
- Use native path display in cleanup diagnostics and retain failed native-test
  logs from every CI platform, making platform-specific regressions actionable.
- Make the bounded pipe-reader regression deterministic under loaded macOS
  runners by synchronizing reader state instead of asserting an 80 ms wall time.
- Surface failures to remove the private unsigned certificate intermediate;
  certificate export no longer reports complete cleanup when that file remains.
- Reject reverification difference-export flags that omit their required path,
  so the command cannot silently skip a requested CSV export.
- Fail closed during schema migration when the ledger file cannot be inspected
  for the required pre-migration backup, rather than treating the error as an
  empty database.
- Clarify that verification restart cursors record folder context but do not
  reuse staged message rows; mutable flags require a full folder rescan.
- Detect flag-only changes during a scan with CONDSTORE/HIGHESTMODSEQ when the
  server advertises it; use bounded, indexed staged-UID FLAGS refetches for
  unsupported mailboxes. Changed folders are retried, and the remaining
  non-atomic cross-server snapshot limitation is documented.
- Redact embedded URI userinfo in opt-in diagnostic transcripts while
  preserving endpoint hosts and paths for troubleshooting.
- Size the Mailboxes state column for the widest translated status badge so
  long states no longer push the column past the panel edge at 1280 px.
- Refresh the README and wiki screenshots in the Retro 80s Neon theme with
  the current typography, and drop the duplicate README-only image copies.

- Anchor `reverify` to the latest completed transfer rather than a later
  dry-run preflight, which also finishes as `completed` but copied nothing; a
  mailbox with only preflight history is refused.
- Read only the newest transfer-progress checkpoint per attempt in report
  exports instead of parsing every checkpoint, and match attempts exactly so
  attempt 1 never reports attempt 10's progress.
- Keep the Markdown project report on the bounded recent-run window it
  displays; only machine-readable exports load the complete run manifest.
- Document that the 32 MiB proof envelope holds roughly 14,000 single-run
  mailboxes in a customer proof.

- Document the Windows automatic-OAuth-refresh fail-closed boundary: refresh
  command execution remains Unix-qualified until native imapsync callback
  semantics and command-shell quoting are tested.
- Fix the headless Docker build by copying the tracked bundled font assets
  required by the shared UI code at compile time; the packaged IMAP
  qualification image now contains the same build inputs as local releases.
- Pseudonymize customer-controlled project identifiers in opt-in diagnostic
  filenames and headers; raw project names remain out of support transcripts.
- Resolve the stale Linux packaging roadmap wording so the shipped RPM package
  is distinguished from still-pending RPM repository metadata.
- Harden the documented systemd deployment by explicitly setting owner-only
  modes for its state and runtime directories.
- Fix the packaged IMAP lab harness to inspect durable state after documented
  headless live exit statuses 3, 4, or 5 instead of aborting under `set -e`.
- Make imapsync transfer hostname verification explicit for both implicit IMAPS
  and STARTTLS endpoints; the qualified engine's STARTTLS defaults did not set
  `SSL_verifycn_scheme` even though its IMAPS defaults did.
- Refresh the README workflow previews with Retro 80s Neon theme screenshots.
- Add bounded serial `reverify-batch` orchestration with strict manifest
  validation, per-mailbox evidence results, and a nonzero exit for any failed
  mailbox; the batch now retains the exclusive ledger lock across the entire
  manifest.
- Route batch child diagnostics through the existing private-metadata redaction
  boundary before including bounded errors in machine-readable output.
- Keep `supervise` alive across transient SQLite lock/busy errors during its
  read-only polling phase while continuing to fail fast on corruption and
  permission failures.
- Add regression coverage proving certificate pins fail closed for both
  transfer engines until connection-level enforcement is qualified.
- Include overall assurance status, message coverage, and flag-coverage counters
  in standalone `reverify` JSON so partial fidelity cannot be mistaken for an
  unqualified verification result by automation.
- Version the standalone `reverify` JSON envelope with a stable format name and
  format version, aligning it with the documented automation contract.
- Centralize standalone reverify result construction and add a regression for
  partial flag coverage, keeping the machine-readable assurance status aligned
  with the evidence model.
- Prevent exact metadata/body evidence from being certified without complete
  FLAGS and keyword coverage; missing flag coverage now remains an explicit
  assurance exception instead of being mislabeled as a flag change.
- Apply the same missing-flag-coverage guard to normal transfer evidence
  commits, while retaining partial and provider-exception coverage as
  reviewable `verification_difference` evidence.
- Centralize exact-result consistency validation across transfer and standalone
  reverification ledger commits to prevent future assurance-policy drift.
- Keep the exact-evidence invariant in one shared evidence predicate, reducing
  duplicated ledger validation while preserving reviewable partial coverage.
- Fix verification-scale benchmark result matching so keyed output fields are
  parsed independently of benchmark dimension order; successful scoped runs
  no longer fail as missing results.
- Reset IMAP verifier reconnect budgets after a folder completes, so isolated
  session drops across a large folder inventory do not exhaust recovery for
  later folders while repeated failures on one path still fail closed.
- Require configured engine identity fingerprints to refer to executable files,
  keeping preflight identity and process-launch qualification consistent.
- Surface documented metadata and bounded body-proof envelope warnings in the
  pre-migration risk report, before a transfer can consume its maintenance
  window without eligible independent evidence.
- Harden opt-in diagnostic transcript redaction for secret-shaped key/value
  fields and bearer-style authorization values that are not in the controller's
  known-secret set.
- Apply exact-evidence consistency checks to standalone re-verification
  history, preventing contradictory message counters from being persisted as
  an exact audit result.
- Preserve the verifier's authoritative/non-authoritative evidence flag in
  standalone re-verification history; audit records can no longer upgrade
  metadata observations into authoritative evidence.
- Make customer and operator JSON exports distinguish verification tier,
  outcome label, evidence scope, and overall assurance status instead of
  placing the same outcome text in multiple level fields.
- Reject symlinked customer-proof files in the provider qualification gate, so
  accepted evidence bundles remain self-contained and reproducible.
- Batch transfer-pass, folder, and progress reads during complete report
  exports in bounded run-ID chunks, avoiding a query-per-run history export
  while preserving the complete manifest and report snapshot semantics.
- Make standalone re-verification hold the exclusive ledger lock, enforce the
  current organization policy, and anchor new evidence to a completed
  imapsync/Dovecot transfer baseline. Re-verification now reports SQLite
  history failures instead of treating them as missing history.
- Prevent verification-only runs from starting against an actively migrating
  job's destination, and make cutover lookup failures fail closed rather than
  being interpreted as an absent cutover workflow.
- Add a regression proving that two verification-only runs cannot overlap on
  case-variant identities for the same destination mailbox.
- Add coverage proving a verification-only run also cannot overlap an active
  transfer in another project when both resolve to the same destination.
- Correct production-status wording for protected mailbox metadata and content
  fingerprints so the privacy boundary is not overstated.
- Sanitize secret-shaped fields in operator-visible engine events and process
  failure diagnostics, including values not present in the known credential set.
- Fix the packaged IMAP integration harness to preserve the MailSwiftSync
  command's exit status through its background `awk` logging pipeline; a
  timeout, failed migration, or unavailable status side-channel can no longer
  be masked by a successful log drainer.
- Add a Linux fault-injection regression for a detached descendant that clears
  parent-death containment and inherits the engine's output pipes; capture now
  demonstrably returns within the bounded reader-drain deadline.
- Persist the completed transfer run as re-verification provenance and report
  a distinct `ledger_persistence_failed` diagnostic when a failed scan cannot
  be finalized durably.
- Bound streamed proof generation to the same 32 MiB envelope enforced by
  proof verification and signing. Oversized customer/operator artifacts now
  fail closed instead of being emitted in a form the product cannot verify.
- Make machine-readable customer and operator exports use a complete durable
  run manifest instead of the bounded recent-run view. Export snapshots now
  load every run up to a 100,000-run safety ceiling and fail closed above it;
  interactive/controller snapshots remain bounded to the newest 20 runs.
- Finalize a verification-only run as `verification_failed` when its terminal
  evidence transaction cannot be committed, while retaining the durable stage
  for retry; failed reverify persistence can no longer leave a permanently
  running run that blocks the next attempt.
- Professional typography and visual polish for the desktop interface: IBM
  Plex Sans and Plex Mono are bundled (unmodified, SIL OFL; license shipped as
  `OFL-IBM-Plex.txt` in archives and packages) with real medium and semibold
  weights for emphasis, headings, and titles; a 14 px body type scale; overline
  table headers and softened column dividers; status glyphs drawn in one symbol
  family; the workflow's recommended step as a filled primary action; export
  actions in a wrapping row; and frameless disclosure rows on modern themes.
  The release bundle now also includes every document the bundled docs link.
- Stream the customer proof and operator project report instead of building
  them in memory. Each mailbox and run entry is generated and serialized one
  at a time in two passes (digest, then file), producing byte-for-byte the
  same canonical digest as before, so existing proofs and verifiers are
  unaffected. A release benchmark over 100,000 mailboxes (177 MiB proof)
  measured export peak memory growth of 1,574 MiB before and 251 MiB after;
  the remainder is the ledger snapshot the entries are derived from.
- Run `reverify` through its own durable verification stage, so verification-
  only reconciliation streams mismatches to disk like live runs (no in-memory
  mismatch cap), resumes its fetch after a failure, and never touches a live
  run's retained stage. The integration lab now exercises `reverify`.
- Stream verification-difference CSV exports page by page into the private,
  atomically replaced output file instead of building them in memory, and
  export the complete inventory: the GUI's 250,000-row and the CLI's
  1,000,000-row export caps are removed (external audit finding 8).
- Add durable operational counters to `status --summary` and `fleet-status`:
  per-project transfer attempts, retries, attempt outcomes by failure class
  (including provider throttling), and current verification outcomes. The
  fields are additive within automation contract v1 (external audit finding 9).
- Show cutover readiness in the live batch confirmation: outstanding queue
  rows that need attention, verification differences, or a delta pass, and an
  explicit statement that destination capacity is not checked per mailbox in
  batch mode (external audit finding 9). An unreadable queue is reported as
  unknown, never as zero outstanding work.
- Stream verification mismatches instead of holding them in memory. Durable
  live runs now write every mismatch row to the verification stage and the
  terminal commit streams them into the ledger in the same transaction, so a
  mapping error that mismatches most of a 500k-message mailbox records every
  finding instead of failing at the 64 MiB in-memory detail budget. A stage
  that changed after reconciliation is rejected. Ephemeral and body-hash runs
  keep the in-memory budget (external audit findings 6 and 8).
- Fix the integration lab's live verification, which the ledger had rejected
  since 2026-10-08 with "an exact verification label contradicts the evidence
  counters". Two duplicate messages identical on Message-ID, folder,
  INTERNALDATE, and size matched as a multiset, but flag verification skipped
  them, so partial flag coverage turned an exact message label into a stored
  contradiction. Identical duplicate groups are now compared as multisets of
  flag sets (a lost or altered flag is still detected), the store's exact-label
  invariant checks the message counters it describes while still rejecting
  real flag mismatches, and exact messages with partial or excepted flag
  coverage can be accepted as reviewed exceptions.
- Remove temporary credential directories on every command-preparation error
  through an ownership guard, instead of per-exit cleanup that a later error
  could bypass (external audit finding). A regression test injects a failure
  after the credential files exist for both engines.
- Add an integrity-defect acceptance test: missing, unexpected, duplicated,
  re-dated, resized, moved, Message-ID-stripped, and lost-duplicate messages
  are detected by metadata reconciliation, and identical metadata with a
  different body is detected by bounded body fingerprints.
- Fail closed instead of hanging when the ledger rejects a terminal result.
  Store invariant rejections were reported as an unexplained "Query is not
  read-only" error and retried on every poll, so a headless live run that hit
  one waited until its external timeout (the cause of the integration lab's
  20-minute timeouts since 2026-10-08). Rejections now name the violated
  invariant, and a deterministic rejection records the run as failed and the
  mailbox for operator review with that reason; transient storage failures
  are still retried.
- Re-hash an engine executable whose modification time is within the
  filesystem's timestamp granularity. The digest cache keyed on path, size, and
  modification time could return a stale identity for a same-size rewrite in
  the same timestamp tick (seen on Windows CI). The final pre-launch check was
  already uncached, so a replaced binary never ran unnoticed.
- Keep 100k-row queues responsive after admission. Presented queue states
  probed `runs` for every row, so with one child run per mailbox each state
  change cost about 290 ms for queue counts and 410 ms for an all-selected
  summary, freezing the first Mailboxes frame for about 760 ms. The queue-facts migration
  adds trigger-maintained run, queued-run, and acceptance counts to the queue
  facts, plus a covering index; counts, filters, selection summaries, and
  visible pages now read that projection (16 ms each, 49 ms first frame).
  Execution paths still read `mailbox_jobs`. Run writes pay about 22 µs each
  for the trigger. The UI scale benchmarks now include a child run per mailbox
  and a sustained 1,000-transitions-per-minute load on a fully selected queue,
  which the old run-free benchmark had hidden.
- Publish a versioned automation contract: `status`, `status --summary`, and
  `fleet-status` JSON now carry `format` and `format_version` (v1), separate
  from the SQLite `schema_version`, which changes with every storage migration
  and was never a safe compatibility signal. Golden field fixtures under
  `tests/fixtures/automation/` and a CI test reject removed, renamed, retyped,
  or undeclared fields; `docs/automation-contract.md` documents the policy.
- Add a Bahasa Indonesia desktop language pack (`locales/id.toml`), selectable
  in Settings → Appearance → Language and marked partial until a native
  reviewer signs it off. The localization check now enforces key, placeholder,
  and source-copy parity for every translated catalog.
- Production-readiness stopping point (2026-10-09): durable verification
  exception acceptance is now required before completion, cutover, queue,
  headless-success, report, and operator-status paths can present a run as
  verified. The release documentation and checks now also align on Debian and
  RPM packaging. The audit record retains the remaining technical-preview
  boundaries: no strict transfer-level certificate pinning for imapsync,
  no signed real-provider qualification evidence or complete fault-injection
  campaign, bounded verification envelopes without 100GB+ end-to-end proof,
  no independent per-message transfer manifest, and no centralized MSP
  control plane with RBAC, tenant isolation, and worker fencing.
- Reject IMAP UIDs outside the protocol's nonzero 32-bit domain, make UID
  range compression overflow-safe, and label exact metadata-only evidence as
  limited confidence because message bodies were not compared.
- Reject certificate-pinned transfer plans until the qualified external engine
  can enforce strict leaf pins on every connection and reconnect. Rust probe
  pin checks remain admission evidence, not transfer-level enforcement.
- Refresh OAuth credentials through qualified imapsync reconnects and again
  before independent verification, atomically rotate token files, and persist
  rotated refresh tokens back to the configured keyring or fail closed.
- Replace sparse-mailbox UIDNEXT-window enumeration with EXISTS-bounded
  sequence-number FETCH pages and snapshot consistency checks; ensure the
  enumeration command uses sequence numbers rather than historical UIDs.
- Reject throttle divisors that would multiply a configured aggregate
  message/byte budget, require complete non-excepted flag coverage for exact
  verification, and bound subprocess output-drainer completion after exit.
- Add a verification workspace drill-down with durable mismatch details,
  mailbox evidence, staged SQL reads, and customer-safe/operator evidence
  views. Restore the CI verification gates and cover the new schema and
  evidence paths with regression tests.
- Harden fail-closed behavior around preview validation races, bounded body
  hash limits, unknown persisted mismatch types, and unavailable batch
  confirmation state. These paths now stop safely with actionable operator
  feedback instead of panicking or starting an unconfirmed migration.
- Localize the batch-confirmation failure fallback in the English and German
  catalogs.
- Page verification differences with an index seek on the run instead of
  walking every run's mismatches, keep folder digests when a legacy mismatch
  table is rebuilt, and let a restarted reconciliation discard every
  intermediate table an aborted pass left behind. Exported difference CSVs
  neutralize server-chosen folder names that would otherwise be evaluated as
  spreadsheet formulas.
- Distinguish verification that stopped at a safety limit from a failed
  transfer and from a verifier error (ledger version 28). Every verifier
  bound (folder inventory, response size, fetched state, message count,
  body-hash count and size, reconciliation state, deadline) tags its error
  with a stable `[verification_limit=<code>]`. A completed transfer whose
  verification hits one is recorded as the new `verification_limit_exceeded`
  attention reason, with the limit and operator guidance in the run detail and
  a structured `verification_limit` object in the operator report. It is
  never recorded as verified. Single-mailbox runs now also record why
  verification produced no evidence; previously the run detail was empty and
  the reason fell back to `unknown`.
- Test the production DNS resolver path. The bounded helper-process parent
  code is now shared with tests, which drive it with stub helpers for argument
  handling, bad output, helper crashes, deadline, cancellation, the
  16-lookup bound, slot release, and (on Linux) cleanup after a controller
  crash. A new packaged-binary test runs `--internal-dns-resolve` for
  literal, named, unresolvable, malformed, and parallel lookups.
- Verify message flags and custom keywords independently (ledger version
  27). Live imapsync verification now parses the FLAGS it already fetched
  (the parser was previously test-only), stages them, and runs a separate
  fidelity pass that compares `\Seen`, `\Answered`, `\Flagged`, `\Draft`,
  `\Deleted`, and keywords for every message pair whose identity is
  unambiguous. Flags are compared case-insensitively and `\Recent` is
  ignored. A message whose flags the destination folder's PERMANENTFLAGS say
  it cannot store is recorded as a provider exception rather than a failure;
  any other difference yields the new `flags_changed` outcome, keeps the
  mailbox out of `verified`, and is reported in the GUI, operator report,
  customer proof, and evidence digest. Duplicates and probable pairings are
  left uncompared, so the compared count is reported as flag coverage.
- Reports and the GUI now state the verification tier achieved and its
  coverage: how many source messages were individually checked and how many
  had their flags compared. Evidence recorded before flag verification reads
  as "flags not verified", never as clean.
- Add migration waves (ledger version 26): named, ordered, approvable subsets of a
  project's mailboxes with a planned start, maintenance window, and
  concurrency ceiling. Status, progress, evidence coverage, and cutover
  readiness are derived from member mailboxes. Live admission refuses members
  of an unapproved wave (dry preflights stay allowed); a selected wave runs
  with its concurrency (never above the plan's) and launches only inside its
  window. Waves are created from a Mailboxes selection, approved and selected
  from Overview, and listed or approved headlessly with `mailswiftsync wave`.
- Add a migration command center to Overview for mailbox queues: operator
  buckets (completed, migrating, paused for retry, needs attention, waiting)
  that open exactly the rows they count, transferred totals, average speed,
  projected finish, and per-provider health that marks Google Workspace,
  Microsoft 365, or an individual self-hosted server as throttled while its
  launches are paused. Count `completed` and `retrying` rows, which previously
  fell out of every queue-health bucket.
- Structure Recovery as why it stopped, what MailSwiftSync already tried
  (including each mailbox's run and transfer-attempt history), and what to do,
  with a one-click retry that re-proves the group through a fresh dry
  preflight, or runs the final catch-up for delta-required mailboxes behind the
  normal live confirmation. The command center's attention bucket opens it.
- The mailbox state filter accepts the command-center groups and names any
  active filter, instead of showing "All states" for filters it did not list.
- Add a manually dispatched **Live provider smoke** workflow that runs the
  provider dry pilot, interruption/recovery, and live pilot against operator
  Google Workspace and Microsoft 365 test tenants in the packaged runtime image,
  and two CLI commands it relies on: `oauth-export-refresh-config` (export a
  keyring-stored refresh configuration to an owner-only file for automation)
  and `oauth-access-token` (exchange that file for an access token through the
  product refresh client, never printing either token).
- Add `scripts/atspi-accessibility-audit.py`, which audits the running app's
  Linux AT-SPI tree (what Orca reads) for unnamed interactive controls, and
  record a pass across every workspace view and dialog (388 controls, none
  unnamed) in the accessibility evidence.
- Present the Overview lifecycle in operator language (Add accounts → Test &
  review risks → Run pilot → Migrate → Catch up → Final sync → Verify results →
  Finish & export proof) instead of internal phase names, and replace "Begin in
  Discovery" guidance with plain wording, in English and German.
- Split `cli.rs`, `runner.rs`, `core/message_verification.rs`, and
  `migration_plan.rs` into responsibility-focused submodules without behavior
  changes; critical-module coverage floors now cover each split module's
  submodules.
- Deduplicate persisted batch plans by SHA-256 of their content rather than by
  allocation pointer, so equal plans persist once however callers allocate
  them.
- Add `validate_internal_invariants` (integrity check plus search-index,
  queue-projection, cross-project reference, and webhook-lease invariants),
  refuse to restore a ledger that violates it, and test it after injected
  failures at each write boundary of import, phase changes, outbox events,
  cutover planning, and webhook outcomes.
- Return a "verification stage is closed" error, instead of panicking, when a
  finished verification stage is used, so a lifecycle mistake fails the
  verification rather than the controller.
- Back `SecretString` with one shared zeroizing allocation: cloning a form,
  batch job, or worker input no longer creates another memory location holding
  the credential, and editing a shared value copies it first.
- Add `scripts/benchmark-verification-scale.sh`, gating metadata-only
  verification at 10k–1M messages per side on peak controller RSS (64 MiB) and
  reconcile throughput, and reporting stage size, peak rollback-journal size,
  staging rate, and CPU time; run it in the release workflow. Measured peak
  controller RSS is 15–28 MiB across that range.
- Record per-process peak RSS and cgroup anonymous versus page-cache memory in
  scale runs (`process-memory-peaks.txt`) so container memory is attributed to
  the controller, imapsync, and Dovecot separately.
- Identify Gmail and Microsoft 365 endpoints only by whole-label DNS matches
  against their documented IMAP domains (with the port stripped) instead of
  substring matching, so look-alike hosts such as `notgmail.example.com`,
  `outlook-proxy.example.net`, or `dovecot.example.com` no longer inherit
  provider-specific failure classification, rate domains, or remediation.
  Dovecot is server software, not a provider; self-hosted Dovecot endpoints are
  `generic`, and an organization-policy `[providers.dovecot]` key is refused
  with a hint to use `[providers.generic]`.
- Fix the Fedora RPM lifecycle check: install documentation despite the image's
  `nodocs` default and assert removal by file rather than bash's hashed path.
- Build an RPM with the same payload as the Debian package for x86_64 and
  aarch64 releases, verify its contents and weak engine dependencies, sign its
  checksum with the release key, and install/run/remove it on Fedora in CI.
- Record the first completed hosted scale qualification runs (100k small
  messages and 10k messages / 1 GiB of bodies, both exact metadata matches
  with peak-memory and wall-clock summaries), and export a second customer
  proof after the harness's incremental pass so retained evidence covers the
  post-delta state.
- Fix the scale qualification workflow's false hour-long stall: the host
  completion watcher ran as the runner user and could not see the marker inside
  the container-private evidence directory, so a harness that finished in about
  80 seconds was reported as a 3800-second watchdog timeout. The watcher now
  runs with access to that directory and fails fast when it cannot read it, and
  the Docker memory summary ignores terminal redraw sequences and unavailable
  samples from streaming `docker stats`.
- Restore the CI build: the OAuth redirect fuzz target still matched the
  removed `RedirectOutcome::Rejected` variant, which failed fuzz compilation
  and skipped the CI test step. `make check` now compiles fuzz targets and runs
  the scale-harness and proof-signature script tests that CI runs.
- Count an expired webhook delivery lease as a failed attempt, so a payload
  that repeatedly crashes its worker dead-letters instead of being reclaimed
  indefinitely.
- Fix a Windows-only unused import in the engine installer tests.
- Send queued lifecycle webhooks with their own outbox event ID, event type,
  and idempotency key instead of a status-snapshot body hash; scope snapshot
  event IDs to the endpoint and project so re-pointing an unchanged snapshot at
  a new endpoint is a new delivery rather than a refused collision. Keep
  file-sourced webhook signing secrets in their zeroizing container.
- Record a `phase_changed` audit event for every cutover approval and advance,
  so `migration.cutover_ready` webhooks are actually published, and refuse to
  advance a cutover on a project that is already Complete.
- Refuse unknown organization-policy keys so a misspelled safety setting fails
  closed instead of falling back to the permissive default.
- Treat rows in a filtered, paged mailbox view as hidden for batch safety
  confirmation, since no membership index is kept for that view.
- Remove the stale version-19 webhook trigger that every open recreated and
  immediately replaced, and drop duplicate fingerprint-coverage scans.

- Add a project-scoped SQLite FTS5 trigram index for mailbox substring search;
  use literal-safe indexed queries for terms of three or more characters and
  preserve the existing fallback for short/control-character queries. Schema
  v25 rebuilds the index transactionally and maintains it with queue-fact
  triggers.
- Reject explicit folder-mapping rules whose destination targets collide after
  case folding, preventing an unannounced merge when provider case semantics
  differ; document that provider-specific normalization remains unqualified.
- Reuse one Rustls webhook HTTP client and connection pool across queued
  deliveries in watch mode instead of rebuilding the transport for each event.
- Page filtered mailbox row IDs directly from SQLite during ordinary browsing;
  explicit selection and transient-state overlays retain exact materialized
  membership. Add SQL match counts and paging-transition regression coverage.
- Add a manually selectable 10k-message / 1 GiB RFC822 body-payload scenario to
  the packaged IMAP scale workflow, with bounded deterministic fixture
  generation and validation; hosted performance/recovery qualification remains
  pending.
- Add an evidence-based Migration confidence panel with typed readiness states,
  transparent unknowns for unqualified providers or unobserved quota, and
  direct routes to plan, mailbox, and verification remediation.
- Bound batch scheduler dispatch and worker-report channels by execution
  concurrency, encoding the one-outstanding-task/report-per-worker limit in
  channel capacity.
- Wrap saved profiles in a named, explicitly versioned TOML envelope; continue
  reading legacy bare-profile files through a designated compatibility path and
  reject unknown formats or future versions instead of default-deserializing.
- Shell-quote the displayed manual Debian engine-install command and clarify
  that a failed package-manager run may have changed system state; review
  apt/dpkg output before retrying.
- Surface durable queue summary/filter read failures instead of converting them
  to zero counts or non-matches; clear stale selection projections and show an
  explicit retryable unavailable state in Mailboxes and Overview.
- Require exact, bidirectional mailbox-key coverage for forensic content
  fingerprints; reject equal-count orphan substitutions and never skip a
  missing fingerprint while comparing a reconciled message pair.
- Keep the OAuth loopback listener alive after malformed or wrong-state
  callbacks; only a state-verified provider denial terminates authorization.
  Added an end-to-end forged-then-legitimate callback regression test.
- Scope webhook lifecycle-event binding and delivery claims to the selected
  project; add atomic leased claims with crash recovery and race-safe retry
  accounting, plus schema-v24 migration and isolation/concurrency tests.
- Make the disposable 100k-message scale qualification finish from the harness
  completion marker and delegate container disposal to the ephemeral hosted runner.
- Add regression coverage for completed harnesses whose Dovecot children keep
  the disposable container from exiting naturally.
- Generate OAuth test credentials at runtime to avoid fixed cryptographic
  values in the test source and binary.
- Update the scale-workflow guard test to cover completion-marker teardown and
  separate harness status from delegated fixture teardown.
- Test that the scale completion waiter reports a timeout and leaves a watchdog
  marker when the harness never signals completion.
- Remove post-harness Docker kill/wait/log/remove calls from scale qualification;
  completed fixtures are disposed with the ephemeral runner to avoid a wedged daemon.

All notable changes to MailSwiftSync are documented here. Detailed pre-alpha
development history is preserved in repository history and is not shipped in
operator distribution archives.

## [Unreleased]

- Added a project-scoped SQLite FTS5 trigram index for mailbox substring search;
  terms of three or more characters use literal-safe indexed queries, while
  short/control-character queries retain the fallback. The schema migration rebuilds the
  index transactionally and maintains it with queue-fact triggers.
- Reject explicit folder targets that collide after case folding, avoiding
  unannounced mailbox merges; provider-specific normalization remains
  unqualified.
- Reuse the connection-pooled Rustls webhook client across event deliveries.
- Filtered, unselected mailbox browsing now obtains its count and virtual-row
  pages directly from SQLite; exact ID materialization remains for explicit
  selection and transient-state overlays.
- Added a reproducible 10k-message / 1 GiB RFC822 body-payload scale scenario
  alongside the 100k-small-message run. The bounded fixture generator and
  selectable workflow scenario are tested; no qualification claim is made
  until hosted evidence completes.
- Added a project-level Migration confidence panel that groups transfer,
  verification, provider qualification, and cutover findings without a numeric
  score; unknown evidence remains explicit and findings route to remediation.
- Bounded batch scheduler dispatch and worker-report channels by concurrency so
  unexpected producer/consumer imbalance cannot grow those queues without
  limit.
- Wrapped saved profiles in a named, explicitly versioned TOML envelope while
  preserving a legacy bare-profile migration path; unknown formats and future
  versions now fail explicitly.
- Shell-quote the displayed manual Debian engine-install command and clarify
  that a failed package-manager run may have changed system state; review
  apt/dpkg output before retrying.
- Made the queue read model propagate SQLite failures from health counts,
  transient row plans, and filtering. Failed filters clear the visible-row
  projection and expose an explicit retryable unavailable card; Overview and
  Mailboxes no longer render healthy-looking zeroed batch status on read error.
- Made body-hash verification fail closed on missing or orphaned source or
  destination fingerprint keys, both at the staged-data boundary and in the
  comparator itself. Clean metadata pairs can no longer silently bypass
  forensic content comparison when a fingerprint is absent.
- Hardened OAuth loopback availability: invalid and wrong-state callback
  requests receive HTTP 400 and are counted while the listener continues
  waiting; a matching-state provider rejection still terminates, and a valid
  later callback can complete the flow.
- Hardened the transactional webhook outbox for MSP isolation: project-scoped
  notifiers bind and claim only that project's lifecycle events, while unscoped
  notifiers retain all-project behavior. Delivery now atomically leases due
  rows, reclaims expired leases after crashes, and fences acknowledgements by
  lease owner; failed-attempt increments are one atomic update. The schema
  migration safely unbinds prior queued lifecycle events that may have been
  associated with the wrong project endpoint.
- Updated disposable 100k-mailbox harness teardown to delegate Dovecot process
  cleanup to container teardown, avoiding an unbounded wait after SIGKILL when
  fixture processes are stuck in filesystem I/O; evidence now marks completion
  of harness cleanup separately from container exit.
- Separated unattributed batch failures from generic cross-provider discovery:
  ambiguous diagnostics now use provider-neutral heuristics and universal IMAP
  codes, and cannot trigger a provider-specific rate-domain cooldown.
- Extended the critical-module CI coverage gate to collect nightly LLVM branch
  coverage and enforce explicit per-module branch floors alongside line floors;
  the LCOV artifact now supports review of both decision paths and line hits.
- Moved the binary's integration/unit test module out of `main.rs` into a
  dedicated test source file, keeping the production entrypoint compact without
  changing test visibility or behavior.
- Separated IMAP probe unit and restart/resume test harnesses from the
  production protocol implementation for more focused security review.
- Split durable SQLite schema layout, constraint, and cleanliness validation
  into a dedicated database schema module; open, backup, and migration code
  remain separate from the schema proof invariants.
- Added the extracted SQLite schema-proof module to the critical-module line
  coverage gate with an 85% floor (89.93% measured across the full local suite).
- Separated independent IMAP reconciliation and Dovecot verification/checkpoint
  validation from process launch/supervision in the runner.
- Adaptive provider scheduling now accepts explicit source and destination
  tenant scopes in the advanced plan and per-row batch imports. These scopes
  are persisted, fingerprinted, and included in run snapshots; blank values
  retain the mailbox-email-domain fallback. This prevents one provider tenant
  spanning multiple email domains from being split into independent limits.
  Tenant scope inputs are bounded before trimming, including whitespace-only
  values; snapshots predating the fields retain the documented domain fallback.
- Single-mailbox terminal failures now retain canonical source/destination
  provider identities through durable run context. Provider-specific error
  handling is used only when engine output unambiguously identifies the failed
  endpoint; ambiguous failures remain generic.
- Bounded disposable Dovecot shutdown in the 100k scale harness and added
  teardown phase markers. The latest hosted run completed initial and
  incremental transfer phases but hung before collecting resource metrics;
  qualification still requires a successful rerun.
- Added a workflow-level hard deadline around the scale wait helper, with a
  separately bounded container kill and retained watchdog-failure marker, so
  a helper regression cannot consume the full hosted job window. A fake-Docker
  test forces the outer deadline and verifies both container-stop fallback
  and marker retention.
- Scale-mode cleanup now leaves its 100k-file workspace inside the disposable
  container for host teardown rather than recursively deleting every message
  file after verification; a phase marker records the deliberate skip.
- The latest 100k scale run completed its incremental transfer, fixture scan,
  Dovecot shutdowns, and harness cleanup, but the Docker container remained
  alive until GitHub's 90-minute job timeout. Run the disposable lab with
  Docker's init shim so orphaned fixture children are reaped and container
  lifetime follows the harness process; qualification still requires a hosted
  rerun with retained resource metrics.
- Signed customer evidence now exposes the immutable migration-plan SHA-256
  and the execution-engine binary SHA-256 when resolved in the plan snapshot;
  the underlying plan and its endpoint or credential-reference details remain
  private.
- Capability reporting now marks provider qualification packs as partial rather
  than planned: schemas, strict evidence validation, and pack construction are
  implemented, but real automated provider-pair qualification evidence and
  bundled live packs remain outstanding.
- Release-readiness guidance now reflects the 90-minute 100k scale job limit,
  and does not claim a peak-memory result until an instrumented run completes
  with its resource summary retained.
- Recorded the latest instrumented 100k run accurately: its durable proof
  confirms exact metadata reconciliation of 100,016 messages, but the separate
  incremental pass hit the 90-minute job limit, so the run provides no
  incremental-recovery or peak-memory qualification. The run and proof artifact
  are linked from the release-readiness guidance.
- Bounded the scale lab's host-side container wait independently of individual
  command deadlines and added non-sensitive phase/timing markers plus process
  state snapshots to retained evidence, so stalled runs can be diagnosed
  without losing the full workflow window.
- Durable batch imports now serialize each shared immutable plan once and
  reference it from mailbox rows, avoiding 100,000 transient duplicate TOML
  profile allocations during large imports. Applying a shared keyring
  credential now changes only mailbox identity deltas, preserving normalized
  shared-plan storage and invalidating the affected preflight.
- The release-mode 100k-row desktop benchmark passed after the shared-plan
  changes: durable import persistence took 1.647 seconds, the full shell's
  first frame took 45 ms, and the 100k-row CSV parse grew RSS by 43 MiB. The
  Mailboxes first frame was 91 ms against its 100 ms budget.
- Failed local preflight checks now include a direct “Fix in Plan” action and
  explain that the unmet check may block readiness.
- Added stable English/German locale catalog entries for the preflight
  remediation action and blocker explanation, and reused the existing
  “Not applicable” translation in the overview table.
- Corrected the capability manifest's retry limitations: typed bounded retry
  and Microsoft server-suggested backoff are implemented; proactive provider
  quota policies, provider-specific default rate policies, and live
  qualification remain outstanding.
- Generated the capability manifest's review date from `capabilities.toml` and
  added a drift check so its release-review timestamp cannot silently become
  stale.
- Expanded preflight remediation guidance to state that the issue applies to
  the current project and that any plan edit requires a fresh preflight.
- Hardened the 100k scale host watchdog to stop the container independently of
  the `docker wait` client and bound a stuck wait client, preserving time for
  diagnostic and resource-summary artifact collection.
- Replaced repeated whole-Maildir scans in the 100k integration harness with a
  single streaming fixture-Message-ID pass; phase markers now isolate that
  post-migration assertion step.
- Recorded evidence from the latest instrumented 100k run: its 100,016-message
  metadata proof and incremental live pass completed, but repeated fixture
  scans kept the workflow from collecting its memory summary. A synthetic
  100,004-file scan now completes in 1.78 seconds; hosted end-to-end rerun is
  still required.
- Removed an obsolete release-readiness statement that contradicted the
  implemented durable webhook retry/dead-letter outbox; a documentation test
  now guards against reintroducing the stale limitation.
- Migration Assurance mismatch details now hash payloads left unmatched by the
  multiset reconciliation, rather than arbitrary representatives that may be
  identical on both sides. A streamed-SQLite/reference-comparator differential
  regression test covers duplicate, missing, extra, modified, and normalized
  path identities.
- imapsync command previews now return validation errors for malformed expert
  options, and invalid plans receive distinct secret-safe fingerprints rather
  than hashes of silently shortened argument lists.
- Proof verification now rejects a non-UTF-8 trusted-key argument instead of
  silently downgrading signer authentication to checksum-only validation.
- Production provider qualification now requires every customer proof to have
  a valid Ed25519 signature from the explicitly pinned qualification key;
  recomputable proof digests no longer suffice as release provenance.
- Added a scheduled/manual packaged-engine scale lab that transfers and
  verifies 100,000 RFC822 messages in one disposable generic-IMAP mailbox;
  its first successful run reconciled 100,016 messages (28.7 MB) with zero differences
  and took 14m10s for the live pass. Scale mode avoids per-message debug noise
  and has a bounded 40-minute command timeout; this is end-to-end scale
  evidence, not hosted-provider qualification. Future runs sample peak
  container memory once per second and retain a wall-clock summary.
- Fixed timeout supervision in packaged-engine smoke labs: GNU `timeout` now
  terminates the full process group, so a surviving engine child cannot keep
  the output pipe open and stall the harness until the CI job timeout. The
  first memory-sampling run exposed this issue and was cancelled before its
  transfer completed; it provides no new scale or memory measurement.
- Raised the 100k scale workflow ceiling to 90 minutes after the instrumented
  rerun confirmed exact reconciliation of 100,016 messages but reached the
  job limit during its subsequent incremental pass, before memory diagnostics
  could be summarized. That cancelled run also provides no memory measurement.
- Made smoke-command deadlines hard bounds with GNU `timeout --kill-after`.
  A second 90-minute scale run again verified the initial 100,016-message
  transfer exactly, but showed that an incremental engine command can ignore
  TERM past the nominal 40-minute timeout. That run was cancelled before
  resource diagnostics; it provides no memory measurement.
- Cutover commands reject non-UTF-8 control arguments rather than silently
  dropping an invalid maintenance-window or confirmation value.
- Production-readiness guidance now distinguishes implemented provider-scoped
  failure intelligence and exact folder mappings from still-pending live
  provider qualification and complete provider-specific capacity simulation.
- The certificate command now refuses output paths that resolve to the SQLite
  ledger or signing-key file, preventing accidental destruction of either.
- Successful signed migration-certificate exports now record a durable
  `migration.proof_ready` event bound to the certificate SHA-256; export reports
  an error if the artifact was written but that ledger event could not commit.
- Durable webhook delivery now distinguishes failed mailbox preflight events as
  `mailbox.preflight_failed` instead of grouping them with other mailbox
  failures.
- The provider smoke harness now applies the owner-only permission check to
  its separate recovery-destination secret as well as the source and live
  destination secrets.
- Corrected the Windows migration-audit staging test to verify directory
  canonicalization semantically instead of comparing equivalent Win32 paths
  with different extended/8.3 spellings.
- Added focused batch-worker tests for credential refresh no-op, retry state,
  task preparation, queued cancellation, and durable unschedulable settlement;
  attempt cancellation and preparation failure also prove no durable claim is
  made, bringing measured line coverage to 68.05% with a 65% CI floor.
- Added 90% critical-module coverage floors for adaptive rate-domain
  scheduling and provider response classification (95.16% and 95.68% currently
  measured, respectively).
- Batch confirmations now block with an explicit durable-read error if queue
  selection projection fails, instead of presenting a partial or empty scope.
- Live OAuth refresh failures now retain their source/destination side; transient
  provider failures are retried and fed into that endpoint's rate domains,
  while permanent refresh failures still settle the mailbox before launch.
- Added a 70% critical-path coverage floor for OAuth refresh handling; the
  current suite measures 74.49% line coverage there.
- Provider-specific batch error policy now requires explicit endpoint-side
  attribution; ambiguous engine diagnostics no longer inherit the source
  provider by default, while sided authentication probes retain their side.
- Updated the migration-snapshot fuzz target's private-directory shim to match
  the stager's canonical-directory validation after the macOS path fix.
- Added coverage-increasing migration-snapshot fuzz inputs to the regression
  corpus.
- Added a CI scheduler stress test for 1,024 synthetic jobs across 32 tenant
  domains at 16 workers; it verifies full settlement, tenant accounting, and
  parallel execution without claiming real-engine/provider load qualification.
- Paged row IDs for the default large-batch Mailboxes view directly from
  SQLite, replacing the eager 100k-row virtual-index materialization on first
  paint; filtered views and explicit select-visible actions retain their
  existing selection semantics.
- Raised 21 critical-module line-coverage floors to values supported by the
  current measured suite (76.87–98.17% observed); coverage artifacts are now
  uploaded only when LCOV generation produced a report, avoiding a secondary
  missing-artifact failure after cancellation.
- Added an MSP operations procedure for customer scoping, change approvals,
  migration-wave controls, incident recovery, and evidence retention, with the
  current lack of centralized fleet/tenant isolation stated explicitly.
- Corrected provider-status wording to distinguish the wired Gmail/Microsoft
  failure classifier from still-absent quota defaults and live qualification.
- Fixed macOS migration-audit staging database opens while retaining SQLite's
  no-follow protection: the private run directory is resolved and revalidated
  before the exclusive owner-only database file is created and opened.
- Reused the queue model's generation-cached health summary during selection-only
  refreshes instead of rescanning all durable rows for unchanged state counts.
- Reconciled production-status maturity wording with the wired provider
  classifier and current release blockers; the docs test now rejects the stale
  “dormant prototype” claim.
- Migration Assurance now marks every mailbox unresolved until verification is
  accepted, including completed transfers awaiting a delta, and no longer calls
  equal aggregate folder counts a reconciled inventory. Difference totals now
  saturate instead of wrapping on extreme durable counters.
- Updated the migration-snapshot fuzz harness for private disk-backed staging;
  the target previously failed to compile after the staging security change.
  Its input files now stay in a private temporary directory, and the
  coverage-increasing local inputs were added to its seed corpus.
- Extracted bounded IMAP LIST parsing, mailbox descriptor construction, and
  folder metadata handling into a dedicated folder-inventory module. Existing
  response-size, literal-size, mailbox-count, retained-inventory, timeout, and
  cancellation bounds remain enforced.
- Extracted adaptive UID FETCH page sizing, compact UID-set encoding, and exact
  requested-versus-parsed page coverage checks into a dedicated fetch-pages
  module; sparse UID ordering and fail-closed coverage tests remain in place.
- Fixed schema-version documentation drift and broadened the documentation gate
  to recognize parenthesized and labeled SQLite schema-version claims.
- Removed a stale schema-version reference from the database layout validator's
  safety documentation; the implementation remains tied to the current schema.
- Grouped SQLite ledger path identity, parent-directory validation, private
  no-follow creation, and sidecar cleanup into a focused database-files module.
- Changed file-based `migrateaudit` staging from SQLite's in-memory database to
  a private, auto-cleaned on-disk database; the JSON reader continues to parse
  and insert one record at a time under the existing input-size bounds.
- Expanded the CI line-coverage gate to include observed floors for batch
  scheduling/start, event ownership, queue handling, polling, recovery, and
  IMAP authentication (85% floor; 87.77% observed) and evidence projection
  (85% floor; 89.54% observed).
- Extracted bounded tagged-response parsing and budget-aware IMAP command I/O
  into a protocol module, preserving literal-safe completion detection.
- Isolated certificate-validated IMAP/STARTTLS setup, deadline-aware handshake
  handling, custom CA roots, and SHA-256 pin enforcement in the TLS module.
- Moved LOGIN/XOAUTH2 exchange and post-auth capability refresh into the IMAP
  authentication module while keeping the shared probe API unchanged; added
  transcript tests for credential quoting, OAuth framing, PREAUTH, and rejection.
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
- The batch process-start limiter takes its token at the engine-release
  boundary after durable registration, so delayed acknowledgements cannot
  release several gatekeepers into a provider-facing burst.
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
