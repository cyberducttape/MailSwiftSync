# MailSwiftSync Capability Manifest

**Status source:** [`capabilities.toml`](capabilities.toml)
**Last verified:** 2026-09-30

This manifest documents what MailSwiftSync actually does, not what it claims to do.
The `[documentation]` policy in `capabilities.toml` also marks terms that must
remain confined to the README's experimental/planned status section; CI checks
that status layout against the machine-readable source.

## Machine-Readable Status

Generated from `capabilities.toml` by `python3 scripts/verify-capability-claims.py --write`;
`make capability-check` fails when this table is stale. Rows below that name a
capability in its `manifest_rows` are checked against these fields.

<!-- capabilities:begin -->
| Capability | code | controller | ui | generic_lab | gmail_live | m365_live | production_supported |
|---|---|---|---|---|---|---|---|
| `aggregate_evidence` | implemented | wired | wired | passed | no | no | no |
| `message_body_proof` | bounded_opt_in | wired | wired | not_run | no | no | no |
| `message_level_metadata_reconciliation` | implemented | wired | partial | passed | no | no | no |
| `provider_live_validation` | available | wired | partial | passed | no | no | no |
| `provider_oauth_authorization` | implemented | wired | wired | passed | no | no | no |
| `provider_oauth_refresh` | implemented | wired | partial | passed | no | no | no |
| `provider_runbooks` | implemented | wired | partial | passed | no | no | no |
| `recovery_guidance` | implemented | wired | partial | passed | no | no | no |
| `uidvalidity_delta_checkpoints` | implemented | wired | not_claimed | not_run | no | no | no |
<!-- capabilities:end -->

## Legend

| Column | Meaning |
|--------|---------|
| **Code** | Feature is implemented in source code (yes/no) |
| **Wired** | Feature is integrated into the actual migration pipeline (yes/no/partial) |
| **Tested** | Highest level of automated testing (unit/integration/none) |
| **Live Provider** | Validated with real provider accounts (yes/no/pending) |

---

## Core Migration Features

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| IMAP message transfer (imapsync) | yes | yes | integration | pending | Engine: imapsync 2.314 |
| Native IMAP message transfer (Dovecot) | yes | yes | integration | pending | Packaged native-engine CI fixture passes; live provider and target-storage qualification remain pending |
| Folder/label mapping | yes | yes | integration | pending | Integration coverage is through generic imapsync mapping/automap; no provider-specific namespace translation engine |
| Message extraction (imapsync) | yes | yes (TLS live path) | unit | pending | Post-transfer verifier enumerates selectable folders and fetches bounded UID, Message-ID, size, and date metadata; live-provider evidence is pending |
| Message extraction (Dovecot) | yes | partial | unit | pending | Native Dovecot aggregate verification includes mailbox UIDVALIDITY context for safe resume binding; the generic IMAP metadata verifier currently services imapsync runs |

---

## Verification Features

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Aggregate evidence** (folder/message counts) | yes | yes | integration | generic-lab | Exercised by imapsync against local Dovecot server fixtures; native-Dovecot coverage pending |
| **Message-level mismatch detection** | yes | yes (TLS imapsync path) | unit+scenario | pending | Folder-aware verifier is called after successful imapsync transfers; failures remain operator-reviewable and are never downgraded to aggregate success |
| **Named message evidence levels** | yes | partial | unit+integration | pending | Metadata reconciliation is the default; explicit encrypted-imapsync body-hash runs emit a distinct Level 4-style bounded body-proof outcome after complete coverage |
| **Checkpoint persistence** per message | no | no | none | no | Not implemented; Dovecot run-level checkpoints are now bound to a complete source/destination UIDVALIDITY digest, while evidence persists per run |
| **Crash recovery** | partial | partial | unit | no | Run-level recovery works and verification staging resumes from snapshot-bound cursors; per-message transfer recovery is not implemented |
| **Exception acceptance workflow** | yes | yes | unit | no | UI accepts exceptions, stored durably |

---

## Provider Support

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Gmail authentication (app password)** | yes | yes | unit | no | Standard IMAP auth |
| **Gmail authentication (OAuth)** | yes | partial | unit | no | Token refresh implemented, scope documentation corrected to https://mail.google.com/ |
| **Gmail-specific throttling presets** | no | no | none | no | No provider-specific IMAP rate is asserted; use configured generic imapsync message/byte limits |
| **Microsoft 365 authentication (OAuth)** | yes | partial | unit | no | Token refresh implemented, scope documentation corrected to IMAP.AccessAsUser.All |
| **OAuth consent (authorization code + PKCE)** | yes | yes | unit + loopback | no | Browser PKCE authorization from the GUI account screen and the `oauth-authorize` CLI command, for Google, Microsoft, and custom providers; stores the refresh configuration in the OS keyring; not yet run against live tenants |
| **Microsoft 365-specific throttling presets** | no | no | none | no | No provider-specific IMAP rate is asserted; use configured generic imapsync message/byte limits |
| **Fastmail authentication (app password)** | yes | yes | unit | no | Standard IMAP auth |
| **Fastmail-specific throttling presets** | no | no | none | no | No provider-specific IMAP rate is asserted; use configured generic imapsync message/byte limits |
| **Generic IMAP provider** | yes | yes | integration | generic-lab | Conservative IMAP implementation |

---

## Error Handling

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Generic error classification** | yes | yes | integration | generic-lab | src/controller/failure.rs consumes shared provider-intelligence signals and maps them to durable controller classes |
| **Provider-specific classification** | yes | partial | unit | no | Generic provider-intelligence patterns are now wired into controller retry classification; provider-context-specific rules remain pending |
| **Automatic retry with backoff** | yes | yes | integration | generic-lab | Transient failures auto-retry with bounded backoff; observed capacity/rate-limit failures also cool down later launches for the same endpoint pair (src/controller/batch_work_item.rs) |
| **Rate limit detection** | yes | partial | unit | no | Pattern matching implemented for generic rate limits; provider-specific patterns NOT applied |
| **Connection exhaustion handling** | yes | partial | unit | no | Configured per provider; generic connection failures retried; provider-specific limits NOT applied |

---

## Operator Guidance

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Pre-migration risk report** | yes | partial | unit | no | Scale report is available through the headless `risk` command; automatic GUI/live gating remains pending |
| **Post-migration exception report** | yes | partial | unit | no | Report is available through the headless `post-report` command; automatic generation from durable live evidence remains pending |
| **Provider-specific runbooks** | yes | partial | unit | no | Exposed through the headless `runbook` command and as read-only guidance for the selected source/destination presets on the GUI Plan page; the runbook does not sequence or gate the workflow |
| **Recovery guidance** (7 scenarios) | yes | partial | unit+integration | no | Fail-closed guidance is exposed through the headless `recovery-guidance` command and rendered in the Activity workspace for interruption, transport, and throttling attention states |
| **Resume/recovery dashboard** | yes | partial | unit+integration | no | Activity exposes durable run state and selected recovery guidance; a dedicated multi-run recovery dashboard remains future work |
| **Live throughput, progress, and ETA** | yes | yes | unit | no | Activity derives rolling throughput, transferred totals, per-mailbox progress, retry countdowns, endpoint cooldowns, and an estimated finish (checked against an optional maintenance window) from content-free counters parsed from imapsync output; values are estimates and Dovecot runs report no progress |

---

## Documentation

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Provider setup guides** | yes | n/a | n/a | n/a | Gmail, O365, Fastmail, generic IMAP |
| **OAuth configuration guide** | yes | n/a | n/a | n/a | Corrected scopes (Gmail, O365) |
| **Architecture documentation** | yes | n/a | n/a | n/a | Message-level verification design doc |
| **Documentation smoke tests** | yes | yes | integration | n/a | Checks required files and selected safety-critical wording; does not prove live evidence or wiring |

---

## Test Inventory

| Category | Source of truth | Status |
|----------|-----------------|--------|
| Core unit/integration tests | CI test summary artifact | CI-enforced |
| Documentation validation tests | CI test summary artifact | CI-enforced |
| Message verification scenarios | CI test summary artifact | CI-enforced |
| **Total** | **CI test summary artifact** | CI-enforced |

Test counts and pass/fail status are intentionally not duplicated here. CI
publishes the runnable test inventory and execution summary for each supported
job; source `#[test]` attributes are not equivalent to tests compiled for a
particular target.

---

## What Actually Works

✅ **Fully integrated and tested:**
- Basic IMAP transfer through imapsync against disposable Dovecot servers
- Native Dovecot transfer against the packaged Dovecot fixture, with aggregate evidence
- Folder mapping and discovery
- Aggregate evidence (folder/message counts)
- Exception recording and acceptance
- OAuth token refresh (transport layer)
- Documentation validation

---

## What's Partially Working

⚠️ **Code exists but not fully qualified:**
- Content-level mismatch detection (bounded body hashing is an explicit encrypted-imapsync forensic mode; provider qualification is outstanding)
- Provider-context-specific error classification (generic provider-intelligence mapping is now applied; provider-specific context remains pending)
- Provider-specific throttling quotas (not implemented; generic profile throttles and observed endpoint cooldowns are enforced)
- Pre/post-migration reports (available as explicit CLI exports, not automatically generated for every live run)
- Dedicated recovery dashboard (supported recovery guidance is visible in Activity; a multi-run dashboard remains future work)

⚠️ **Integration-tested but not fully qualified:**
- Native Dovecot engine execution (`doveadm sync`, native preflight, and aggregate verification) remains provider- and target-storage-unqualified
- OAuth token refresh (refreshes correctly, not tested end-to-end with actual IMAP session)
- Provider-specific error handling (classifies correctly, not tested against real errors)
- Confidence scoring algorithms (match correctly, not validated with mixed message sets)

---

## What Doesn't Exist

❌ **Not implemented:**
- Per-message checkpoint/restart persistence (Dovecot run-level checkpoints are UIDVALIDITY-context-bound)
- Provider-specific throttling enforcement (adaptive cooldown reacts to observed signals, but provider quota policies are not encoded)
- Automatic retry with provider-specific backoff
- Pre-migration risk report generation during migration
- Automatic post-migration exception-report generation after each live run (an explicit headless `post-report` export is available)
- Dedicated multi-run resume/recovery dashboard UI
- Runbook steps that sequence or gate the GUI workflow (the Plan page shows them as read-only guidance)
- Real provider integration testing (requires live credentials)
- Crash recovery for message-level checkpoints

---

## Known Limitations

1. **Message-level verification is metadata reconciliation by default; bounded content proof is opt-in**
   - Wired after successful TLS imapsync transfers for the full selectable-folder inventory
   - The default path uses Message-ID, INTERNALDATE, and RFC822.SIZE; an explicit encrypted-imapsync forensic mode also hashes bounded RFC822 bodies with SHA-256
   - Bounded fetch pages, account/message limits, and any unstable folder fail closed; partial account evidence is not emitted
   - Live verification stages fetched metadata in SQLite and reconciles it in bounded batches; the estimated 256 MiB fetched-state budget is an admission guard, not a whole-process peak-memory guarantee
   - Large-account/provider qualification remains outstanding; mismatch details are still accumulated for evidence persistence, and durable per-message checkpoint restart semantics remain future work

2. **Provider-specific throttling is not qualified**
   - Configurations are defined for Gmail, O365, Fastmail
   - Profile limits and observed endpoint-scoped cooldowns are applied before launches
   - Provider quota policies and live throttle behavior still require qualification

3. **OAuth works for token refresh, but needs live testing**
   - Scopes corrected (Gmail, O365)
   - Token refresh implemented
   - Not tested end-to-end with actual IMAP session
   - Awaits real provider validation

4. **Operator guidance is split between CLI and GUI**
   - Runbooks are exposed through `runbook` and shown read-only on the Plan page for the selected presets; they do not drive the workflow
   - Recovery guidance is exposed through `recovery-guidance` and is presented in Activity for the supported interruption, transport, and throttling attention states
   - The scale risk report is CLI-only (`risk`); the GUI's **Assess plan** checks plan readiness, not scale risk

---

## Implications for MSPs

**Safe to use for:**
- Technical previews on non-critical mailboxes
- Testing provider connectivity and configuration
- Validating IMAP transfer mechanics
- Early adoption with careful operator oversight

**Not yet ready for:**
- Unattended migrations (no external scheduler/recovery approval and provider quotas remain unqualified)
- Large-scale deployments without a qualified provider pilot or large-account load qualification
- Unattended very-large migrations requiring durable per-message checkpoint/restart semantics
- Critical customer mailboxes (lacking live provider validation)
- Automated migration orchestration (Activity recovery guidance is advisory; it does not schedule, resume, or autonomously retry migrations)

---

## Path to Production

To reach GA 1.0, the following work is required:

**Immediate (Before shipping v0.1):**
- [x] Wire metadata-level message verification into the imapsync migration pipeline
- [x] Add bounded opt-in content-fingerprint verification for encrypted imapsync
- [x] Implement UIDVALIDITY-aware run-level checkpoint binding; per-message checkpoint persistence remains open
- [x] Add supported recovery guidance to the operator Activity UI
- [ ] Validate OAuth with real provider accounts
- [ ] Test provider throttling with real connections

**Short-term (v0.2):**
- [ ] Live provider validation (Gmail, O365, Fastmail)
- [ ] Integrate pre/post-migration reports into UI
- [x] Show provider runbooks in the GUI (read-only on the Plan page)
- [ ] Let provider runbook steps sequence the GUI workflow
- [ ] Implement resume/recovery dashboard
- [ ] Provider-specific error classification in retry logic

**Medium-term (v1.0 GA):**
- [x] Implement SQLite-backed streaming reconciliation in the live metadata-verification path
- [ ] Qualify large-account performance and memory behavior with 100k+ message load tests
- [ ] Implement durable per-message transfer checkpoints (run-level Dovecot restart binding and snapshot-bound restartable verification staging are implemented)
- [ ] Performance benchmarks with 100k+ message mailboxes
- [ ] Provider edge case testing
- [ ] Production support runbooks
- [ ] Load testing with multiple concurrent migrations

---

## How to Update This Manifest

Update [`capabilities.toml`](capabilities.toml) first. This manifest is
explanatory prose; its status vocabulary must agree with the machine-readable
file and release notes must not introduce a separate capability status table.

**Never make unsupported claims.** If a feature is unit-tested but not integrated, say so. If it's integrated but not live-tested, say so.
