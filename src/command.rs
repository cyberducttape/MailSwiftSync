//! Safe command-line construction primitives shared by engines and plans.

/// Remove all occurrences of a generated secret option/value pair.
///
/// The preview builder uses a fixed placeholder, so a following option token
/// is unambiguously a missing value and must be preserved. All matching pairs
/// are removed even when the shape is malformed, ensuring callers never retain
/// a duplicate generated secret argument while still receiving an error.
pub(crate) fn remove_placeholder_options(
    args: &mut Vec<String>,
    option: &str,
    placeholder: &str,
) -> Result<(), String> {
    let mut filtered = Vec::with_capacity(args.len());
    let mut occurrences = 0usize;
    let mut malformed = false;
    let mut index = 0usize;

    while index < args.len() {
        if args[index] != option {
            filtered.push(args[index].clone());
            index += 1;
            continue;
        }

        occurrences += 1;
        match args.get(index + 1) {
            Some(value) if value == placeholder => index += 2,
            Some(value) if value.starts_with("--") => {
                malformed = true;
                // Preserve the adjacent option; it is not this option's value.
                index += 1;
            }
            Some(_) => {
                malformed = true;
                // Do not retain an unexpected value where a secret placeholder
                // was expected: it may itself be sensitive.
                index += 2;
            }
            None => {
                malformed = true;
                index += 1;
            }
        }
    }

    *args = filtered;
    if malformed {
        Err(format!("malformed generated argument pair for {option}"))
    } else if occurrences > 1 {
        Err(format!("duplicate generated argument {option}"))
    } else {
        Ok(())
    }
}

pub(crate) fn parse_shell_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut in_token = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            in_token = true;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            in_token = true;
            continue;
        }
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if in_token {
                    words.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            None => current.push(character),
        }
        if !character.is_whitespace() || quote.is_some() {
            in_token = true;
        }
    }
    if escaped {
        return Err("unfinished escape".into());
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if in_token {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::{parse_shell_words, remove_placeholder_options};

    const PLACEHOLDER: &str = "••••••••";

    #[test]
    fn placeholder_option_removal_rejects_duplicates_and_removes_all_values() {
        let mut args = vec![
            "--password1".into(),
            PLACEHOLDER.into(),
            "--host1".into(),
            "source.example".into(),
            "--password1".into(),
            PLACEHOLDER.into(),
        ];

        assert!(remove_placeholder_options(&mut args, "--password1", PLACEHOLDER).is_err());
        assert_eq!(args, ["--host1", "source.example"]);
    }

    #[test]
    fn placeholder_option_removal_detects_missing_value_at_end() {
        let mut args = vec![
            "--host1".into(),
            "source.example".into(),
            "--password1".into(),
        ];

        assert!(remove_placeholder_options(&mut args, "--password1", PLACEHOLDER).is_err());
        assert_eq!(args, ["--host1", "source.example"]);
    }

    #[test]
    fn placeholder_option_removal_preserves_adjacent_flag() {
        let mut args = vec![
            "--password1".into(),
            "--host1".into(),
            "source.example".into(),
        ];

        assert!(remove_placeholder_options(&mut args, "--password1", PLACEHOLDER).is_err());
        assert_eq!(args, ["--host1", "source.example"]);
    }

    #[test]
    fn placeholder_option_removal_removes_single_pair() {
        let mut args = vec![
            "--password1".into(),
            PLACEHOLDER.into(),
            "--host1".into(),
            "source.example".into(),
        ];

        remove_placeholder_options(&mut args, "--password1", PLACEHOLDER).unwrap();
        assert_eq!(args, ["--host1", "source.example"]);
    }

    #[test]
    fn parse_shell_words_preserves_quoted_values() {
        assert_eq!(
            parse_shell_words("--foo 'two words' \"three four\"").unwrap(),
            vec!["--foo", "two words", "three four"]
        );
        assert!(parse_shell_words("--broken '").is_err());
    }

    #[test]
    fn arbitrary_shell_option_strings_never_panic() {
        let mut state = 0x5348_454c_u32;
        for length in 0..512 {
            let mut input = String::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                input.push(char::from(state as u8));
            }
            let _ = parse_shell_words(&input);
        }
    }
}
