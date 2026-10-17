# Production qualification environment

Use two separate environments:

1. A deterministic disposable Dovecot lab for repeatable product, recovery,
   storage, and parser tests.
2. Disposable real-provider accounts for provider-specific authentication,
   throttling, quota, reconnect, and namespace qualification.

Do not combine their results. A passing Dovecot fixture proves the controller
and generic IMAP path under that fixture; it does not qualify Gmail, Microsoft
365, hosted cPanel, or any other provider.

## Environment components

| Component | Required evidence |
|---|---|
| Dovecot source and destination | Version/configuration, mailbox generator seed, UIDVALIDITY and folder inventory |
| Packaged MailSwiftSync worker | Release commit, binary digest, schema version, execution profile |
| Packaged imapsync/Dovecot engine | Exact version and executable digest |
| Fault-injection layer | Injected condition, start/end time, affected connection, and recovery result |
| Resource monitoring | Controller/engine/container RSS, CPU, disk, network, process count, and timestamps |
| Artifact storage | Owner-only immutable evidence directory with signed manifest and retention owner |
| Disposable provider accounts | Provider tenant/account, auth consent, quotas, throttling observations, and teardown record |

The repository's `imap-integration-smoke.sh`, controller recovery/chaos labs,
engine storage-fault lab, scale workflows, process sampler, and provider
qualification-pack builder are the starting harness. A test is not release
evidence unless its artifacts identify the exact software and configuration
used.

## Mandatory scenario matrix

| Scenario | Expected result |
|---|---|
| 100,000-message transfer | Completes with bounded resources and accurate independent evidence |
| Interrupt at approximately 50% | Durable interrupted state is inspectable; resume has no false completion or unowned process |
| Controller restart during transfer | Process identity is recovered conservatively; duplicate execution is prevented or explicitly reviewed |
| OAuth token revoked/expired | Authentication failure is classified and stops/retries only under policy; no success is recorded |
| Destination quota exhausted | Actionable quota/capacity failure; ledger remains readable and non-verified |
| Provider throttles connections | Bounded retry budget, provider-aware cooldown, tenant isolation, and telemetry |
| TLS trust and identity failures | In both the Rustls probe and actual pinned imapsync transfer path, expired certificates, hostname mismatch, and untrusted CAs are rejected; a custom CA succeeds only for the intended certificate identity |
| STARTTLS downgrade and protocol floor | STARTTLS refusal/downgrade is rejected; record negotiated TLS version and peer certificate identity for probe and engine connections. The product currently makes no universal minimum-TLS-version claim across Rustls and Perl/IO::Socket::SSL |
| TLS reconnect and certificate rotation | A changed or wrong certificate is rejected on transfer reconnects; probe pinning is not treated as transfer enforcement, and pinned plans remain refused until the engine path is qualified |
| Source UIDVALIDITY changes | Existing checkpoint is rejected and a fresh scan is required |
| Destination receives new messages | Missing/extra/changed classification follows the verification policy |
| SQLite unavailable or corrupted | No false completion; status/recover fail closed; verified backup restores into a usable ledger |
| Worker loses network for 30 minutes | Bounded timeout/retry behavior and auditable terminal or retry state |
| Same-size content mutation | Detected only when supported content-proof mode covers the message; metadata-only evidence says so explicitly |
| Verification exceeds configured limits | Explicit incomplete/attention evidence; never an unqualified verified result |

## Evidence and promotion rules

For every scenario retain the raw harness log, sanitized status JSON, run and
plan IDs, transfer summary, independent verification result, resource summary,
fault schedule, and SHA-256 manifest. Sign the final qualification pack and
bind it to the release commit, engine identities, provider pair, and dataset
digest.

Promotion requires every expected result above. A failed or skipped scenario
must be visible in the qualification report and keeps the relevant provider
route or release gate blocked. Synthetic scale, unit tests, and a successful
process exit cannot substitute for provider or power-loss evidence.
