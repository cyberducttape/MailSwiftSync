//! Storage-specific policies shared by the durable state layer.

/// Bound durable event details so structured diagnostics cannot create
/// unbounded SQLite records. Raw engine transcripts are rejected separately
/// by the event writers.
pub(crate) const MAX_EVENT_DETAIL_BYTES: usize = 16 * 1024;
pub(crate) const EVENT_DETAIL_TRUNCATION_SUFFIX: &str =
    " [diagnostic detail truncated by MailSwiftSync]";

/// Validate operator-authored values that are included in durable audit
/// details. Unicode line and paragraph separators are rejected alongside
/// control characters because they can also forge multiline log output.
pub(crate) fn single_line_audit_text(value: &str, max_bytes: usize) -> Option<String> {
    let value = value.trim();
    let contains_line_break = value
        .chars()
        .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'));
    (!value.is_empty() && value.len() <= max_bytes && !contains_line_break)
        .then(|| value.to_owned())
}

pub(crate) fn bounded_event_detail(detail: &str) -> String {
    if detail.len() <= MAX_EVENT_DETAIL_BYTES {
        return detail.to_owned();
    }

    let content_limit = MAX_EVENT_DETAIL_BYTES.saturating_sub(EVENT_DETAIL_TRUNCATION_SUFFIX.len());
    let mut end = content_limit.min(detail.len());
    while !detail.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}{}", &detail[..end], EVENT_DETAIL_TRUNCATION_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::{
        EVENT_DETAIL_TRUNCATION_SUFFIX, MAX_EVENT_DETAIL_BYTES, bounded_event_detail,
        single_line_audit_text,
    };

    #[test]
    fn event_detail_limit_is_utf8_safe_and_bounded() {
        let bounded = bounded_event_detail(&"é".repeat(MAX_EVENT_DETAIL_BYTES * 2));
        assert!(bounded.len() <= MAX_EVENT_DETAIL_BYTES);
        assert!(bounded.ends_with(EVENT_DETAIL_TRUNCATION_SUFFIX));
    }

    #[test]
    fn operator_audit_text_is_trimmed_bounded_and_single_line() {
        assert_eq!(
            single_line_audit_text("  operator  ", 8).as_deref(),
            Some("operator")
        );
        assert!(single_line_audit_text("", 10).is_none());
        assert!(single_line_audit_text("operator\nforged-event", 64).is_none());
        assert!(single_line_audit_text("operator\u{2028}forged-event", 64).is_none());
        assert!(single_line_audit_text("operator\u{2029}forged-event", 64).is_none());
        assert!(single_line_audit_text("operator-too-long", 8).is_none());
    }
}
