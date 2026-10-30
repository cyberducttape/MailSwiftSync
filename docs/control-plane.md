# Central control plane

`mailswiftsync control` is an optional service for MSPs that run migrations
for several customers. It adds authenticated users, organizational roles,
tenant isolation, scoped API keys, and a tamper-evident audit log. The
desktop application stays the local-first workstation interface, and each
worker keeps its own SQLite execution ledger.

**Mailbox credentials never reach the control plane.** Passwords, OAuth
refresh tokens, and app passwords stay on the worker that runs the
migration. Project plans sent to the service are non-secret settings, and
the service refuses any plan field whose name looks like a credential
(`password`, `secret`, `token`, `credential`, `private_key`).

This implements the authentication, authorization, tenant-isolation, worker
fencing, and audit parts of [the RBAC and tenant-isolation contract](msp-rbac-and-tenant-isolation.md).
Still to come: a worker agent that runs leased assignments through the local
engine (today the worker protocol is server-side only).

## Running it

```sh
umask 077
printf '%s' 'a-long-admin-password' > admin-password
mailswiftsync control init /srv/mailswiftsync/control.db \
  --admin-email admin@msp.example --admin-password-file admin-password
mailswiftsync control serve /srv/mailswiftsync/control.db \
  --listen 0.0.0.0:9470 --tls-cert server.pem --tls-key server-key.pem
mailswiftsync control audit-verify /srv/mailswiftsync/control.db
```

- `init` creates the database (owner-only, `0600`) and the first
  administrator. It refuses to run once any user exists; add further
  administrators through the API so the change is audited.
- `serve` listens on `127.0.0.1:9470` by default. A non-loopback address
  needs `--tls-cert`/`--tls-key`, or `--allow-non-loopback` to acknowledge a
  TLS-terminating reverse proxy in front.
- `audit-verify` recomputes the audit hash chain offline.

The service is single-node: one process owns the SQLite file. Do not put the
file on a network filesystem or share it between processes.

## Signing in with OIDC

Users can sign in through the MSP's identity provider (Microsoft Entra ID,
Google, Okta, Keycloak, or any OpenID Connect provider) instead of, or as
well as, a password. Pass an owner-only (`0600`) TOML file to `serve`:

```toml
# --oidc-config /srv/mailswiftsync/oidc.toml
public_url = "https://control.msp.example"   # how browsers reach this service

[[provider]]
name = "entra"                                # used in the URL: /v1/auth/oidc/entra/…
issuer = "https://login.microsoftonline.com/<directory-id>/v2.0"
client_id = "<application id>"
client_secret_file = "/srv/mailswiftsync/entra-secret"   # optional; owner-only
link_verified_email = true                    # see below
allowed_email_domains = ["msp.example"]       # optional
```

Register `https://control.msp.example/v1/auth/oidc/<name>/callback` as the
redirect URI at the provider.

**Sign-in flow:**
1. A browser opens `GET /v1/auth/oidc/<name>/start`, which redirects to the
   provider (authorization code with PKCE S256, plus a random `state` and
   `nonce`).
2. The provider redirects back to `…/callback`. The service exchanges the
   code and returns `{token, expires_at}`: the same 12-hour session as a
   password sign-in.
3. Each pending sign-in is stored by the digest of its `state`, expires in
   10 minutes, and can be completed once.

**ID-token checks.** The ID token must:
- be signed with RS256 or ES256 by a key in the provider's JWKS (`none` and
  HMAC are refused; keys are matched by `kid` and algorithm; the JWKS is
  cached for an hour and refetched at most once a minute on an unknown key)
- name the configured issuer, and this client as audience (and as `azp`
  when there are several audiences)
- be unexpired, allowing 60 seconds of clock skew
- carry this sign-in's nonce

Discovery and token requests use HTTPS only, follow no redirects, and are
bounded in time and size.

**Accounts are never created by signing in.** An administrator creates the
user first (a password is optional). The first OIDC sign-in links the
provider identity (issuer + subject) to that user only if all of these hold:
- the provider has `link_verified_email = true`
- the ID token says `email_verified: true`
- the email matches the user and, when `allowed_email_domains` is set, one
  of those domains
- the user has no identity from that issuer yet

Later sign-ins match on the subject alone, so a changed email address does
not move the account, and a second provider account cannot take over a
linked user. Linking is audited.

> **Providers without `email_verified`:** some providers, Microsoft Entra ID
> among them, do not put an `email_verified: true` claim in ID tokens by
> default. Only that claim enables linking; other provider-specific claims
> are not consulted. For such a provider, verified-email linking never
> happens, so an identity that is not already linked cannot sign in.

## Roles

Roles follow the contract. The administrator role is service-wide; every
other role is granted per tenant, and a user can hold different roles in
different tenants. Anything not listed is denied.

| Permission | Administrator | Manager | Engineer | Technician | Auditor | Customer |
|---|---|---|---|---|---|---|
| `service:admin` (tenants, users, grants, every key and audit entry) | ✓ | | | | | |
| `tenant:read` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `project:read` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `project:write` | ✓ | ✓ | | | | |
| `project:approve` | ✓ | ✓ | | | | |
| `assignment:read` | ✓ | ✓ | ✓ | ✓ | ✓ | |
| `assignment:create` | ✓ | ✓ | ✓ | | | |
| `worker:manage` | ✓ | ✓ | | | | |
| `evidence:read` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `audit:read` | ✓ | ✓ | | | ✓ | |

**Separation of duties:** whoever last changed a project's plan cannot
approve it. Approval names the plan revision being approved, and any later
plan change withdraws the approval.

## Tenant isolation

- Every tenant-owned route carries the tenant in its path
  (`/v1/tenants/{tenant}/…`). The caller's permission is checked for that
  tenant, and the database query also filters by it. A project ID from
  another tenant matches nothing.
- A tenant (or a resource in a tenant) the caller cannot see is answered
  `404 Not Found`, not `403`, so tenant and project IDs cannot be
  enumerated. The refusal is still audited.
- `GET /v1/tenants` lists only tenants the caller holds a role in
  (administrators see all).

## Credentials

| Credential | Prefix | Lifetime | Notes |
|---|---|---|---|
| Session | `mss_s_` | 12 hours | From `POST /v1/auth/login`; revoked by `POST /v1/auth/logout` or when the user is disabled. |
| API key | `mss_k_` | 1–365 days, required | Bound to one tenant and a set of scopes. |
| Enrollment | `mss_e_` | 1–60 minutes, single use | Exchanged once for a worker credential. |
| Worker | `mss_w_` | until revoked | One tenant; worker endpoints only. |

Only SHA-256 digests of tokens are stored. Present tokens as
`Authorization: Bearer <token>`.

**Passwords** are hashed with Argon2id at the RFC 9106 / OWASP defaults and
need at least 12 characters. Five consecutive failures lock the account for
15 minutes. Unknown accounts, wrong passwords, and disabled accounts all get
the same answer and cost the same hashing time.

**API keys:**
- Issued only from a signed-in session; a key cannot mint keys.
- Each requested scope must be a permission the owner currently holds in
  that tenant. `service:admin` can never be a key scope.
- Every request re-checks the key's scopes against the owner's current
  grants, so removing a grant also narrows the owner's existing keys.
- Disabling a user revokes all their sessions and keys.

## Audit log

Every mutation writes its audit entry in the same SQLite transaction as the
change itself, so a committed change can never be missing from the log.
Refusals (wrong role, other tenant, conflict, invalid input) and reads of
tenant data are audited too. Each entry records:

- time, request ID, and actor (user, plus the API key if one was used)
- tenant, action, and resource
- the decision (`allow` or `deny`) and a short detail

Entries are hash-chained: each hash covers the previous entry's hash and
every field of the entry. Triggers make the table append-only, and
`audit-verify` (or `GET /v1/audit/verify`) detects any edit, deletion, or
reordering made to the file directly.

## Workers and fenced leases

A worker belongs to exactly one tenant, so it never receives another
customer's work.

1. **Enroll.** A user with `worker:manage` in the tenant calls
   `POST /v1/tenants/{t}/workers/enrollments` `{name, expires_in_minutes}`
   (1–60 minutes, default 15). This returns a single-use `mss_e_` enrollment
   token.
2. **Exchange.** The worker calls `POST /v1/worker/enroll`
   `{capabilities: [...]}` with that token as its bearer and receives a
   `mss_w_` worker credential, shown once. Worker credentials authenticate
   only the `/v1/worker/…` endpoints, and user credentials never
   authenticate them.
3. **Queue work.** A user with `assignment:create` queues one assignment per
   mailbox label: `POST /v1/tenants/{t}/projects/{p}/assignments`
   `{mailboxes: [...]}`. The project's plan must be approved at its current
   revision.
4. **Lease.** The worker calls `POST /v1/worker/lease` `{lease_seconds}`
   (30–900, default 300) and receives one assignment: the non-secret plan,
   the mailbox label, and a **fencing token**. The mailbox's credentials are
   already on the worker.
5. **Renew and complete.** `POST /v1/worker/assignments/{a}/renew` and
   `/complete` `{fencing_token, outcome: completed|failed, result}` succeed
   only for the worker holding the lease, presenting the current token,
   before the lease expires. Otherwise the answer is `409 Conflict` (or `401`
   if the worker was revoked), and the worker must stop.

Fencing rules:
- Every lease, including re-leasing an expired one, raises the fencing
  token. A worker that stalls past its lease therefore cannot overwrite the
  result of the worker that took over.
- After 10 leases that expired without completing, the assignment fails.
- Revoking a worker (`POST /v1/tenants/{t}/workers/{w}/revoke`) stops its
  credential at once and returns its leases to the queue.
- A plan change withdraws approval. Its queued assignments stop leasing,
  and renewals of running leases are refused so workers stop.
- Results are non-secret JSON and refused if they carry credential-like
  fields.
- Every lease, renewal, completion, and refusal is audited with the worker
  as actor.

## Running a worker agent

`mailswiftsync control worker` runs on the host that holds the mailbox
credentials. It executes leases against that host's own ledger, so the
batch must already be imported and validated there, the same as for
`headless batch-live`.

1. **Export the plan.** `mailswiftsync control worker plan <state.db>
   [--project <ledger-project-id>]` prints a plan document (format
   `mailswiftsync.worker-plan` v1). It holds the ledger project ID and, for
   each durable batch mailbox, its label and a SHA-256 of that row's
   effective plan, keyed by the local job ID. It contains no credentials or
   plan settings.
2. **Approve it.** Store the document as the project's plan
   (`PUT /v1/tenants/{t}/projects/{p}/plan`) and have a second person approve
   it. Queue assignments using the job IDs from the document as mailbox
   labels.
3. **Enroll.** `mailswiftsync control worker enroll --server <url>
   --enrollment-token-file <file> --credential-out <file>` exchanges the
   single-use token and writes the worker credential to a new owner-only
   file.
4. **Run.** `mailswiftsync control worker run <state.db> --server <url>
   --credential-file <file> [--ca-cert <pem>] [--lease-seconds 300]
   [--poll-seconds 30] [--once] [--acknowledge-destination-loss]` leases one
   assignment at a time.

For each lease, the agent:
- Recomputes the local row's plan digest. If the approved document names
  another ledger project, does not list the mailbox, or lists a different
  digest, the agent reports the assignment as `failed` ("refused before
  starting") and starts no transfer. Re-export and re-approve after any
  local plan change.
- Runs the mailbox through the same preflight and live batch path as
  `headless batch-live --mailboxes <job-id>`. An assignment completes only
  when the mailbox reaches a verified terminal state. Any other result is
  reported as `failed` with the controller's message.
- Renews the lease every third of its length. If a renewal is refused
  (`409` or `401`), or the service stays unreachable until the lease would
  expire, it cancels the local run, waits for the engine to stop, and
  reports nothing, because the service would refuse that result.

`--server` must be `https`, except for a loopback address. The agent
ignores proxy settings, does not follow redirects, and sends its credential
only to that URL. `--ca-cert` adds a private CA for the service
certificate. A refused worker credential ends the agent with an error.
Network failures are retried with capped backoff.

## API

Request bodies are JSON (`Content-Type: application/json`, at most 64 KiB,
`Content-Length` framing only). Every response carries `X-Request-Id`, which
matches the audit entry.

| Method and path | Permission |
|---|---|
| `GET /healthz` | none |
| `POST /v1/auth/login` `{email, password}` | none |
| `POST /v1/auth/logout` | session |
| `GET /v1/auth/oidc/{provider}/start`, `GET …/callback?code=&state=` | none (OIDC configured) |
| `GET /v1/me` | any |
| `GET /v1/tenants` / `POST /v1/tenants` `{name}` | any (filtered) / `service:admin` |
| `GET /v1/users` / `POST /v1/users` `{email, display_name, password?}` | `service:admin` |
| `POST /v1/users/{id}/disable` | `service:admin` |
| `POST /v1/users/{id}/grants` and `/grants/remove` `{role, tenant_id?}` | `service:admin` |
| `POST /v1/tenants/{t}/api-keys` `{name, scopes, expires_in_days}` | session in tenant |
| `GET /v1/api-keys`, `POST /v1/api-keys/{id}/revoke` | own keys (all for administrators) |
| `GET`/`POST /v1/tenants/{t}/projects` `{name, plan}` | `project:read` / `project:write` |
| `GET /v1/tenants/{t}/projects/{p}` | `project:read` |
| `PUT /v1/tenants/{t}/projects/{p}/plan` `{plan, revision}` | `project:write` |
| `POST /v1/tenants/{t}/projects/{p}/approve` `{revision}` | `project:approve`, not the last editor |
| `GET /v1/tenants/{t}/workers`, `POST …/workers/enrollments`, `POST …/workers/{w}/revoke` | `worker:manage` |
| `GET`/`POST /v1/tenants/{t}/projects/{p}/assignments` `{mailboxes}` | `assignment:read` / `assignment:create` |
| `POST /v1/worker/enroll` `{capabilities}` | enrollment token |
| `POST /v1/worker/lease`, `POST /v1/worker/assignments/{a}/renew` and `/complete` | worker credential |
| `GET /v1/tenants/{t}/audit?limit=&before=` | `audit:read` |
| `GET /v1/audit`, `GET /v1/audit/verify` | `service:admin` |
