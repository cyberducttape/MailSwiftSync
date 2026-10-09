# Adoption qualification plan

This plan is the evidence gate for replacing or supplementing direct imapsync
operations. A passing unit test, generic lab fixture, or successful process
exit is not a qualification result.

## Required scenario matrix

Each scenario must record the exact MailSwiftSync commit, OS/distribution,
Rust build profile, packaged imapsync version and digest, Dovecot/provider
version, configuration digest, source/destination transport and auth mode,
mailbox generator seed, and test artifact digests.

| Scenario | Required cases |
|---|---|
| Size | 1 GiB, 10 GiB, 50 GiB, and 100 GiB mailboxes |
| Message shape | High message count, duplicate-heavy Message-IDs, large attachments, missing metadata, unusual and Unicode folder names |
| Recovery | Controller crash, worker termination, engine interruption, power-loss simulation, restart inspection before resume, and stale process identity |
| Incremental | Repeated seed/catch-up/final-delta passes with delivery and flag changes between passes |
| Concurrent mutation | Source delivery, expunge, destination edits, folder changes, and UIDVALIDITY change attempts during transfer/verification |
| Provider pairs | Each claimed provider pair separately, including hosted cPanel/Dovecot, Google Workspace, and Microsoft 365 where applicable |

Every case needs a signed result bundle containing the transfer result and
independent verification result. `completed` means the engine reported a
successful transfer; it must never be rendered as `verified` unless the
configured verifier produced authoritative evidence for the same run and plan.

## Required measurements

Capture wall-clock duration, source/destination message and byte totals,
effective transfer throughput, retries and retry time, rediscovery work,
verification duration, peak RSS by controller/engine/container, CPU, disk
growth, provider throttling/cooldown duration, process count, and recovery
time. Compare each case with the exact same imapsync version and equivalent
engine settings run directly. Report MailSwiftSync overhead separately from
engine and verification work; an overhead increase is acceptable only when
the evidence/recovery benefit is stated and reviewed.

## Release acceptance

- No scenario may have an unexplained message or folder difference.
- Transfer completion and independent verification are separate fields in the
  result and customer artifact.
- Interrupted work must be inspectable before resume and must not create an
  unowned process or false completion.
- A failed or incomplete verification must remain review/attention state.
- Qualification artifacts are immutable, signed, reproducible from their
  recorded versions, and linked to the compatibility matrix.
- Results that pass only on disposable generic IMAP fixtures remain generic
  lab evidence and do not qualify hosted providers.
