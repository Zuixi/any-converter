//! Agent wiring primitives.
//!
//! The crate is intentionally a pure library: it does not depend on a web
//! framework, a runtime, or the network. The [`edit`] module contains
//! byte-in/byte-out, format-preserving editors for the configuration formats
//! used by supported agents.

#![cfg_attr(test, allow(clippy::panic, clippy::expect_used, clippy::unwrap_used))]
#![warn(missing_docs)]

pub mod edit;
