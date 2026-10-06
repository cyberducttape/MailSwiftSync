//! Shell completion scripts generated from the shipped command list.
//!
//! `COMMANDS` is the single source for completion candidates; a test checks
//! it against every command the CLI dispatches, so a new command cannot ship
//! without completion.

/// Public commands, in help order.
pub(crate) const COMMANDS: &[&str] = &[
    "verify",
    "verify-certificate",
    "sign",
    "certificate",
    "migrateaudit",
    "runbook",
    "risk",
    "post-report",
    "post-report-state",
    "backup",
    "restore",
    "status",
    "fleet-status",
    "recover",
    "support-bundle",
    "customer-proof",
    "notify-webhook",
    "supervise",
    "cutover",
    "wave",
    "headless",
    "oauth-authorize",
    "oauth-export-refresh-config",
    "oauth-access-token",
    "recovery-guidance",
    "doctor",
    "install-engine",
    "completions",
    "help",
    "version",
];

pub(crate) const HEADLESS_MODES: &[&str] = &["preflight", "live", "batch-preflight", "batch-live"];
pub(crate) const SHELLS: &[&str] = &["bash", "zsh", "fish"];

/// The completion script for `shell`, or `None` for an unsupported shell.
pub(crate) fn script(shell: &str) -> Option<String> {
    let commands = COMMANDS.join(" ");
    let modes = HEADLESS_MODES.join(" ");
    let shells = SHELLS.join(" ");
    match shell {
        "bash" => Some(format!(
            r#"# bash completion for mailswiftsync
_mailswiftsync() {{
    local cur="${{COMP_WORDS[COMP_CWORD]}}"
    if [[ $COMP_CWORD -eq 1 ]]; then
        COMPREPLY=($(compgen -W "{commands}" -- "$cur"))
        return
    fi
    case "${{COMP_WORDS[1]}}" in
        headless)
            if [[ $COMP_CWORD -eq 3 ]]; then
                COMPREPLY=($(compgen -W "{modes}" -- "$cur"))
                return
            fi
            ;;
        doctor)
            if [[ "$cur" == -* ]]; then
                COMPREPLY=($(compgen -W "--strict" -- "$cur"))
                return
            fi
            ;;
        completions)
            COMPREPLY=($(compgen -W "{shells}" -- "$cur"))
            return
            ;;
    esac
    COMPREPLY=($(compgen -f -- "$cur"))
}}
complete -o filenames -F _mailswiftsync mailswiftsync
"#
        )),
        "zsh" => Some(format!(
            r#"#compdef mailswiftsync
_mailswiftsync() {{
    if (( CURRENT == 2 )); then
        compadd -- {commands}
        return
    fi
    case "$words[2]" in
        headless)
            (( CURRENT == 4 )) && {{ compadd -- {modes}; return; }}
            ;;
        doctor)
            [[ "$PREFIX" == -* ]] && {{ compadd -- --strict; return; }}
            ;;
        completions)
            compadd -- {shells}
            return
            ;;
    esac
    _files
}}
_mailswiftsync "$@"
"#
        )),
        "fish" => Some(format!(
            r#"# fish completion for mailswiftsync
complete -c mailswiftsync -n __fish_use_subcommand -f -a "{commands}"
complete -c mailswiftsync -n "__fish_seen_subcommand_from headless; and test (count (commandline -opc)) -eq 3" -f -a "{modes}"
complete -c mailswiftsync -n "__fish_seen_subcommand_from doctor" -l strict -d "Set the exit status from the checks"
complete -c mailswiftsync -n "__fish_seen_subcommand_from completions" -f -a "{shells}"
"#
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{COMMANDS, SHELLS, script};

    /// Every command `cli.rs` dispatches must be offered for completion.
    #[test]
    fn completion_command_list_matches_cli_dispatch() {
        let cli = include_str!("cli.rs");
        let mut dispatched = Vec::new();
        for line in cli.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("if command == std::ffi::OsStr::new(\"") {
                dispatched.push(rest.split('"').next().unwrap().to_owned());
            }
            if let Some(rest) = line.strip_prefix("if matches!(command.to_str(), Some(") {
                let alternatives = rest.split(')').next().unwrap();
                dispatched.extend(
                    alternatives
                        .split('|')
                        .map(|name| name.trim().trim_matches('"').to_owned()),
                );
            }
        }
        assert!(
            dispatched.len() > 15,
            "dispatch parser found {dispatched:?}"
        );
        for command in dispatched {
            // Flags and internal plumbing are not user commands; `audit` is
            // a legacy alias of `migrateaudit`.
            if command.starts_with('-') || command == "audit" {
                continue;
            }
            assert!(
                COMMANDS.contains(&command.as_str()),
                "`{command}` is dispatched but missing from completions"
            );
        }
    }

    #[test]
    fn every_supported_shell_has_a_script_listing_all_commands() {
        for shell in SHELLS {
            let script = script(shell).unwrap();
            for command in COMMANDS {
                assert!(script.contains(command), "{shell} is missing {command}");
            }
        }
        assert!(script("powershell").is_none());
    }
}
