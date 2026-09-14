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
    let mut safe = line.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            safe = safe.replace(secret, "[REDACTED]");
        }
    }
    safe
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

/// Allocation-free matching for the ASCII identifiers used by workspace
/// filters. The query is normalized by callers when repeated matching is
/// needed; this helper preserves the original strings.
pub(crate) fn contains_ascii_case_insensitive(value: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    value
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}
