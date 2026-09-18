# PSA and ticketing notifications

MSPs generally track migration work in a PSA/ticketing platform
(ConnectWise, Autotask, Halo, Syncro, ...) rather than by polling
MailSwiftSync directly. `notify-webhook` bridges the two without a
vendor-specific integration: it sends the same secret-free JSON
`status --summary` already produces as one HTTPS POST to an
operator-configured URL.

```bash
mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123
```

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

If the receiving endpoint itself needs authentication, embed the token in
the URL (as its own path segment or query parameter) — the same pattern
Slack, Zapier, and PagerDuty inbound webhooks already use. MailSwiftSync
does not attach any additional credential to the request, and does not
persist the webhook URL anywhere but the invoking command line/script.

## When to call it

`notify-webhook` is a standalone primitive, not something MailSwiftSync
fires automatically on every state change (that would mean silently making
outbound network calls as a side effect of ordinary operation, which is not
this project's default). Call it explicitly from whatever already drives
your automation:

```bash
# After a scripted batch pass:
mailswiftsync headless /path/to/state.db batch-live && \
  mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123

# On a timer, independent of any particular run:
mailswiftsync notify-webhook /path/to/state.db https://hooks.example.com/in/abc123
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
- The payload is exactly what `status --summary` already returns:
  secret-free, but not customer-anonymized the way `customer-proof` is (it
  includes endpoint hostnames and project names). Treat the receiving
  endpoint and any automation downstream of it as trusted infrastructure,
  the same as you would the host running MailSwiftSync itself.
