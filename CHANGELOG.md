# Changelog

All notable changes to MailSwiftSync are documented here.

## [Unreleased]

### Changed

- Moved migration-plan readiness invalidation, assessment, and project creation
  into `ui/plan.rs`, further reducing plan orchestration in `main.rs`.
- Added the controller crash and engine-interruption recovery fixture to the
  tagged release workflow, so release publication executes every evidence gate
  claimed by the compatibility matrix.
- Moved live IMAP authentication probe setup into `ui/plan.rs`, completing
  extraction of plan readiness probe coordination from the application shell.
- Moved capability-probe validation and request construction into the plan UI
  module, reducing controller/UI coordination in `main.rs`.
- Extracted capability and live IMAP authentication probe workers into the
  controller layer, keeping network I/O and secret-bearing probe requests out
  of the egui composition root.
- Extended the packaged controller recovery lab to interrupt an active engine
  and verify durable `attention` classification, closing the release matrix's
  engine-interruption evidence gap.
- Prevented bulk-import row construction from copying base-form passwords
  into transient row forms before applying row credentials.
- Extracted the batch launch presentation bridge from `main.rs` into the UI
  batch module; durable admission and worker policy remain controller-owned.
- Moved batch queue indexing, summary caching, and keyring-application UI
  helpers alongside the batch launch bridge, further narrowing `main.rs`.
- Moved batch queue reset and cache-invalidation helpers into the batch UI
  module so queue lifecycle presentation state remains localized.
- Moved bulk import result application and worker dispatch into the batch UI
  module; imported rows use credential-free form defaults throughout dispatch.
- Moved batch filtering, selection, and bounded selection export beside the
  batch workspace state and launch actions.
- Extracted batch queue, worksheet, and live-migration confirmation dialogs
  into the batch UI module, keeping destructive prompts localized.
- Extracted advanced migration-plan controls into `ui/plan.rs`, separating
  engine tuning presentation from the application composition root.
- Extracted the batch queue presentation dialog into `ui/batch.rs`, leaving
  `main.rs` focused on shared workspace composition and lifecycle dispatch.
- Extracted the OS-keyring credential dialog into `ui/account.rs`, keeping
  credential presentation and storage controls out of `main.rs`.
- Extracted engine selection and local Dovecot execution settings into
  `ui/engine.rs`, keeping migration-engine presentation out of `main.rs`.
- Moved the redacted execution-plan preview into `ui/plan.rs` alongside the
  migration-plan controls.
- Extracted the activity workspace, bounded output viewer, and durable run
  history presentation into `ui/activity.rs`.
- Extracted the verification workspace and mailbox evidence review into
  `ui/verification.rs`, keeping report actions and exception review together.
- Extracted GUI report-export actions into `ui/reports.rs`, keeping customer
  proof, operator reports, health exports, and support bundles at the UI/report
  boundary while leaving artifact construction in `reports/`.
- Moved project-browser filtering and project selection into `ui/workspace.rs`,
  keeping durable project navigation with the cached workspace read model.
- Extracted the migration lifecycle stepper into `ui/overview.rs`, keeping the
  operator-facing phase workflow separate from application composition.
- Moved the cleartext source-transport warning into `ui/overview.rs` so the
  first-run safety guidance remains alongside the migration lifecycle view.
- Moved the Overview first-run summary, process-ownership review gate, and
  historical read-only banner into `ui/overview.rs`.
- Moved Overview preflight/readiness controls and completed-project reopen
  review into `ui/overview.rs`, keeping assessment actions with the workflow
  presentation.
- Moved the Settings navigation adapter into `ui/settings.rs`, keeping
  appearance/workspace navigation behavior with the settings presentation.
- Moved the stop/cancellation confirmation into `ui/activity.rs`, keeping
  active-run controls with the live output and run-history surface.
- Tightened the service-manager deployment checklist around credentialless
  queue restoration, keyring references, approved runtime paths, and operator
  recovery ownership.
- Moved the paged historical mailbox renderer into `ui/workspace.rs`, keeping
  read-only mailbox navigation on the cached durable read-model boundary.
- Added strict compatibility-matrix validation to the release workflow so
  unresolved evidence markers cannot pass a tagged release gate.
- Added regression coverage proving credential-free bulk form clones preserve
  plan defaults while clearing both password fields.
- Centralized conversion and persistence of pending structured execution
  events in the controller event module, removing duplicate SQLite tuple
  plumbing from `poll()` while preserving its durability retry ordering.
- Hardened native CI with a per-job timeout and uncaptured platform test
  output, preventing an indefinitely stuck runner and making cross-platform
  failures actionable from the workflow log.
- Added failure-only artifacts for Windows cross-build and native-test logs,
  making hosted MSVC/compiler and platform-test failures diagnosable without
  reproducing them on a non-Windows development host.
- Added matching timeouts to verification, cross-platform build, and container
  jobs so no CI path can remain indefinitely active without a conclusion.
- Added an explicit `status --summary` headless projection that uses exact
  SQLite mailbox state counts without materializing mailbox rows, while
  preserving the existing detailed status output for callers that need it.
- Removed per-row lowercase string allocation while calculating large batch
  queue summaries; imported display-case states are now classified directly
  through the shared allocation-free state parser.
- Moved bulk file and worksheet worker creation into `bulk_import`, leaving
  the egui shell responsible only for starting an import and applying its
  result while keeping parsing and background dispatch together.
- Moved batch event-channel creation, cancellation ownership, and worker
  startup behind the controller boundary so the egui shell retains only the
  handles needed to render and cancel an admitted batch.
- Bounded support-bundle mailbox data to a 1,000-row status sample while
  retaining exact mailbox totals and state counts, so incident exports remain
  usable for large projects without materializing the full queue in memory.
- Moved capability-probe results and stale-result matching into the preflight
  controller module, keeping asynchronous readiness identity policy out of the
  egui composition root and covered by controller-level regression tests.
- Centralized batch-project naming and endpoint metadata derivation in batch
  admission, so GUI and headless queue paths share one durable project identity
  contract.
- Consolidated batch queue selection, durable admission, project preparation,
  plan materialization, and active-run creation behind one controller launch
  API, reducing policy and persistence coordination in `start_bulk()`.
- Centralized active-run output ownership checks in the controller event
  contract, with coverage for foreign and post-terminal process lines before
  they can enter the operator journal.
- Updated the migration plan to present local Dovecot as a local destination,
  removing misleading destination IMAP port/TLS controls while retaining the
  symmetric remote-account form for imapsync migrations.
- Added direct controller coverage proving batch launch admission rejects an
  empty queue before creating durable project state.
- Hardened bounded output buffers so an individual oversized UTF-8 line is
  truncated at a character boundary and cannot exceed the configured aggregate
  byte budget.
- Exposed customer/project naming in the batch cockpit and made customized
  workspace names apply when queue rows still carry a generic default, keeping
  durable projects and customer evidence identifiable without editing a CSV.
- Corrected architecture and operations documentation to state that raw engine
  transcripts remain bounded process-local diagnostics and are not persisted in
  the SQLite evidence ledger.
- Locked the batch customer/project identity field while a batch is running so
  the displayed name cannot diverge from the already-admitted durable project.
- Clarified the README’s audit-storage wording: the durable ledger contains
  structured lifecycle/evidence events, while raw engine transcripts remain
  process-local diagnostics.
- Removed obsolete durable raw-output retention/pruning code and its misleading
  retention test; the SQLite event writers now have one explicit contract that
  rejects `run_output` transcripts.
- Extracted durable event-detail bounding into `storage.rs`, keeping SQLite
  record-size policy separate from the core state machine and covered by a
  storage-level UTF-8 regression test.
- Removed the obsolete event-kind parameter from durable detail bounding so
  every SQLite detail writer uses the same explicit storage policy.
- Extracted bounded output and Markdown-presentation helpers into `ui/output.rs`,
  reducing presentation utility code in the application composition root.
- Consolidated allocation-free case-insensitive filtering in the shared UI
  layer so project and mailbox search paths use one tested utility.
- Invalidated capability observations before each UI frame so edited plans
  cannot briefly display readiness results from an older endpoint or trust
  configuration.
- Added a first-run safe-start card that explains the Connect → Preflight →
  Pilot workflow and provides a direct mailbox-list import action in an empty
  workspace.
- Expanded the batch cockpit summary to distinguish failed, attention,
  delta-required, and verification-difference rows, and to show the active
  selection scope alongside the aggregate unresolved count.
- Removed duplicate CLI/report signing wrappers from the application shell so
  report signing and verification are called directly through `reports`.
- Added a 100,000-mailbox durable read-model regression test proving workspace
  pages remain bounded while aggregate state counts cover the full project.
- Moved workspace project-resolution policy into the UI read-model module with
  direct tests, reducing another pure selection rule in `main.rs`.
- Moved persisted batch-plan decoding into batch admission so corrupt or
  missing queue configuration has one controller-owned failure policy.
- Moved the durable run-snapshot schema and decoder into `migration_plan.rs`,
  so plan serialization and report reconstruction share one domain owner.
- Moved certificate-pin validation into the migration-plan domain so TLS
  policy validation is no longer implemented by the application shell.
- Corrected the architecture documentation to state that raw engine
  transcripts are process-local diagnostics, not durable ledger data.
- Surfaced cancelled rows separately in the batch cockpit so interrupted work
  is distinguishable from failed and verification-review rows.
- Moved run-snapshot decoder coverage into `migration_plan.rs`, keeping the
  durable plan contract and its corruption test together.
- Moved certificate-pin validation coverage into `migration_plan.rs`, keeping
  TLS input policy and its regression tests together.
- Moved worksheet-import state and workspace-view identity into their owning
  bulk-import and UI modules, reducing workflow declarations in `main.rs`.
- Moved process-registration timeout and output-observer types into
  `runner.rs`, keeping child-supervision policy with execution code.
- Replaced the poller’s positional pending-event tuples with named durable
  event records, making run/event ownership explicit during commit batching.
- Simplified pending durable-event construction so the poller no longer binds
  unused project metadata while creating run-scoped commits.
- Centralized ownership checks for asynchronous process lifecycle and engine
  metadata events, extending the tested controller event contract beyond output
  lines and reducing duplicated safety predicates in `poll()`.
- Extracted batch-child terminal-state policy into the controller module. Run
  status and mailbox verification-state mapping now have one tested contract
  instead of being embedded in the egui event loop, reducing GUI/headless
  divergence risk during batch orchestration changes.
- Batch admission, plan materialization, and worker launch now receive the
  explicit `BatchExecutionMode` enum rather than independent live booleans,
  making preflight/live intent visible and consistent across controller
  boundaries.
- Bulk imports now accept an optional `project_name` column. Customer/project
  identity is carried into the durable batch project while the existing `name`
  column remains the per-mailbox queue label, preventing imported migrations
  from collapsing into generic project names.
- Reframed the Settings dialog as workspace/operator settings and surfaced
  engine, credentials, advanced options, and readiness controls directly on
  the Migration plan, keeping migration configuration in the operator's main
  workflow instead of hiding it under Settings.
- Moved durable parent/child batch-run admission and active ownership-context
  construction into `controller::admit_batch_run`, reducing persistence and
  process-ownership assembly in the egui start path.
- Extracted typed batch-start safety decisions into the controller policy
  module. Stale durable state, profile/read-only/process-review blocks, live
  confirmation, and storage availability now share a tested ordering contract
  before the GUI opens dialogs or launches work.
- Moved single-run durable admission and active ownership-context construction
  into `controller::admit_single_run`, aligning single-mailbox startup with
  the batch controller boundary and reducing state-machine assembly in
  `main.rs`.
- Extracted the Settings dialog into `ui/settings.rs` with explicit navigation
  actions, keeping operator/workspace preferences separate from the main
  application shell and leaving migration configuration on the Migration plan.
- Extracted the single-run start safety gate into a typed controller decision,
  matching batch admission's tested fail-closed ordering before credentials,
  engine preparation, or durable run creation begins.
- Extracted batch-child terminal persistence into `controller::finish_batch_child`,
  keeping evidence, state, preflight-plan, and checkpoint commits together
  outside the large egui event reducer.
- Updated the batch cockpit's import guidance to expose `project_name` and
  distinguish durable customer-migration identity from per-mailbox `name`
  labels.
- Expanded the distribution installation guide with release-manifest, SBOM
  checksum, and optional GitHub build-provenance verification instructions.
- Moved the shared worker/event contract into `controller/events.rs`, removing
  execution protocol definitions from the egui composition root while keeping
  runners, batch workers, headless mode, and UI polling on one event type.
- Hardened batch completion state mapping so evidence can upgrade only
  successful/continuable child states; late evidence can no longer mask a
  failed or cancelled transfer as `verified`.
- Extracted typed batch-start safety decisions into the controller policy
  module. Stale durable state, profile/read-only/process-review blocks, live
  confirmation, and storage availability now share a tested ordering contract
  before the GUI opens dialogs or launches work.

- Extracted deterministic batch plan materialization into
  `controller/batch_admission.rs`: selected durable IDs, Dovecot checkpoints,
  plan fingerprints, run snapshots, and child plans are now produced by one
  controller operation before SQLite run admission.
- Extracted selected batch-row validation and preparation into
  `controller/batch_admission.rs`, centralizing live durable-state checks,
  credential fingerprints, form validation, transport acknowledgements, and
  duplicate-destination protection outside the egui dispatcher.
- Batch projects now persist the actual source and destination endpoints from
  the imported queue instead of generic `batch` metadata, making project
  switching and exported reports unambiguous for operators managing multiple
  migrations.
- Moved durable batch project reuse/creation policy into
  `controller/batch_admission.rs`, keeping exact queue matching and atomic
  project creation outside the egui dispatcher.
- Extracted the batch worker-pool coordinator from `main.rs` into
  `controller/batch_worker.rs`. GUI batch admission now hands an explicit,
  already-admitted work specification to the controller worker, keeping
  process launch, retry, cancellation, and batch event production outside the
  egui dispatcher.
- Added a persistent global-header `DURABLE VIEW STALE` indicator and an
  explicit application status when workspace refresh fails, so retained
  cached state cannot look current merely because the operator is on another
  page or scrolled past the warning.
- Made failed workspace read-model refreshes explicit: retained cached data
  is marked stale, the UI shows the last successful refresh age and error, and
  single/batch execution is blocked until durable state can be refreshed.
- Bound imapsync verification to the packaged `2.314` output profile when
  engine version metadata is available. Explicitly unknown versions now
  produce incomplete evidence instead of being treated as compatible; the
  existing strict grammar remains the fallback when version probing is
  unavailable.
- Tightened imapsync evidence parsing into an explicit summary contract:
  aggregate records must use the expected units, the completion marker must be
  exact, and the error summary must be exactly `Detected N errors`. Prose that
  merely resembles a summary now remains incomplete evidence. The operator
  guide documents this fail-closed compatibility boundary.
- Reworked authenticated IMAP LIST handling into a streaming inventory
  consumer. Readiness now retains only mailbox and SPECIAL-USE counts, while
  bounding individual records, literals, processing time, and mailbox count;
  large folder inventories no longer accumulate in a 1 MiB response string or
  fail solely because their legitimate aggregate size is larger.
- Moved OS-keyring credential loading out of the egui start path. Runs now
  show an explicit loading state while the keyring is queried in a worker;
  results are discarded if the operator edits the migration plan meanwhile.
- Moved best-effort engine `--version` probing off the controller/UI path.
  Single and batch workers now request version metadata asynchronously, with
  content-identity keyed caching and in-flight deduplication, so an unresponsive
  wrapper cannot stall a launch for its five-second probe timeout or spawn one
  probe per mailbox.
- Hardened Windows headless secret-file reads against reparse-point
  substitution by validating the already-open handle with
  `FILE_FLAG_OPEN_REPARSE_POINT`; validation and reading now use the same
  filesystem object on all supported platforms.
- Unix headless secret files must also have a single directory entry, blocking
  hard-linked operator inputs that could make the same secret inode reachable
  through an unrelated path.
- Legacy `.xls` validation now opens the file with calamine's BIFF parser after
  checking the OLE signature. Header-only or malformed legacy workbooks are
  rejected during validation with an actionable error instead of reaching the
  import worker and failing later.
- Added a cheap durable read-model revision check. The workspace now polls for
  committed event-sequence changes and skips rebuilding project, mailbox, run,
  and verification projections when the ledger is unchanged, while preserving
  refreshes for local invalidation and external controller activity.
- Added project-scoped revisions and conditional run-history loading so activity
  in another project, or an inactive Activity view, does not rebuild the
  selected project's mailbox and run projections.
- Restore now snapshots its source through SQLite's backup API, preserving
  committed WAL-visible state instead of copying only the main database file.
  The standalone snapshot is integrity-checked before installation.
- Restore now rejects live SQLite sources with `-wal` or `-shm` sidecars. This
  prevents a changing ledger from being silently restored at an ambiguous point
  in time; operators must first create a standalone snapshot with `backup`.
- Immutable run-plan snapshots now hash the canonical typed imapsync option
  sequence, keeping plan identity aligned with the exact arguments generated
  after validation (for example, `--timeout=30` and `--timeout 30`).
- Command preparation now rechecks the canonical extra-option validator at the
  engine boundary, preventing an invalid profile from reaching a prepared
  imapsync or Dovecot invocation even when called outside normal admission.
- Expanded the Dovecot pinning regression coverage to assert that both source
  and destination certificate pins are rejected rather than silently treated
  as enforced controls.
- Bound successful capability observations to the plan fingerprint that
  produced them. Editing an endpoint, account, transport, trust setting, or
  other fingerprinted plan input now clears both cached capabilities and the
  derived readiness assessment, including after an asynchronous probe has
  completed.
- Added explicit first-run provider endpoint presets for Generic IMAP, Google
  Workspace, Microsoft 365, Fastmail, and Zoho Mail. Presets provide
  connection hints and honest policy notes only; discovery and preflight remain
  authoritative and no provider provisioning or OAuth lifecycle is implied.
- Removed the unused locked dry-run flag from application execution state;
  batch mode remains owned by its explicit controller mode and single-run mode
  remains bound to the active run context.
- Removed the obsolete full-report field from the workspace read model so UI
  code cannot accidentally treat interactive state as a complete export
  snapshot.
- Replaced the workspace Verification refresh's full project report load with
  a paged compact mailbox/evidence projection. Interactive verification now
  reads at most 200 rows at a time, uses grouped durable counts for totals, and
  retains acceptance metadata for selected rows; full reports remain available
  through explicit export paths.
- Cached best-effort engine version probes by executable content identity for
  both single and batch starts. Repeated launches no longer pay the blocking
  probe timeout, while replacing an executable invalidates the cache key.
- Hardened ledger restore durability: the verified temporary database is
  flushed before installation and the destination directory is synchronized
  after the atomic rename, so a successful restore now reflects the on-disk
  commit boundary on Unix.
- Batch projects now preserve an explicit profile name or derive a bounded
  source-to-destination label instead of creating indistinguishable generic
  “Batch migration” entries. Durable batch restoration recognizes the batch
  marker independently of the display label.
- Made full evidence-report loading demand-driven in the workspace snapshot:
  overview and mailbox views use compact projections, while verification and
  activity load the detailed report only when opened. Explicit snapshot
  invalidation clears the detailed report so the next relevant view reloads a
  fresh durable model.
- Tightened imapsync evidence admission: an authoritative success now requires
  the engine's explicit `Detected 0 errors` summary in addition to aggregate
  counts and the success marker. Missing error summaries remain incomplete
  evidence instead of being interpreted as zero failures.
- Added aggregate mailbox state counts for overview health cards. Large
  durable queues no longer require loading every mailbox record just to render
  total, ready, running, verified, and attention counts.
- Added compact durable mailbox state counts and a 200-row historical mailbox
  page. Workspace refreshes no longer materialize every durable mailbox row
  for overview/history rendering, and large historical projects can be browsed
  with explicit paging controls.
- Exposed stale workspace snapshots to the operator: failed durable refreshes
  retain the last good projection for resilience but now show a warning with
  the age of that projection and the failed read boundary. The historical
  mailbox view also avoids cloning the complete durable mailbox collection.
- Canonicalized trusted extra imapsync options after validation, so execution
  now uses the exact allowlisted `--option` spelling and typed integer values
  that were validated. Malformed single/double-dash variants are rejected
  instead of being passed through literally.
- Hardened headless secret-file reads on Unix by validating the already-open
  descriptor, rejecting symlinks, requiring effective-user ownership, and
  setting close-on-exec. A single terminal CR/LF is trimmed for shell-friendly
  secret files without changing embedded credential whitespace.
- Fixed legacy `.xls` batch imports: BIFF/OLE workbooks now use a format-
  appropriate signature check instead of being sent through the `.xlsx` ZIP
  validator, while the existing input-size limit remains in force. Updated
  the import controls and guide to describe CSV/Excel support consistently.
- Added a regression test at the workbook-dispatch boundary to ensure legacy
  `.xls` inputs remain on the OLE validation path.
- Moved mailbox evidence values, evidence-scope semantics, and verification
  acceptance records into a dedicated durable-core module while preserving the
  existing `core::*` API.
- Moved report snapshot read-model types alongside the durable evidence model,
  keeping report assembly boundaries explicit without changing persistence.
- Extracted durable project, mailbox, run, and batch read-model types into
  `core::models`, preserving the existing public core API.
- Extracted IMAP capability and quota read-model parsing from the SQLite store
  into `core::capabilities`, keeping protocol policy independently testable.
- Moved run-completion status wording and severity policy into the shared UI
  status module so controller polling no longer owns presentation rules.
- Expanded the distribution installation guide with the attended-operation
  boundary and macOS fail-closed process-supervision limitation.
- Moved the terminal durability/lifecycle admission predicate into the
  controller policy module, keeping durable phase advancement out of the UI
  shell.
- Hardened Markdown operator reports by escaping durable identifiers, states,
  timestamps, and plan-fence delimiters before rendering untrusted values.
- Bound asynchronous IMAP capability observations to a request and plan
  fingerprint, discarding results after endpoint, trust, auth, or other plan
  edits instead of presenting stale readiness data.
- Made certificate pinning fail closed for Dovecot plans, where the native
  engine does not enforce application-level leaf pins, and clarified the UI
  to expose only the applicable source CA control.
- Made ledger restore reject SQLite WAL/SHM sidecars beside the source, which
  prevents copying a stale main database file from a non-standalone snapshot.
- Cached project-browser filter indices and render rows from the durable
  snapshot without cloning the full project list on every repaint, preserving
  searchable project selection for large operator portfolios.
- Moved project-browser matching into the workspace presentation module with
  focused name/endpoint search tests, reducing filtering policy in `main.rs`.
- Removed the remaining full project-list clone from the header switcher;
  project selection and full-index loading are now deferred until after the
  snapshot-backed menu renders.
- Moved batch throughput admission validation into `controller::batch_admission`
  so GUI and headless callers share the same concurrency safety policy.
- Removed stale single-mailbox dry-run mutations from Mailboxes-page batch
  actions; batch preflight/live/final-delta mode is now controlled only by
  the batch mode state.
- Extracted shell-word parsing, operator quoting, and trusted option removal
  into the shared `command` module, decoupling engine and plan construction
  from `main.rs` while retaining focused parser tests.
- Extracted private atomic artifact writing and platform directory syncing into
  `atomic_artifact`, so reports, headless exports, and plan persistence share
  one crash-safe permission boundary outside `main.rs`.
- Extracted durable `MailboxState` and `Phase` wire/state definitions into
  `core/state.rs`, preserving the `core` API while reducing the ledger module's
  state-machine concentration.
- Moved durable attention categories and their stable labels/actions into
  `core/state.rs`, preserving wire values while further reducing policy in the
  ledger implementation.
- Extracted durable transfer-engine selection and descriptions into
  `core/engine.rs`, preserving the `core::Engine` API and serialized values.
- Extracted batch row selection by durable state and operator scope into the
  batch controller, with focused coverage for selected IDs and retry policy.
- Moved elapsed-time presentation into `ui::status` with focused formatting tests, further reducing pure UI formatting logic in `main.rs`.
- Extracted plan assessment and capability/quota presentation policy into `controller::preflight`, reducing `App` responsibility while keeping the explicit network readiness gate separate.
- Moved the local plan-completeness calculation into `migration_plan`, reducing UI-controller responsibility while keeping network readiness checks in the explicit preflight path.
- Moved verification-report assembly into `reports::operator` and made it consume one durable project snapshot, preventing mixed-commit mailbox/evidence/run artifacts; obsolete single-query helpers are now test-only.
- Extracted operator JSON report construction into `reports::operator`, keeping the GUI wrapper focused on project selection and private atomic writing while preserving proof digests and complete run manifests.
- Extracted operator project-report assembly into `reports::operator`, leaving the GUI responsible only for project selection and private artifact writing while preserving the existing snapshot and evidence contract.
- Replaced per-frame batch state-count allocation with a typed, generation-cached queue summary, reducing large-batch UI repaint overhead while preserving immediate invalidation on queue or state changes.
- Updated the distributed installation and first-launch guides with platform-specific checksum verification, release-versus-source startup instructions, engine prerequisites, and the recommended first migration sequence.
- Consolidated the database-backed UI read model into an owned `WorkspaceSnapshot`, reducing `App` state surface and making snapshot-only rendering an explicit boundary.
- Added a structural compatibility-matrix release gate and run it in CI and tagged-release verification, requiring a reviewable data row with all provider, engine, execution, recovery, and evidence columns populated without overstating support.
- Moved single-mailbox durable identity admission into the controller admission module so GUI and headless execution share one project/mailbox identity check.
- Extracted the cached workspace read-model refresh into `ui::workspace`, keeping database-backed presentation loading behind one throttled UI boundary while preserving live action-time storage checks.

- Moved the pre-live dual IMAP authentication/quota gate into `imap_probe` so
  GUI, headless, and future controller callers share one readiness boundary.
- Extracted durable state-path resolution and verified SQLite ledger restore
  (including WAL/SHM sidecars, rollback artifacts, and permission checks) into
  `storage_paths` so recovery code is independent of the UI dispatcher.
- Moved the readable UI-scale cycle policy into the theme module alongside
  appearance preferences.
- Moved persisted appearance preferences into the UI theme module so
  application startup retains only workspace and execution state.
- Moved proof digests, canonical signed-payload handling, and hexadecimal
  artifact encoding into `reports::integrity`; plan fingerprint digesting now
  lives in `plan_identity` rather than the application composition root.
- Documented the extracted migration-plan, controller-policy, and bounded
  output boundaries in the architecture guide, and linked the compatibility
  matrix from the README with its release-gate meaning.
- Extracted bounded operator-journal and process-tail storage into the
  dedicated `output` module, keeping line and aggregate-byte limits together
  as one reusable memory-safety boundary.
- Moved failure classification, retry eligibility, retry backoff, and durable
  attention-detail formatting into `controller::failure` so controller policy
  is independent of the egui dispatcher.
- Extracted the migration profile, form validation, engine-plan construction,
  credential preparation, and immutable run-plan snapshot logic into the
  dedicated `migration_plan` module so GUI and headless callers share one
  tested plan boundary.
- Removed the direct unmaintained `rustls-pemfile` dependency and now load
  additional CA bundles through Rustls's maintained PEM certificate API.
- Replaced the headless module’s wildcard crate import with explicit
  controller, state, recovery, and artifact dependencies to keep the
  GUI-independent path isolated as controller extraction continues.
- Corrected the security policy’s transport wording to document certificate
  validation on both IMAPS and STARTTLS readiness probes.
- Updated the compatibility matrix to reflect the current packaged
  MailSwiftSync integration lab and its exact tested scope, while keeping
  engine-interruption, storage-fault, and provider-specific coverage explicit.
- Added a service-manager deployment guide with a least-privilege systemd
  example and a Windows wrapper pattern, including the current unattended
  automation and credential-broker boundaries.
- Execution journal clipboard actions now default to a metadata-free support
  summary; copying raw redacted engine output is explicitly labeled as
  potentially containing mailbox metadata.
- Replaced report signing/verification and process-registration invariant
  panics with explicit error paths, keeping malformed artifacts and unusual
  controller queue states from aborting the application.
- Moved batch admission policy—destination identity checks, queue matching,
  keyring application, and sanitized selection export—into
  `controller::batch_admission` for reuse outside the egui shell.
- Moved CSV/XLS/XLSX validation, worksheet selection, credential-safe row
  conversion, and import limits into the dedicated `bulk_import` module;
  `App` now owns only import scheduling and queue presentation.
- Extracted executable, trust-file, and snapshot identity hashing into the
  dedicated `plan_identity` module, reducing plan-admission policy's coupling
  to the GUI monolith without changing the fingerprint format.
- Capability summaries now describe server capabilities as observed
  possibilities rather than implying that the selected transfer engine will
  execute each advertised strategy. Security documentation now matches the
  enforced remote-Dovecot policy.
- Headless single and batch execution now pass their explicit ledger path
  directly into application construction instead of mutating the process-wide
  `MAILSWIFTSYNC_STATE_PATH` environment variable.
- Kept customer-proof export completion-gated by default and added an explicit
  CLI `--allow-incomplete` opt-in for progress evidence only. Incomplete
  artifacts are labeled in JSON and documented as non-certificates.
- Clarified customer-proof semantics: exported artifacts identify their
  durable-completion claim as ledger evidence, not an independent completion
  certificate, and the verifier now distinguishes artifact integrity/signer
  validation from migration-completion certification.
- Gated customer-proof exports on durable project completion, verified mailbox
  states, present evidence, and absence of active runs. The same completion
  contract now protects both GUI and read-only CLI exports.
- Represented UI/headless status updates as typed `StatusMessage` objects with
  explicit `Info`, `Success`, `Warning`, or `Error` severity, keeping display
  wording structurally independent from semantic styling.
- Replaced English substring matching for UI status severity with an explicit
  typed severity carried alongside each status message, so wording changes
  cannot silently alter error, warning, success, or informational styling.
- Precomputed normalized searchable mailbox fields and cached filtered bulk
  row indices, recalculating them only when the queue, search query, state
  filter, or mailbox state changes instead of during every repaint.
- Reused the bulk mailbox filter's visible-row index buffer across egui
  repaints. Large queues retain table virtualization without allocating a
  fresh filtered index vector on every frame.
- Made project, mailbox, activity, and verification search matching
  allocation-free per row by using ASCII case-insensitive window matching
  instead of lowercasing every candidate during each egui frame.
- Preserved the selected project in the workspace index even when it falls
  outside the compact recent-project window. Added an explicit searchable
  “All projects…” action to the header switcher; it loads the complete durable
  project index into the Projects browser without making every normal frame
  query the full ledger.
- Fixed header label refresh after selecting a project outside the initial
  switcher view. Project selection now invalidates the throttled workspace
  snapshot immediately and falls back to the selected durable project record,
  so the header no longer misleadingly says “Select project”.
- Removed the project switcher's hidden eight-project limit. The header now
  keeps every project loaded in the cached workspace snapshot selectable;
  search and the dedicated Projects browser remain available for large
  ledgers.
- Added a controller crash/recovery integration lab that uses a deterministic
  blocking engine, verifies the running mailbox is durably recorded, simulates
  an ungraceful controller crash, and confirms `recover` clears ownership and
  classifies the interrupted job as operator attention. CI now runs this
  alongside the real-server product integration lab.
- Promoted the disposable IMAP lab into a product-level integration gate: it
  now starts self-signed STARTTLS Dovecot fixtures, writes an isolated profile
  and owner-only secret files, drives the packaged binary through headless
  preflight, live, incremental live, customer-proof export, and verification,
  and checks the resulting Maildir. The fixture selects Dovecot 2.3 or 2.4
  configuration syntax to match the runtime, uses bounded 180-second product
  invocations, and the Docker image now includes OpenSSL for deterministic test
  certificates. The lab now also asserts the durable `ready` and verified
  terminal states after each controller operation.
- Added paired `--source-secret-file` and `--destination-secret-file` options
  for single-mailbox headless runs. Secret files are size-limited, owner-only
  on Unix, and handed to the zeroizing credential path without entering the
  durable ledger; batch modes continue to require an already-admitted queue.
- Extracted single-run worker orchestration into
  `src/controller/orchestrator.rs`; GUI state now submits an owned execution
  specification while process cleanup, verification sequencing, panic
  containment, and result events are reusable by headless callers.
- Extracted the source/destination account editor and password-visibility
  policy into `src/ui/account.rs`, reducing UI responsibility in `App` while
  keeping secret edits behind the dedicated `SecretString` boundary.
- Kept password and OAuth-token copies in zeroizing containers throughout
  process redaction, Dovecot verification environments, and worker handoff;
  imapsync argument construction now always uses placeholders and never
  materializes real credentials in an ordinary `Vec<String>`.
- Kept the generated XOAUTH2 payload and keyring-loaded credential inside
  zeroizing ownership from acquisition through socket/process handoff.
- Replaced the worker-facing zeroizing alias with a dedicated `SecretString`
  type whose debug output is redacted and whose string access is explicit.
- Hardened signing-key loading on Windows and zeroized the in-memory PKCS#8
  bytes before the key leaves the signing operation.
- Hardened the Windows signer further by verifying the protected DACL contains
  only the file owner and LocalSystem; broad or inherited ACLs now fail closed.
- Corrected the Windows security API feature wiring used by the signing-key
  DACL verifier so the gated implementation is compiled with its required
  system-services bindings.
- Plan fingerprints and immutable run snapshots now bind the SHA-256 content
  identity of the selected engine executable, configured trust bundles, and
  Dovecot configuration, so replacing a file at the same path invalidates a
  prior preflight; SQLite admission coverage verifies the same-path replacement
  behavior at the durable boundary.
- Made IMAP tagged-status and untagged `LIST` parsing case-insensitive for
  protocol keywords, while retaining exact command-tag matching.
- Centralized IMAP atom tokenization and case-insensitive protocol matching for
  tagged responses, greetings, capabilities, `LIST`, and `QUOTA` parsing;
  capability detection no longer relies on substring matches.
- Corrected SPECIAL-USE inventory detection to parse only LIST attributes,
  recognize the complete standard attribute set, exclude non-standard `\Inbox`,
  and avoid mailbox-name false positives.
- IMAP readiness now requests RFC 6154 `RETURN (SPECIAL-USE)` LIST attributes
  when the authenticated server advertises the `SPECIAL-USE` capability.
- Dovecot checkpoint recognition now validates the documented decoded state
  format (legacy empty state or v1 header, mailbox-record sizing, and CRC32),
  rejecting arbitrary printable or merely Base64-shaped diagnostic lines;
  surrounding whitespace is rejected rather than normalized into a checkpoint.
- Customer and operator report exports now read project, mailbox, run, and
  evidence data from one SQLite snapshot, preventing mixed-commit artifacts
  during concurrent migration updates.
- Enforced `RunLine` ownership checks so delayed or foreign worker output is
  rejected instead of being appended to the active migration journal, with
  the ownership rule centralized alongside the other run-controller checks;
  lines arriving after a valid process-ended event are rejected as stale too.
- Completed the XOAUTH2 error handshake: IMAP error continuations are now
  acknowledged with the required empty SASL response before the tagged result
  is read, preventing failed OAuth probes from hanging or being misclassified.
- Fixed XOAUTH2 handling when Gmail reports an invalid or expired token in the
  initial SASL continuation; the exchange is now cancelled and reported as a
  clean authentication failure after consuming the tagged `NO`, instead of
  waiting for a timeout or leaving the AUTH exchange incomplete.
- Added a durable-state-driven overview workflow indicator for Connect,
  Assess, Preflight, Migrate, Verify, and Deliver, with explicit copy that
  the presentation step never bypasses execution gates.
- Moved batch retry scopes, mailbox-selection state sets, and live-confirmation
  summary data into `src/controller/batch.rs`, so retry eligibility and
  automation policy are shared controller rules rather than UI-local types.
- Extracted shared phase, mailbox-state, status-severity, and health-summary
  presentation helpers into `src/ui/status.rs`, reducing UI logic in the
  application entrypoint while keeping the semantic palette centralized.
- Extracted process execution, diagnostic capture, Dovecot verification, and
  engine-version probing into `src/runner.rs`, leaving the application layer
  responsible for durable event handling rather than child-process mechanics.
- Moved status, recovery, support-bundle, headless execution, batch
  supervision, and controller-wait logic into `src/headless.rs`; CLI and GUI
  now share the same headless service boundary.
- Moved CLI command parsing, help text, and headless command dispatch into
  `src/cli.rs`, leaving the desktop entrypoint as a thin wrapper shared with
  the automation surface.
- Extra imapsync options now use typed boolean/integer specifications with
  explicit numeric bounds, so malformed timeout and retry values are rejected
  before command construction.
- Extracted the IMAP readiness probe into `src/imap_probe.rs`, including
  endpoint handling, TLS/CA trust, certificate pins, capability discovery,
  and authenticated readiness checks.
- Batch execution now uses an explicit controller-owned `BatchExecutionMode`
  instead of a second boolean dry-run flag, keeping GUI and headless admission
  on one source of truth.
- Isolated XOAUTH2 payload construction and IMAP challenge handling in the
  dedicated OAuth module so provider authentication work can evolve outside
  the main application controller.
- Extracted IMAP endpoint, TLS, certificate-pin, capability, and authenticated
  readiness probing into `src/imap_probe.rs`, reducing the controller's network
  and protocol surface without changing GUI or headless admission behavior.
- Headless batch-live now requires every selected mailbox to be in an
  explicitly verified terminal state; an unchanged `ready` row or missing
  durable row cannot produce a false success result.
- Added per-endpoint OAuth 2.0 access-token authentication for imapsync via
  XOAUTH2. Tokens can be supplied for the current session or through the OS
  keyring and are delivered to imapsync through short-lived private files;
  they are excluded from previews, plan snapshots, SQLite, and reports.
- Output journals and process tails now maintain aggregate byte counts during
  append and eviction, avoiding a full-buffer scan for every diagnostic line.
- Moved remaining rendered project, mailbox, evidence, and batch-confirmation
  reads onto the cached workspace snapshot; durable reads now occur during
  refresh or explicit controller actions rather than egui rendering.
- Password reveal controls now disable and clear their transient visibility
  state centrally on every locked frame, including when the Plan view is not
  currently visible.
- Routed all operator-facing status, action, warning, and explanatory colors
  through the contrast-aware semantic theme palette; removed legacy low-contrast
  color constants and one-off status colors.
- XLSX imports now inspect ZIP entry metadata before workbook expansion and
  reject excessive archive entry counts or uncompressed size.
- Spreadsheet imports now show the workbook's worksheet names and require the
  operator to select which worksheet contains migration rows; they no longer
  silently import worksheet 1.
- Plain-source imapsync plans now explicitly disable both implicit SSL and
  opportunistic STARTTLS with `--nossl1 --notls1`.
- Corrected imapsync debug option validation: `debug`, `debugimap1`, and
  `debugimap2` are boolean switches and no longer consume or accept values.
- Removed execution-capable imapsync `pipemess` from the extra-option
  allowlist; it is now rejected as unsafe rather than presented as a tuning
  option.
- Engine output is now process-local presentation data only. Raw imapsync
  lines, including message subjects and metadata, are no longer written to
  SQLite; existing `run_output` events are purged during ledger opening while
  classified diagnostics and verification summaries remain durable.
- Durable ledgers now repair and refresh stored destination identities from
  their secret-free batch configuration, including already-open current-schema
  databases; empty legacy identities also use the full configured endpoint
  during live admission.
- Unified GUI and durable SQLite destination-account identity canonicalization
  through the shared endpoint module, including DNS trailing-dot handling,
  canonical IP literals, transport-derived ports, and mailbox case rules.
- Corrected the packaged Dovecot integration fixture to use the Dovecot 2.3
  configuration syntax provided by the pinned Debian Bookworm runtime, instead
  of silently testing it with Dovecot 2.4-only configuration keys.
- Fixed custom CA bundle encoding for imapsync. Verification and
  `SSL_ca_file` are now emitted as separate repeatable `--sslargsN` values,
  preserving paths containing spaces and matching imapsync's parser.
- Fixed imapsync STARTTLS command generation to use its supported
  `--sslargs1`/`--sslargs2` parameters; nonexistent `--tlsargs1` and
  `--tlsargs2` options are no longer emitted.
- Fixed batch admission paths to switch both the form and batch controller to
  the requested mode. Headless `batch-live` and GUI selected-row actions can
  no longer rerun dry preflight and report ready mailboxes as successfully
  migrated.
- Extracted shared theme and contrast primitives into `src/ui/`, reducing the
  presentation surface owned directly by `main.rs` while preserving the
  existing egui workflow.
- Overview project and mailbox summaries now consume the cached workspace
  snapshot instead of querying SQLite during repaint.

### Added

- Added a guided first-run empty state with a clear **Connect → Assess →
  Prove** path and a direct action to configure the first mailbox.
- Release archives now include installation guidance, the first-launch guide,
  README, and license alongside the binary so operators know to install the
  selected migration engine before starting MailSwiftSync.
- Moved execution ownership and live-authentication proof types into
  `src/controller/`, reducing the controller domain embedded directly in the
  egui application without changing admission or recovery behavior.
- Batch migration now opens with an at-a-glance cockpit summary for total,
  ready, running, verified, and unresolved rows, with explicit failed,
  attention, unresolved, and clear-selection actions. These actions only
  select durable rows; existing preflight and live-confirmation gates remain
  mandatory.
- Batch operators can export the selected failed, attention, unresolved, or
  manually chosen rows as a secret-free JSON handoff for ticketing and review;
  the export is not an executable migration plan.

- Added separate dark and light semantic UI palettes with high-contrast
  primary, secondary, informational, success, warning, danger, link, border,
  and selection colors. Contrast regression tests enforce a 4.5:1 target for
  normal operator-facing text.
- Execution journals now provide an explicit Copy output action and use
  non-wrapping monospace rows with horizontal scrolling, keeping terminal
  output predictable during long migrations.
- Projects, activity history, and verification views now render from a
  throttled cached workspace snapshot rather than querying SQLite during every
  egui repaint. Selected-mailbox retry guidance also consumes that snapshot.

- Failed or cancelled mailbox runs can no longer commit a newly captured
  Dovecot checkpoint; resume state advances only with a successful terminal
  engine result.
- Batch `JobState` notifications are now presentation-only for terminal
  outcomes; mailbox state, child-run status, evidence, checkpoints, and dry
  preflight data commit together in `JobFinished`, preserving the durability
  boundary if terminal SQLite persistence fails.
- Immutable run snapshots now include a digest of the Dovecot checkpoint used
  for launch, preserving resume-point provenance without storing the raw state
  token in reports.
- Batch snapshots now include checkpoint identity only for live Dovecot rows;
  dry validation and imapsync runs cannot imply that a Dovecot resume point was
  used.
- Checkpoint-aware batch admission now reads resume state only for live Dovecot
  children, avoiding unnecessary state reads and misleading provenance for
  other execution modes.
- Durable checkpoint completion APIs now reject empty, whitespace-containing,
  control-character, or oversized Dovecot state values at the storage boundary;
  malformed resume state cannot be written by non-UI callers.
- A changed preflight digest now invalidates the previously committed Dovecot
  checkpoint, preventing stateful resume from crossing endpoint or policy drift;
  repeated preflight of the same plan preserves the resume point.
- Transient batch failures now release the child run and mailbox claim before
  retrying, so retries reacquire durable ownership instead of being rejected as
  duplicate active executions.
- Dovecot checkpoint candidates from a failed transient attempt are discarded
  before retry, preventing an eventual success from committing stale resume
  state when the retry emits no replacement.
- Live Dovecot syncs now pass the mailbox's last committed `-s` state string
  and atomically commit a newly emitted checkpoint with the terminal child
  result; dry preflight remains non-stateful and failed runs preserve the
  prior checkpoint.
- Dovecot preflight and verification capture now retains only a bounded
  diagnostic prefix while continuing to drain the child pipe, preventing
  unusually verbose commands from consuming unbounded memory.
- Lifecycle phase advancement now requires a clean durability cycle in
  addition to a successful external engine result; persistence uncertainty
  leaves the project in place for recovery review.
- Configuration and credential dialogs are now read-only while an execution
  is active, so the visible plan cannot be changed through a modal while live
  output is being produced.
- Batch retry, authentication, claim, and failure diagnostics now use the
  mailbox child run identity instead of the parent batch run; parent-level
  output is reserved for wave summaries.
- Process supervision now treats a migration leader exiting with surviving
  process-group descendants as an abnormal result, terminates the remaining
  group, and refuses to report clean success; a Unix regression test covers
  the wrapper/helper case.
- Documented Dovecot checkpoint behavior: live stateful `doveadm -s` resume is
  supported with atomic child-result/checkpoint commits, while UIDVALIDITY-aware
  verification evidence remains a separate future capability.
- Terminal SQLite commit failures now leave the durable run available for
  recovery instead of emitting a misleading completion event or advancing the
  project phase; the production runbook documents the operator response.
- Multi-run diagnostic event persistence now has regression coverage proving a
  partially invalid batch rolls back all earlier inserts atomically.
- The legacy generic batch-claim helper is test-only; production code must use
  the mailbox-specific child-run claim path.
- imapsync verification parsing now accepts only explicit `Detected N errors`
  summary lines, preventing unrelated diagnostic wording from changing failure
  evidence.
- Workspace views and exports now use an explicit selected-project identity,
  separate from active execution ownership, so a completed batch cannot shadow
  a subsequently selected single-mailbox project.
- Lifecycle navigation now resolves its phase from the selected workspace
  project, keeping the stepper consistent with views and exports after a batch.
- Plan readiness now reports explicit passed-check counts instead of a
  pseudo-precise percentage that could be confused with verification
  confidence.
- Durable batch destination checks now use the secret-free destination
  endpoint, port, and mailbox identity when imported configuration provides
  it, while legacy rows retain conservative mailbox-only fallback behavior.
- Schema version advanced to 2 for the durable destination-identity column;
  older alpha databases are migrated and future versions remain rejected.
- Runs now retain the project phase observed at admission as provenance
  metadata, and the schema advances to version 3 with legacy rows marked
  `legacy_unknown` rather than being assigned a fabricated stage.
- Single-mailbox verification reports now include the captured phase-at-start
  provenance alongside the immutable run identity.

- Run-scoped diagnostic and verification events are now writable only while the run is active; terminal audit records cannot be appended later.
- Preflight persistence now reports missing mailbox IDs as errors instead of silently succeeding with no durable update.
- Evidence insertion is now test-only; production callers can only create durable evidence through run-owned terminal completion transactions.
- Schema migration now clears legacy non-digest preflight plans instead of retaining potentially sensitive generated arguments; affected mailboxes must be preflighted again before live admission.
- Recorded-orphan termination now fails closed on macOS and Windows, where the Linux PID/start-time/process-group identity proof is unavailable; only the Linux path may signal a recorded orphan.
- Generic mailbox state updates can no longer create an unowned `running` mailbox; execution ownership must be established through the atomic single-run or batch-child claim APIs.
- Project-health state counts now use an ordered map, making JSON health exports deterministic for audit diffs and ticket automation.
- Preflight plan storage now accepts only canonical 64-character hexadecimal SHA-256 digests, and unset SQL values correctly read as no preflight instead of a type error.
- Generic mailbox terminal completion now rejects contradictory run/state pairs, preventing failed or cancelled executions from being recorded as successful mailbox outcomes.
- Durable batch admission now rejects duplicate mailbox IDs and normalized duplicate destination mailboxes at the transactional start boundary, including legacy queues that bypassed import validation.
- Durable batch start now rejects duplicate mailbox IDs before creating the parent or child runs, preserving one execution owner per mailbox even for non-UI callers.
- Core project and mailbox creation now reject normalized duplicate destination mailboxes, preserving duplicate-target protection for non-UI callers.
- Durable process registration now has a bounded five-second acknowledgement window; an unresponsive controller causes the child to be cancelled and reaped rather than waiting indefinitely.
- A parent batch cannot be marked `completed` when any terminal child is failed, cancelled, abandoned, or otherwise unsuccessful; successful waves now require successful completion of every child.
- Process-registration rejection now has an integration-style runner test that proves a long-lived child is cancelled before the runner returns.
- Process startup now waits for an explicit durable-registration acknowledgement before supervising the child; lost or rejected registration cancels and reaps the process instead of allowing an untracked engine to continue.
- Parent-run terminal statuses are now validated at the core boundary; unknown status values cannot enter the durable run ledger.
- The durable store now refuses to finish a parent batch while any queued or running child remains, preventing incomplete waves from being recorded as terminal.
- Live dual-IMAPS batch children now re-authenticate both endpoints immediately before each launch attempt, so credential rotation cannot bypass the successful dry-preflight gate.
- Generic terminal run completion can no longer promote a mailbox to `verified`; only the evidence-aware transaction may make that transition, preventing reuse of evidence from an earlier run.
- Dual-IMAPS imapsync live runs now perform a fresh certificate-validated authentication probe immediately before mutation. The proof is bound to the current plan and process-local credential digest, so edits during the probe require renewed admission.
- Batch retry-scope selection now has direct coverage for every durable state category, protecting the default Verified exclusion and explicit all-row re-run behavior.
- Live batch execution now has explicit retry scopes for unresolved rows, failed/Attention rows, delta-required rows, or an all-row re-run. The default skips verified mailboxes, and live plan/fingerprint validation is performed only for the selected durable rows.
- Activity queries now use a lightweight run-list projection that omits full immutable plan snapshots; detailed snapshots remain available through explicit run lookup for reports and forensic review.
- Verification now exposes a typed `EvidenceScope` (`engine-confirmed` or `aggregate-reconciled`) at the core boundary, and exported JSON uses those explicit labels instead of inventing scope from report-local conditionals.
- Process-layer tests now pin the typed cancellation and timeout outcomes, including child-group cleanup, independently of higher-level UI error handling.
- Process supervision now returns a typed outcome distinguishing exit codes, operator cancellation, and execution timeout; engine-specific interpretation remains above the process layer instead of relying on error-string matching.
- Post-spawn stdout/stderr pipe failures now terminate and reap the child before the process runner returns, keeping every execution path supervised or explicitly failed closed.
- The process runner now fails closed when the reliable `ProcessStarted` event cannot be delivered: it cancels and reaps the spawned child before returning, preventing an engine from continuing without durable supervision.
- Verification Markdown and project JSON exports now include a deterministic evidence digest bound to the producing run, immutable plan snapshot, and recorded evidence dimensions; the digest is an integrity reference, not a signature or message-level verification claim.
- Single-mailbox verification reports now use the evidence-producing run’s immutable plan snapshot for endpoint and mailbox identity, with an explicit legacy fallback label when an older run has no parseable snapshot.
- CI and release workflows now pin third-party GitHub Actions to reviewed commit SHAs while retaining release-version comments, reducing supply-chain drift in builds and artifact publication.
- Batch live promotion now reloads referenced keyring credentials and compares each selected row against a process-local digest captured by its successful dry validation; changed or missing credentials require a new dry validation without persisting secret-derived material.
- Single-mailbox live promotion now reloads referenced keyring credentials and compares a process-local credential digest captured by the successful dry preflight; changed or missing session credentials require a new dry preflight without persisting secret-derived material.
- Live batch retries now exclude evidence-backed `Verified` mailboxes by default, with an explicit operator opt-in for intentional re-runs; selected child runs and event updates retain their original queue-row identity. Excluded rows no longer need restored session credentials loaded or revalidated.
- Startup orphan escalation now revalidates the recorded Linux PID, process group, session, and start time immediately before `SIGKILL`; recycled identities are never escalated.
- If durable process registration fails after an engine is spawned, the controller now requests immediate cancellation and refuses to let an untracked migration continue.
- Process startup now activates stdout/stderr drainers before the reliable `ProcessStarted` event is delivered, preventing a full bounded event queue from blocking pipe setup and stalling a newly spawned engine.
- Subprocess diagnostic lines now use nonblocking delivery to the bounded UI queue; slow operators cannot stop pipe draining and stall the migration engine. Omitted-line counts are surfaced when the queue is saturated.
- Preflight plan records now persist only a SHA-256 digest of the canonical plan; full generated arguments remain in memory for admission checks but are no longer stored in mailbox job state.
- Evidence-aware terminal completion now rejects a `verified` mailbox state when any modeled evidence dimension differs, keeping the verification claim enforced at the storage boundary rather than only in UI orchestration.
- Restart recovery now abandons queued child runs belonging to an interrupted batch parent while leaving never-claimed mailboxes queued; partial-claim and pre-claim crash scenarios are covered by regression tests.
- Startup now defers age-based secret-directory cleanup whenever a recorded process identity cannot be verified, preserving passfiles that an unverified orphan may still need and surfacing the deferred cleanup to the operator.
- Overview now reports mailbox totals and durable ready/running/verified counts from the project ledger, with an explicit operator-attention count instead of inferring readiness from the in-memory import queue.
- Legacy evidence insertion is now test-only; production code must use run-owned terminal evidence APIs so verification cannot be created outside an execution transition.
- Process registration now validates run ownership and inserts the active-process identity in one transaction, preventing a delayed worker from recreating process state after terminal cleanup.
- Active process registration now requires a mailbox-specific child run; parent batch runs cannot claim arbitrary same-project mailboxes.
- `record_evidence_for_run` now verifies that the durable run is the exact child run for the supplied mailbox, preventing same-project or cross-project evidence misattribution.
- Lifecycle events (`run_started`, `mailbox_claimed`, and `run_finished`) now carry the exact durable run ID, completing run-scoped forensic attribution alongside output and verification events.
- SQLite startup now records a schema version and refuses to open a database stamped with a newer unsupported version, preventing unsafe partial migrations during downgrade or mixed-version use.
- Execution output and verification events now persist their producing `run_id` in SQLite; the run resolves its project identity transactionally, improving forensic reconstruction without changing legacy project-scoped event compatibility.
- Batch dry/live waves now reuse an existing durable queue only when every row's mailbox identity and secret-free persisted configuration match; edited queues receive a fresh project association instead of reusing stale mailbox IDs.
- Durable subprocess diagnostic and error event details now have a UTF-8-safe per-entry size cap with an explicit truncation marker, preventing pathological engine output from creating oversized SQLite records.
- Account identity and credential fields are now locked while an execution is active, keeping the visible plan aligned with the immutable run context and preventing edits from contaminating the next retry.
- Live single-mailbox startup now forcibly reloads referenced keyring credentials before credential fingerprinting and fresh authentication, so keyring rotation cannot be silently ignored by an older in-memory password.
- Batch concurrency, retry scope, row credentials, and keyring-application controls are now disabled while workers are active, keeping the visible queue immutable for the lifetime of a wave.
- The active single-run plan is now restored from an in-memory non-secret lock snapshot while execution is active, covering ports, TLS, engine, dry/live mode, and advanced policy fields in addition to account credentials.
- Live batch admission now forcibly reloads each selected row's referenced keyring credentials before comparing dry-validation fingerprints, so rotated secrets cannot be hidden by cached row passwords.
- Verification, project, JSON, and health export failures now appear in the operator status and journal instead of being silently discarded.
- Security documentation now matches runtime-secret cleanup timing and identifies exported verification/health artifacts, rather than describing the visible journal as the audit record.
- Project and health views now prefer the immutable active run’s project identity while execution is active, preventing a loaded single project from shadowing an active batch wave.
- Active project precedence is covered by a focused regression test, preserving batch-versus-single report routing during future controller changes.
- A panic in the single-run worker is now converted into a failed `Finished` event, allowing normal transactional terminal cleanup and operator review instead of leaving the UI apparently running until restart.
- When startup cannot verify a recorded process identity, all new execution is blocked until the operator explicitly confirms the host has no surviving MailSwiftSync engine; this closes the non-Linux retry-overlap gap.
- Cockpit phase-advance failures are now shown as errors instead of being reported as a successful Preflight transition.
- The production runbook now documents that unverified process identities persist across restarts until explicit operator review clears them.
- Moved application instance-lock ownership and acquisition into `src/process.rs`, while centralizing restrictive file permissions in `src/credentials.rs`; lock/recovery policy is now separated from the UI controller.
- Subprocess line framing now reads in fixed-size chunks and caps any single unterminated line at 64 KiB with an explicit truncation marker, preventing pathological engine output from causing unbounded allocation.
- Subprocess stdout/stderr readers are joined on every completion path, including timeout and cancellation, so reader failures or panics cannot be silently discarded or leave unmanaged reader threads.
- Subprocess line readers now propagate pipe I/O failures and reader-thread panics instead of treating them as clean EOF; incremental lossy-UTF-8 streaming remains intact.
- Visible journal retention now uses `VecDeque`, keeping the 10,000-line UI window bounded without repeatedly shifting the entire log.
- Batch admission now creates a durable child run for every mailbox with its own engine and secret-free plan snapshot; the parent run remains the wave-level record while child identities are available for process and terminal-result attribution.
- Batch child runs now retain their parent wave relationship in SQLite, and claims/process registration verify that relationship before a worker can own a mailbox.
- Batch worker state and completion events now carry durable mailbox and child-run IDs instead of routing persistence by queue index.
- Live imapsync batch children retain bounded summary markers and commit per-mailbox engine evidence with the child terminal result when available.
- Batch evidence events are validated against the active immutable child mapping before they can affect durable state.
- Evidence parsing now lives in a dedicated verification module, separating engine-output interpretation from UI and process orchestration.
- Live Dovecot batch children now run the existing two-sided verification commands and bind aggregate evidence to each child run before terminal completion.
- Dovecot verification execution is shared between single and batch workflows, keeping redaction, two-sided parsing, and failure handling consistent.
- Verification mismatches now use a distinct `verification_difference` state instead of being mislabeled as another required delta; only explicit engine delta outcomes remain `delta_required`.
- Overview, Activity, Markdown reports, and JSON exports now count verification differences as operator-review items.
- Native Dovecot live transfers now request up to five minutes for an existing dsync mailbox lock before failing, reducing collisions with other administrative sync work.
- Active batch contexts now retain immutable child job IDs and launch-time fingerprints; completion persistence no longer consults the mutable queue to identify or certify a mailbox.
- Duplicate-destination detection now canonicalizes endpoint ports, so an implicit IMAPS port and an explicit `:993` cannot bypass the concurrent-write guard.
- Process-start events now carry the durable mailbox job ID rather than relying on a queue index when registering OS process identity, reducing routing risk if UI collection order changes.
- Moved timeout handling, process-group setup/termination, Linux process identity, and recorded-process matching into `src/process.rs`; supervision policy now has a dedicated module boundary alongside output framing and launch pacing.
- Batch child mailboxes remain `queued` until an active parent worker atomically claims them; claims increment attempts and are recorded as durable events, while generic state mutation can no longer manufacture `verified` by reusing evidence from an older run.
- Session-held password buffers now use `zeroize::Zeroizing<String>` throughout the form, imported rows, and keyring loads; saved profiles, snapshots, and the durable ledger remain password-free.
- Added a SQLite partial unique index enforcing one `running` job-bound run per mailbox; parent batch runs with a null `job_id` remain valid, and direct-insert regression coverage proves the database rejects overlap.
- Added an exhaustive mailbox state-transition matrix test covering every known state pair, including retry, recovery, delta, and verification paths.
- Batch mailbox configuration persisted in SQLite is now restore-safe: raw expert-option values are removed from durable row configs while launch-time snapshots retain only their digest.
- Replaced the imapsync extra-option denylist with an explicit safe allowlist for trusted application configuration; connection, credential, transport, destructive, logging, and unknown options are rejected.
- Bounded migration event delivery with a 4,096-event backpressure queue for single and batch runs; capability discovery remains on its separate one-result channel so chatty engine output cannot grow an unbounded UI backlog.
- Moved byte-safe, lossy subprocess line framing and redacted capture helpers into `src/process.rs`; process output policy is now reusable without depending on the UI controller.
- Bulk imports now accept optional per-row `source_credential_id` and `destination_credential_id` references, allowing password-free queues to resolve distinct OS-keyring entries without copying secrets into spreadsheets.
- Bulk CSV/XLS/XLSX imports now reject `extra_options`; executable engine settings remain trusted application configuration rather than spreadsheet-controlled input. The shipped template and migration guide now reflect that boundary.
- Extracted imapsync argument construction and expert-option safety validation into `src/engine.rs`, including transport policy, per-worker throttle shaping, and product-owned logging controls.
- Added regression coverage proving sequential process-launch admission is paced and that durable plan snapshots retain the expert-option digest while excluding its raw value.
- Extracted secret-runtime, passfile, cleanup-guard, and permission helpers into `src/credentials.rs`; per-run secret creation no longer triggers stale-directory sweeping, keeping cleanup under the instance-owned startup path.
- Scoped Dovecot exit-code-2 `DeltaRequired` handling to live sync/backup runs; dry source and destination readiness checks now require ordinary successful exit status.
- Moved application-owned state-directory permission setup into the desktop layer; `StateStore::open` now secures only the database and SQLite sidecars instead of changing permissions on an arbitrary caller-supplied parent directory. Stale secret cleanup now runs only after instance ownership and startup recovery are established.
- Single-mailbox starts now compare the edited form with the durable project/mailbox identity: dry runs create a fresh project for a changed identity, while live starts require a new matching preflight instead of attaching work to stale metadata.
- Hardened run-plan snapshots with a dedicated serialized schema: raw expert-option values are excluded from SQLite and reports while an SHA-256 digest preserves configuration identity. Process registration now also rejects a same-project mailbox that does not match the single-mailbox run.
- Began separating process orchestration from the UI controller by extracting the shared batch process-launch limiter into `src/process.rs`; the seam is behavior-preserving and covered by the existing limiter tests.
- Added a shared cancellation-aware process-launch token bucket for batch workers and aggregate imapsync throttle shaping, preventing configured message/byte ceilings from multiplying with batch concurrency while smoothing connection bursts; unsafe targets below worker count are rejected.
- Added a production migration runbook covering pilot-to-bulk sequencing, instance-lock and crash recovery, Attention review, evidence/health exports, and the policy exceptions for plain IMAP and remote Dovecot credentials.
- Plain IMAP now requires explicit cleartext-transport acknowledgement before any authenticated operation, including dry preflight; the UI guidance and validation error explain that simulation still transmits credentials and protocol traffic.
- Subprocess stdout/stderr is now decoded and emitted line-by-line through a callback, preserving live journal visibility and preventing complete migration logs from accumulating in memory; bounded collection remains limited to verification capture.
- Added a compact credential-free project-health JSON export for ticketing and operator wrappers, including phase, mailbox state counts, Attention items, and recent run outcomes.
- Stabilized instance-lock release after denied contender opens by explicitly closing the failed lock descriptor before returning contention to the UI.
- Replaced fixed atomic-index batch dispatch with an MPMC channel queue. Workers now consume owned jobs from a disconnect-terminated queue while retaining bounded concurrency, cancellation, retries, and durable failure reporting.
- Added immutable execution-plan snapshots to durable run records. Snapshots are captured at run start, exclude session passwords, and are included in verification reports so historical artifacts do not depend on later-edited UI fields.
- Restricted the invariant-bypassing run-insertion helper to test builds and made single-mailbox verification reports derive project, endpoint, mailbox, and engine identity from durable records rather than mutable form fields.
- Clarified lock-contention recovery guidance and the `--nolog` behavior in the README and security/runbook docs, so operators know to close the existing owner and use MailSwiftSync’s journal rather than searching for an unmanaged imapsync log.
- Simplified workspace navigation by keeping the sidebar focused on Overview, Plan, Mailboxes, Activity, and Verification; modal tools remain in the header. Overview now surfaces durable preflight results and the current next action. Batch queues can apply OS-keyring credential references to rows missing credentials without copying password values or overwriting existing row credentials.
- Added a Linux startup-recovery integration test covering the complete orphan path: persist a real process identity, verify ownership, terminate the matching process group, recover the mailbox to `Attention`, clear the active-process row, and mark the run abandoned.
- Remote Dovecot validation now rejects SSH hosts and usernames beginning with `-` or containing whitespace, preventing imported endpoint values from being interpreted as SSH options.
- Added a Unix process-group termination regression test that launches a dedicated child session and verifies recorded-process cleanup actually stops it, complementing the existing identity-mismatch safety test.
- Plain source IMAP transport now requires an explicit live-execution acknowledgement. The acknowledgement is visible in the workspace, included in the secret-free plan fingerprint, and applies to batch runs so cleartext credentials/data cannot be enabled accidentally during promotion.
- Batch worker panics are no longer silently discarded. The controller detects failed worker joins, moves unresolved children to `Attention`, emits an operator-visible diagnostic, and fails the parent batch result so unfinished work cannot appear successful.
- Added an exclusive cross-platform application-state lock. A second MailSwiftSync window no longer performs orphan recovery against a live first window; it enters a non-durable session and cannot start migrations until the existing owner releases the lock.
- Secret cleanup is now confined to the application-owned `mailswiftsync` directory beneath `XDG_RUNTIME_DIR`; it no longer scans or changes permissions on the shared runtime root. Batch imports and queue clearing also discard stale durable batch associations, and queue mutation is disabled while a run is active.
- The durable mailbox ledger now rejects a second run while a mailbox is already running and refuses to create `verified` state without stored evidence. imapsync's own persistent log is disabled by default, reserved logging flags are rejected, and one-dash forms of protected options cannot bypass the safety policy.
- Child migration and capture processes receive null stdin so external tools cannot unexpectedly block waiting for interactive input. Default source ports now follow transport mode: 993 for IMAPS and 143 for STARTTLS/plain.
- Dovecot process exit code 2 is now preserved as a `DeltaRequired` outcome rather than being flattened into a generic failure; batch and single-run state handling retain that meaning, with a Unix runner regression test.
- Active executions now carry an immutable run context containing their project, mailbox, mode, engine, and launch-time plan fingerprint. Completion and process-registration handling no longer infer ownership or dry/live semantics from mutable form fields or an unrelated restored batch queue.
- Subprocess output now uses byte-oriented line framing with lossy UTF-8 conversion, so malformed external-tool output cannot stop pipe draining; a regression test covers continued reading after invalid bytes.
- Live bulk runs now reject duplicate destination host/port/mailbox targets before any process starts, preventing concurrent migration engines from mutating the same destination mailbox.
- Project phase recovery from `Attention` to `Preflight` or `Verification` now works as intended, and phase changes plus their audit events commit atomically.
- Updated user-facing troubleshooting and architecture language to consistently call imapsync output engine-confirmed rather than authoritative, and to distinguish a successful process exit from verified completion.
- Live batch promotion now validates every child plan fingerprint inside the same SQLite transaction that marks the batch running, preventing a partially edited or stale queue from starting.
- Verification exports now use explicit evidence levels and identify engine-confirmed versus aggregate evidence; compatibility percentages are labelled as internal comparison metrics rather than probabilities. Durable project queries also have indexes for mailbox state, run history, events, evidence history, and active process lookup.
- Secret runtime fallback now uses the system temporary directory rather than persistent application data, enforces owner-only Unix permissions, and performs stale-run cleanup at application startup as well as before a new run.
- Persistent state now creates and enforces an owner-only database directory on Unix, containing SQLite sidecars as well as the primary database file.
- Process recovery now records Linux start-time, session, and process-group identity and refuses to signal a recorded PID when those values cannot be proven to match.
- Batch run completion now updates the terminal run status and clears active process identities in one SQLite transaction.
- Dovecot dry validation now checks both sides: the remote `imapc` source plus destination user and mailbox-list readiness, for single and batch validation runs.
- Added explicit Dovecot execution-location choices for local or SSH invocation; automatic hostname inference remains available only for legacy compatibility.
- Added durable active-process records and startup reconciliation so recorded Unix migration process groups are terminated before interrupted runs become retryable operator-review jobs.
- imapsync execution now requests certificate verification for both encrypted endpoints (`SSL_verify_mode=1`); expert options cannot override the transport policy.
- Added bounded redacted stdout/stderr tails to process failures so failure classification and transient retry decisions receive the engine's actual diagnostic output.
- Added five-second graceful shutdown before process-group escalation, Linux parent-death signalling, and retry-safe handling of timeout/cancellation cleanup.
- Aggregate evidence mismatches now report zero compatibility confidence instead of the misleading 85% fallback; exact aggregate matches remain explicitly labelled as aggregate rather than message-level proof.
- IMAP readiness now tries every resolved address before reporting connectivity failure, and bulk imports preserve password whitespace exactly.
- Removed the superseded non-terminal evidence persistence API after atomic terminal completion became the only supported path.
- Promoted the durable batch queue from validation-only to gated live execution after matching dry-validation fingerprints and explicit operator confirmation.
- Added elapsed-time running feedback, Escape-key cancellation, dark/light theme switching, and contextual help for performance controls.
- Added explainable evidence levels to complement the internal confidence percentage in the verification UI and architecture model.
- IMAP readiness now treats missing NAMESPACE support as compatible while parsing capabilities strictly from the post-auth response.
- Clarified that Automatic engine selection is a conservative imapsync default, not server-environment autodetection.
- Added an atomic evidence-backed terminal completion path so successful live verification can move `running` to `verified` without violating ordinary mailbox state transitions.
- Added a persistent lifecycle stepper to the main workspace, password show/hide controls, and inline account-field validation for common input errors.
- Clarified in the README and wiki that current UI images are workflow illustrations rather than pixel-accurate application screenshots.
- Batch workers now claim immutable jobs through an atomic index instead of contending on a mutex-protected iterator.
- Added a configurable 1–720 hour per-process timeout for large mailbox migrations.
- Added explicit imapsync message/byte-per-second throttles for provider-friendly runs.
- Pinned CI and release builds to Rust 1.92.0 so linting and published artifacts use a reproducible toolchain.
- Added the MIT license and clarified the product identity and control-plane positioning.
- Repositioned the main UI around engine-neutral planning and verification/audit outcomes.

- Added explicit migration engine selection with Dovecot-native `doveadm`/`imapc` execution and an `imapsync` fallback.
- Added local and SSH-based Dovecot destination execution with non-interactive SSH and shell-quoted remote arguments.
- Added Dovecot post-run mailbox reconciliation using folder, message, and virtual-size status.
- Added durable redacted run output, lifecycle events, mailbox states, and verification evidence.
- Added `imapsync` summary parsing for automatic evidence capture.
- Added child-process-only credential delivery for live `imapsync` runs (superseded by protected ephemeral passfiles below).
- Added exportable Markdown verification reports and a tagged-release workflow for Linux, Windows, and macOS artifacts.
- Added explicit `ready`/`Preflight` outcomes for successful dry runs; dry validation no longer masquerades as a completed migration.
- Added conservative evidence scope, live-run preflight/persistence gates, process-group termination on Unix, and transactional project/evidence writes.
- Added abandoned-run recovery and structured run lifecycle records; verification failures now enter operator Attention instead of masquerading as transfer failures.
- Added bounded, cancellable verification subprocesses with Unix process-group termination.
- Batch cancellation now stops subsequent jobs and records cancelled mailbox states.
- Hardened atomic profile writes and endpoint validation, including trimmed ports and malformed IPv6 rejection.
- Added regression coverage for command generation, credential handling, evidence parsing, and verification confidence.
- Added durable batch validation jobs, per-run evidence IDs, bounded execution, and operator cancellation.
- Bound live execution to the exact secret-free plan captured by a successful dry preflight, and require the mailbox job to be explicitly ready.
- Made project creation transactional in the cockpit, reject advanced options that override controlled endpoints or credentials, and redact worker-thread output before it reaches the event journal.
- Added cancellation status to structured run records and expanded exported reports with mailbox state and evidence scope.
- Made batch project and mailbox creation atomic, attached batch validation to a durable run record, and journaled batch activity under its project.
- Restored the selected mailbox identity from durable state and marked interrupted run records as abandoned with recovery events on restart.
- Corrected IMAP capability discovery to consume the server greeting first, honor configured ports, and clearly limit the probe to IMAPS.
- Bounded the in-memory visible execution journal to 10,000 lines while retaining the durable event ledger.
- Persisted secret-free batch row configuration and restored durable batch queues for review after restart; credentials remain session-only.
- Upgraded the GUI/spreadsheet dependency line to remove the reported high-severity `quick-xml` vulnerabilities and added `cargo audit` to CI.
- Batch startup now validates every row before launching any process, preventing restored queues with blank credentials from running.
- Tightened the live project-phase gate to recognized execution and verification phases only.
- Local Dovecot commands and verification now receive source credentials through a child environment variable and Dovecot `$ENV:` expansion; remote SSH exposure remains documented.
- imapsync credentials now use short-lived owner-only passfiles instead of environment variables, with cleanup after process completion.
- imapsync plans now explicitly force encrypted transport for IMAPS/STARTTLS and reject destructive expert flags; stale credential directories are cleaned up after forced termination.
- Credential files are now created atomically with owner-only permissions before any password bytes are written, then flushed before execution.
- Activity now exposes durable run history after restart and offers an explicit safe-retry path that returns failed or interrupted mailboxes to dry preflight.
- Restricted profile and SQLite state files to owner-only permissions on Unix systems.
- Added durable run metadata to exported verification reports so each report is traceable to a specific execution after restart.
- Strengthened crash durability with flushed atomic profile writes, SQLite writer backoff, and rejection of evidence records that reference unknown runs.
- Made direct run completion atomic across run status, mailbox state, and the terminal audit event; persistence failures are now surfaced in the operator journal instead of being silently ignored.
- Project-phase and batch-completion persistence errors are now surfaced to the operator rather than being discarded by the event poller.
- Verification evidence and its derived `verified`/`delta_required` mailbox state are now committed in one transaction; event-poller persistence failures are surfaced instead of silently ignored.
- A reported durability failure can no longer be overwritten by a later “completed successfully” status; the run is explicitly marked for durability review.
- Reworked the README for first-time operators with above-the-fold screenshots, CI/license trust signals, audience guidance, alternative-tool comparison, and honest release/install boundaries.
- Expanded `SECURITY.md` with supported-version policy, credential/transport/persistence boundaries, engine trust assumptions, and a safe vulnerability-reporting checklist.
- Added package metadata and a deliberately marked technical-preview release note covering artifact coverage and known production limitations.
- Added optional OS-keyring credential references with session-only loading, deletion controls, and documentation that distinguishes keyring password storage from OAuth and unattended secret brokering.
- Added stable failure taxonomy for operator action and retry policy; only transport/throttling failures are classified as transient, while run details retain the failure class.
- Bound the exact preflight fingerprint to non-secret OS-keyring credential references so changing the credential source requires a fresh preflight.
- Added cross-platform release-target compilation to pull-request CI, covering Linux, Windows, and both macOS architectures instead of waiting for a tag build to catch platform regressions.
- Closed the IMAP probe validation bypass: capability discovery now validates form input and its quoting helper rejects control characters defensively; redundant secret-directory cleanup was removed from worker execution.
- Clarified that the imapsync text-summary `unmatched_messages` value is a proof-pending sentinel, not a literal unresolved-message count.
- Live mailbox starts now atomically create the durable run record and move the job to `running`; persistence failures abort before the migration process is spawned.
- The IMAPS readiness probe now authenticates, refreshes post-auth capabilities, and requests `NAMESPACE` and `LIST` so capability results are not presented as sufficient migration readiness on their own.
- IMAPS readiness now requires tagged `OK` responses for capability, authentication, namespace, and folder-inventory commands; completed `NO`/`BAD` responses are reported as failures.
- Reject control characters in hosts, usernames, and passwords before validation, process execution, or authenticated IMAP probing.
- Batch startup now atomically records the parent run and marks every child job as running before worker execution, so restart recovery cannot lose queued children between UI events.
- Dovecot mode no longer offers the dual-IMAPS readiness probe when the destination has no IMAP credential; native `doveadm` dry preflight is now the explicit destination check.
- Remote Dovecot execution was disabled by default pending secret-broker
  delivery. The former opt-in compatibility path was subsequently removed;
  current builds reject remote execution rather than exposing destination
  process-argument credentials.
- Bulk imports no longer require password columns; missing credentials can be entered in masked per-row fields, reducing the need to keep plaintext passwords in migration spreadsheets.
- Separated structural bulk-import validation from credential-required execution validation, so passwordless identity imports are accepted but cannot be launched until credentials are supplied.
- Added project-level Markdown verification reports covering every durable mailbox job, evidence scope, confidence, unresolved states, and recent runs.
- Replaced sequential batch validation with a bounded 1–16 worker pool and an operator-selected concurrency setting; cancellation and indexed output remain shared across workers.
- Batched concurrent worker output into one SQLite transaction per UI cycle, retaining redacted durable output while avoiding one synchronous disk transaction per line.

### Changed

- Added Drop-based cleanup guards for prepared secret directories, retaining stale-directory cleanup for forced termination.
- Batch validation retries only classified transient transport failures up to three times with exponential backoff; authentication and configuration failures remain terminal.

- Verification exports are now written atomically, flushed before rename, and restricted to owner-only permissions on Unix.
- Added secret-free, versioned JSON project reports alongside Markdown exports for automation and ticketing workflows.

- Dovecot migrations default to additive `sync -1`; destination mirroring requires explicit destructive configuration.
- Automatic engine selection is conservative and uses `imapsync` unless Dovecot mode is explicitly selected.
- Updated operator, architecture, and security documentation to describe the native Dovecot workflow and its limitations.
- Added explicit source port/TLS controls, including correct Dovecot mapping for plaintext (`imapc_ssl=no`), and stricter CSV/option validation.
- Renamed the product and runtime namespace to MailSwiftSync, while retaining one-time loading of legacy profile files.

### Security

- Passwords remain excluded from saved profiles and the SQLite ledger.
- Dovecot source credentials still use a destination-side `imapc_password` override for remote execution and may be visible to process inspection; provider OAuth and unattended secret brokering remain future work.
- Automatically advances fully evidence-verified live projects through Verification to Complete; incomplete projects remain in Verification for operator review.
- Batch workers now wait for an acknowledged durable child/mailbox claim before launching an engine process, eliminating the asynchronous unclaimed-process window.
- Project Markdown and JSON reports now include the durable run ID that produced each mailbox's latest evidence.
- Project and health exports now include a stable SHA-256 reference to each persisted run plan for change-ticket correlation without duplicating the snapshot.
- imapsync destination endpoints now support typed implicit-TLS or STARTTLS transport and transport-appropriate default ports, including explicit nonstandard destination ports.
- Operator verification surfaces now lead with named evidence levels instead of pseudo-precise confidence percentages; the compatibility score remains internal for legacy callers.
- Single-mailbox verification exports now use named evidence levels consistently with project reports and the Verification workspace.
- New interactive sessions initialize destination transport explicitly to implicit TLS, avoiding an ambiguous blank TLS selector for legacy/imported profile defaults.
- Instance-lock ownership now has an explicit RAII unlock path, making release deterministic on normal shutdown as well as automatic on process termination.
- CI now pins cargo-audit to 0.22.2 so the dependency-security gate is reproducible across runs.
