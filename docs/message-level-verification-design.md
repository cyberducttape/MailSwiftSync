# Message-Level Verification Design

## Implementation Status & Priority

**STATUS:** 🟡 **METADATA RECONCILIATION AND MISMATCH PERSISTENCE WIRED; CONTENT/SCALE DESIGN PENDING**
**Priority:** 🚨 **CRITICAL — Largest Product-Level Trust Gap**  
**Last verified:** 2026-09-20

The independent metadata verifier closes the selective-loss gap for encrypted
imapsync runs, but it does not prove body-content equality. Aggregate counts can
match while individual mailbox contents are corrupted or rewritten, so content
hashing and durable per-message staging remain production trust work.

> **Implementation boundary:** The current live imapsync path independently
> fetches selectable folders and compares Message-ID, INTERNALDATE, and
> RFC822.SIZE using the verifier below. It must not compare source and
> destination UIDs as identities. Mismatch records now commit atomically with
> terminal evidence and are visible in the operator verification report.
> Content hashes and per-message extraction staging remain target-design work.

**Current Limitation:** MailSwiftSync can now verify portable metadata for the
same message population on encrypted imapsync runs, but it does not yet prove
that the message bodies are byte-for-byte identical. The live verifier also
materializes both account message maps and several reconciliation indexes. Its
one-million-record cap and estimated 256 MiB state budget are fail-closed
admission guards, not peak-memory guarantees. SQLite-backed streaming
reconciliation is a production blocker for very large MSP migrations, not a
mere optimization.

**Impact When Implemented:** Enables operators to report:
- ✅ 19,998 metadata matches (not content verified)
- ⚠️ 2 missing (list them)
- ⚠️ 0 changed
- ⚠️ 0 unexplained extras
- With drill-down evidence per mailbox

This feature would **differentiate MailSwiftSync** from competitors and justify enterprise adoption.

---

## Problem Statement

The current live imapsync path now performs **metadata-level reconciliation** in addition to aggregate evidence: it compares folder placement, Message-ID when available, INTERNALDATE, and RFC822.SIZE. It does not hash message bodies. Metadata reconciliation can still miss:

- **Selective message loss** — a migration that drops messages in one folder but maintains aggregate counts elsewhere
- **Selective duplication** — messages duplicated only in certain mailboxes
- **Content corruption** — messages with same count/size but different content (rare, but possible with transformation)
- **Partial migrations** — when a subset of messages silently fail and are skipped

**Example:** Source has 1000 messages across 5 folders. Destination also has 1000 messages across 5 folders (aggregate match). But: Folder A has 100 messages on source but only 50 on destination; Folder B has 900 messages on source and 950 on destination (duplication compensates for loss). Aggregate match masks a real problem.

**Production impact:** Operators can accept a migration as "verified" when it actually lost data in specific mailboxes.

## Solution Overview

Add **message-level reconciliation** as a separate, opt-in verification phase that:

1. Extracts message identifiers (UIDs, Message-IDs) from both source and destination
2. Compares them to identify missing, extra, or modified messages
3. Stores mismatches in the durable ledger
4. Reports them in verification exports and operator UI
5. Allows operators to accept specific mismatches (e.g., "expected loss from spam folder") or require remediation

## Phase 1: Design & Schema (This Doc)

### Data Model

#### Mailbox-Level Mismatch Summary

Store per-mailbox mismatch counts in `MailboxEvidence`:

```rust
pub struct MailboxEvidence {
    // Existing fields...
    pub source_messages: u64,
    pub destination_messages: u64,
    // NEW: Message-level reconciliation
    pub missing_messages: u64,      // Present in source, absent in destination
    pub extra_messages: u64,         // Present in destination, absent in source
    pub modified_messages: u64,      // Same portable identity but different metadata
}
```

#### Per-Message Mismatch Table

New SQLite table `message_mismatches`:

```sql
CREATE TABLE message_mismatches (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  mismatch_type TEXT NOT NULL,  -- 'missing' | 'extra' | 'modified'
  source_folder TEXT,
  destination_folder TEXT,
  source_uidvalidity INTEGER,
  destination_uidvalidity INTEGER,
  source_uid TEXT,               -- UID from source (NULL if extra)
  dest_uid TEXT,                 -- UID from destination (NULL if missing)
  source_message_id TEXT,         -- Message-ID header (for cross-check)
  dest_message_id TEXT,
  source_size_bytes INTEGER,      -- For "modified" detection
  dest_size_bytes INTEGER,
  source_date TEXT,               -- RFC 2822 date
  dest_date TEXT,
  recorded_at TEXT,
  FOREIGN KEY(job_id) REFERENCES mailbox_jobs(id),
  FOREIGN KEY(run_id) REFERENCES runs(id)
);
```

### Data Extraction

#### From imapsync

The parser must consume the transfer progress grammar emitted by the pinned
engine, for example:

```
msg INBOX/5 {279010} copied to backup/INBOX/49 0.57 msgs/s 154.916 KiB/s 272.471 KiB copied
```

This provides local source/destination UID mapping and message size only. It
does not provide Message-ID, INTERNALDATE, or a content hash; those require
explicit IMAP FETCH extraction. The repository currently has parser contract
tests for this grammar, but does not claim that a captured 2.314 integration
log fixture has been validated until the packaged integration container is
available.

The checked-in message verification tests are scenario/model tests, not
product-level integration tests: they do not invoke the binary, SQLite, or
report generation. Real imapsync 2.314 parser fixtures still need to be
captured from the packaged runtime before this parser can claim that level of
coverage.

**Implementation:**
- Add optional imapsync flag in plan: `--debug 2` (message-level output)
- Parse output for `msg <folder>/<uid> {<size>} copied to <folder>/<uid>` records
- Extract source UID, destination UID, size, date
- Cross-reference with final aggregate counts to identify missing/extra

#### From Dovecot (doveadm)

`doveadm mailbox status` provides counts and must separately obtain each
mailbox's UIDVALIDITY. Message extraction must select a machine-readable
formatter explicitly rather than parse Dovecot's human-oriented default:

```bash
doveadm -f tab fetch -u user@example.com \
  "uid hdr.message-id size.virtual date.received.unixtime" mailbox "INBOX"
```

This is a per-user query; it must not be combined with `-A`. The extractor is
currently unwired and its tabular contract still requires validation against each
admitted Dovecot runtime before it can become authoritative evidence.

**Implementation:**
- After doveadm migration, run `doveadm mailbox status` on both sides
- For mismatches, run `doveadm fetch` on both to extract UIDs and Message-IDs
- Compare sets to identify missing/extra
- Store in `message_mismatches`

### Target Verification Approach: Multi-Factor Matching

The following nine-level reconciliation model is a target design, not a
currently shipped production capability. Until the extractor supplies the
required evidence and the live migration path persists the results, these
labels must not be used as provider compatibility claims or customer proof.

To achieve high-confidence verification, match messages on **combinations** rather than single identifiers:

**Primary Signals (highest confidence):**
- ✅ Message-ID header (RFC 2822) — globally unique
- ✅ Content hash (SHA-256 of message body) — detects corruption
- ✅ Internal date + size — near-unique combination

**Secondary Signals (supporting evidence):**
- ✅ Folder path — identifies routing errors
- ✅ IMAP UID — engine-specific, may not cross-host
- ✅ Subject + From + Date — heuristic recovery

**Mismatch Classifications:**
- `METADATA_MATCHED` — Message-ID plus available metadata match; not content verification
- `CONTENT_MATCH` — Hash and date match, Message-ID missing/differs (strongly matched)
- `DATE_SIZE_MATCH` — Internal date + size match (probable match only)
- `MESSAGE_ID_ONLY` — Message-ID matches but date/size differ (changed evidence requiring review)
- `MISSING` — Present in source, absent in destination
- `EXTRA` — Present in destination, absent in source (unclear origin)
- `DUPLICATED` — Multiple instances of same message-ID in destination
- `FOLDER_MISMATCH` — Same message in different folder on destination
- `MESSAGE_PRESENT_WRONG_FOLDER` — Message identity and metadata match, but
  the destination folder differs from the expected mapped folder
- `CHANGED` — Same portable identity but different metadata or content fingerprint

The verifier distinguishes reconciliation from proof strength. A unique
internal-date + size pair is recorded as `PROBABLE_MATCH`; it removes the
candidate from missing/extra results but does not increment metadata matches
and cannot make `is_perfect_metadata_match()` succeed. Metadata-matched status
currently requires a unique Message-ID with matching available metadata, but
it does not establish content equality or folder placement. A future
folder-aware content fingerprint can promote evidence to `CONTENT_VERIFIED`;
UID equality alone never can.

The current library verifier still receives complete in-memory extraction maps.
Its transient indexes borrow mailbox keys and message IDs, and its metadata
fallback uses a typed borrowed key rather than allocating delimiter-joined
fingerprint strings. This reduces avoidable duplication but does not make
500,000-message verification bounded-memory. MSP-scale operation still requires
streaming source and destination metadata into the existing SQLite evidence
store, then reconciling through indexed queries before this feature is wired to
the migration controller.

The authoritative summary exposes named evidence levels rather than a
percentage: `metadata_matched`, `strong_metadata_match`, `probable_match`, `ambiguous`,
`missing`, `changed`, or `unexpected`. Negative categories take precedence, so
successful matches cannot conceal missing, changed, duplicate, or extra
messages. The legacy display structures retain mismatch counts only; they do
not calculate an independent confidence score.

**Example Output (The Killer Feature):**
```
Migration Summary: user@example.com
───────────────────────────────────────
INBOX:
  19,998 metadata matches (Message-ID + available metadata; content not verified)
  2 missing (Message-ID <a@x>, Message-ID <b@x>)
  0 changed
  0 unexplained extras
  ⚠️ METADATA MATCHED (content and folder placement not verified)

Sent:
  500 metadata matches
  0 missing
  0 changed
  1 extra (Message-ID <c@x>, 2024-02-15)
  ⚠️ REVIEW (unexplained extra)

Overall: 20,498 exact, 2 missing, 1 extra → ACCEPT or REMEDIATE
```

### Verification Workflow

#### Step 1: Aggregate Check (Existing)
- Run migration (dry or live)
- Collect folder/message/byte counts
- Determine aggregate match or mismatch

#### Step 2: Message Extraction (New)
- If operator requests message-level verification:
  - Extract UIDs and Message-IDs from destination (requires fresh IMAP auth)
  - Extract UIDs and Message-IDs from source (requires fresh IMAP auth)
  - Store extraction results in new `message_extraction` table
  - Perform set comparison (source ∪ destination = union; identify Δ)

#### Step 3: Mismatch Classification (New)
- For each mismatch, classify as missing/extra/modified
- Retain only identity and reconciliation metadata (Message-ID, folder,
  UID/UIDVALIDITY, size, date, flags, and content fingerprint)
- Store in `message_mismatches` table

#### Step 4: Operator Review (New)
- UI shows per-mailbox mismatch counts
- Operator can review mismatches and accept specific ones
- Store acceptance with reason in `message_mismatch_acceptance` table

#### Step 5: Final Evidence
- Report includes message-level section:
  ```
  Mailbox: INBOX
    Aggregate: 500 source → 500 destination ✓
    Message-level: 500 matched, 2 missing, 1 extra
    Mismatches:
      - Missing: Message-ID <foo@example.com> (2024-01-01)
      - Extra: Message-ID <bar@example.com> (2024-02-01)
  ```
- Customer proof includes summary of mismatches
- Operator report includes bounded identity details (folders, dates, sizes,
  UIDs, and fingerprints); message content is not stored in the durable ledger

### Production-scale reconciliation boundary

The `ExtractedMessages` verifier API, keyed by `MailboxMessageKey`, is a bounded in-memory
comparison primitive for unit tests and small, explicitly requested checks. It
is not the production architecture for a 500,000-message mailbox. A live
message-level verification path must stream extraction records into the
per-run SQLite staging table, with source/destination side, mailbox,
UIDVALIDITY, local UID, normalized Message-ID, metadata, and any content
fingerprint stored as rows. Indexed SQL joins (or bounded batches over those
indexes) should perform reconciliation and persist mismatches transactionally.

This keeps process memory bounded and makes the evidence durable even if the
report process is interrupted. The current controller path wires bounded
in-memory metadata reconciliation and durable mismatch persistence; the SQLite
extraction staging design remains required before claiming full production-scale
content verification for very large mailboxes.

## Phase 2: Implementation Plan

### 2.1 Schema Migration (CURRENT_SCHEMA_VERSION → N+1)

- Add `missing_messages`, `extra_messages`, `modified_messages` to `mailbox_evidence`
- Create `message_mismatches` table
- Create `message_mismatch_acceptance` table
- Create `message_extraction` (temporary, per-run)

### 2.2 Verification Accumulator Extension

Update `ImapsyncEvidenceAccumulator`:
- Add message-UID extraction from imapsync output
- Parse `msg <folder>/<uid> {<size>} copied to <folder>/<uid>` records and UID mappings
- Detect missing/extra/modified messages

Update `DovecotStatusAccumulator`:
- Post-migration, run `doveadm fetch` to extract UIDs
- Persist to `message_extraction` table

### 2.3 Comparison & Mismatch Persistence

New module `message_verification`:
- Function `extract_and_compare_messages()` — fetches UIDs from both sides
- Function `identify_mismatches()` — set comparison (A \ B = missing, B \ A = extra)
- Function `persist_mismatches()` — writes to `message_mismatches` table

### 2.4 UI Integration

**Verification workspace:**
- Add "Message-level verification" section
- Show per-mailbox mismatch counts
- Paginated list of mismatches (with Date, Subject, UID context)
- Accept-mismatch dialog for each one

### 2.5 Export Integration

**Customer proof:**
- Include mismatch summary (total missing/extra per mailbox)
- Include per-mailbox mismatch counts
- Omit full mismatch details (privacy: recipient names in Subject)

**Operator report:**
- Full mismatch details (UID, Message-ID, Date, Subject, size)
- Sanitized Subject (bounded, no personal info)
- Acceptance reasons and timestamps

### 2.6 Testing

**Unit tests:**
- Mismatch set comparison logic
- Edge cases: empty mailboxes, all messages missing, all extra, duplicates

**Integration tests:**
- `scripts/imap-integration-smoke.sh` variant with intentional message loss
- Verify message-level detection catches it
- Verify aggregate-match case that should have failed at message-level

## Phase 3: Roadmap

### v0.2 (Optional, post-1.0)

- [ ] Add message-level verification framework (design + schema)
- [ ] Implement imapsync message extraction
- [ ] Implement Dovecot doveadm message extraction
- [ ] Mismatch detection and persistence
- [ ] Basic UI: read-only mismatch display
- [ ] Integration test: intentional message loss scenario

### v0.3+

- [ ] Operator mismatch acceptance workflow
- [ ] Full-report mismatch details
- [ ] Provider-specific mismatch rules (e.g., ignore [Gmail]/All Mail)
- [ ] Delta remediation: re-run with specific mailboxes targeted
- [ ] UIDVALIDITY-aware persistence (robust across reconnects)

## Considerations

### Performance

- Message extraction requires fresh IMAP commands (N+1 risky for large mailboxes)
- **Mitigation:** Make it opt-in, run async, show progress
- Limit to recent messages or specific mailboxes if needed

### Privacy

- Storing Subject, Date, Message-ID is minimal but still PII-adjacent
- **Mitigation:**
  - Sanitize Subject (truncate, remove email addresses)
  - Operator report only (not customer proof)
  - Clear after acceptance or review window (e.g., 30 days)

### Provider-Specific Quirks

- Gmail's [Gmail]/All Mail folder doesn't migrate (expected)
- Some providers have read-only folders (expected, not a mismatch)
- Outlook may have system folders that auto-sync (expected)

**Mitigation:**
- Document provider-specific ignore rules
- Allow operators to bulk-accept mailbox-wide mismatches
- Link to compatibility-matrix notes

### Storage

- `message_mismatches` could grow large (e.g., 100k messages in a large project)
- **Mitigation:** Pagination in UI, configurable retention window, optional export-only mode

## Success Criteria

1. **Catches selective loss:** Integration test with intentional message loss passes
2. **Operators can review:** UI lists mismatches per mailbox with context
3. **Audit trail:** Mismatches and acceptances are durably recorded
4. **Reports include details:** Operator report shows full mismatch info
5. **Doesn't block shipping:** Message-level verification is opt-in; current aggregate-based workflow unchanged

## Example: Detecting a Real Problem

**Scenario:** Migrate 5 mailboxes (4000 messages total) from Gmail to Microsoft 365.

**Aggregate evidence:**
- Source: 4000 messages, 500 MB
- Destination: 4000 messages, 500 MB
- Verdict: "Aggregate match" ✓

**Message-level verification (NEW):**
- Extract UIDs from both sides
- Discover:
  - INBOX: 1000 source → 980 destination (20 missing)
  - Sent: 1000 source → 1000 destination ✓
  - Archive: 1000 source → 1020 destination (20 extra — duplicates?)
  - Drafts: 1000 source → 1000 destination ✓

**Operator review:**
- Sees 20 missing in INBOX (likely old messages; acceptable)
- Sees 20 extra in Archive (unexpected; requires investigation)
- Accepts 20 missing, **rejects 20 extra** → requires remediation
- Re-runs with specific Mailbox=Archive, target=dedup flag

**Final verdict:** After remediation, metadata reconciliation is complete. A
content-verified verdict requires the future folder-aware SHA-256 path.

**Without message-level verification:** Migration can be accepted as "complete"
despite a selective data-integrity issue. The current encrypted-imapsync path
now supplies metadata-level verification; this design's remaining target is
content proof and scalable staging.

---

## Questions for Implementer

1. **Scope for v0.1 → 1.0?** This is high-effort. Is content proof a hard blocker for 1.0, or is metadata reconciliation acceptable with a documented "known limitation"?

2. **Privacy boundary:** Subject truncation OK? Date/Message-ID OK to store durably? (Recommendation: yes to all, but operator-only)

3. **Provider testing:** When adding new providers to compatibility matrix, should they include message-level verification evidence, or is aggregate OK for MVP?

4. **imapsync vs Dovecot:** Priority — which engine should get message-level support first? (imapsync is more common, Dovecot native is more capable)
