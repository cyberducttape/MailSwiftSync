# Service-manager deployment

`supervise` is a foreground controller for automation-safe queued work. It
does not provide OAuth refresh, a secret broker, or remote Dovecot credential
delivery. Use a service manager only after the queue has been imported,
reviewed, and provisioned with the approved credential references. Keep
Attention and verification-difference rows for operator review.

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

## Operational boundary

This deployment pattern is appropriate for restart-aware supervision of work
that has already passed MailSwiftSync’s durable gates. It is not evidence that
MailSwiftSync supports unattended production cutovers: provider OAuth consent
and refresh, secret-safe remote Dovecot execution, and independent approval of
Attention rows remain operator responsibilities.

