# MailSwiftSync Capability Manifest

**Generated from code analysis and test inventory**  
**Last verified:** 2026-09-20

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
| IMAP message transfer (Dovecot) | yes | yes | integration | pending | Engine: Dovecot 2.3+ |
| Folder/label mapping | yes | yes | integration | pending | Provider-specific namespace handling |
| Message extraction (imapsync) | yes | yes | integration | no | Parses debug output for message UIDs/sizes |
| Message extraction (Dovecot) | yes | yes | integration | no | Uses doveadm fetch output |

---

## Verification Features

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Aggregate evidence** (folder/message counts) | yes | yes | integration | generic-lab | Works on local Dovecot fixture |
| **Message-level mismatch detection** | yes | no | unit+integration | no | Code complete, NOT wired to migration pipeline |
| **Multi-factor confidence scoring** (100%-80%) | yes | no | unit | no | Implements matching algorithm, not used operationally |
| **Checkpoint persistence** per message | no | no | none | no | NOT implemented; evidence persists per run, not per message |
| **Crash recovery** | partial | partial | unit | no | Run-level recovery works; message-level recovery not wired |
| **Exception acceptance workflow** | yes | yes | unit | no | UI accepts exceptions, stored durably |

---

## Provider Support

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Gmail authentication (app password)** | yes | yes | unit | no | Standard IMAP auth |
| **Gmail authentication (OAuth)** | yes | partial | unit | no | Token refresh implemented, scope documentation corrected to https://mail.google.com/ |
| **Gmail throttling (100 msgs/sec)** | yes | no | unit | no | Throttle config defined, NOT applied to actual migration |
| **Microsoft 365 authentication (OAuth)** | yes | partial | unit | no | Token refresh implemented, scope documentation corrected to IMAP.AccessAsUser.All |
| **Microsoft 365 throttling (150 msgs/sec)** | yes | no | unit | no | Throttle config defined, NOT applied to actual migration |
| **Fastmail authentication (app password)** | yes | yes | unit | no | Standard IMAP auth |
| **Fastmail throttling (50 msgs/sec)** | yes | no | unit | no | Throttle config defined, NOT applied to actual migration |
| **Generic IMAP provider** | yes | yes | integration | generic-lab | Conservative IMAP implementation |

---

## Error Handling

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Provider error classification** (7 types) | yes | no | unit | no | Classifier exists, NOT integrated into retry logic |
| **Automatic retry with backoff** | yes | yes | integration | generic-lab | Imapsync's built-in retry, not MailSwiftSync-specific |
| **Rate limit detection** | yes | no | unit | no | Pattern matching implemented, NOT applied |
| **Connection exhaustion handling** | yes | no | unit | no | Configured per provider, NOT applied |

---

## Operator Guidance

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Pre-migration risk report** | yes | partial | unit | no | Code exists, generates warnings; NOT integrated into UI workflow |
| **Post-migration exception report** | yes | no | unit | no | Data structures exist, NOT generated during migration |
| **Provider-specific runbooks** | yes | no | unit | no | 6 provider pairs, NOT exposed in UI or CLI |
| **Recovery guidance** (7 scenarios) | yes | no | unit | no | Guidance text defined, NOT surfaced to operators |
| **Resume/recovery dashboard** | yes | no | unit | no | Data structures defined, NOT implemented in UI |

---

## Documentation

| Capability | Code | Wired | Tested | Live Provider | Notes |
|------------|------|-------|--------|---------------|-------|
| **Provider setup guides** | yes | n/a | n/a | n/a | Gmail, O365, Fastmail, generic IMAP |
| **OAuth configuration guide** | yes | n/a | n/a | n/a | Corrected scopes (Gmail, O365) |
| **Architecture documentation** | yes | n/a | n/a | n/a | Message-level verification design doc |
| **Documentation consistency tests** | yes | yes | 9 tests | n/a | Validates docs stay aligned with code |

---

## Test Inventory

| Category | Count | Status |
|----------|-------|--------|
| Core unit/integration tests | 372 | ✅ Passing |
| Documentation validation tests | 9 | ✅ Passing |
| Message verification scenarios | 14 | ✅ Passing |
| **Total** | **395** | ✅ 100% pass rate |

---

## What Actually Works

✅ **Fully integrated and tested:**
- Basic IMAP transfer (imapsync, Dovecot)
- Folder mapping and discovery
- Aggregate evidence (folder/message counts)
- Exception recording and acceptance
- OAuth token refresh (transport layer)
- Documentation validation

---

## What's Partially Working

⚠️ **Code exists but not wired to pipeline:**
- Message-level mismatch detection (algorithms work, not used operationally)
- Provider error classification (patterns defined, not applied)
- Provider throttling (configs exist, not enforced)
- Pre/post-migration reports (data structures exist, not generated)
- Recovery guidance (text exists, not surfaced)

⚠️ **Unit-tested but not integration-tested:**
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

1. **Message-level verification is a prototype**
   - Fully implemented and tested in isolation
   - Not wired to the actual migration pipeline
   - Would require checkpoint persistence per message (not implemented)
   - Would require UI integration (not implemented)

2. **Provider throttling is not enforced**
   - Configurations defined for Gmail, O365, Fastmail
   - Adaptive throttling not applied to actual IMAP operations
   - Imapsync's built-in backoff is used instead

3. **OAuth works for token refresh, but needs live testing**
   - Scopes corrected (Gmail, O365)
   - Token refresh implemented
   - Not tested end-to-end with actual IMAP session
   - Awaits real provider validation

4. **Operator guidance exists but isn't surfaced**
   - Runbooks written but not exposed in UI/CLI
   - Recovery guidance defined but not presented during interruption
   - Pre-migration risk assessment exists but not integrated into workflow

---

## Implications for MSPs

**Safe to use for:**
- Technical previews on non-critical mailboxes
- Testing provider connectivity and configuration
- Validating IMAP transfer mechanics
- Early adoption with careful operator oversight

**Not yet ready for:**
- Unattended migrations (no adaptive throttling or recovery)
- Large-scale deployments (message-level verification not wired)
- Critical customer mailboxes (lacking live provider validation)
- Automated migrations (recovery guidance not surfaced)

---

## Path to Production

To reach GA 1.0, the following work is required:

**Immediate (Before shipping v0.1):**
- [ ] Wire message-level verification into migration pipeline
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
- [ ] Performance benchmarks with 100k+ message mailboxes
- [ ] Provider edge case testing
- [ ] Production support runbooks
- [ ] Load testing with multiple concurrent migrations

---

## How to Update This Manifest

This manifest should be regenerated monthly from:
1. Code analysis (grep for implemented features)
2. Test inventory (cargo test --list)
3. Integration verification (features actually wired to pipeline)
4. Live provider testing (as credentials become available)

**Never make unsupported claims.** If a feature is unit-tested but not integrated, say so. If it's integrated but not live-tested, say so.
