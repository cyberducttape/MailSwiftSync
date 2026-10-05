# Provider Testing Guide

This guide describes how to add a new row to the compatibility matrix by testing MailSwiftSync against a real IMAP provider.

## Overview

Each compatibility matrix row requires evidence for four outcomes:
1. **Dry pilot** — preflight validation succeeds without making changes
2. **Live pilot** — actual migration succeeds and evidence matches expectations
3. **Recovery** — interrupted work can be resumed correctly
4. **Evidence** — customer proof, integrity, and verification paths work

The release gate reads `tests/provider-evidence/policy.json`. That file is the
authoritative list of providers, required phases, and supported engine
version; changing prose in the compatibility matrix cannot weaken the gate.
Each phase must be recorded in its own JSON evidence file with
`results.overall_result: "pass"`. A single file cannot stand in for multiple
phases. Required scenarios are phase-specific, so a dry preflight does not
claim live-transfer totals. The release routes are Gmail/Workspace → Microsoft
365 (OAuth/OAuth), Microsoft 365 → Gmail/Workspace (OAuth/OAuth), and Fastmail
→ Microsoft 365 (app-specific password/OAuth).

The checked-in `provider-integration-test.sh` is a small-run smoke harness, not
a provider qualification run: it exercises a basic mailbox and forced
interruption only. It retains digest-verifiable customer-proof artifacts in a
private, unique output directory, but does not create or measure the full
release dataset and cannot generate records accepted by the release gate. Its
success message explicitly says the run is not qualification evidence. Do not
submit its smoke proofs to the release evidence policy.

## Setup Requirements

### For each provider test:

1. **Disposable test accounts** (or temporary ones with full cleanup)
   - Source account: populated with representative mailboxes
   - Destination account: empty, ready to receive migration

2. **IMAP access details**
   - Endpoint (host:port)
   - Transport (IMAPS/STARTTLS)
   - Authentication method independently for source and destination
     (`password` or `oauth2`; Microsoft 365 IMAP requires `oauth2`)
   - Provider-specific IMAP features (SPECIAL-USE, namespace, etc.)

3. **Mailbox test data**
   - Empty mailboxes (to test folder creation)
   - Small mailboxes (< 100 messages)
   - At least 100,000 messages and 20 GiB for the large-mailbox qualification
   - Special-use folders (if exposed by provider)
   - Unicode folder names (to test encoding)
   - Message-size variety (plain text, HTML, attachments)

4. **Engine version**
   - Pinned `imapsync` version tested against
   - Provider-specific version string if applicable

5. **Qualification edge fixtures**
   - Unicode/SPECIAL-USE folders, Gmail label mapping where applicable,
     large messages, and duplicate Message-ID cases
   - A source message added during seed and caught up by a later delta
   - Destination-side activity during final delta, checked against the
     configured destination mutation policy
   - Observed throttling followed by recovery, plus quota/folder-limit errors
     classified without data loss

Large-mailbox observations must identify the durable mailbox job ID and match
that mailbox's source message/byte totals in the referenced digest-verified
customer proof. Aggregate totals across several smaller mailboxes do not count
as a 100k-message or 20-GiB mailbox qualification.

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

**Record:** Proof digest, run manifest, and evidence scope. For encrypted imapsync
runs, record whether bounded metadata reconciliation completed and include the
per-message mismatch summary; native Dovecot runs remain aggregate-level.

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
- Prefer OAuth/XOAUTH2 for IMAP. An app password may be used only for accounts
  eligible for Google app passwords when testing password-based IMAP tooling;
  it is not required for OAuth and is not the preferred modern sign-in path.
- Ensure accounts have different folder structures (to test namespace)

**Dry pilot:**
```bash
# Configure the source/destination profile and owner-only secret files first.
mailswiftsync headless /tmp/gmail-test.db preflight \
  --source-secret-file /run/secrets/gmail-source \
  --destination-secret-file /run/secrets/gmail-dest
```

**Expected:** Connection succeeds, Gmail's SPECIAL-USE folders are discovered (All Mail, Sent Mail, etc.).

**Live pilot:**
```bash
mailswiftsync headless /tmp/gmail-test.db live \
  --source-secret-file /run/secrets/gmail-source \
  --destination-secret-file /run/secrets/gmail-dest
```

**Expected:** All messages transfer, labels become IMAP folders, evidence shows matching message/byte counts.

## Provider-Specific Notes

### Gmail / Google Workspace

- **Namespace:** Gmail uses labels (not folders); IMAP exposes them as folders with `[Gmail]/` prefix
- **SPECIAL-USE:** `All Mail` (\All), `Sent Mail` (\Sent), `Drafts` (\Drafts), `Trash` (\Trash)
- **Quirks:** Sent messages may appear on both source and destination; aggregate counts may not match exactly
- **Auth:** OAuth/XOAUTH2 preferred. App passwords are an optional fallback for
  eligible accounts using password-based IMAP authentication.
- **Usage limits:** Gmail enforces connection and usage limits. Start
  conservatively, respond to observed provider throttling, and validate live
  limits against current Google documentation; this guide does not assert a
  fixed commands-per-second rate.

### Microsoft 365 / Outlook

- **Namespace:** Flat folder structure; special folders prefixed (e.g., `Deleted Items`, `Sent Items`)
- **SPECIAL-USE:** Mailbox has explicit special-use attributes
- **Quirks:** Some folders may be read-only or system-generated
- **Auth:** OAuth2 delegated Modern Authentication is required for Exchange
  Online IMAP. App passwords do not restore the removed Basic Authentication
  path.
- **Rate limits:** Connection throttling possible under load

For a Gmail → Microsoft 365 smoke run, set the source/destination provider
variables explicitly as shown below and use
`MAILSWIFTSYNC_PROVIDER_DEST_AUTH=oauth2`. The destination secret file
must contain a current OAuth access token with the delegated IMAP scope; a
password or app password is rejected before the test starts. The source side
may independently use `password` or `oauth2`:

```bash
export MAILSWIFTSYNC_SOURCE_PROVIDER=gmail
export MAILSWIFTSYNC_DESTINATION_PROVIDER=microsoft365
export MAILSWIFTSYNC_PROVIDER_SOURCE_AUTH=oauth2
export MAILSWIFTSYNC_PROVIDER_DEST_AUTH=oauth2
bash scripts/provider-integration-test.sh gmail
```

Every provider smoke run also requires a separate, empty recovery
destination account. Set `MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_ENDPOINT`,
`MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_USER`, and
`MAILSWIFTSYNC_PROVIDER_RECOVERY_DEST_SECRET`; the harness rejects reuse of
the normal live destination so recovery behavior is exercised against an
isolated mailbox. This smoke run does not qualify a provider pair.

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

The **Live provider smoke** workflow (`.github/workflows/provider-smoke.yml`,
manual dispatch) runs this guide's dry pilot, interruption/recovery, and live
pilot against Google Workspace and Microsoft 365 test tenants in the packaged
runtime image. Its proofs are smoke artifacts, not release qualification.

### One-time setup

1. **Test mailboxes.** Create four dedicated mailboxes that hold no real data:
   two in Google Workspace (`primary`, `recovery`) and two in Microsoft 365
   (`primary`, `recovery`). Each direction migrates from one tenant's
   `primary` into the other tenant's `primary`, and the recovery phase writes
   into the destination tenant's `recovery` mailbox. Enable IMAP for all four.
2. **Seed the source mailboxes.** Put at least a few messages in two or three
   folders (labels on Google) in both `primary` mailboxes; the harness migrates
   whatever is present and does not seed.
3. **OAuth clients.** Register one OAuth client per tenant as described in
   [OAUTH_SETUP.md](../OAUTH_SETUP.md) (Google: `https://mail.google.com/`
   scope, desktop client; Microsoft: delegated `IMAP.AccessAsUser.All` and
   `offline_access`, public client with an `http://localhost` redirect).
4. **Authorize and export each mailbox** on a workstation with an OS keyring,
   signing in as that mailbox:

   ```bash
   mailswiftsync oauth-authorize google ci-google-primary --client-id <id> \
     --client-secret-file <secret-file> --login-hint <google-primary-address>
   mailswiftsync oauth-export-refresh-config ci-google-primary google-primary.json
   # repeat for ci-google-recovery, and for Microsoft:
   mailswiftsync oauth-authorize microsoft ci-m365-primary --client-id <id> \
     --tenant <tenant-id-or-domain> --login-hint <m365-primary-address>
   mailswiftsync oauth-export-refresh-config ci-m365-primary m365-primary.json
   ```

5. **GitHub environment.** Create a repository environment named
   `live-providers` (optionally with required reviewers) and add eight
   secrets; paste each exported JSON file's contents as its `_REFRESH_CONFIG`
   value, then delete the files:

   | Secret | Value |
   |---|---|
   | `GOOGLE_PRIMARY_USER` / `GOOGLE_PRIMARY_REFRESH_CONFIG` | Google primary address / its exported JSON |
   | `GOOGLE_RECOVERY_USER` / `GOOGLE_RECOVERY_REFRESH_CONFIG` | Google recovery address / its exported JSON |
   | `M365_PRIMARY_USER` / `M365_PRIMARY_REFRESH_CONFIG` | Microsoft primary address / its exported JSON |
   | `M365_RECOVERY_USER` / `M365_RECOVERY_REFRESH_CONFIG` | Microsoft recovery address / its exported JSON |

6. **Run** *Actions → Live provider smoke → Run workflow* for
   `gmail-to-microsoft365` and `microsoft365-to-gmail`. The job exchanges each
   refresh configuration for a short-lived access token with
   `mailswiftsync oauth-access-token` (never printed), runs the harness, deletes
   the staged credentials, and uploads credential-free smoke proofs.

Microsoft rotates refresh tokens on use and expires unused ones after about
90 days; the job can only rewrite its temporary copy, so re-export and update
the `M365_*_REFRESH_CONFIG` secrets if a run reports `invalid_grant`. Run the
workflow manually or on a schedule, never on every push, because both
providers rate-limit IMAP.

## When Qualification Is Complete

Only after every policy-required scenario and phase has been executed, the
evidence has been reviewed, and `scripts/verify-evidence-gate.sh` passes for the
pair, update the compatibility matrix row with:
- Dry pilot: `Documented manual test [YYYY-MM-DD]` (or `Automated CI fixture`)
- Live pilot: `Documented manual test with incremental delta` (or `Automated`)
- Recovery: `Manual interruption test confirming no duplicates`
- Evidence: `Durable state, message counts verified, customer-proof and verifier checks passed`
- Notes: Date tested, provider version, any quirks discovered
