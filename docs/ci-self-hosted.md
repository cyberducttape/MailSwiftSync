# MailSwiftSync CI: Self-Hosted Runner Configuration

## Overview

GitHub Actions **hosted runners** have security restrictions that prevent certain tests from running:

- **Windows Job Object tests**: Nested job object assignment is not permitted
- **Windows signing tests**: ACL modifications on temporary files are not permitted

To run the complete privileged Windows suite, configure a dedicated
**self-hosted runner** with the required permissions.

For stable release tags, this is now an actual publication gate: the release
workflow requires a runner labelled `self-hosted`, `windows`, `x64`, and
`mailswiftsync-release`. Prerelease tags continue to use the hosted suite. The
privileged tests skip only when `RUNNER_ENVIRONMENT=github-hosted`; on a
self-hosted runner they must execute. If the dedicated runner is unavailable,
stable publication intentionally waits/fails rather than silently omitting the
qualification.

Use a dedicated, isolated runner group restricted to this repository. Prefer an
ephemeral VM that is rebuilt after each job; do not place production secrets on
it. Protect stable release tags and do not run untrusted pull-request code on
this machine. The self-hosted workflow grants the job only `contents: read` and
asserts both the runner type and exact release commit before testing.

## GitHub Actions Hosted Runner Limitations

The following tests skip on GitHub Actions Windows hosted runners:

1. `process::tests::job_supervisor_kills_the_engine_tree_when_dropped` — Requires nested job object assignment
2. `main::tests::proof_signing_validates_file_signature` — Requires ACL modification on temporary keys

These tests are critical for production deployments and must pass on your self-hosted infrastructure.

## Setting Up a Self-Hosted Windows Runner

### Prerequisites

- A Windows machine (Windows Server 2019+ or Windows 10/11 Pro/Enterprise)
- Administrator access to the machine
- Network connectivity to GitHub
- At least 10 GB free disk space

### Installation Steps

1. **Create the runner from GitHub's repository/organization Actions runner settings.**
   Use the short-lived registration token and platform-specific setup command
   shown by GitHub; do not create a broad-scope PAT or save a registration token
   in shell history.

2. **Download and Configure Runner**
   ```powershell
   # Follow the current download and extraction commands displayed by GitHub.
   # Register with these labels in addition to the default windows/x64 labels:
   # mailswiftsync-release
   .\config.cmd --url https://github.com/OWNER/REPOSITORY `
     --token $env:RUNNER_REGISTRATION_TOKEN `
     --labels mailswiftsync-release
   Remove-Item Env:RUNNER_REGISTRATION_TOKEN
   ```

3. **Install and Run as Service**
   ```powershell
   # Install as Windows Service (requires admin)
   .\install_svc.cmd
   
   # Start the service
   Start-Service -Name "GitHub Actions Runner"
   ```

4. **Verify Installation**
   ```powershell
   Get-Service "GitHub Actions Runner" | Select-Object Status, Name
   ```

### Repository Configuration

No workflow edits are needed: `.github/workflows/release.yml` contains the
stable-tag gate and uses the labels above. The ordinary pull-request CI remains
on GitHub-hosted runners and reports the documented restricted-test skips.

## Verification

After setting up the self-hosted runner, use a protected stable release tag
(or an authorized test tag in a disposable repository) and verify:

1. The self-hosted runner appears in GitHub Settings → Actions → Runners
2. The status shows "Idle" (green)
3. The stable release workflow reaches the privileged qualification job
4. The job records `RUNNER_ENVIRONMENT=self-hosted` and the exact tag SHA
5. Windows tests no longer skip with "⊘ Skipping: GitHub-hosted Windows runner..." messages

## Monitoring

Monitor runner health:

```powershell
# Check service status
Get-Service "GitHub Actions Runner"

# View runner logs
Get-Content "C:\actions-runner\_diag\*" -Tail 100
```

## Maintenance

### Updating the Runner

```powershell
# Stop the service
Stop-Service -Name "GitHub Actions Runner"

# Uninstall current version
cd C:\actions-runner
.\remove_svc.cmd

# Download and install new version (repeat steps 2-4 above)

# Start the service
Start-Service -Name "GitHub Actions Runner"
```

### Removing the Runner

```powershell
# Stop the service
Stop-Service -Name "GitHub Actions Runner"

# Uninstall
cd C:\actions-runner
.\remove_svc.cmd

# Deregister from GitHub
# Generate a fresh short-lived removal token in GitHub runner settings first.
.\config.cmd remove --token <short-lived-removal-token>

# Clean up
cd ..
Remove-Item -Recurse C:\actions-runner
```

## Troubleshooting

### Runner Not Connecting

1. Check network connectivity: `Test-NetConnection github.com -Port 443`
2. Verify the registration token came from the correct repository/runner group and has not expired
3. Check runner logs: `Get-Content "C:\actions-runner\_diag\*" -Tail 50`

### Tests Still Skipping

1. Verify `RUNNER_ENVIRONMENT` is `self-hosted` on the qualification runner
2. Check that `skip_on_windows_hosted_runner!()` macro evaluates correctly
3. Confirm runner is self-hosted (not GitHub-hosted)

### Performance Issues

1. Ensure sufficient disk space: `Get-Volume C:`
2. Monitor CPU/Memory during test runs
3. Consider running tests serially: `cargo test -- --test-threads=1`

## Security Considerations

- **Registration tokens**: Use only short-lived tokens generated by GitHub; never store them in runner scripts or shell history
- **Runner Machine**: Keep Windows and runner software updated
- **Network**: Use firewall rules to restrict runner access if possible
- **Credentials**: Never store repository secrets on the runner machine

For enterprise deployments, consider:
- Isolated network segments for CI runners
- Credential management via GitHub Secrets
- Regular security audits of runner infrastructure
