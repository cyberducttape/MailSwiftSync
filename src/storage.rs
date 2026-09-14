//! Storage-specific policies shared by the durable state layer.

/// Bound durable event details so structured diagnostics cannot create
/// unbounded SQLite records. Raw engine transcripts are rejected separately
/// by the event writers.
pub(crate) const MAX_EVENT_DETAIL_BYTES: usize = 16 * 1024;
pub(crate) const EVENT_DETAIL_TRUNCATION_SUFFIX: &str =
    " [diagnostic detail truncated by MailSwiftSync]";

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
    use super::{EVENT_DETAIL_TRUNCATION_SUFFIX, MAX_EVENT_DETAIL_BYTES, bounded_event_detail};

    #[test]
    fn event_detail_limit_is_utf8_safe_and_bounded() {
        let bounded = bounded_event_detail(&"é".repeat(MAX_EVENT_DETAIL_BYTES * 2));
        assert!(bounded.len() <= MAX_EVENT_DETAIL_BYTES);
        assert!(bounded.ends_with(EVENT_DETAIL_TRUNCATION_SUFFIX));
    }
}
