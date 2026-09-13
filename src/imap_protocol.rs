//! Small, case-insensitive helpers for IMAP response atoms.

pub(crate) fn atom_eq(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

pub(crate) fn is_tagged_response(line: &str, tag: &str) -> bool {
    let mut fields = line.split_whitespace();
    fields.next() == Some(tag) && fields.next().is_some()
}

pub(crate) fn is_untagged_response(line: &str, keyword: &str) -> bool {
    let mut fields = line.split_whitespace();
    fields.next() == Some("*") && fields.next().is_some_and(|actual| atom_eq(actual, keyword))
}

pub(crate) fn advertises_capability(response: &str, capability: &str) -> bool {
    response.lines().any(|line| {
        let mut fields = line.split_whitespace();
        if fields.next() != Some("*")
            || !fields
                .next()
                .is_some_and(|keyword| atom_eq(keyword, "CAPABILITY"))
        {
            return false;
        }
        fields.any(|actual| atom_eq(actual, capability))
    })
}

#[cfg(test)]
mod tests {
    use super::{advertises_capability, atom_eq, is_tagged_response, is_untagged_response};

    #[test]
    fn protocol_atoms_compare_without_case_or_substring_false_positives() {
        assert!(atom_eq("LiSt", "LIST"));
        assert!(is_untagged_response("* pReAuTh ready", "PREAUTH"));
        assert!(is_tagged_response("a001 oK completed", "a001"));
        assert!(advertises_capability(
            "* CaPaBiLiTy imap4rev1 StArTtLs",
            "STARTTLS"
        ));
        assert!(!advertises_capability(
            "* OK STARTTLS is unavailable",
            "STARTTLS"
        ));
    }
}
