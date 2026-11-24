//! protocol-core: the language-neutral core of the DeFi strategy execution protocol.
//!
//! Status: simulation-only. No module in this crate holds private keys or talks to a
//! real chain. See `docs/architecture.md`.

pub mod adapter;
pub mod amount;
pub mod engine;
pub mod external;
pub mod hash;
pub mod kv;
pub mod manifest;
pub mod risk;
pub mod runtime;
pub mod sandbox;
#[cfg(target_os = "linux")]
pub mod seccomp;
pub mod sha256;
pub mod store;
pub mod strategy;
pub mod supervisor;
pub mod types;

/// Hard cap on proposals a strategy may return in one tick.
pub const MAX_PROPOSALS_PER_TICK: usize = 20;
