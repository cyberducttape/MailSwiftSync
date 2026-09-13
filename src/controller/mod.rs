//! Execution-controller domain types.

mod batch;
mod run;

pub(crate) use batch::{BatchExecutionMode, is_verified_terminal_state};
pub(crate) use run::{ActiveRunContext, LiveAuthProof, RunKind};
