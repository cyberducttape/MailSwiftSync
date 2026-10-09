# MSP migration operations runbook

This procedure is for an MSP coordinating attended MailSwiftSync migrations
across customer organizations. It defines operator controls and evidence
handling; it does not provide tenant isolation, centralized dispatch, or
role-based access control. Until those fleet controls exist, use a separate
locked-down host and ledger per customer and keep an operator responsible for
each live migration window. MailSwiftSync remains a technical preview, not a
generally supported migration service.

For the host-level command sequence and recovery mechanics, use the
[production migration runbook](wiki/Production-runbook.md). This document adds
the MSP project, approval, and support controls around that procedure.

## 1. Open a customer change

Before collecting credentials or importing mailbox data, create a customer
change record containing:

- customer/tenant identifier, project owner, technical approver, and escalation
  contact;
- source and destination provider pair, tenant/account scope, migration window,
  and rollback/cutover decision owner;
- approved mailbox population, exclusions, destination mutation policy, and
  minimum acceptable verification level;
- data-retention location and expiry for the ledger, exports, qualification
  evidence, and support bundle.

Use a customer-specific MailSwiftSync state directory and ledger. Do not put
multiple customers in one ledger and treat project names as access boundaries:
the application does not enforce tenant-level authorization. Restrict host and
backup access through the MSP's operating-system and storage controls.

Record approvals outside the application when required by the customer. The
durable approval-gated cutover workflow records its own operator and decision
history, but it does not replace the MSP's change-management system.

## 2. Qualify the project before scheduling

1. Check the current [production status](../PRODUCTION_STATUS.md) and
   [provider qualification matrix](compatibility-matrix.md). A preset, generic
   integration test, or provider classifier is not live provider qualification.
2. Run the provider-pair checklist and verify the actual authentication method,
   TLS, permissions, source/destination limits, mailbox selection, and
   destination policy. Do not promise an unsupported verification level or
   provider capacity guarantee.
3. Import a pilot population first. Resolve actionable blockers and perform
   preflight; inspect the exact mailbox identities, duplicate destinations,
   quota/maximum-message observations, and unknown-capacity warnings.
4. Run a non-production or approved low-risk pilot. Have a second operator
   review the pilot evidence and the first live destination before increasing
   concurrency or wave size. Keep concurrency within observed tenant limits;
   adaptive scheduling is not a substitute for provider qualification.
5. For each wave, state the stop conditions in the change record: unresolved
   transfer failures, verification differences, unapproved destination
   mutations, unexpected throttling, or loss of ledger durability stop
   promotion to the next wave.

Do not treat a successful engine exit as mailbox completion. The durable
evidence state and the configured verification policy decide whether a
mailbox is complete, needs a delta, or requires operator review.

## 3. Run and monitor a wave

- Take an integrity-checked ledger backup before a high-value wave and verify
  that the restore destination and rollback owner are known.
- Keep each project to one active controller instance. Use the application's
  instance lock; never remove a lock file to force a second operator session.
- Start with a small pilot, then advance only after reviewing failures,
  throttling, queue eligibility, verification coverage, and destination
  capacity observations. Unknown quota is a blocker for any customer policy
  that requires known capacity.
- When provider throttling pauses launches, the Activity view reports the
  limiting domain, adaptive ceiling versus configured ceiling, and escalation
  event count. Treat this as run telemetry: adaptive cooldown state is held in
  the active controller process and is re-qualified after a restart; it is not
  a durable provider guarantee. Unrelated tenant domains should remain eligible
  unless the evidence escalates to their shared provider or global domain.
- Use the durable cutover workflow for staged Seed, Catch-up, Final Delta, and
  Verification where the customer change requires it. External DNS/MX
  confirmation remains an explicit operator acknowledgement.
- If using `notify-webhook --watch`, configure HTTPS, a customer-approved
  signing secret, a service-manager restart policy, and an owner for dead-letter
  review. The outbox is durable within the ledger, but delivery still depends
  on the host, network, and receiving system.
- Keep credentials in the supported OS keyring/OAuth storage. Never place
  passwords, access tokens, or refresh tokens in tickets, CSV files, command
  arguments, chat, or support bundles.

## 4. Incident and recovery ownership

On any unexpected destination mutation, authentication-domain lockout,
verification mismatch, process-identity uncertainty, or durability error:

1. Stop new wave admission and preserve the active host and customer ledger.
2. Do not manually edit SQLite, delete lock files, or retry a run whose terminal
   durability is uncertain.
3. Capture the project ID, run/mailbox IDs, UTC timestamps, redacted diagnostic
   context, engine/version identity, and the customer's change reference.
4. Follow the production runbook's crash/restart procedure. Treat interrupted
   rows as unresolved until reviewed; do not infer success or failure solely
   from a process exit or message count.
5. Escalate provider throttling or authentication incidents to the provider
   administrator with the affected tenant/account domain and observed retry
   guidance. Do not globally raise concurrency to work around a domain limit.
6. Resume only after the incident owner confirms process ownership, destination
   state, required preflight, and customer approval. Record the decision and
   any residual exceptions in the customer change record.

## 5. Closeout and support handoff

For each customer wave, retain together under the customer's retention policy:

- the ledger and its verified backup;
- verification, project-health, and post-migration reports;
- completed customer-proof export and signature verification result, if used;
- qualification evidence for the exact provider pair, software release, and
  engine binary, if available;
- the change approvals, exception acknowledgements, and incident references.

Customer proof is a signed statement about evidence durably recorded by the
ledger; it is not an independent attestation of every message unless its
verification evidence explicitly supports that claim. Never label a provider
pair qualified from a customer proof alone. Delete or archive artifacts only
under the customer's documented retention and legal-hold policy.

The MSP service owner should review dead-letter webhooks, unresolved mailbox
states, backup/restore drills, supported runtime versions, and open qualification
gaps before accepting the next customer wave. Centralized fleet monitoring,
tenant boundaries, and RBAC remain outside the current product boundary.
