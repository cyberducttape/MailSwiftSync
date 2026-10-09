# MSP RBAC and tenant isolation contract

This is a required control-plane design, not a capability provided by the
current desktop application. The local GUI, SQLite ledger, and `fleet-status`
command must not be presented as an authorization boundary.

## Required security model

The control plane must authenticate every request, authorize every resource
action, and apply the tenant/project scope in the data layer. Hiding a project
in a UI is insufficient. A worker must receive only an opaque assignment and
the minimum credentials needed for that assignment; it must never receive a
customer's unrelated projects or organization policy.

| Role | Organization scope | Project scope | Sensitive actions |
|---|---|---|---|
| Administrator | Configure policy, users, roles, providers, workers, webhooks | Full | Delete only under retention/hold workflow |
| MSP manager | Manage assigned customers and projects | Create, edit, approve, pause | Cannot approve their own high-risk cutover |
| Migration engineer | No organization policy | Assigned projects | Execute approved work and permitted recovery |
| Technician | No organization policy | Assigned projects/read-limited | Recovery actions explicitly granted per project |
| Auditor | Read-only | Authorized organization/project set | Evidence and audit export; no credentials or mutations |
| Customer | Own customer tenant | Own authorized projects | Progress and final reports only |

Authorization requirements:

- Deny by default; role grants are additive only within an organization and
  project scope.
- Organization, customer, project, mailbox, worker, webhook destination, and
  evidence export are separate resources with explicit parent-scope checks.
- Every read, export, credential operation, policy change, approval, pause,
  resume, deletion request, and webhook change is audit logged with actor,
  tenant, resource, decision, request ID, and timestamp.
- High-risk cutover approval requires separation of duties. The actor who
  changes the plan or requests approval cannot be the sole approver.
- Evidence exports are authorization-checked at export time and are served
  from tenant-scoped storage. Historical evidence is immutable and deletion is
  a retention-governed administrator workflow, never a technician action.
- Webhook destinations and signing secrets are project/organization scoped;
  changing one requires authorization and secret rotation, and cannot redirect
  another customer's events.
- Worker registration uses a short-lived credential, capability declaration,
  revocation, and mTLS or an equivalent mutually authenticated channel.
  Assignments are leased with fencing tokens so a stale worker cannot continue
  after reassignment.

## Data isolation requirements

The control-plane database must enforce tenant ownership with foreign keys and
row-level authorization in every repository query. Object storage paths,
temporary credential files, logs, diagnostics, webhook payloads, and report
downloads must carry the same tenant/project authorization context. Tests must
include cross-tenant identifier substitution, guessed project IDs, revoked
sessions, stale worker leases, and export/download URL reuse.

Local SQLite remains appropriate for a worker's private execution ledger. It
must not be placed on NFS or shared between workers as a substitute for the
control-plane ownership and lease protocol.

## Required acceptance tests

Before MSP production approval, test each role against every project action,
including negative cases, and run concurrent approval/assignment/revocation
tests. Verify that revocation fences an active worker, that audit records are
tamper-evident and tenant-scoped, and that backups/restores preserve tenant
boundaries. A successful UI navigation test is not evidence of isolation.
