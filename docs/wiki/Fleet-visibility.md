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

- start, stop, or influence any of the ledgers it reads
- require the scanned instances to know about each other, or run on the same
  host — point it at a directory synced from several machines and it works
  the same way
- replace `status`/`customer-proof` for a single project's full detail; use
  `fleet-status` to see where attention is needed across the fleet, then
  drill into the specific ledger with `status <that-ledger> <project-id>`

If you want this pushed to you instead of pulled — for example, to update a
ticket in a PSA when a migration finishes — see
[PSA and ticketing notifications](PSA-notifications.md).
