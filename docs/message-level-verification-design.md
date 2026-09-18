# Message-Level Verification Design

## Problem Statement

Current verification is **aggregate-only**: MailSwiftSync compares folder counts, message counts, and byte counts between source and destination. This can miss:

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
    pub modified_messages: u64,      // Same UID/Message-ID but different hash/size
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
  source_uid TEXT,               -- UID from source (NULL if extra)
  dest_uid TEXT,                 -- UID from destination (NULL if missing)
  source_message_id TEXT,         -- Message-ID header (for cross-check)
  dest_message_id TEXT,
  source_size_bytes INTEGER,      -- For "modified" detection
  dest_size_bytes INTEGER,
  source_date TEXT,               -- RFC 2822 date
  dest_date TEXT,
  subject BLOB,                   -- Sanitized subject (bounded, for context)
  recorded_at TEXT,
  FOREIGN KEY(job_id) REFERENCES mailbox_jobs(id),
  FOREIGN KEY(run_id) REFERENCES runs(id)
);
```

### Data Extraction

#### From imapsync

imapsync `--debug` output includes per-message details:

```
Msg # 1 {size=1234 date="2024-01-01 12:00:00"} ...
Msg # 1 -> 2 (new UID on host2)
```

**Implementation:**
- Add optional imapsync flag in plan: `--debug 2` (message-level output)
- Parse output for `Msg #` records
- Extract source UID, destination UID, size, date
- Cross-reference with final aggregate counts to identify missing/extra

#### From Dovecot (doveadm)

`doveadm mailbox status` provides counts. `doveadm fetch` can extract UID + Message-ID:

```bash
doveadm -u user@example.com fetch -A "uid messageids" MAILBOX "INBOX"
```

**Implementation:**
- After doveadm migration, run `doveadm mailbox status` on both sides
- For mismatches, run `doveadm fetch` on both to extract UIDs and Message-IDs
- Compare sets to identify missing/extra
- Store in `message_mismatches`

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
- Fetch message metadata (Date, Subject) for context
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
- Operator report includes full mismatch details (subject, Date, UIDs)

## Phase 2: Implementation Plan

### 2.1 Schema Migration (CURRENT_SCHEMA_VERSION → N+1)

- Add `missing_messages`, `extra_messages`, `modified_messages` to `mailbox_evidence`
- Create `message_mismatches` table
- Create `message_mismatch_acceptance` table
- Create `message_extraction` (temporary, per-run)

### 2.2 Verification Accumulator Extension

Update `ImapsyncEvidenceAccumulator`:
- Add message-UID extraction from imapsync output
- Parse `Msg #` records and UID mappings
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

**Final verdict:** After remediation, message-level verification shows exact match. Operator confirms in final report.

**Without message-level verification:** Migration accepted as "complete" despite data integrity issue.

---

## Questions for Implementer

1. **Scope for v0.1 → 1.0?** This is high-effort. Is it a hard blocker for 1.0, or is aggregate-only acceptable with a documented "known limitation"?

2. **Privacy boundary:** Subject truncation OK? Date/Message-ID OK to store durably? (Recommendation: yes to all, but operator-only)

3. **Provider testing:** When adding new providers to compatibility matrix, should they include message-level verification evidence, or is aggregate OK for MVP?

4. **imapsync vs Dovecot:** Priority — which engine should get message-level support first? (imapsync is more common, Dovecot native is more capable)
