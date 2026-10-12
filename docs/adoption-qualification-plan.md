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

## Recent non-qualifying generic-lab evidence

The following reproducible lab runs are retained as engineering evidence
only; they do not satisfy the required size matrix, direct-imapsync comparison,
provider qualification, or signed-result acceptance criteria.

### 2026-10-12: 100k small-message run on current main

| Field | Observed value |
|---|---|
| GitHub Actions run / commit | [38215860881](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38215860881) / `55242c879118610b7d54043743c549e8197201ef` |
| Runner / runtime | `ubuntu-24.04`; Debian Bookworm container; Rust `1.92` release build |
| Fixture | Disposable generic IMAP / Dovecot `2.3.19.1 (9b53102964)`; deterministic sequential message IDs, 100,000 generated small messages plus 16 baseline messages |
| Packaged engine | imapsync `2.314`, SHA-256 `dda1a7380cdab8c8bfc981724ab7a158c041b1ef78ca559f47dc1fa377d0870` |
| Plan SHA-256 | `15f41abf60ee19a9100a281a16b4466a898ebe8ddc2c9b3eb6174ba02141b2dd` |
| Harness wall time | 813 seconds, including fixture setup, seed transfer, verification, re-verification, delta, and proof export; no direct-imapsync A/B baseline |
| Initial proof | 100,016 source/destination messages and 28,681,943 bytes; zero missing, extra, modified, failed, or unmatched; 100% FLAGS coverage; Level 2 metadata only, bodies not compared |
| Post-delta proof | 100,017 source/destination messages and 28,682,257 bytes; zero missing, extra, modified, failed, or unmatched; 100% FLAGS coverage; Level 2 metadata only, bodies not compared |
| Sampled resources | 1,269,162,835-byte sampled container peak; cgroup anonymous peak 532,496,384 bytes and file-cache peak 1,207,676,928 bytes; sampled MailSwiftSync RSS peak 29,798,400 bytes and imapsync Perl RSS peak 472,358,912 bytes |
| Artifact | [Actions artifact 11733295124](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38215860881/artifacts/11733295124), SHA-256 `c7ecfa4c0497e41a0dac3bc84ceb412b5d0e1bafdc332c3f493e73192dcff2ff`; expires 2027-01-10 |

This is unsigned CI evidence from a generic disposable fixture. The recorded
container and package digests make the run identifiable, but do not establish
provider compatibility, body integrity, or a direct-engine performance
comparison. The generated fixture is deterministic and has no separate random
seed. It is not a 1/10/50/100-GiB qualification.

| Field | Observed value |
|---|---|
| GitHub Actions run | [38197707897](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38197707897) |
| MailSwiftSync commit | `f99dfa4ed11596b5b219fa91bbaa76295e867270` |
| Fixture | Disposable generic IMAP / Dovecot `2.3.19.1 (9b53102964)` |
| Packaged engine | imapsync `2.314`, SHA-256 `dda1a7380cdab8c8bfc981724ab84ef1f5b18d630221df8d0517fed708271875` |
| Scenario | 10,000 generated messages; 1 GiB aggregate body payload |
| Harness wall time | 84 seconds, including fixture setup, transfer, verification, delta, and evidence exports; not a direct-engine A/B measurement |
| Initial proof | 10,016 source and destination messages; zero missing, extra, modified, or flag-mismatched messages; 100% FLAGS coverage; metadata only, bodies not compared |
| Post-delta proof | 10,017 source and destination messages; zero missing, extra, modified, or unmatched messages; metadata only, bodies not compared |
| Sampled resources | 373,397,913-byte sampled peak container memory; cgroup peak anonymous 223,051,776 bytes and file cache 2,251,911,168 bytes |
| Artifact | [Actions artifact 11724766653](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38197707897/artifacts/11724766653), archive SHA-256 `39cb92158efc925a306ce88ab3c9830052355fb8e6d25a8dc6429aa382da8e3b` |

| Field | Observed value |
|---|---|
| GitHub Actions run | [38198688096](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38198688096) |
| MailSwiftSync commit | `f99dfa4ed11596b5b219fa91bbaa76295e867270` |
| Fixture | Disposable generic IMAP / Dovecot `2.3.19.1 (9b53102964)` |
| Packaged engine | imapsync `2.314`, SHA-256 `dda1a7380cdab8c8bfc981724ab84ef1f5b18d630221df8d0517fed708271875` |
| Scenario | 100,000 generated messages; small-message payload shape |
| Harness wall time | 1,052 seconds; first live pass 15m04s and incremental live pass 79s, based on recorded phase markers |
| Initial proof | 100,016 source and destination messages / 28,681,943 bytes; zero missing, extra, modified, or flag-mismatched messages; 100% FLAGS coverage; metadata only, bodies not compared |
| Post-delta proof | 100,017 source and destination messages; zero missing, extra, modified, or unmatched messages; metadata only, bodies not compared |
| Sampled resources | 1,270,236,577-byte sampled peak container memory; cgroup peak anonymous 533,807,104 bytes and file cache 1,208,299,520 bytes; sampled imapsync Perl RSS peak 472,309,760 bytes |
| Artifact | [Actions artifact 11724779821](https://github.com/cyberducttape/MailSwiftSync/actions/runs/38198688096/artifacts/11724779821), archive SHA-256 `033917ecc0d77c7d03ca65ef3a77f094f3495fb460db03c6cb4a258de442f32f` |

The artifact is an unsigned CI artifact (the customer-proof JSON is
digest-wrapped, not independently signed). Its retention expires on
2027-01-10 for both runs. These results are specific to the disposable fixture
and must not be presented as hosted-provider, content-integrity, 1/10/50/100-GiB
large-mailbox, or production-readiness qualification.

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
