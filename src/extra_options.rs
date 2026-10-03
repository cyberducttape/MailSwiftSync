//! Fail-closed parser and canonicalizer for the trusted imapsync tuning field.

const MAX_EXTRA_OPTIONS_BYTES: usize = 64 * 1024;
const MAX_EXTRA_OPTION_TOKENS: usize = 128;

/// Parse the trusted tuning field once and regenerate canonical argv tokens.
/// The returned values are the only representation that execution may use;
/// this prevents validation from accepting one spelling while the runner
/// launches a different literal token sequence.
pub(crate) fn canonical(extra_options: &str) -> Result<Vec<String>, String> {
    if extra_options.len() > MAX_EXTRA_OPTIONS_BYTES {
        return Err(format!(
            "Extra options exceed the {MAX_EXTRA_OPTIONS_BYTES}-byte limit"
        ));
    }
    let options = crate::command::parse_shell_words(extra_options)
        .map_err(|error| format!("Extra options: {error}"))?;
    if options.len() > MAX_EXTRA_OPTION_TOKENS {
        return Err(format!(
            "Extra options contain more than {MAX_EXTRA_OPTION_TOKENS} tokens"
        ));
    }
    // This is deliberately an allowlist. The field is trusted application
    // configuration, but imapsync's option surface is powerful and changes
    // over time; an ever-growing denylist cannot establish a safe boundary.
    // Connection, credential, destructive, logging, and execution options
    // remain owned by the typed migration plan.
    #[derive(Clone, Copy)]
    enum OptionType {
        Boolean,
        Integer { min: u64, max: u64 },
    }
    struct OptionSpec {
        name: &'static str,
        kind: OptionType,
    }
    const OPTION_SPECS: &[OptionSpec] = &[
        OptionSpec {
            name: "nofoldersizes",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "skipcrossduplicates",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "subscribe",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "debug",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "maxlinelength",
            kind: OptionType::Integer {
                min: 1,
                max: 16 * 1024 * 1024,
            },
        },
        OptionSpec {
            name: "timeout",
            kind: OptionType::Integer {
                min: 1,
                max: 86_400,
            },
        },
        OptionSpec {
            name: "reconnectretry1",
            kind: OptionType::Integer { min: 0, max: 100 },
        },
        OptionSpec {
            name: "reconnectretry2",
            kind: OptionType::Integer { min: 0, max: 100 },
        },
        OptionSpec {
            name: "errorsmax",
            kind: OptionType::Integer {
                min: 0,
                max: 100_000,
            },
        },
        OptionSpec {
            name: "maxsleep",
            kind: OptionType::Integer {
                min: 0,
                max: 86_400,
            },
        },
        OptionSpec {
            name: "sleep",
            kind: OptionType::Integer {
                min: 0,
                max: 86_400,
            },
        },
    ];
    let mut canonical = Vec::with_capacity(options.len());
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        let (name, inline_value) = option
            .split_once('=')
            .map_or((option.as_str(), None), |(name, value)| (name, Some(value)));
        if !name.starts_with("--") || name.starts_with("---") {
            return Err(format!(
                "Extra options: {name} must use the canonical --option spelling"
            ));
        }
        let normalized_name = &name[2..];
        let Some(spec) = OPTION_SPECS
            .iter()
            .find(|spec| spec.name == normalized_name)
        else {
            return Err(format!(
                "Extra options: {name} is not in the safe imapsync option allowlist; use the typed migration controls for settings controlled by the plan"
            ));
        };
        if option.chars().any(char::is_control) {
            return Err("Extra options cannot contain control characters.".into());
        }
        match spec.kind {
            OptionType::Boolean => {
                if inline_value.is_some() {
                    return Err(format!("Extra options: {name} does not accept a value."));
                }
                canonical.push(format!("--{}", spec.name));
            }
            OptionType::Integer { min, max } => {
                let value = if let Some(value) = inline_value {
                    value
                } else {
                    let Some(value) = options.get(index + 1) else {
                        return Err(format!("Extra options: {name} requires a value."));
                    };
                    if value.starts_with('-') || value.is_empty() {
                        return Err(format!(
                            "Extra options: {name} requires a non-option value."
                        ));
                    }
                    index += 1;
                    value.as_str()
                };
                let parsed = value.parse::<u64>().map_err(|_| {
                    format!("Extra options: {name} requires an integer between {min} and {max}.")
                })?;
                if !(min..=max).contains(&parsed) {
                    return Err(format!(
                        "Extra options: {name} must be between {min} and {max}."
                    ));
                }
                canonical.extend([format!("--{}", spec.name), parsed.to_string()]);
            }
        }
        index += 1;
    }
    Ok(canonical)
}
