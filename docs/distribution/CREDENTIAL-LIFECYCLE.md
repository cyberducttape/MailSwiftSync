# Credential lifecycle and incident handling

This procedure covers credentials used by a local MailSwiftSync operator or
service account. The product has no central secret broker, tenant boundary, or
authenticated access-control plane. For MSP work, use a separate operating
system identity, host, and ledger per customer; filesystem permissions and
operator procedures are the security boundary.

## Provisioning for a headless service

For the distributed container image, `/var/lib/mailswiftsync` is the only
declared persistent volume. The ledger and its recovery history survive
container replacement when that volume is retained; runtime passfiles and
short-lived process files under `/run/user/10001` do not. A replacement
container must regain the approved keyring or secret-manager access and pass
authenticated preflight before live work can resume. Do not persist the runtime
directory merely to make a missing credential file appear recoverable. CI
tests Docker's state-volume versus container-filesystem boundary using marker
files; it does not prove SQLite ledger recovery or emulate host power loss,
volume-driver guarantees, backups, or restoration. Qualify those against the
actual deployment storage.

OAuth refresh-rotation recovery records are protected by owner-only filesystem
permissions, but are not encrypted by MailSwiftSync. They may contain refresh
configuration material. Root access, storage snapshots, backups, or a
compromised service UID remain within the trust boundary. Use encrypted storage
or an external secret manager with a tested restore path when those threats
are in scope; filesystem permissions alone are not encryption.

1. Create a dedicated, non-login service identity where practical, with access
   only to its ledger/runtime directories, approved engine binary, and
   explicitly selected CA files. Never run the controller or migration engine
   as root.
2. Provision credentials for that same identity using a supported keyring
   backend. A desktop user's keyring is not automatically available to a
   system service. Backend availability, unlock behavior, and persistence vary
   by OS and service configuration.
3. Before admitting a customer queue, exercise credential retrieval and an
   authenticated, read-only IMAP preflight as the service identity under the
   actual unit sandbox. Do not infer service access from a successful desktop
   preflight. If the backend cannot be reached reliably after reboot, do not
   run unattended work with that credential setup.
4. Record the credential owner, provider tenant/account, approved scopes,
   keyring ID (not its value), rotation owner, expiry/consent review date, and
   incident contact in the organization's protected credential inventory.
   Keep secret values out of that inventory.

The sample systemd unit in [SERVICE.md](SERVICE.md) applies filesystem and
process restrictions to the controller and its child engines. The controller
and imapsync/doveadm still run under the same Unix UID; this is not per-engine
privilege separation or a sandbox against a compromised engine. Use a
dedicated host/identity and OS-level egress policy as compensating controls.
Qualify any additional systemd restrictions against process supervision,
keyring access, TLS trust stores, and IMAP connectivity before production use.

## Planned OAuth rotation

1. Create or authorize the replacement provider credential with the minimum
   IMAP scopes and tenant/account restrictions required by the migration.
   Prefer a new keyring ID so the old credential remains a rollback option
   during validation.
2. Run token refresh and authenticated IMAP preflight under the identity that
   will execute the migration. Confirm the account and tenant are the intended
   ones without printing or copying tokens into logs.
3. Update the approved profile/queue to the new keyring ID, re-review its plan
   fingerprint, and perform a dry preflight. Only then admit new work.
4. After existing runs using the old credential have ended and the new
   credential has passed its validation window, revoke the old grant at the
   provider and delete its local keyring entry. Removing an entry in the UI or
   keyring only deletes the local copy; it does **not** revoke provider-side
   consent or invalidate a token already issued.
5. Record the change ID, approver, affected profiles/projects, validation
   result, and provider revocation confirmation without recording secret
   material.

Providers may rotate a refresh token during token exchange. MailSwiftSync
persists the rotated configuration to the configured keyring or refresh
configuration file and uses a private recovery record if persistence fails.
Resolve and validate that recovery state before manually replacing the same
credential. Do not restore a stale exported copy over a newer rotated token.
Recovery comparisons use a random owner-only fingerprint key in the local
MailSwiftSync application-data directory; do not copy or delete that key while
OAuth recovery records are pending. The key is not a credential backup and
does not encrypt refresh configuration or recovery files. Version-1 recovery
records remain readable during upgrade; new records use keyed fingerprints.

## Exported refresh configurations

`mailswiftsync oauth-export-refresh-config` writes a refresh token and client
secret to an owner-only file and refuses to overwrite an existing path. Treat
that file as a live credential, not as ordinary configuration:

- Create it in a private directory on a protected local filesystem; do not put
  it in source control, shared storage, a ticket, email, or an unencrypted
  backup.
- Transfer it directly into the approved secret manager, verify the stored
  value by running the token helper, and remove the local export promptly.
- If file-backed automation is explicitly used, restrict both the config and
  its sibling recovery sidecar to the service identity. The helper may update
  the file when the provider rotates its refresh token; external copies are
  not automatically synchronized.
- Re-export or securely update the external secret manager after rotation,
  then remove stale copies under the organization's media/backup retention
  controls. Unlinking a file is not guaranteed secure erasure on SSDs,
  snapshots, journaled filesystems, or backups.
- On suspected exposure, revoke the provider grant immediately, replace the
  credential, invalidate relevant access tokens where the provider allows,
  inspect access/audit logs, and treat every copy in snapshots/backups as
  exposed until its retention expires or the storage is cryptographically
  erased.

## Failed, interrupted, or abandoned migrations

Failure alone is not a reason to revoke credentials if an approved recovery
or retry is expected. Keep them only for the documented recovery window, with
the same least-privilege and access controls. At closeout, revoke provider
grants that are no longer needed, remove static credentials and refresh
entries from the local keyring, delete temporary exported configs and recovery
sidecars after checking for unresolved rotation state, and update the protected
credential inventory.

Do not manually delete MailSwiftSync run directories while a child may still
be active. On startup, MailSwiftSync cleans run directories after it confirms
all recorded process identities are gone. If process ownership is ambiguous,
age-based cleanup is delayed for seven days; if the ledger cannot be read,
cleanup is blocked. Resolve ownership through the supported recovery flow and
preserve ambiguous data for investigation. A hard kill, host crash, or power
loss can leave owner-only temporary passfiles on disk; cleanup does not imply
secure erasure from storage snapshots or backups.

For suspected credential compromise, stop new admissions, preserve the ledger
and relevant audit context, terminate active work through the owning
controller after establishing process ownership, revoke credentials at the
provider, and follow the organization's incident-response and customer
notification requirements. Do not delete the ledger or force-remove its lock
to make cleanup appear complete.

## Policy and evidence administration

`organization-policy.toml` is local-owner controlled. Keep it owner-only,
manage reviewed changes through the host's configuration-management/audit
system, and retain the approved before/after policy revision with the change
record. Restrict ledger, report, backup, and diagnostic access at the OS and
storage layers. The application does not authenticate operators or centrally
authorize policy edits, credential use, evidence reads/exports, or approvals;
the local audit record is not a substitute for identity-backed, tamper-evident
central administration. Do not expose a shared web dashboard or place
unrelated customers in one trust domain until those controls exist.
