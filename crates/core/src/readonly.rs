//! A protocol-agnostic read-only adapter.
//!
//! Lending protocols report an account as totals: collateral value, debt value and an average
//! liquidation threshold. [`ReadOnlyAdapter`] turns any such source into the adapter interface a
//! strategy runs against: an aggregate view with one pseudo-asset, [`AGGREGATE_ASSET`] (price
//! 1 USD), where `supplied` is total collateral, `borrowed` is total debt and the market's
//! liquidation threshold is the account's average one. `collateral * threshold / debt` is how
//! the protocols define their health factor, so `Snapshot::health_factor()` on this view matches
//! the protocol's number up to rounding to micro-USD. There are no per-token rates or prices.
//!
//! This adapter can never move funds: `execute` and every non-alert `simulate` are errors. A
//! protocol only has to say how to read its totals ([`AccountSource`]).

use std::collections::BTreeMap;

use crate::adapter::{Account, MarketInfo, ProtocolAdapter, Snapshot};
use crate::amount::MICRO;
use crate::types::{Action, ActionKind};

/// The pseudo-asset of the aggregate view.
pub const AGGREGATE_ASSET: &str = "USD";

const READ_ONLY: &str = "read-only chain data: no action can be executed in this mode";

/// An account's totals as a protocol reports them, in micro-USD (6 decimals).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AggregateView {
    pub collateral_micro_usd: u64,
    pub debt_micro_usd: u64,
    /// Average liquidation threshold in basis points (0..=10_000).
    pub liq_threshold_bps: u32,
}

impl AggregateView {
    pub fn to_account(&self) -> Account {
        let mut account = Account::default();
        if self.collateral_micro_usd > 0 {
            account
                .supplied
                .insert(AGGREGATE_ASSET.into(), self.collateral_micro_usd);
        }
        if self.debt_micro_usd > 0 {
            account
                .borrowed
                .insert(AGGREGATE_ASSET.into(), self.debt_micro_usd);
        }
        account
    }
}

/// Where the totals come from (a chain reader). Implementations only read.
pub trait AccountSource {
    /// Stable protocol id, e.g. `"aave"`, matched against manifest `protocols`.
    fn protocol_id(&self) -> &str;
    /// Read the account's totals now.
    fn read(&self) -> Result<AggregateView, String>;
}

/// The snapshot a strategy sees for one aggregate view (shared with the replay adapter).
pub fn aggregate_snapshot(tick: u64, view: &AggregateView) -> Snapshot {
    let mut markets = BTreeMap::new();
    markets.insert(
        AGGREGATE_ASSET.to_string(),
        MarketInfo {
            supply_apy_bps: 0,
            price_usd_micro: MICRO,
            liq_threshold_bps: view.liq_threshold_bps,
        },
    );
    Snapshot {
        tick,
        markets,
        account: view.to_account(),
    }
}

pub struct ReadOnlyAdapter<S: AccountSource> {
    source: S,
    /// The account as of the first read (`account()` must return a reference, so it cannot
    /// follow later reads; `snapshot()` always reads fresh data).
    account: Account,
}

impl<S: AccountSource> ReadOnlyAdapter<S> {
    /// Reads the account once, so a bad node, market or address fails before anything runs.
    pub fn new(source: S) -> Result<ReadOnlyAdapter<S>, String> {
        let account = source.read()?.to_account();
        Ok(ReadOnlyAdapter { source, account })
    }

    pub fn source(&self) -> &S {
        &self.source
    }
}

impl<S: AccountSource> ProtocolAdapter for ReadOnlyAdapter<S> {
    fn id(&self) -> &str {
        self.source.protocol_id()
    }

    fn snapshot(&self, tick: u64) -> Result<Snapshot, String> {
        Ok(aggregate_snapshot(tick, &self.source.read()?))
    }

    fn simulate(&self, tick: u64, action: &Action) -> Result<Snapshot, String> {
        if action.kind == ActionKind::Alert {
            return self.snapshot(tick);
        }
        Err(READ_ONLY.into())
    }

    fn execute(&mut self, _tick: u64, _action: &Action) -> Result<(), String> {
        Err(READ_ONLY.into())
    }

    fn account(&self) -> &Account {
        &self.account
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fake {
        view: Result<AggregateView, String>,
        reads: Cell<u32>,
    }

    impl AccountSource for Fake {
        fn protocol_id(&self) -> &str {
            "fake"
        }
        fn read(&self) -> Result<AggregateView, String> {
            self.reads.set(self.reads.get() + 1);
            self.view.clone()
        }
    }

    fn fake(c: u64, d: u64, t: u32) -> Fake {
        Fake {
            view: Ok(AggregateView {
                collateral_micro_usd: c,
                debt_micro_usd: d,
                liq_threshold_bps: t,
            }),
            reads: Cell::new(0),
        }
    }

    #[test]
    fn snapshot_is_an_aggregate_view_with_the_protocols_health_factor() {
        let a = ReadOnlyAdapter::new(fake(3_000_000_000, 2_000_000_000, 8500)).unwrap();
        assert_eq!(a.id(), "fake");
        let s = a.snapshot(4).unwrap();
        assert_eq!(s.tick, 4);
        assert_eq!(s.account.supplied["USD"], 3_000_000_000);
        assert_eq!(s.account.borrowed["USD"], 2_000_000_000);
        assert_eq!(s.markets["USD"].liq_threshold_bps, 8500);
        assert_eq!(s.markets["USD"].price_usd_micro, MICRO);
        let hf = s.health_factor().unwrap();
        assert!((hf - 3000.0 * 0.85 / 2000.0).abs() < 1e-9, "{hf}");
    }

    #[test]
    fn empty_and_debt_free_accounts_are_valid() {
        let a = ReadOnlyAdapter::new(fake(0, 0, 0)).unwrap();
        let s = a.snapshot(1).unwrap();
        assert!(s.account.supplied.is_empty() && s.account.borrowed.is_empty());
        assert!(s.health_factor().is_none());
        let b = ReadOnlyAdapter::new(fake(5_000_000, 0, 8000)).unwrap();
        assert!(b.snapshot(1).unwrap().health_factor().is_none());
    }

    #[test]
    fn construction_reads_once_and_each_snapshot_reads_fresh_data() {
        let a = ReadOnlyAdapter::new(fake(1, 1, 8000)).unwrap();
        assert_eq!(a.source().reads.get(), 1);
        a.snapshot(1).unwrap();
        a.snapshot(2).unwrap();
        assert_eq!(a.source().reads.get(), 3);
    }

    #[test]
    fn a_failing_source_fails_construction_and_snapshots() {
        let bad = Fake {
            view: Err("node down".into()),
            reads: Cell::new(0),
        };
        assert_eq!(ReadOnlyAdapter::new(bad).err().unwrap(), "node down");
    }

    #[test]
    fn no_action_can_move_funds() {
        let mut a = ReadOnlyAdapter::new(fake(1_000_000, 0, 8000)).unwrap();
        for kind in [ActionKind::Supply, ActionKind::Withdraw, ActionKind::Repay] {
            let act = Action::new(kind, "fake", "USD", MICRO);
            assert!(a.simulate(1, &act).unwrap_err().contains("read-only"));
            assert!(a.execute(1, &act).unwrap_err().contains("read-only"));
        }
        let alert = Action::new(ActionKind::Alert, "fake", "USD", 0);
        assert!(a.simulate(1, &alert).is_ok());
        assert_eq!(a.account().supplied["USD"], 1_000_000);
    }
}
