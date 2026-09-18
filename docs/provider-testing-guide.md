# Provider Testing Guide

This guide describes how to add a new row to the compatibility matrix by testing MailSwiftSync against a real IMAP provider.

## Overview

Each compatibility matrix row requires evidence for four outcomes:
1. **Dry pilot** — preflight validation succeeds without making changes
2. **Live pilot** — actual migration succeeds and evidence matches expectations
3. **Recovery** — interrupted work can be resumed correctly
4. **Evidence** — customer proof, integrity, and verification paths work

## Setup Requirements

### For each provider test:

1. **Disposable test accounts** (or temporary ones with full cleanup)
   - Source account: populated with representative mailboxes
   - Destination account: empty, ready to receive migration

2. **IMAP access details**
   - Endpoint (host:port)
   - Transport (IMAPS/STARTTLS)
   - Authentication method (password/OAuth2/app password)
   - Provider-specific IMAP features (SPECIAL-USE, namespace, etc.)

3. **Mailbox test data**
   - Empty mailboxes (to test folder creation)
   - Small mailboxes (< 100 messages)
   - Large mailboxes (> 10k messages if provider supports)
   - Special-use folders (if exposed by provider)
   - Unicode folder names (to test encoding)
   - Message-size variety (plain text, HTML, attachments)

4. **Engine version**
   - Pinned `imapsync` version tested against
   - Provider-specific version string if applicable

## Testing Procedure

### 1. Dry Pilot

Run a preflight check to verify:
- [ ] DNS resolution succeeds
- [ ] TLS certificate validation passes
- [ ] Authentication succeeds
- [ ] IMAP capabilities are discovered (LIST, SPECIAL-USE, etc.)
- [ ] Folder inventory is read (namespace handling correct)
- [ ] Destination has sufficient quota
- [ ] No changes are made to either side

**Record:** imapsync `--dry` output, discovered capabilities, folder count, namespace structure.

### 2. Live Pilot

Migrate from source to destination:
- [ ] Plan fingerprint matches dry pilot
- [ ] Durable state records all mailboxes
- [ ] Each mailbox gets a run record
- [ ] No messages are lost or duplicated
- [ ] Folder structure is preserved
- [ ] Special-use annotations survive (if provider supports)
- [ ] Migration completes and verification succeeds

**Record:** Run IDs, source/destination message counts, Message-ID sample, customer proof output.

### 3. Recovery

Simulate interruption and verify restart:
- [ ] Kill the migration mid-process (while a mailbox is running)
- [ ] Verify the durable ledger marks it as `running` or `attention`
- [ ] Restart and attempt delta migration
- [ ] Verify no duplicate messages appear on destination
- [ ] Final evidence matches full migration

**Record:** Interruption point, recovery behavior, delta mailbox count.

### 4. Evidence Export

Verify all export paths work:
- [ ] `mailswiftsync customer-proof` produces valid JSON
- [ ] `mailswiftsync sign` works with Ed25519 keys
- [ ] `mailswiftsync verify` confirms proof integrity
- [ ] Operator JSON report contains all run metadata
- [ ] Support bundle sanitization works (no credentials in output)

**Record:** Proof digest, run manifest, evidence scope (aggregate only, no per-message detail).

## Adding the Row to the Matrix

Once all four evidence columns are complete, add a row to `docs/compatibility-matrix.md`:

```markdown
| [Provider Name] | [Provider Name] | imapsync | [TLS/Auth] | [Namespace] | [Provider Version] | [Evidence] | [Evidence] | [Evidence] | [Evidence] | [Test date: YYYY-MM-DD, [Notes]] |
```

Mark each evidence column with one of:
- **Automated lab** — reproducible CI fixture or repeatable script
- **Manual test** — documented one-time evidence with date
- **Packaged controller recovery lab** — uses the crash-recovery fixture
- **Durable state, [details], customer-proof export and verifier checks** — exact verification performed

## Example: Gmail (Google Workspace)

**Setup:**
- Create disposable Google Workspace accounts (source and destination)
- Generate app passwords (IMAP access requires app password, not account password)
- Ensure accounts have different folder structures (to test namespace)

**Dry pilot:**
```bash
mailswiftsync --headless /tmp/gmail-test.db preflight \
  --source user1@workspace.example.com \
  --destination user2@workspace.example.com
```

**Expected:** Connection succeeds, Gmail's SPECIAL-USE folders are discovered (All Mail, Sent Mail, etc.).

**Live pilot:**
```bash
mailswiftsync --headless /tmp/gmail-test.db live
```

**Expected:** All messages transfer, labels become IMAP folders, evidence shows matching message/byte counts.

## Provider-Specific Notes

### Gmail / Google Workspace

- **Namespace:** Gmail uses labels (not folders); IMAP exposes them as folders with `[Gmail]/` prefix
- **SPECIAL-USE:** `All Mail` (\All), `Sent Mail` (\Sent), `Drafts` (\Drafts), `Trash` (\Trash)
- **Quirks:** Sent messages may appear on both source and destination; aggregate counts may not match exactly
- **Auth:** Requires app password (not regular Gmail password)
- **Rate limits:** Up to 2,500 IMAP commands/second per account

### Microsoft 365 / Outlook

- **Namespace:** Flat folder structure; special folders prefixed (e.g., `Deleted Items`, `Sent Items`)
- **SPECIAL-USE:** Mailbox has explicit special-use attributes
- **Quirks:** Some folders may be read-only or system-generated
- **Auth:** Supports OAuth2 Modern Auth or app password
- **Rate limits:** Connection throttling possible under load

### Fastmail

- **Namespace:** JMAP-first, but IMAP access available with `/` separators
- **SPECIAL-USE:** Full SPECIAL-USE support
- **Auth:** Supports passwords and app-specific passwords
- **Rate limits:** Generous; well-documented

## Recording Evidence

For each tested provider, maintain:

1. **Test metadata file** (`docs/provider-tests/[provider]-test.md`):
   - Date tested
   - Accounts used (anonymized)
   - Source/destination message counts
   - Run IDs
   - Any failures and resolutions

2. **Sanitized logs** (no credentials):
   - Dry pilot preflight output
   - Live run evidence
   - Customer proof JSON (demonstrating structure)

3. **Compatibility notes**:
   - Discovered quirks
   - Known limitations
   - Recommended settings

## Automation

For CI/CD, consider:
- Using disposable test accounts (temporary API-created accounts, auto-deleted after test)
- Storing credentials in GitHub Secrets (not committed)
- Running provider tests on a schedule (not on every push, due to rate limits)
- Publishing evidence artifacts alongside releases

## When Complete

Update the compatibility matrix row with:
- Dry pilot: `Documented manual test [YYYY-MM-DD]` (or `Automated CI fixture`)
- Live pilot: `Documented manual test with incremental delta` (or `Automated`)
- Recovery: `Manual interruption test confirming no duplicates`
- Evidence: `Durable state, message counts verified, customer-proof and verifier checks passed`
- Notes: Date tested, provider version, any quirks discovered
