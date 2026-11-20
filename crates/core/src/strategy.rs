//! The language-neutral strategy interface.
//!
//! A strategy is a pure-ish function: (snapshot, params, previous state) -> (logs,
//! proposals, new state). It cannot sign, send or hold keys. Official strategies
//! implement this trait in-process; strategies in any language implement the same
//! contract over a line protocol (see `external`).

use std::collections::BTreeMap;

use crate::adapter::Snapshot;
use crate::types::Action;

pub struct TickInput<'a> {
    pub tick: u64,
    pub snapshot: &'a Snapshot,
    pub params: &'a BTreeMap<String, String>,
    pub state: &'a BTreeMap<String, String>,
}

#[derive(Default, Debug, Clone)]
pub struct TickOutput {
    pub logs: Vec<String>,
    pub proposals: Vec<Action>,
    /// Full replacement of the strategy's persisted state.
    pub state: BTreeMap<String, String>,
}

pub trait Strategy: Send {
    fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String>;
}
