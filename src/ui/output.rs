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
