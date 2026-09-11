# Forgepad

An independently designed desktop Git workbench written in Rust. It provides a focused view of working-tree changes, staging, inspecting file diffs, committing, branch switching, history, pushing, and safe local recovery actions.

Forgepad asks for confirmation before discarding a tracked file's working-tree changes or undoing the latest commit. Undoing a commit uses `git reset --soft HEAD~1`, so the commit's contents remain staged.

## Run

```bash
cargo run --release
```

Select a local Git repository using **Browse** or paste its path. Forgepad invokes the installed `git` executable, so Git must be available on your PATH.

## Administration

- Settings live in the operating system's standard configuration directory under `forgepad/settings.conf`. You can pin Forgepad to a managed Git binary.
- Forgepad uses existing Git authentication (SSH agent, Git credential helpers, proxy, and certificate settings). It never stores credentials.
- Network actions only occur when you explicitly use a Git network operation such as **Push**. Forgepad has no telemetry.
- The optional local audit log is stored beside the settings. It records command outcomes and repository paths; commit messages are redacted.
- Run `scripts/package.sh` on each target platform to create a host-native tarball and SHA-256 checksum. Signing and OS-native installers require your own release keys and distribution policy.
