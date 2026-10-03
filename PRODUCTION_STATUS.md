# MailSwiftSync Production Readiness Status

<!-- release-metadata:begin -->
| Field | Value |
|---|---|
| Status | Technical Preview |
| Package version | `0.1.0-alpha.1` |
| SQLite schema version | `21` |
| Last reviewed | 2026-10-02 |
| Qualified engine | `imapsync 2.314` |
<!-- release-metadata:end -->
**Test Coverage:** See the CI-generated test summary artifact for the current
target-specific test inventory and execution result.
**Code Maturity:** Technical Preview; several advertised subsystems remain dormant prototypes

Capability claims are authoritative in [`CAPABILITY_MANIFEST.md`](CAPABILITY_MANIFEST.md).
This document provides operational context and release guidance without duplicating
the capability inventory.

> **Adoption-critical clarification:** A passing unit or integration test for a
> library module does not mean that module participates in a live migration.
> The matrix below distinguishes executable-path integration from prototype
> coverage. Live encrypted imapsync runs perform bounded metadata reconciliation
> by default, with an explicit bounded body-hash mode available for forensic runs.

## Executive Summary

MailSwiftSync is suitable for controlled technical-preview deployments. The
live system provides:

1. **Aggregate verification** from engine summaries and mailbox status
2. **Bounded metadata-level message reconciliation** for supported TLS imapsync plans
3. **Provider endpoint presets** for Gmail, Microsoft 365, Fastmail, and others
4. **Safe recovery** from interrupted migrations with durable checkpoints
5. **Typed controller failure handling** and bounded retries
6. **Operator documentation** for setup and troubleshooting

The primary blockers for GA are live provider validation, policy-aware
verification for non-default migration plans, and scalable reconciliation for
very large accounts.

> **Integration status:** Prototype modules remain explicitly marked below.
> The live migration path must not claim capabilities that are only covered by
> library tests; supported verification behavior is limited to the documented
> TLS imapsync metadata path.

---

## Feature Completeness Matrix

The authoritative release feature matrix is generated from `capabilities.toml`.

<!-- production-features:begin -->
| Feature | Status | Evidence |
|---|---|---|
| Pre/post-migration reports | implemented | Explicit report views and durable snapshot exports are wired; automatic post-run generation is not claimed. |
| Resume/recovery dashboard | implemented | Recovery workspace and durable recovery guidance are wired; autonomous resume remains operator-controlled. |
| Provider error classification | implemented | Provider identity is enforced at the execution boundary; documented provider signatures are scoped to the selected provider while RFC IMAP response codes remain universal. Live provider qualification remains pending. |
| Shared durable batch plans | implemented | Schema v21 stores one normalized batch plan per distinct policy plus mailbox identity/credential deltas and an approval-gated cutover workflow, alongside the durable webhook outbox and transaction-bound lifecycle event trigger. |
| Run-level batch throttle policy | implemented | Current concurrency and throughput settings are snapshotted, confirmed, and applied when rebuilding durable mailbox rows. |
| Adaptive launch and worker ceilings | implemented | Global launch and worker ceilings are configurable with conservative bounds; launch pressure halves on observed capacity failures and recovers additively under the configured ceiling, while provider and tenant rate domains adapt from observed signals. |
| Provider qualification packs | planned | Qualification evidence schema and procedures exist, but bundled live provider-pair packs are not present. |
| Signed migration certificate | implemented | The certificate command exports only durably completed customer evidence and atomically publishes an Ed25519-signed certificate; the artifact explicitly remains an authenticated ledger claim rather than independent message-level attestation. |
| Migration simulation | partial | Plan risk assessment now shows observed source/destination quota facts, RFC 7889 APPENDLIMIT maximum-message facts, and explicit unknown-capacity states; provider-specific folder-limit and complete destination-capacity simulation is not yet guaranteed. |
| Automatic destination capacity checks | partial | Quota parsing, RFC 7889 APPENDLIMIT observation, explicit unknown-capacity warnings, and destination quota blocking exist where IMAP data is exposed; provider-wide capacity discovery is not qualified. |
| Advanced typed folder mapping | partial | The advanced plan editor now supports bounded exact source-to-destination mappings and source-folder exclusions; rules are immutable plan state, emitted as imapsync --f1f2/anchored --exclude arguments, and reused by independent verification. Arbitrary regex transforms remain unsupported. |
| Executable cutover orchestration | partial | Durable approval-gated Seed, Catch-up, Final Delta, and Verification stages are executable through the cutover CLI with persisted schedule/window enforcement; external MX/DNS confirmation remains an explicit operator acknowledgement. |
| Organization policy enforcement | partial | Owner-only organization-policy.toml enforcement covers TLS, destination mutation, minimum verification, and concurrency at preflight/batch admission; provider-tenant quotas and role-scoped policy administration remain open. |
| Durable signed webhook delivery | partial | HTTPS webhook delivery now has deterministic event IDs, idempotency headers, optional HMAC signatures, transaction-bound lifecycle event production, and a SQLite outbox with bounded retry/backoff/dead-letter state; notify-webhook --watch provides continuous operator-managed delivery while fleet-centralized management remains open. |
<!-- production-features:end -->

## Provider Qualification Matrix

Generated from `capabilities.toml`; provider presets and generic lab results do
not become live qualification without reviewed evidence.

<!-- provider-qualification:begin -->
| Provider pair | Status | Last qualified | Tested engine | Limitations |
|---|---|---|---|---|
| Google Workspace → Microsoft 365 | not qualified | none | `imapsync 2.314` | No live provider-pair evidence is bundled. |
| Microsoft 365 → Google Workspace | not qualified | none | `imapsync 2.314` | No live provider-pair evidence is bundled. |
| Fastmail → Generic IMAP | not qualified | none | `imapsync 2.314` | No live provider-pair evidence is bundled. |
| Generic IMAP → Generic IMAP | generic lab only | generic lab fixtures | `imapsync 2.314` | Generic fixtures are not live provider qualification. |
<!-- provider-qualification:end -->

## Detailed Evidence Notes

### Core Verification ⚠️ METADATA-LEVEL BY DEFAULT; OPT-IN BODY PROOF UNQUALIFIED
| Feature | Status | Evidence |
|---------|--------|----------|
| Message-level mismatch detection | ✅ Wired for TLS imapsync | Post-transfer account verifier calls `MessageVerification`; live provider qualification remains pending |
| Multi-factor matching | ✅ Wired for TLS imapsync | Message-ID plus metadata fallback runs against independently fetched source/destination records |
| Source/destination message extraction | ✅ Wired for TLS imapsync | Bounded authenticated IMAP LIST/SELECT/UID FETCH path; plain IMAP fails closed |
| Missing/extra/changed detection | ✅ Wired | Durable mismatch rows commit with terminal evidence and render in the operator verification report; GUI pagination remains limited |
| Durable aggregate evidence storage | ✅ Wired | SQLite schema version is generated above; aggregate and supplied message counters and per-attempt transfer-pass provenance survive reports |
| Plan-aware verification modes | ⚠️ Fail-closed | `automap`, `justfolders`, `addheader`, disabled internal-date sync, and allowed size mismatches refuse independent exact message evidence; bounded body proof is available only for stable metadata-preserving plans |
| Bounded body-content proof | ⚠️ Wired, not provider-qualified | Explicit encrypted-imapsync mode hashes bounded RFC822 bodies on both sides, persists only the proof classification/mismatches, and fails closed on coverage or byte-budget violations |

### Provider Support ⚠️ PRESETS, NOT PROVIDER INTEGRATIONS
| Provider | Status | Coverage |
|----------|--------|----------|
| Gmail/Workspace | ⚠️ Endpoint preset | GUI/CLI delegated OAuth with PKCE is wired using an operator-registered app; no provider-specific execution intelligence or live qualification evidence |
| Microsoft 365 | ⚠️ Endpoint preset | GUI/CLI delegated OAuth with PKCE is wired using an operator-registered app; no provider-specific execution intelligence or live qualification evidence |
| Fastmail | ⚠️ Endpoint preset | No provider-specific intelligence is wired into execution |
| Generic IMAP | ✅ Generic path | Uses typed plan controls and controller retry behavior |

### Operator Guidance ✅ COMPLETE
| Document | Status | Content |
|----------|--------|---------|
| PROVIDER_SETUP.md | ✅ | Step-by-step setup for all providers |
| OAUTH_SETUP.md | ✅ | OAuth token lifecycle and configuration |
| provider_runbooks.rs | ✅ CLI + partial GUI | `mailswiftsync runbook <source-provider> <destination-provider>`; Migration plan includes a read-only provider checklist |
| provider_testing_guide.md | ✅ | How to validate providers with live accounts |

### Error Handling ⚠️ PARTIAL
| Scenario | Status | Handling |
|----------|--------|----------|
| Rate limiting | ⚠️ Configured limits + observed adaptive cooldown | Profile-supplied imapsync limits apply; batch launches now apply bounded endpoint-scoped cooldowns after observed capacity/rate-limit signals, but provider-specific quotas remain unqualified |
| Network timeouts | ✅ | Process and webhook timeout paths are wired |
| Authentication failures | ✅ | Clear error with remediation steps |
| Connection exhaustion | ⚠️ Partial | Generic bounded concurrency and failure handling; no provider-specific connection-pool controller |
| Provider unavailability | ⚠️ Partial | Provider-aware classification and bounded adaptive cooldowns are wired at batch execution; live provider qualification and provider-specific connection-pool guarantees remain open |

### Recovery & Durability ✅ DURABLE CORE; QUALIFICATION OUTSTANDING
| Feature | Status | Implementation |
|---------|--------|-----------------|
| Checkpoint persistence | ⚠️ Partial | Run-level state and UIDVALIDITY-bound Dovecot checkpoints are durable; per-message transfer checkpoints remain incomplete |
| Transfer attempt history | ✅ Initial layer | Engine attempt start/finish and bounded failure class are durably recorded and exposed in customer-proof run metadata; this is not a per-message resume index |
| Resume from interruption | ⚠️ Engine-dependent | Dovecot resumes from its validated opaque engine state; imapsync reruns idempotently and may rescan/revisit prior work |
| Crash recovery | ✅ | Startup process identity/recovery paths are wired |
| Time-to-completion estimates | ⚠️ Estimate for imapsync only | Activity derives bounded rolling throughput and an explicitly labelled ETA from imapsync progress; Dovecot runs do not expose transfer progress and therefore do not produce an ETA |
| Recovery guidance | ⚠️ Partial GUI | Activity renders localized fail-closed guidance for interruption, transport, and throttling attention states; configuration and verification findings retain their dedicated remediation views |

---

## Testing Coverage

Test counts and pass/fail status are generated by CI and reported in each release rather than maintained in this document. The local command remains `cargo test --locked --all-targets --all-features`.

**Coverage areas include:**
- Core verification logic
- Provider intelligence (observed-signal classification prototype)
- Pre/post-migration reports
- Recovery state management
- Runbook generation
- Verification detail formatting
- Documentation smoke tests (required files, safety-critical wording)
- Provider/authentication guidance smoke checks
- Message verification scenarios (exact match, missing/extra/mismatched messages, duplication, large migrations, special characters, large attachments)

These are model/scenario tests. They establish reconciliation behavior but do
not invoke the MailSwiftSync binary, SQLite persistence, or report generation.
The packaged IMAP/controller labs are separate product-level integration
tests. The controller labs exercise durable failure and recovery; the IMAP
lab must be run with Dovecot and qualified imapsync installed to exercise the
new metadata verifier against real accounts.

---

## Known Limitations

### Live Provider Testing [REQUIRES CREDENTIALS]
- [ ] Real Gmail migration end-to-end
- [ ] Real O365 migration end-to-end
- [ ] Real Fastmail migration end-to-end
- [ ] Provider-specific edge cases (quota full, folder limits, etc.)
- [ ] Rate limit throttling validation

**Status:** Code paths and controller wiring verified, awaiting live provider validation

### Code Refactoring [QUALITY IMPROVEMENT]
- [ ] Continue modularizing the core and application entry point as needed; file sizes are intentionally not maintained as status claims.
- [ ] Batch controller context structs

**Status:** Functional and tested, not blocking production use

### Documentation [OPERATIONAL IMPROVEMENT]
- [ ] Compatibility matrix with live test results
- [ ] Provider-specific troubleshooting guides
- [ ] MSP operational runbooks

**Status:** Foundation in place, can be extended from real-world usage

---

## Security Assessment

### Credential Handling ✅ VERIFIED
- ✅ Passwords stored in OS keyring, not in files
- ✅ OAuth tokens refreshed automatically, stored securely
- ✅ Support bundle sanitizes credentials before export
- ✅ No passwords in logs, events, or reports
- ⚠️ Provider-specific authentication guidance is documented; live validation remains pending

### TLS/Transport ⚠️ NATIVE PROBE VERIFIED; EXTERNAL ENGINE SEPARATE
- ✅ IMAPS (port 993) with certificate validation
- ✅ STARTTLS (port 143) supported
- ✅ Native MailSwiftSync probes use Rustls, certificate roots, and hostname verification
- ⚠️ External imapsync transfer uses its own Perl/SSL runtime; no universal TLS-minimum policy is claimed across both stacks

### File Operations ✅ VERIFIED
- ✅ Cross-platform atomic file replacement (Windows ReplaceFileW, Unix rename)
- ✅ Temporary files use UUIDs (not predictable)
- ✅ State-directory and SQLite opens reject untrusted directory boundaries and
  final-component symlinks; remaining same-UID filesystem races are not
  claimed as fully eliminated.
- ✅ Directory sync after writes (durability)

### Audit & Compliance ✅ VERIFIED
- ✅ Migration lifecycle, state, and aggregate evidence events are stored durably; verbose engine transcripts remain bounded diagnostics
- ✅ Operator acceptance recorded with timestamp
- ✅ No message content in logs
- ✅ Customer proof export with integrity validation
- ✅ Support bundle for diagnostics

---

## Deployment Readiness

### For Technical Preview (Now)
- ⚠️ Core migration path is tested; dormant prototype modules are not operational features
- ⚠️ Operator guides are maintained, but provider-specific live validation remains pending
- ✅ Error messages clear and actionable
- ✅ Recovery procedures documented

### For Early Adoption (Recommended)
- ✅ Use with test/disposable mailboxes first
- ✅ Validate provider configuration in preflight
- ✅ Run incremental delta after full migration
- ✅ Review verification reports carefully
- ✅ Have rollback plan ready

### For GA 1.0 (After Live Testing)
- ⏳ Successful dry pilots with real accounts
- ⏳ Successful live pilots with real data
- ⏳ Recovery/interruption testing with production accounts
- ⏳ Provider-specific edge case validation
- ⚠️ Synthetic durable SQLite reconciliation benchmark measured 100k messages per endpoint, and the current Linux release host passes the importer/UI/reload scale gates; end-to-end large-mailbox/provider load qualification (100k+ messages) remains outstanding. See [verification envelope](docs/verification-envelope.md).

---

## Feature Roadmap

### Completed (This Release)
- ✅ TLS imapsync metadata-level message reconciliation wired into live runs; bounded opt-in body proof is wired but remains provider-unqualified
- ⚠️ Observed provider-signal classification with bounded endpoint-scoped adaptive launch cooldown; live provider qualification remains pending
- ✅ Durable controller recovery and maintenance-window supervision
- ✅ Recovery dashboard/planner CLI command (`recovery-guidance`)
- ✅ Provider runbook generation CLI command (`runbook`) and read-only provider checklist in the Migration plan
- ✅ Post-migration report export is live-wired to one consistent durable project snapshot; it remains fail-closed when evidence or counters are incomplete
- ✅ Comprehensive setup documentation
- ✅ OAuth token lifecycle management
- ✅ Automated test suite enforced by CI; counts are published per target

### Recommended (Next Release)
- 🔲 Live provider validation (Gmail, O365, Fastmail)
- 🔲 Real-world performance benchmarks
- 🔲 MSP operational runbooks
- 🔲 Advanced folder mapping rules

### Future (Post-1.0)
- 🔲 More providers (Yahoo, ProtonMail, etc.)
- 🔲 Cloud storage (OneDrive, Google Drive) migration
- 🔲 Calendar/contacts migration
- 🔲 Machine learning for duplicate detection
- 🔲 Multi-tenant MSP dashboard

---

## Recommended Next Steps

### For MSPs / Consultants
1. Review PROVIDER_SETUP.md for your provider
2. Run preflight on test mailboxes
3. Perform dry run and review findings
4. Execute live migration on small batch
5. Review MailSwiftSync verification report
6. Accept exceptions or retry if needed
7. Run incremental delta to catch new mail
8. Export customer proof for audit trail

### For Developers
1. Run test suite: `cargo test --locked --all-targets --all-features`
2. Review PROVIDER_SETUP.md and OAUTH_SETUP.md
3. Check provider_runbooks.rs for setup requirements
4. Examine message_verification.rs for mismatch detection logic
5. Examine recovery_dashboard.rs for interruption handling
6. Read architecture.md for system design overview

### For Operators
1. Start with test/disposable mailboxes
2. Use CLI `mailswiftsync runbook <source-provider> <destination-provider>` for setup guidance
3. Monitor MailSwiftSync logs during migration
4. Review verification report after completion
5. Accept exceptions as needed
6. Export customer proof for records
7. Validate with provider tools if needed

---

## Support & Feedback

### Reporting Issues
Open an issue at: https://github.com/cyberducttape/MailSwiftSync/issues

Include:
- Operator-facing error message (if applicable)
- Support bundle: `mailswiftsync support-bundle [state.db]`
- Provider (Gmail, O365, etc.)
- Mailbox size (approximate message count)

### Security Vulnerabilities
Please report privately: See SECURITY.md

### Feature Requests
Open an issue with: `[FEATURE REQUEST]` prefix

---

## Version Information

The generated release metadata at the top of this document is authoritative for
product version, status, schema version, review date, and qualified engine.
Other engine versions may transfer, but their output cannot provide trusted
MailSwiftSync verification evidence; the packaged native-Dovecot CI fixture now
passes, while live provider and target-storage qualification remain pending.

---

## Conclusion

MailSwiftSync is **ready for technical preview deployments** with the following caveats:

1. **Use with test/disposable mailboxes initially** — Validate configuration and recovery procedures
2. **Have provider test accounts available** — Setup and preflight validation require real credentials
3. **Review verification evidence carefully** — Metadata runs do not claim body equality; forensic body-hash runs are bounded and labeled separately
4. **Follow provider-specific runbooks** — Each provider has unique requirements

The system provides a durable, safety-gated migration controller suitable for
technical-preview use. It provides bounded, opt-in body-content proof only for
encrypted imapsync runs; it does not yet provide provider-specific execution
intelligence; provider runbooks are guidance only, not provider-specific
execution or qualification. Activity exposes an explicitly labelled imapsync
ETA, while native Dovecot runs remain progress- and ETA-free.

**Next milestone:** Live validation with real provider mailboxes to reach GA 1.0 status.
