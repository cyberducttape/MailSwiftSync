# Service-manager deployment

`supervise` is a foreground controller for automation-safe queued work. When
automatic OAuth refresh is configured with an OS-keyring refresh credential,
it refreshes access tokens before live launches, including queued batch
children. It does not provide provider consent, a secret broker, or remote
Dovecot credential delivery. Use a service manager only after the queue has
been imported, reviewed, and provisioned with the approved credential
references. Keep Attention and verification-difference rows for operator
review.

Automatic OAuth refresh is currently qualified only on the Unix shell path.
Windows refuses to launch a plan that requests `--oauthrefreshcmd1/2` until
the packaged imapsync callback contract and native command-shell quoting have
been qualified. On Windows, use a manually refreshed access token or run the
automated migration under a qualified Unix service host; do not treat a saved
refresh-credential reference as proof that a Windows unattended run can
refresh tokens.

## Linux systemd

Create a dedicated service account and ledger directory, install the verified
release binary as `/usr/bin/mailswiftsync`, and review the canonical unit below
for your deployment. Do not run the service as root.

The canonical, syntax-checkable unit is
[`mailswiftsync-supervise.service`](mailswiftsync-supervise.service). Debian
and RPM packages install it as documentation at
`/usr/share/doc/mailswiftsync/docs/distribution/mailswiftsync-supervise.service`; inspect and
copy it to `/etc/systemd/system/mailswiftsync-supervise.service`, adjusting
the paths and service identity if needed. The unit drops all Linux
capabilities, restricts address families to Unix and IP networking, makes
system paths read-only, and limits writes to the ledger directory. These
systemd restrictions do not isolate the engine from data accessible to the
worker UID; keep that account dedicated and unprivileged.
CI runs `systemd-analyze --threshold=40 --offline=yes security` against the
canonical unit and fails if its exposure exceeds 4.0/10.0. Treat this score as
a regression gate, not as a substitute for reviewing the unit and qualifying
it on the target distribution.

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
- Verify the keyring backend can retrieve each required credential as
  `mailswiftsync` under this exact unit sandbox, including after a service
  restart/reboot. Desktop-session keyring access must not be assumed. Run an
  authenticated read-only preflight under the service identity before
  admitting live work. If that backend cannot be made reliably available, do
  not treat keyring references as provisioned for unattended service use.
- The sample unit sets `XDG_RUNTIME_DIR=/run/mailswiftsync` to a private
  runtime directory; it is not a D-Bus session bus. If the selected Linux
  keyring backend uses Secret Service, provision a persistent user bus and
  keyring lifecycle for the dedicated service account, then configure
  `DBUS_SESSION_BUS_ADDRESS` in a systemd drop-in to that account's bus only
  after verifying its socket path and permissions on the target distribution.
  Do not copy a desktop operator's bus address or point the service at another
  UID's bus. The bus and keyring must be available and usable after reboot,
  without an interactive desktop login; otherwise unattended keyring-backed
  OAuth is not provisioned. Re-run the authenticated preflight with the final
  drop-in and sandbox.
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
does not replace provider OAuth consent or initial refresh-token provisioning
(run `mailswiftsync oauth-authorize` once per account, or use the desktop
**Connect … account** flow),
and it does not make remote Dovecot execution available. Independent approval
of Attention rows remains an operator responsibility.

The supervisor treats transient SQLite lock/busy responses during its polling
read as contention and waits for the next poll interval. It still exits on
malformed, unavailable, or permission-denied ledgers so a service manager can
raise a genuine storage incident instead of masking it as a retry.

Credential setup, rotation, revocation, exported refresh-config handling, and
crash-residue response are specified in
[CREDENTIAL-LIFECYCLE.md](CREDENTIAL-LIFECYCLE.md). The controller and engine
share the service UID; systemd restrictions narrow filesystem and kernel
access but do not isolate a compromised engine from credentials accessible to
that account.
