//! Agent wiring primitives.
//!
//! The crate is intentionally a pure library: it does not depend on a web
//! framework, a runtime, or the network. The [`edit`] module contains
//! byte-in/byte-out, format-preserving editors for the configuration formats
//! used by supported agents. [`atomic`] provides crash-safe file replacement,
//! [`backup`] the durable recovery stores used around agent writes, [`sync`] the
//! shared model catalog, [`spec`] one self-contained module per agent, and
//! [`wiring`] the connect / disconnect orchestration.

#![cfg_attr(test, allow(clippy::panic, clippy::expect_used, clippy::unwrap_used))]
#![warn(missing_docs)]

pub mod atomic;
pub mod backup;
pub mod edit;
pub mod error;
pub mod spec;
pub mod sync;
pub mod wiring;

pub use error::WiringError;
pub use spec::{AgentSpec, DetectResult, HomeCtx, WiredState, WiringCtx, all_specs, gw_base_match};
pub use sync::catalog::{CatalogModel, ModelCatalog, TierModels};
pub use wiring::{ConnectReport, DisconnectReport, Wiring};
