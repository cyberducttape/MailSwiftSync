//! Bounded presentation helpers for operator-facing output.

use crate::output::BoundedLineBuffer;
use crate::{MAX_DIAGNOSTIC_LINE_BYTES, MAX_VISIBLE_OUTPUT_BYTES, MAX_VISIBLE_OUTPUT_LINES};

pub(crate) fn markdown_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
}

pub(crate) fn push_visible_output(output: &mut BoundedLineBuffer, line: String) {
    output.push_bounded(
        truncate_utf8(&line, MAX_DIAGNOSTIC_LINE_BYTES),
        MAX_VISIBLE_OUTPUT_LINES,
        MAX_VISIBLE_OUTPUT_BYTES,
    );
}

/// Replace only caller-supplied secrets before text enters an operator-facing
/// journal. Keeping this beside bounded output handling makes it harder for a
/// future event path to skip sanitization accidentally.
pub(crate) fn redact_secrets<'a>(line: &str, secrets: impl IntoIterator<Item = &'a str>) -> String {
    crate::process::redact_known_secrets(line.to_owned(), secrets)
}

pub(crate) fn truncate_utf8(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Case-fold text for search matching. ASCII stays allocation-light; other
/// text gets Unicode lowercasing plus the full-folding expansions that plain
/// lowercasing misses (ß/ẞ → "ss", final sigma → σ), so "STRASSE" finds
/// "Straße" and "ΟΔΟΣ" finds "οδος".
pub(crate) fn fold_search_text(value: &str) -> String {
    if value.is_ascii() {
        return value.to_ascii_lowercase();
    }
    let mut folded = String::with_capacity(value.len());
    for lower in value.chars().flat_map(char::to_lowercase) {
        match lower {
            'ß' => folded.push_str("ss"),
            'ς' => folded.push('σ'),
            other => folded.push(other),
        }
    }
    folded
}

/// Case-insensitive substring match for filter boxes. Mailbox identities and
/// labels are routinely internationalized, so this folds Unicode case; the
/// all-ASCII path stays allocation-free for large queues.
pub(crate) fn contains_case_insensitive(value: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if value.is_ascii() && needle.is_ascii() {
        return value
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()));
    }
    fold_search_text(value).contains(&fold_search_text(needle))
}
