# Scaling large migrations

A single MailSwiftSync batch queue is bounded to 1–16 concurrent workers
(`batch_concurrency`, both for preflight and live migration). That ceiling is
deliberate, not arbitrary: it balances migration-window speed against
provider-side throttling, and most hosted providers rate-limit or temporarily
block a source or destination account well before 16 simultaneous IMAP
sessions become the bottleneck. Raising it in code would trade a real safety
margin for a number, without changing what the destination or source server
will actually tolerate — so it isn't something to work around inside one
queue.

For a migration larger than one 16-worker queue comfortably covers in the
available window, shard across multiple independent MailSwiftSync instances
instead of trying to widen one queue.

## Sharding by instance

Each MailSwiftSync instance owns exactly one durable SQLite ledger, guarded by
an instance lock (`state_lock_prevents_two_instances_and_releases_on_drop`) so
two processes can never contend for the same ledger file. That is also the
scale-out primitive: point separate instances at separate ledgers and they
cannot interfere with each other, on the same host or different ones.

1. Split the source mailbox list into shards (by row count, by customer, by
   destination namespace — whatever divides your batch spreadsheet
   cleanly). Each shard becomes its own CSV/XLS/XLSX import.
2. Give each shard its own state path:

   ```bash
   MAILSWIFTSYNC_STATE_PATH=/var/lib/mailswiftsync/shard-a/state.db mailswiftsync headless ... batch-live
   MAILSWIFTSYNC_STATE_PATH=/var/lib/mailswiftsync/shard-b/state.db mailswiftsync headless ... batch-live
   ```

   or pass an explicit path to `headless`/`supervise` directly. Each shard is
   an independent 1–16-worker queue, so N shards give you up to `16N`
   concurrent transfers — bounded by what the source and destination can
   actually absorb, not by this product.
3. Keep shards on separate hosts (or at least separate outbound IPs) when the
   destination provider's throttling is per-source-IP rather than
   per-account; check with the destination provider before assuming
   horizontal scale-out helps.

## Seeing all shards at once

Splitting the ledger also splits visibility: `status` and `customer-proof`
each read one ledger. Use `fleet-status` to get one combined view across every
shard without opening each ledger by hand:

```bash
mailswiftsync fleet-status /var/lib/mailswiftsync
```

This scans the given directory (recursively) for `*.db` files that look like
MailSwiftSync ledgers, reads each with the same secret-free summary
`status --summary` uses, and returns one JSON object combining every shard's
project and mailbox-state counts plus a per-ledger breakdown. It is read-only
and does not take the instance lock, so it is safe to run alongside live
shards. See [Fleet visibility](Fleet-visibility.md) for the full command
reference.

## What sharding does not solve

- **Provider-side tenant limits.** Splitting your own concurrency does not
  raise the destination's or source's own rate limits; coordinate migration
  windows with the provider for genuinely large tenants.
- **Message-level reconciliation.** Each shard's evidence is exactly as
  strong as a single-instance run's (aggregate/engine evidence, not
  independent per-message proof); sharding does not change that boundary.
- **A shared scheduler.** Nothing here coordinates *when* shards run beyond
  what you script yourself (cron, a service manager, or a simple wrapper that
  launches N `supervise` processes with different state paths). See the
  [service deployment guide](../distribution/SERVICE.md) for single-instance
  service-manager examples to replicate per shard.
