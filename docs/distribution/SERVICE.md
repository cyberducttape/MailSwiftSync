# Service-manager deployment

`supervise` is a foreground controller for automation-safe queued work. When
automatic OAuth refresh is configured with an OS-keyring refresh credential,
it refreshes access tokens before live launches, including queued batch
children. It does not provide provider consent, a secret broker, or remote
Dovecot credential delivery. Use a service manager only after the queue has
been imported, reviewed, and provisioned with the approved credential
references. Keep Attention and verification-difference rows for operator
review.

## Linux systemd

Create a dedicated service account and ledger directory, install the verified
release binary at `/usr/local/bin/mailswiftsync`, and adjust the paths below to
your deployment. Do not run the service as root.

```ini
# /etc/systemd/system/mailswiftsync-supervise.service
[Unit]
Description=MailSwiftSync supervised migration controller
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=mailswiftsync
Group=mailswiftsync
ExecStart=/usr/local/bin/mailswiftsync supervise /var/lib/mailswiftsync/state.db 30 0
Restart=on-failure
RestartSec=10s
TimeoutStopSec=90s
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/mailswiftsync
StateDirectory=mailswiftsync
RuntimeDirectory=mailswiftsync
Environment=XDG_RUNTIME_DIR=/run/mailswiftsync

[Install]
WantedBy=multi-user.target
```

Before enabling it, verify the binary and ledger as the service account:

```text
sudo -u mailswiftsync /usr/local/bin/mailswiftsync --version
sudo -u mailswiftsync /usr/local/bin/mailswiftsync status /var/lib/mailswiftsync/state.db
sudo systemctl daemon-reload
sudo systemctl enable --now mailswiftsync-supervise.service
sudo journalctl -u mailswiftsync-supervise.service -f
```

Before starting the service, confirm the operational hand-off:

- The imported queue has been reviewed and its plan fingerprints are the ones
  approved for this window.
- Every row selected for automation has a usable OS-keyring credential
  reference. Passwords and OAuth tokens are never restored from the SQLite
  ledger, and a queue containing only transient in-memory credentials will
  remain blocked until an operator provisions credentials again.
- The service account can access the approved engine executable, runtime
  directory, ledger, and any explicitly configured trust bundle, but not
  unrelated customer files.
- A separate operator has a recovery path for Attention, verification
  differences, cancellation, and storage failures. The service intentionally
  leaves those states for review instead of retrying them blindly.

Keep the GUI closed while the service owns the ledger. The instance lock is
intentional; never delete it to bypass ownership. Back up the ledger while the
service is stopped or use the `backup` command, which takes the same lock.

## Windows service wrapper

The portable Windows binary is a console process rather than a native Windows
service. Use an approved wrapper such as WinSW or NSSM if a service manager is
required. Configure the wrapper to run under a dedicated, non-administrator
account with “restart on failure” and a graceful stop timeout.

The wrapper command is:

```text
C:\Program Files\MailSwiftSync\mailswiftsync-x86_64-pc-windows-msvc.exe supervise C:\ProgramData\MailSwiftSync\state.db 30 0
```

Grant the service account access only to the ledger/runtime directories and
the approved engine executable. Keep the ledger on a protected volume, verify
the release checksum before installation, and collect wrapper stdout/stderr as
restricted operational logs. Do not place passwords or OAuth tokens in the
wrapper command line, environment configuration, or service definition.

MailSwiftSync requests `CREATE_BREAKAWAY_FROM_JOB` before assigning an engine
to its own kill-on-close Job Object. This is required when the wrapper or host
already places the controller in a Windows job. The service account and wrapper
policy must permit that breakaway; if the host denies it, MailSwiftSync fails
closed before starting the engine rather than running an unowned migration
process. Validate this prerequisite with a preflight on the target service
host before scheduling a customer migration window.

## Operational boundary

This deployment pattern is appropriate for restart-aware supervision of work
that has already passed MailSwiftSync’s durable gates. Automatic token refresh
does not replace provider OAuth consent or initial refresh-token provisioning,
and it does not make remote Dovecot execution available. Independent approval
of Attention rows remains an operator responsibility.
