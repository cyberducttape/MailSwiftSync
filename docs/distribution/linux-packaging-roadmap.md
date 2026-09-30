# Linux packaging roadmap

Portable archives and a deterministic signed Debian package are the current
alpha distribution. A stable Linux release
should provide deterministic installation and upgrade paths for operators who
cannot safely assemble a migration control plane from unrelated archives and
packages.

The release gate is:

| Target | Requirement |
|---|---|
| Debian/Ubuntu | Signed `.deb` is implemented; signed APT repository metadata remains |
| RHEL/Alma/Rocky | Signed `.rpm` and signed YUM/DNF repository metadata |
| Architectures | x86_64 and ARM64 artifacts, each tested on its target architecture |
| Shell integration | Implemented: `mailswiftsync completions` for bash, zsh, and fish; the `.deb` installs all three, generated from the shipped binary |
| Documentation | Installed man page plus the one-page safe first migration path |
| Diagnostics | Implemented: `mailswiftsync doctor` reports the transfer engine, state and secret-runtime directory permissions, free space, and engine qualification |
| Engine contract | Implemented: `doctor --strict` exits 3 unless imapsync is exactly `2.314`; the `.deb` declares `Suggests: imapsync (= 2.314)`, and unqualified versions are already transfer-only at run time |
| Lifecycle | The `.deb` has no maintainer scripts and owns only its installed files, so upgrade and removal never touch ledgers; RPM parity remains |

The package must not silently install an unqualified current imapsync release.
If the distribution cannot carry the qualified engine under its licensing and
maintenance constraints, the installer must make the dependency explicit and
block trusted verification until `2.314` is present.

Container deployments have a separate boundary: persist only
`/var/lib/mailswiftsync`; mount `/run/user/10001` as an explicitly sized,
owner-only tmpfs. Runtime credentials and process files must never be placed
in a durable Docker volume.
