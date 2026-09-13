//! Report builders and artifact writers.
//!
//! Reports depend on durable read models, not on egui state. Keeping this
//! boundary explicit makes the GUI and CLI callers interchangeable.

pub(crate) mod customer;
pub(crate) mod operator;
pub(crate) mod signing;
