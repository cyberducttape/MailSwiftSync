# Migration snapshot audit benchmark

`scripts/benchmark-migrate-audit.py` measures the end-to-end `migrateaudit`
command on equal synthetic source and destination snapshots. It reports input
bytes, elapsed time, peak process RSS from Linux `/proc`, and the maximum
sampled size of the private SQLite staging directory. The staging directory is
sampled every 10 ms. The benchmark is an engineering measurement, not a
capacity guarantee; it does not measure persistent-disk durability or every
possible resource distribution.

Run it after building the release binary:

```sh
cargo +1.92.0 build --locked --release --bin mailswiftsync
python3 scripts/benchmark-migrate-audit.py \
  --binary target/release/mailswiftsync \
  --sizes 1000 10000 100000 \
  --payload-bytes 512
```

## Local measurement, 2026-11-16

| Field | Observed value |
|---|---|
| Source commit | `3d489dd0012cfaa71e73ce279a01e6b1a43e4c6a` |
| Release binary SHA-256 | `2f8afa2bb9e9e76ecd27417c4fe62b62c256643495d3e16b5e5e89d4c4152270` |
| Host | Ubuntu 7.0.0-43-generic, AMD Ryzen 7 7735U, 16 logical CPUs, 14,734 MiB RAM |
| Toolchain | Rust `1.92.0 (ded5c06cf 2025-12-08)` |
| Storage | `/tmp` tmpfs, 7.2 GiB total; both input files and private staging used this filesystem |
| Workload | Unique `id` identities, identical source/destination records, 512-byte payload field |

| Records per side | Input per side | Elapsed | Peak staging sampled | Peak RSS |
|---:|---:|---:|---:|---:|
| 1,000 | 553,016 B | 1.987 s | 1,548,288 B (1.48 MiB) | 16,328 KiB (15.95 MiB) |
| 10,000 | 5,530,016 B | 13.410 s | 15,409,152 B (14.70 MiB) | 25,272 KiB (24.68 MiB) |
| 100,000 | 55,300,016 B | 104.932 s | 154,275,840 B (147.13 MiB) | 39,708 KiB (38.78 MiB) |

All three reports returned `pass` with equal source and destination input
digests. The 100,000-record result shows roughly linear elapsed time and
staging growth for this one unique-identity/equal-payload shape. It does not
establish mismatch-heavy, duplicate-heavy, near-limit (256 MiB) input, slow
filesystem, or persistent-storage behavior. In particular, tmpfs results do
not qualify disk latency or disk-full recovery.

The staging index uses `(side, category, identity, payload_hash)`; reconciliation
also compares the retained full payload after hash equality. This avoids
indexing the full serialized payload while preserving exact collision-safe
comparison. The measurements above quantify the current implementation's
observed end-to-end cost for the stated fixture only.
