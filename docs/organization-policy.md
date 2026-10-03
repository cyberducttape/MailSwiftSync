# Organization policy

Administrators can place an owner-only file at:

```text
<OS config directory>/mailswiftsync/organization-policy.toml
```

Example:

```toml
require_tls = true
allow_plain_imap = false
allow_destination_deletion = false
minimum_verification = "metadata" # aggregate, metadata, or body
max_concurrency = 10

[providers.google]
max_concurrency_per_tenant = 4
```

The policy is read when preflight is assessed and is rechecked immediately
before batch admission. A malformed, unreadable, group/world-accessible, or
otherwise invalid policy blocks the operation. Missing policy means the
backwards-compatible permissive defaults; it does not create an organization
policy by itself.

Provider entries cap concurrent work independently for each mailbox domain
on that provider, on either source or destination. They do not reduce the
global batch worker ceiling, so unrelated tenants may use available workers
while one tenant remains capped. `google`, `google_workspace`, and `o365` are
accepted aliases for the canonical provider names. This is a local safety
boundary, not a tenant-management system: provider quota discovery, RBAC, and
centrally administered policy distribution remain future control-plane work.
