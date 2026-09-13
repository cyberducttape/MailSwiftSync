# Headless container deployment

The repository includes a Linux container definition for CLI and supervised
control-plane operation. It is not a hosted service and does not provide a
display server. Build it from a checked-out commit so the image contains the
locked Rust dependency graph and the repository's pinned toolchain:

```text
docker build --tag mailswiftsync:local .
docker run --rm mailswiftsync:local --version
```

Persist only the application data volume at `/var/lib/mailswiftsync`. It holds
the SQLite ledger and should be backed up with the `backup` command. The
runtime volume `/run/user/10001` is an optional runtime mount for short-lived
credential files; do not replace it with a shared host directory.

Example status and backup commands:

```text
docker run --rm \
  -v mailswiftsync-state:/var/lib/mailswiftsync \
  mailswiftsync:local status /var/lib/mailswiftsync/state.db

docker run --rm \
  -v mailswiftsync-state:/var/lib/mailswiftsync \
  -v "$PWD/backups:/backups" \
  mailswiftsync:local backup \
    /var/lib/mailswiftsync/state.db /backups/state.db
```

For continuous automation, run `supervise` under an external service manager
or container restart policy. The container does not grant the process access
to a secret manager, OAuth provider, host Dovecot socket, or a display; those
must be explicitly integrated and documented by the deployment owner.

The image installs pinned Debian Bookworm `dovecot-core` and `dovecot-imapd`
packages and a pinned upstream `imapsync` Debian artifact because Bookworm does not provide `imapsync` in its
default repositories. The artifact version and SHA-256 are recorded in the
Dockerfile and verified before installation. Record the image digest and the
reported engine versions in the migration change record. Native image signing
and a published registry are release engineering work still required before
treating an image as an official distribution artifact.
