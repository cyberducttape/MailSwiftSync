/// Execution mode owned by the batch controller. Keeping this distinct from
/// the single-mailbox form mode prevents a page-local UI toggle from changing
/// the meaning of a restored or headless batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchExecutionMode {
    Preflight,
    Live,
}

impl BatchExecutionMode {
    pub(crate) fn is_preflight(self) -> bool {
        matches!(self, Self::Preflight)
    }

    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

pub(crate) fn is_verified_terminal_state(state: &str) -> bool {
    matches!(state, "verified" | "verified_with_exceptions")
}
