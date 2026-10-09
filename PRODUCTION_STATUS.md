# MailSwiftSync Production Readiness Status

<!-- release-metadata:begin -->
| Field | Value |
|---|---|
| Status | Technical Preview |
| Package version | `0.1.0-alpha.1` |
| SQLite schema version | `29` |
| Last reviewed | 2026-10-03 |
| Qualified engine | `imapsync 2.314` |
<!-- release-metadata:end -->
**Test Coverage:** See the CI-generated test summary artifact for the current
target-specific test inventory and execution result.
**Code Maturity:** Technical Preview; live provider qualification, representative large-scale execution, and per-message transfer checkpoints remain outstanding

Capability claims are authoritative in [`CAPABILITY_MANIFEST.md`](CAPABILITY_MANIFEST.md).
This document provides operational context and release guidance without duplicating
the capability inventory.

> **Adoption-critical clarification:** A passing unit or integration test for a
> library module does not mean that module participates in a live migration.
> The matrix below distinguishes live-path integration from component-level
> test coverage. Live encrypted imapsync runs perform bounded metadata reconciliation
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
| Shared durable batch plans | implemented | The SQLite schema stores one normalized batch plan per distinct policy plus mailbox identity/credential deltas and an approval-gated cutover workflow, alongside the durable webhook outbox and transaction-bound lifecycle event trigger. |
| Run-level batch throttle policy | implemented | Current concurrency and throughput settings are snapshotted, confirmed, and applied when rebuilding durable mailbox rows. |
| Adaptive launch and worker ceilings | implemented | A configurable global process-launch ceiling is supplemented by adaptive mailbox, credential, tenant, provider-endpoint, and global token buckets; attributed capacity failures reduce only implicated domains and escalate when broader scopes are implicated. Worker concurrency adapts across the same hierarchy, while configured imapsync message/byte limits remain run-level policy. No provider quota defaults are encoded or live-qualified. |
| Provider qualification packs | partial | Qualification evidence schema, strict phase-evidence validation, and a pack builder bound to one provider pair, release commit, engine binary, proof set, and explicit limitations are implemented. Real automated provider-pair qualification jobs have not yet produced signed evidence, so no live qualification pack is bundled. |
| Signed migration certificate | implemented | The certificate command exports only durably completed customer evidence and atomically publishes an Ed25519-signed certificate; evidence exposes each mailbox's immutable plan-snapshot SHA-256 and, when captured as a SHA-256 in that snapshot, its execution-engine binary digest. The artifact remains an authenticated ledger claim rather than independent message-level attestation. |
| Migration simulation | partial | Plan risk assessment now shows observed source/destination quota facts, RFC 7889 APPENDLIMIT maximum-message facts, and explicit unknown-capacity states; provider-specific folder-limit and complete destination-capacity simulation is not yet guaranteed. |
| Automatic destination capacity checks | partial | Quota parsing, RFC 7889 APPENDLIMIT observation, explicit unknown-capacity warnings, and destination quota blocking exist where IMAP data is exposed; provider-wide capacity discovery is not qualified. |
| Advanced typed folder mapping | partial | The advanced plan editor supports bounded exact source-to-destination mappings and source-folder exclusions; rules are immutable plan state, emitted as imapsync --f1f2/anchored --exclude arguments, and reused by independent verification. Explicit destination targets that collide after case folding are rejected to prevent unintended folder merges. Provider-specific normalization and arbitrary regex transforms remain unsupported. |
| Executable cutover orchestration | partial | Durable approval-gated Seed, Catch-up, Final Delta, and Verification stages are executable through the cutover CLI with persisted schedule/window enforcement; external MX/DNS confirmation remains an explicit operator acknowledgement. |
| Organization policy enforcement | partial | Owner-only organization-policy.toml enforcement covers TLS, destination mutation, minimum verification, global worker concurrency, and provider-endpoint/tenant/credential runtime ceilings at preflight and batch admission; provider quota discovery and role-scoped policy administration remain open. |
| Durable signed webhook delivery | partial | HTTPS webhook delivery now has deterministic event IDs, idempotency headers, optional HMAC signatures, transaction-bound lifecycle event production, and a SQLite outbox with bounded retry/backoff/dead-letter state; notify-webhook --watch provides continuous operator-managed delivery while fleet-centralized management remains open. |
| MSP RBAC and tenant isolation | not implemented | The local application has no authenticated multi-user control plane, role enforcement, row-level tenant boundary, or worker fencing; see `docs/msp-rbac-and-tenant-isolation.md`. |
| PSA/ticket lifecycle integration | partial | The signed/idempotent durable webhook and lifecycle event contract are available for receiver-side automation; ticket ownership, correlation, acknowledgements, and vendor API integrations remain outside the product. |
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
| Gmail/Workspace | ⚠️ Endpoint preset | GUI/CLI delegated OAuth with PKCE and provider-scoped failure classification are wired; provider quota defaults and live qualification evidence are absent |
| Microsoft 365 | ⚠️ Endpoint preset | GUI/CLI delegated OAuth with PKCE and provider-scoped failure classification are wired; provider quota defaults and live qualification evidence are absent |
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
| Rate limiting | ⚠️ Configured limits + observed hierarchical adaptation | A configurable global process-launch ceiling is supplemented by adaptive mailbox, credential, tenant, provider-endpoint, and global token buckets; attributed capacity failures reduce only implicated domains and escalate when broader scopes are implicated. Worker concurrency adapts across the same hierarchy, while configured imapsync message/byte limits remain run-level policy. No provider quota defaults are encoded or live-qualified. |
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
- Provider intelligence (observed-signal classification wired; live provider qualification pending)
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

**Status:** Functional and tested; additional refactoring remains outside the technical-preview gate

### Documentation [OPERATIONAL IMPROVEMENT]
- [ ] Compatibility matrix with live test results
- [ ] Provider-specific troubleshooting guides
- [x] MSP operations procedure for customer scoping, change approvals, wave controls, incident handling, and evidence retention; centralized fleet control remains unavailable.
- [ ] Central MSP control plane with worker registration, heartbeats, distributed job ownership, remote controls, tenant isolation, and RBAC; `fleet-status` is intentionally read-only aggregation only.

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
- ⚠️ Core migration path is tested, but this does not qualify live providers, non-default verification policies, or large-scale provider behavior
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
- ⏳ Signed real-provider qualification artifacts for Gmail/Workspace ↔
  Microsoft 365 routes; generic IMAP fixtures and synthetic reconciliation do
  not satisfy this gate
- ⏳ Hypervisor power-cut campaign with durable-ledger and evidence comparison
- ⏳ Real-engine 1/4/8/16-worker concurrency/resource matrix
- ⏳ Capacity-planning evidence for the 1,000,000-message metadata and
  100,000-message body-proof envelopes, including the shared body-byte budget;
  unknown capacity remains fail-closed rather than scale-qualified
- ⚠️ The packaged-engine scale lab completed both hosted generic-IMAP scenarios on 2026-10-04: 100,016 messages / 28.7 MB in 1,045 s (≈1.17 GiB sampled peak container memory) and 10,016 messages / 1.08 GB of bodies in 90 s (≈349 MiB), each with seed, incremental delta, exact metadata reconciliation, and zero unresolved differences. Per-process attribution shows the MailSwiftSync controller at 25–29 MiB; imapsync (≈451 MiB for one 100k-message folder) and reclaimable page cache account for the rest. This is not a 20 GiB, multi-migration, or hosted-provider qualification; Gmail/Microsoft 365 qualification remains outstanding. See [release readiness](docs/release-readiness.md) and the [100k run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37246504219) and [1 GiB run](https://github.com/cyberducttape/MailSwiftSync/actions/runs/37246501700).
- ⚠️ CI scheduler stress coverage settles 1,024 synthetic mailbox jobs across 32 tenant domains at 16 workers; this does not exercise concurrent IMAP engines or qualify provider limits.

---

## Feature Roadmap

### Completed (This Release)
- ✅ TLS imapsync metadata-level message reconciliation wired into live runs; bounded opt-in body proof is wired but remains provider-unqualified
- ⚠️ Observed provider-signal classification with hierarchical adaptive launch buckets and worker cooldowns; live provider qualification remains pending
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
- ✅ MSP single-customer operational procedure ([runbook](docs/msp-operations-runbook.md)); centralized fleet controls and tenant isolation remain future work.
- 🔲 Complete provider-specific folder and capacity simulation beyond exact mappings and exclusions

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
encrypted imapsync runs and provider-scoped failure classification with
adaptive rate-domain feedback. This execution intelligence is limited to
observed provider signals: provider-specific quota defaults and live provider
qualification remain absent. Provider runbooks are guidance only, not provider
qualification. Activity exposes an explicitly labelled imapsync ETA, while
native Dovecot runs remain progress- and ETA-free.

**Next milestone:** Live validation with real provider mailboxes to reach GA 1.0 status.
