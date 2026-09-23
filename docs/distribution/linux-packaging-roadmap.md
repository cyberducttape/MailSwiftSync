# Linux packaging roadmap

Portable archives are the current alpha distribution. A stable Linux release
should provide deterministic installation and upgrade paths for operators who
cannot safely assemble a migration control plane from unrelated archives and
packages.

The release gate is:

| Target | Requirement |
|---|---|
| Debian/Ubuntu | Signed `.deb` and signed APT repository metadata |
| RHEL/Alma/Rocky | Signed `.rpm` and signed YUM/DNF repository metadata |
| Architectures | x86_64 and ARM64 artifacts, each tested on its target architecture |
| Shell integration | Bash, Zsh, and Fish completions generated from the shipped CLI |
| Documentation | Installed man page plus the one-page safe first migration path |
| Diagnostics | `mailswiftsync doctor` reports missing tools, permissions, runtime mounts, and engine qualification |
| Engine contract | The package or dependency doctor verifies the exact qualified imapsync version (`2.314`) before migration |
| Lifecycle | Upgrade and uninstall preserve the ledger by explicit operator choice and remove only owned package files |

The package must not silently install an unqualified current imapsync release.
If the distribution cannot carry the qualified engine under its licensing and
maintenance constraints, the installer must make the dependency explicit and
block trusted verification until `2.314` is present.

Container deployments have a separate boundary: persist only
`/var/lib/mailswiftsync`; mount `/run/user/10001` as an explicitly sized,
owner-only tmpfs. Runtime credentials and process files must never be placed
in a durable Docker volume.
