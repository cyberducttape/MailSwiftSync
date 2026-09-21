# MailSwiftSync CI: Self-Hosted Runner Configuration

## Overview

GitHub Actions **hosted runners** have security restrictions that prevent certain tests from running:

- **Windows Job Object tests**: Nested job object assignment is not permitted
- **Windows signing tests**: ACL modifications on temporary files are not permitted

To run the complete test suite with full verification, configure optional **self-hosted runners** with unrestricted permissions.

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

1. **Create a GitHub Personal Access Token (PAT)**
   - Go to GitHub Settings → Developer Settings → Personal Access Tokens
   - Create token with `admin:org_self_hosted_runner` scope
   - Save the token securely

2. **Download and Configure Runner**
   ```powershell
   # Create a directory for the runner
   mkdir C:\actions-runner
   cd C:\actions-runner
   
   # Download the latest runner
   Invoke-WebRequest -Uri "https://github.com/actions/runner/releases/download/v2.317.0/actions-runner-win-x64-2.317.0.zip" `
     -OutFile "actions-runner-win-x64-2.317.0.zip"
   
   # Extract the runner
   Expand-Archive -Path "actions-runner-win-x64-2.317.0.zip"
   ```

3. **Configure the Runner**
   ```powershell
   cd C:\actions-runner
   
   # Configure with your GitHub org/repo and PAT
   .\config.cmd --url https://github.com/YOUR-ORG/MailSwiftSync `
     --token YOUR_PAT_TOKEN `
     --labels windows,self-hosted `
     --runnergroup Default
   ```

4. **Install and Run as Service**
   ```powershell
   # Install as Windows Service (requires admin)
   .\install_svc.cmd
   
   # Start the service
   Start-Service -Name "GitHub Actions Runner"
   ```

5. **Verify Installation**
   ```powershell
   Get-Service "GitHub Actions Runner" | Select-Object Status, Name
   ```

### Repository Configuration

Update `.github/workflows/ci.yml` to use self-hosted runner:

```yaml
jobs:
  native-runtime-tests:
    name: Native runtime tests (${{ matrix.os }})
    strategy:
      matrix:
        include:
          - os: windows-latest
            runs-on: ubuntu-latest  # Use for hosted runner (limited tests)
          - os: windows-self-hosted
            runs-on: [self-hosted, windows]  # Use for self-hosted runner (full tests)
    runs-on: ${{ matrix.runs-on }}
```

Or, create a separate job for self-hosted Windows:

```yaml
  native-runtime-tests-self-hosted:
    name: Native runtime tests (Windows - self-hosted)
    runs-on: [self-hosted, windows]
    steps:
      # Full test suite runs here without skipping Windows Job Object or ACL tests
```

## Verification

After setting up the self-hosted runner, push a test commit and verify:

1. The self-hosted runner appears in GitHub Settings → Actions → Runners
2. The status shows "Idle" (green)
3. Workflow runs appear in the Actions tab
4. Windows tests no longer skip with "⊘ Skipping: GitHub Actions Windows runner..." messages

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
.\config.cmd remove --token YOUR_PAT_TOKEN

# Clean up
cd ..
Remove-Item -Recurse C:\actions-runner
```

## Troubleshooting

### Runner Not Connecting

1. Check network connectivity: `Test-NetConnection github.com -Port 443`
2. Verify PAT token is valid and not expired
3. Check runner logs: `Get-Content "C:\actions-runner\_diag\*" -Tail 50`

### Tests Still Skipping

1. Verify GITHUB_ACTIONS environment variable is set in runner context
2. Check that `skip_on_windows_hosted_runner!()` macro evaluates correctly
3. Confirm runner is self-hosted (not GitHub-hosted)

### Performance Issues

1. Ensure sufficient disk space: `Get-Volume C:`
2. Monitor CPU/Memory during test runs
3. Consider running tests serially: `cargo test -- --test-threads=1`

## Security Considerations

- **PAT Token**: Store securely; rotate regularly
- **Runner Machine**: Keep Windows and runner software updated
- **Network**: Use firewall rules to restrict runner access if possible
- **Credentials**: Never store repository secrets on the runner machine

For enterprise deployments, consider:
- Isolated network segments for CI runners
- Credential management via GitHub Secrets
- Regular security audits of runner infrastructure
