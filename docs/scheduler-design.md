# MailSwiftSync Scheduler Design

## Implementation Status

**STATUS:** ✅ **PARTIALLY IMPLEMENTED**  
**Last verified:** 2026-09-20  
**Commit:** See `src/maintenance_window.rs` and `supervise` command implementation

### What's Implemented
- ✅ Maintenance window CLI support (`supervise <state> [poll] [n] [window]`)
- ✅ Time-window validation and enforcement
- ✅ Durable state persistence across window boundaries
- ✅ Exit codes for "work complete" vs "window closed"
- ✅ Integration with batch queue processing

### What Remains
- ⏳ Config file parsing (`scheduler.toml`)
- ⏳ Systemd timer templates and documentation
- ⏳ `supervise-dry` command (preview mode)

---

## Problem Statement

Current `supervise` is foreground-only:
- Runs continuously in a terminal or under a service manager
- No scheduling (must be started manually or via systemd service)
- ~~No maintenance-window config~~ ✅ **NOW IMPLEMENTED**
- Limited to "process work until idle" — can now express "run between 2am and 4am, retry if not complete"

**Production need:** Operators want to run migrations during approved maintenance windows without operator attendance, with automatic retry/resume across multiple windows if needed.

## Solution: Maintenance Window Scheduler

### Phase 1: CLI Enhancements (MVP)

Extend `supervise` to accept maintenance window config:

```bash
# Current (foreground): run until 10 consecutive idle polls
mailswiftsync supervise /var/lib/state.db 30 10

# Enhanced: accept maintenance window constraints
mailswiftsync supervise /var/lib/state.db \
  --maintenance-window "02:00-04:00" \
  --max-duration 3600 \
  --retry-on-incomplete

# With config file
mailswiftsync supervise /var/lib/state.db \
  --config /etc/mailswiftsync/scheduler.toml

# Dry-run: show what would be executed this window
mailswiftsync supervise-dry /var/lib/state.db \
  --maintenance-window "02:00-04:00"
```

### Phase 2: Config File

`/etc/mailswiftsync/scheduler.toml`:

```toml
[scheduler]
# Required: time window (local time, 24-hour format)
maintenance_window = "02:00-04:00"

# Optional: max time to run in this window (seconds)
max_duration = 3600

# Optional: after completion/timeout, wait for next window instead of exiting
retry_on_incomplete = true

# Optional: blackout dates/times (no migrations)
blackout_dates = ["2025-12-25", "2025-12-26"]  # Christmas
blackout_windows = ["12:00-13:00"]  # Lunch hours (weekly recurring)

# Optional: max concurrent workers
max_workers = 2

# Optional: log file (if not running under systemd)
log_file = "/var/log/mailswiftsync/supervise.log"

# Optional: email alert on completion/failure
notify_email = "ops@example.com"
notify_on = ["failure", "incomplete"]
```

### Phase 3: Service Integration

#### systemd timer (Linux)

```ini
# /etc/systemd/system/mailswiftsync-supervise.timer
[Unit]
Description=MailSwiftSync nightly migration supervisor
After=network-online.target

[Timer]
# Run at 2am daily
OnCalendar=*-*-* 02:00:00
Persistent=true
AccuracySec=1m

# Stagger starts if multiple timers
RandomizedDelaySec=5m

[Install]
WantedBy=timers.target
```

```ini
# /etc/systemd/system/mailswiftsync-supervise.service
[Unit]
Description=MailSwiftSync supervised migration controller
PartOf=mailswiftsync-supervise.timer

[Service]
Type=oneshot
User=mailswiftsync
ExecStart=/usr/local/bin/mailswiftsync supervise /var/lib/mailswiftsync/state.db \
  --config /etc/mailswiftsync/scheduler.toml
TimeoutStartSec=3h
StandardOutput=journal
StandardError=journal
```

#### cron (any system)

```cron
# Run at 2am every day
0 2 * * * /usr/local/bin/mailswiftsync supervise /var/lib/mailswiftsync/state.db \
  --config /etc/mailswiftsync/scheduler.toml >> /var/log/mailswiftsync/supervise.log 2>&1
```

### Phase 4: Optional REST API

For advanced scheduling (not MVP):

```bash
# Schedule a maintenance window to start now
curl -X POST http://localhost:9999/maintenance-window \
  -H "Authorization: Bearer $token" \
  -d '{
    "project_id": "customer-migration-2025",
    "duration_minutes": 180,
    "max_workers": 4,
    "retry_on_incomplete": true
  }'

# Get scheduler status
curl http://localhost:9999/scheduler-status

# List pending work
curl http://localhost:9999/pending-work
```

## Implementation Plan

### Phase 1: CLI Enhancement (MVP for v0.2)

1. **Extend CLI parser:**
   - Add `--maintenance-window`, `--max-duration`, `--retry-on-incomplete`, `--config` flags to `supervise`
   - Add `supervise-dry` command (show planned work without executing)

2. **Config file parsing:**
   - Parse TOML maintenance-window config
   - Validate time windows (24-hour format)
   - Support comments and defaults

3. **Window validation:**
   - At startup, check if current time is within maintenance window
   - If not in window, exit (let cron/systemd re-trigger at next scheduled time)
   - If in window, run supervise loop for up to `max_duration`

4. **Incomplete handling:**
   - At end of window, if work remains:
    - `retry_on_incomplete=true` → exit 0 (let cron/systemd retry)
    - `retry_on_incomplete=false` → exit 1 (alert operator)

5. **Exit codes:**
   - 0 = completed successfully
   - 1 = incomplete or failed
   - 2 = config error
   - 3 = not in maintenance window

### Phase 2: Config File & Logging (v0.2+)

1. Load config from `/etc/mailswiftsync/scheduler.toml`
2. Add JSON/structured logging for monitoring
3. Support blackout dates (JSON holiday calendar file)
4. Validate config at startup

### Phase 3: Systemd Integration (v0.3+)

Provide official systemd timer and service files in `docs/systemd/`.

### Phase 4: REST API (v1.x, not MVP)

Use a thin HTTP wrapper (`actix-web` or similar) to expose:
- GET `/maintenance-window` — current window status
- GET `/pending-work` — list queued mailboxes
- POST `/maintenance-window` — trigger on-demand window

## Example: Nightly Migration Window

**Scenario:** Customer has a 500-mailbox migration that takes 3 hours. Run every night between 2am and 5am.

**Setup:**

1. Create scheduler config:
   ```toml
   [scheduler]
   maintenance_window = "02:00-05:00"
   max_duration = 10800  # 3 hours
   retry_on_incomplete = true
   ```

2. Create systemd timer:
   ```ini
   [Timer]
   OnCalendar=*-*-* 02:00:00
   ```

3. Verify the queue is imported and provisioned:
   ```bash
   mailswiftsync status /var/lib/state.db  # No Attention rows
   sudo systemctl start mailswiftsync-supervise.timer
   ```

4. Monitor:
   ```bash
   journalctl -u mailswiftsync-supervise.service -f
   mailswiftsync status /var/lib/state.db --summary
   ```

**Execution:**
- Night 1: 2am: starts, completes 400 mailboxes, 100 remain at 4:58am
- Night 1: 5am: time expires, exit 0, cron reschedules for night 2
- Night 2: 2am: starts, resumes from checkpoint, completes remaining 100 by 2:30am
- Night 2: 2:35am: all work done, complete, send alert to ops@example.com

## Success Criteria

1. **Window-aware:** Respects configured maintenance windows, exits when window closes
2. **Retry-capable:** Can resume incomplete work on next window without operator intervention
3. **Monitoring-friendly:** Logs completion/failure with structured JSON for parsing
4. **Systemd-native:** Works with systemd timers on Linux
5. **Cron-compatible:** Works with standard cron for non-systemd systems
6. **Config file support:** Operators can set window in one place, not per invocation
7. **Dry-run:** `supervise-dry` shows what would run without executing
8. **Blackout support:** Can skip blackout dates/windows (holidays, etc.)

## Limitations (By Design)

- Not a full scheduler service (no daemon listening 24/7)
- No cross-project coordination (run one project's migrations in a window, not all)
- No smart retry scheduling (just "next window"); better retry logic is future work
- No API in MVP (only CLI; REST API in v1.x if demand exists)

## Deployment Pattern

```
┌─ Cron / Systemd Timer ─────────────────────────┐
│ "Run at 2am every night"                       │
└──────────────┬──────────────────────────────────┘
               │
               ▼
    mailswiftsync supervise
    /var/lib/state.db
    --config /etc/mailswiftsync/scheduler.toml
               │
               ├─ Check: Am I in the configured window?
               │  (02:00-05:00)
               │
               ├─ If YES: Run batch_live for up to max_duration
               │
               ├─ If NO: Exit 3 (not in window)
               │
               ├─ If INCOMPLETE at time limit:
               │    retry_on_incomplete=true → exit 0
               │    (let cron/timer schedule next window)
               │
               └─ If FAILED:
                  exit 1 (alert operator)

        Service manager handles restart/alerting
        Durable SQLite ledger handles resume
        No operator needed for routine maintenance windows
```

## Questions for Stakeholders

1. **MVP scope:** Is Phase 1 (CLI + basic window validation) enough for v0.2, or required for v1.0?
2. **Config location:** Use `/etc/mailswiftsync/scheduler.toml` or embedded in state.db?
3. **Blackout format:** JSON holiday file or inline config?
4. **Notifications:** Email alerts in MVP, or post-v1.0?
5. **Multi-window:** Can a migration span multiple nights (Friday→Monday) with database checkpoints? (Recommended: yes, but not MVP)

---

## Implementation Task Checklist

- [ ] Add `--maintenance-window`, `--max-duration`, `--retry-on-incomplete`, `--config` to CLI
- [ ] Implement `supervise-dry` command
- [ ] Add time-window validation logic
- [ ] Add TOML config file parsing
- [ ] Add blackout-date support (optional, v0.3)
- [ ] Provide systemd timer/service templates
- [ ] Document cron integration
- [ ] Add integration test: trigger window, verify exit codes
- [ ] Update SERVICE.md deployment guide
- [ ] Add scheduler-status command (show current window, pending work)
