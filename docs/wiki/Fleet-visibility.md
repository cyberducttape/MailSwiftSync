# Fleet visibility

MailSwiftSync is local-first and single-operator by design: each instance
owns exactly one durable SQLite ledger, and an instance lock prevents two
processes from ever contending for the same one. That is deliberate — it
keeps the trust boundary small and the ledger simple — but it also means
nothing aggregates status across several ledgers automatically. If you run
several instances (several techs, several client engagements, or several
[shards of one large migration](Scaling-large-migrations.md)), `status` only
ever shows you one of them at a time.

`fleet-status` closes that specific gap without changing the underlying
architecture: it is a read-only scan that finds every MailSwiftSync ledger
under a directory and combines their secret-free summaries into one JSON
document.

```bash
mailswiftsync fleet-status /var/lib/mailswiftsync
```

```json
{
  "root": "/var/lib/mailswiftsync",
  "ledger_count": 2,
  "totals": { "total": 41, "ready": 3, "running": 2, "verified": 34, "needs_review": 2 },
  "ledgers": [
    { "path": "/var/lib/mailswiftsync/shard-a/state.db", "summary": { "...": "same shape as `status --summary`" } },
    { "path": "/var/lib/mailswiftsync/shard-b/state.db", "summary": { "...": "..." } }
  ],
  "unreadable": []
}
```

## How it finds ledgers

`fleet-status` walks the given directory (symlinks are not followed, to
avoid a cyclical scan) looking for files ending in `.db`, up to 8 directories
deep and 10,000 candidate files. Every candidate is opened the same
read-only way `status` opens a single ledger — **no instance lock is taken**,
so this is safe to run continuously alongside live shards, including from a
monitoring cron job.

A candidate that is not actually a MailSwiftSync ledger (an unrelated SQLite
file, a stray file that happens to end in `.db`, a corrupted ledger) is
never silently dropped: it is reported under `unreadable` with the reason it
could not be summarized, so a misconfigured scan root or a genuinely broken
shard stays visible instead of quietly under-counting.

## What it is (and isn't)

`fleet-status` is a read-only aggregation view, not a coordination layer. It
does not:

- start, stop, or influence any of the ledgers it reads (`serve` can only
  approve waves, and only for named operators; see below)
- require the scanned instances to know about each other, or run on the same
  host — point it at a directory synced from several machines and it works
  the same way
- replace `status`/`customer-proof` for a single project's full detail; use
  `fleet-status` to see where attention is needed across the fleet, then
  drill into the specific ledger with `status <that-ledger> <project-id>`

## Continuous fleet API

Running `fleet-status` from cron works, but monitoring systems and PSA
integrations usually want an endpoint to poll. `serve` keeps the same view
available over HTTP:

```bash
openssl rand -hex 32 > /etc/mailswiftsync/fleet-api.token
chmod 600 /etc/mailswiftsync/fleet-api.token
mailswiftsync serve /var/lib/mailswiftsync --token-file /etc/mailswiftsync/fleet-api.token
```

It listens on `127.0.0.1:9464` by default (`--listen` changes it) and
rescans the directory every 30 seconds (`--refresh-seconds`, 5–3600). Every
request is answered from the latest scan, so a busy dashboard cannot multiply
the ledger reads.

| Endpoint | Token | Returns |
|---|---|---|
| `/healthz` | no | `ok` while the process accepts connections (liveness) |
| `/readyz` | no | 200 only while the latest scan succeeded and is at most three refresh intervals old, else 503; the JSON body has counts and ages, never paths or errors |
| `/v1/fleet` | yes | exactly the `fleet-status` JSON above |
| `/metrics` | yes | Prometheus text: the [`metrics`](../automation-contract.md#sre-operational-coverage-and-limits) families summed across every readable ledger, plus readable/unreadable ledger counts and the scan time |

Send the token as `Authorization: Bearer <token>`. A Prometheus scrape job
looks like this:

```yaml
scrape_configs:
  - job_name: mailswiftsync
    authorization:
      credentials_file: /etc/prometheus/mailswiftsync-fleet-api.token
    static_configs:
      - targets: ["127.0.0.1:9464"]
```

Alert on `up == 0`, on `mailswiftsync_fleet_ledgers{state="unreadable"} > 0`,
on `increase(mailswiftsync_fleet_refresh_failures_total[15m]) > 0`, and on
`time() - mailswiftsync_fleet_last_successful_scan_timestamp_seconds` growing
beyond a few refresh intervals. Point a load balancer or uptime check at
`/readyz` rather than `/healthz`: a live process whose scans fail is not
ready.

Safety properties:

- **Read-only unless you name operators.** Without `--operators-file`, only
  `GET` and `HEAD` are served. Wave approval is the only change the API can
  ever make; starting, pausing, or cancelling work stays with `supervise` and
  the GUI.
- **Never unauthenticated.** `--token-file` is required. The file must be
  owner-only, and the token must be at least 32 printable characters.
  `/v1/fleet` carries project names and server hostnames, so treat the token
  like customer data.
- **Loopback unless it is encrypted.** With `--tls-cert <chain.pem>
  --tls-key <key.pem>` the listener terminates TLS 1.2+ itself and may bind
  any address, for example `--listen 0.0.0.0:9464`. The key file must be
  owner-only, like the tokens; restart `serve` to pick up a renewed
  certificate. Without TLS the API speaks plain HTTP, and binding beyond
  loopback requires `--allow-non-loopback` and a TLS reverse proxy in front.
  Either way, restrict which hosts can reach the port.
- **Fails visibly.** A scan that fails returns 503 with the reason instead of
  stale numbers, and `/metrics` returns 503 if any ledger's projection is
  truncated rather than publishing partial sums. `serve` refuses to start on
  a directory it cannot read.
- **Bounded.** Request heads are capped at 8 KiB and 5 seconds, and at most
  16 connections are served at once; extra connections get 503 with
  `Retry-After`.

### Approving waves remotely

A wave's mailboxes may migrate live only after someone approves the wave.
To let named people do that from a dashboard or a PSA workflow, give each of
them their own token in an owner-only operators file, one `<token> <name>`
per line:

```text
# /etc/mailswiftsync/fleet-operators — chmod 600
3f9c…e1 Alice Example
8a20…7b bob@msp.example
```

```bash
mailswiftsync serve /var/lib/mailswiftsync \
  --token-file /etc/mailswiftsync/fleet-api.token \
  --operators-file /etc/mailswiftsync/fleet-operators
```

Operator tokens follow the same rules as the read token, must all differ from
it and from each other, and also grant everything the read token does.

| Request | Token | Effect |
|---|---|---|
| `GET /v1/projects/<project-id>/waves` | read or operator | the same JSON as `wave <state> list <project-id>` |
| `POST /v1/waves/<wave-id>/approve` | operator | approves the wave; send no body |

Project IDs appear in `/v1/fleet`, and wave IDs appear in the wave list.
`serve` finds the ledger that holds the ID by itself. What happens on
approval:

- The ledger records the approver as the operator's configured name plus
  `(fleet API)`, for example `Alice Example (fleet API)`. A caller cannot
  choose the name, so the audit trail names whoever holds that token.
- Approval is the ledger's normal one-way approval: a second request returns
  409 with the existing approver, and an approved wave cannot change.
- `serve` takes the same instance lock as `wave approve`. While a supervised
  batch pass is running or the GUI has the ledger open, the request returns
  409 with `Retry-After: 60`, and the approval lands between passes.
- It refuses, with 409, a ledger on an older schema (open it locally once to
  upgrade it) and an ID found in more than one ledger, such as a stray
  backup copy under the served directory.
- Each approval is also written to the service log.

Remove a person's line and restart `serve` to revoke their access.

### Running it as a service

On a systemd host, start from the hardening in the canonical
[`mailswiftsync-supervise.service`](../distribution/mailswiftsync-supervise.service)
unit, run it as the same unprivileged user that owns the ledgers, and change
only the description and command line:

```ini
ExecStart=/usr/bin/mailswiftsync serve /var/lib/mailswiftsync --token-file /etc/mailswiftsync/fleet-api.token
```

Keep `ReadWritePaths` for the ledger directory: SQLite readers of a
write-ahead-logged database still update its shared-memory index.

Keep one `supervise` service per ledger for the actual work; `serve` only
watches them. For several customers, run one `serve` per customer directory
with its own token, so a token never reveals another customer's fleet.

If you want this pushed to you instead of pulled — for example, to update a
ticket in a PSA when a migration finishes — see
[PSA and ticketing notifications](PSA-notifications.md).
