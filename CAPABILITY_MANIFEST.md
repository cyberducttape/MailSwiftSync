# MailSwiftSync Capability Manifest

**Status source:** [`capabilities.toml`](capabilities.toml)
**Last verified:** 2026-09-24

This manifest documents what MailSwiftSync actually does, not what it claims to do.

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
| Native IMAP message transfer (Dovecot) | yes | yes | no | pending | Dedicated native-engine fixture added; no successful CI execution has yet been recorded |
| Folder/label mapping | yes | yes | integration | pending | Integration coverage is through generic imapsync mapping/automap; no provider-specific namespace translation engine |
| Message extraction (imapsync) | yes | yes (TLS live path) | unit | pending | Post-transfer verifier enumerates selectable folders and fetches bounded UID, Message-ID, size, and date metadata; live-provider evidence is pending |
| Message extraction (Dovecot) | yes | partial | unit | pending | Native Dovecot aggregate verification remains live; the generic IMAP metadata verifier currently services imapsync runs |

---

## Verification Features

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Aggregate evidence** (folder/message counts) | yes | yes | integration | generic-lab | Exercised by imapsync against local Dovecot server fixtures; native-Dovecot coverage pending |
| **Message-level mismatch detection** | yes | yes (TLS imapsync path) | unit+scenario | pending | Folder-aware verifier is called after successful imapsync transfers; failures remain operator-reviewable and are never downgraded to aggregate success |
| **Named message evidence levels** | yes | partial | unit | pending | MetadataMatched/StrongMetadataMatch are now produced by the live verifier; content hashing remains unimplemented and is not claimed |
| **Checkpoint persistence** per message | no | no | none | no | NOT implemented; evidence persists per run, not per message |
| **Crash recovery** | partial | partial | unit | no | Run-level recovery works; message-level recovery not wired |
| **Exception acceptance workflow** | yes | yes | unit | no | UI accepts exceptions, stored durably |

---

## Provider Support

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Gmail authentication (app password)** | yes | yes | unit | no | Standard IMAP auth |
| **Gmail authentication (OAuth)** | yes | partial | unit | no | Token refresh implemented, scope documentation corrected to https://mail.google.com/ |
| **Gmail-specific throttling presets** | no | no | none | no | No provider-specific IMAP rate is asserted; use configured generic imapsync message/byte limits |
| **Microsoft 365 authentication (OAuth)** | yes | partial | unit | no | Token refresh implemented, scope documentation corrected to IMAP.AccessAsUser.All |
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
| **Automatic retry with backoff** | yes | yes | integration | generic-lab | Transient failures auto-retry with bounded backoff (src/controller/batch_work_item.rs); also uses imapsync's native retry |
| **Rate limit detection** | yes | partial | unit | no | Pattern matching implemented for generic rate limits; provider-specific patterns NOT applied |
| **Connection exhaustion handling** | yes | partial | unit | no | Configured per provider; generic connection failures retried; provider-specific limits NOT applied |

---

## Operator Guidance

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Pre-migration risk report** | yes | partial | unit | no | Scale report is available through the headless `risk` command; automatic GUI/live gating remains pending |
| **Post-migration exception report** | yes | partial | unit | no | Report is available through the headless `post-report` command; automatic generation from durable live evidence remains pending |
| **Provider-specific runbooks** | yes | partial | unit | no | Runbooks are exposed through the headless `runbook` command; GUI workflow surfacing remains pending |
| **Recovery guidance** (7 scenarios) | yes | partial | unit | no | Fail-closed guidance is exposed through the headless `recovery-guidance` command; dashboard/UI integration remains pending |
| **Resume/recovery dashboard** | yes | no | unit | no | Data structures defined, NOT implemented in UI |

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
- Folder mapping and discovery
- Aggregate evidence (folder/message counts)
- Exception recording and acceptance
- OAuth token refresh (transport layer)
- Documentation validation

---

## What's Partially Working

⚠️ **Code exists but not fully wired to pipeline:**
- Content-level mismatch detection (the live path performs metadata reconciliation; body hashing is not enabled)
- Provider-context-specific error classification (generic provider-intelligence mapping is now applied; provider-specific context remains pending)
- Provider-specific throttling (not implemented; generic profile throttles are enforced)
- Pre/post-migration reports (available as explicit CLI exports, not automatically generated for every live run)
- Recovery guidance (available as explicit CLI output, not yet a UI dashboard)

⚠️ **Unit-tested but not integration-tested:**
- Native Dovecot engine execution (`doveadm sync`, native preflight, and native verification)
- OAuth token refresh (refreshes correctly, not tested end-to-end with actual IMAP session)
- Provider-specific error handling (classifies correctly, not tested against real errors)
- Confidence scoring algorithms (match correctly, not validated with mixed message sets)

---

## What Doesn't Exist

❌ **Not implemented:**
- Message-level checkpoint persistence (per-message durability)
- Provider throttling enforcement (adaptive rate limiting)
- Automatic retry with provider-specific backoff
- Pre-migration risk report generation during migration
- Post-migration exception report generation
- Resume/recovery dashboard UI
- Provider-specific runbook exposure in UI
- Real provider integration testing (requires live credentials)
- Crash recovery for message-level checkpoints

---

## Known Limitations

1. **Message-level verification is metadata reconciliation, not content proof**
   - Wired after successful TLS imapsync transfers for the full selectable-folder inventory
   - Uses Message-ID, INTERNALDATE, and RFC822.SIZE; it does not hash message bodies
   - Bounded fetch pages, account/message limits, and any unstable folder fail closed; partial account evidence is not emitted
   - The one-million-record and estimated 256 MiB limits are admission guards, not peak-memory guarantees; SQLite-backed streaming reconciliation is a production blocker for very large MSP migrations
   - Mismatch rows are durably committed with terminal evidence and rendered in the operator verification report; per-message checkpoint persistence remains future work

2. **Provider throttling is not enforced**
   - Configurations defined for Gmail, O365, Fastmail
   - Adaptive throttling not applied to actual IMAP operations
   - Imapsync's built-in backoff is used instead

3. **OAuth works for token refresh, but needs live testing**
   - Scopes corrected (Gmail, O365)
   - Token refresh implemented
   - Not tested end-to-end with actual IMAP session
   - Awaits real provider validation

4. **Operator guidance is CLI-only**
   - Runbooks are exposed through `runbook`, but not embedded in the GUI workflow
   - Recovery guidance is exposed through `recovery-guidance`, but not presented automatically during interruption
   - Pre-migration risk assessment exists but not integrated into workflow

---

## Implications for MSPs

**Safe to use for:**
- Technical previews on non-critical mailboxes
- Testing provider connectivity and configuration
- Validating IMAP transfer mechanics
- Early adoption with careful operator oversight

**Not yet ready for:**
- Unattended migrations (no adaptive throttling or recovery approval)
- Large-scale deployments without a qualified provider pilot (message metadata verification is bounded, materializes multiple in-memory indexes, and requires live-provider validation)
- Very large MSP migrations before SQLite-backed streaming reconciliation is implemented
- Critical customer mailboxes (lacking live provider validation)
- Automated migrations (recovery guidance not surfaced)

---

## Path to Production

To reach GA 1.0, the following work is required:

**Immediate (Before shipping v0.1):**
- [x] Wire metadata-level message verification into the imapsync migration pipeline
- [ ] Add content-fingerprint verification and durable per-message checkpoints
- [ ] Implement message-level checkpoint persistence
- [ ] Add recovery guidance to operator UI
- [ ] Validate OAuth with real provider accounts
- [ ] Test provider throttling with real connections

**Short-term (v0.2):**
- [ ] Live provider validation (Gmail, O365, Fastmail)
- [ ] Integrate pre/post-migration reports into UI
- [ ] Expose provider runbooks in UI workflow
- [ ] Implement resume/recovery dashboard
- [ ] Provider-specific error classification in retry logic

**Medium-term (v1.0 GA):**
- [ ] Implement SQLite-backed streaming reconciliation; this is a production blocker for very large MSP migrations
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
