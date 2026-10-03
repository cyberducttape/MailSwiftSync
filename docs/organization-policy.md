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
max_concurrency = 8
max_concurrency_per_tenant = 4
max_concurrency_per_credential = 2
```

The policy is read when preflight is assessed and is rechecked immediately
before batch admission. A malformed, unreadable, group/world-accessible, or
otherwise invalid policy blocks the operation. Missing policy means the
backwards-compatible permissive defaults; it does not create an organization
policy by itself.

Provider entries may cap provider-endpoint, per-tenant, and per-credential
concurrency independently on either source or destination. They do not reduce
the global batch worker ceiling, so unrelated domains may use available workers
while a narrower domain remains capped. If aliases configure the same provider
at multiple limits, the strictest value wins. `google`, `google_workspace`,
and `o365` are accepted aliases for canonical provider names. These local
safety ceilings are not provider quota discovery: live provider limits,
credential semantics, RBAC, and centrally administered policy distribution
still require qualification or future control-plane work.
