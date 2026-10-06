# Release upgrade and provider-failure validation

## Persisted schema upgrade matrix

The unit suite exercises migration mechanics, including legacy schema shapes,
current-schema repair, readonly migration copies, backup creation, and
rollback-preserving upgrades. Those tests are not a substitute for opening
databases written by each released binary.

Before an RC, create owner-only SQLite fixtures with the exact binaries below.
Each fixture must contain real persisted states, not an empty database:

| Source fixture | Target binary | Required states |
|---|---|---|
| 0.8 release | 0.9 release | queued, running, cancelled, verification difference, verified, recovery guidance, webhook outbox |
| 0.9 release | RC release | the same states plus active wave/batch rows and transfer-pass provenance |
| RC release | 1.0 release | the same states plus signed customer certificate, backup, and partially staged verification |

For every upgrade, verify schema version, SQLite integrity, migration backup,
row counts, foreign-key/index invariants, durable state transitions, evidence
history, recovery output, and customer-proof verification. Then resume one
interrupted mailbox and one interrupted batch from each fixture, and compare
the final state with a clean control run. Preserve the fixture digest and
upgrade logs with the release evidence.

Until these binary-produced fixtures exist, the project should claim
backward-compatible migration coverage from the automated legacy-schema tests,
not proven 0.8→0.9→RC→1.0 persistence compatibility.

## Provider failure matrix

Provider qualification must inject or observe each failure class on every
provider pair and record the exact engine output, classified failure, retry
decision, durable state, operator guidance, and final verification result:

- throttling and HTTP/IMAP 429-style responses;
- destination quota exhaustion and APPEND limits;
- access-token expiry and refresh-token rotation during a long run;
- temporary IMAP disconnects and reconnect exhaustion;
- server-side folder-count/name/size limits;
- messages exceeding provider or engine size limits; and
- suspended, disabled, or policy-blocked accounts.

These cases are release evidence only when exercised against the real provider
or a faithful provider-controlled test tenant. Unit signatures and generic
IMAP fixtures validate classification plumbing, not provider behavior.
