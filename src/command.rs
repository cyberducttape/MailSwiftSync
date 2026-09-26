//! Safe command-line construction primitives shared by engines and plans.

pub(crate) fn remove_option(args: &mut Vec<String>, option: &str) {
    if let Some(index) = args.iter().position(|arg| arg == option) {
        args.remove(index);
        if index < args.len() {
            args.remove(index);
        }
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
    use super::parse_shell_words;

    #[test]
    fn parse_shell_words_preserves_quoted_values() {
        assert_eq!(
            parse_shell_words("--foo 'two words' \"three four\"").unwrap(),
            vec!["--foo", "two words", "three four"]
        );
        assert!(parse_shell_words("--broken '").is_err());
    }
}
