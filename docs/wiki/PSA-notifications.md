# PSA and ticketing notifications

MSPs generally track migration work in a PSA/ticketing platform
(ConnectWise, Autotask, Halo, Syncro, ...) rather than by polling
MailSwiftSync directly. `notify-webhook` bridges the two without a
vendor-specific integration. By default it sends credential-free operational
status—project IDs, phases, aggregate mailbox counts, and counts by durable
attention reason, excluding customer names, endpoints, and process details—to
an operator-configured HTTPS URL.
The payload is still sensitive operational metadata and should go only to a
trusted receiver.

```bash
mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123
```

To include project names and source/destination endpoints, explicitly opt in:

```bash
mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123 --include-customer-metadata
```

For a secret-bearing URL path, put the URL in an owner-readable file and set
`MAILSWIFTSYNC_WEBHOOK_URL_FILE`; it takes precedence over the command-line
URL and keeps the path out of process listings and shell history:

```bash
MAILSWIFTSYNC_WEBHOOK_URL_FILE=/run/user/1000/mailswiftsync/webhook-url \
  mailswiftsync notify-webhook /path/to/state.db ignored
```

For a continuously running worker that drains lifecycle events and durable
retries, use `--watch`. The default poll interval is 30 seconds; configure a
bounded interval explicitly when needed:

```bash
mailswiftsync notify-webhook /path/to/state.db ignored --watch --poll-seconds=15
```

Run the worker under the host service manager and keep the URL/authentication
configuration in owner-readable environment files. The SQLite ledger stores
only the endpoint digest and credential-free event payloads.

- With no project ID, the payload covers every project in the ledger (the
  same shape `status --summary` returns with no project ID).
- With a project ID, the payload is scoped to that one project:

  ```bash
  mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123 <project-id>
  ```

- Exit code `0` means the endpoint responded 2xx; any other outcome
  (non-2xx response, connection failure, TLS failure) exits non-zero with a
  message on stderr, so a wrapper script can tell delivery apart from
  silence.

## Wiring it to your PSA

MailSwiftSync does not implement ConnectWise's, Autotask's, or any other
vendor's API directly — it sends one generic JSON POST. Almost every
PSA and automation platform can turn that into a ticket update on its own
side:

- **Zapier / Make / n8n**: use their "Catch Hook" trigger as the URL; build
  the ticket-update step from the JSON fields in their editor. This is the
  lowest-effort path and works with any PSA that platform already supports.
- **ConnectWise / Autotask / Halo native webhooks or REST APIs**: if the
  platform accepts inbound webhooks directly, point `notify-webhook` at that
  URL. If it only exposes an authenticated REST API, put a small receiver
  (a cloud function, or the automation platforms above) between
  MailSwiftSync and the PSA to hold that API credential — MailSwiftSync
  itself does not store or send PSA API credentials.
- **A generic ingest endpoint you already run**: point `notify-webhook`
  directly at it.

If the receiving endpoint itself needs authentication, use a URL file for a
secret path, or configure a Bearer token/custom header through the documented
environment variables or owner-readable secret files. MailSwiftSync rejects
control characters in those values and validates custom header names as HTTP
tokens. It does not persist the webhook URL or credentials in the ledger.

## When to call it

`notify-webhook` is an explicit operator action, not something MailSwiftSync
fires automatically as a side effect of ordinary GUI operation. Call it once
from a scheduler, or run its explicit `--watch` worker under a service manager:

```bash
# After a scripted batch pass:
mailswiftsync headless /path/to/state.db batch-live && \
  mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123

# On a timer, independent of any particular run:
mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123
```

```bash
mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123 --watch
```

For a fleet of shards (see [Scaling large migrations](Scaling-large-migrations.md)),
call it once per shard, or build your own small wrapper around
[`fleet-status`](Fleet-visibility.md) if you want one combined ticket update
instead of one per shard.

## Security notes

- Only `https://` URLs are accepted; there is no way to configure a
  plaintext endpoint.
- The TLS connection is certificate-validated against the standard WebPKI
  root store, the same trust store MailSwiftSync's other outbound HTTPS
  calls (OAuth token refresh) use. There is currently no option to pin a
  private/internal CA for this specific path — use a public-CA endpoint or a
  receiver that has one.
- The default payload is credential-free operational status, not nonsensitive
  data. Its IDs, states, and counts can still identify customer operations.
  With `--include-customer-metadata`, project names and endpoint hosts are also
  transmitted. Treat the receiver and downstream automation as trusted
  infrastructure.
