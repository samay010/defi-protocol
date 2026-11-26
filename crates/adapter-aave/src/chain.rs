//! Read-only Aave adapter backed by live chain data (testnet).
//!
//! `getUserAccountData` returns account TOTALS, not per-token balances, so the snapshot a
//! strategy sees is the generic aggregate view of [`protocol_core::readonly`]: one pseudo-asset,
//! `USD`, with total collateral, total debt and the account's average liquidation threshold.
//! Aave defines `health factor = collateral * average liquidation threshold / debt`, so
//! `Snapshot::health_factor()` on this view matches the protocol's number (up to rounding
//! to micro-USD). There are no per-token rates or prices in this view.
//!
//! This adapter can never move funds: `execute` and every non-alert `simulate` are errors.

use protocol_core::adapter::{Account, ProtocolAdapter, Snapshot};
use protocol_core::readonly::{AccountSource, AggregateView, ReadOnlyAdapter};
use protocol_core::types::Action;

use crate::onchain::{AaveReader, AccountData, Address, EthCall};
use crate::PROTOCOL_ID;

pub use protocol_core::readonly::AGGREGATE_ASSET;

/// Base-currency units (USD, 8 decimals) per micro-USD (6 decimals).
const BASE_PER_MICRO: u128 = 100;

/// Reads one account's `getUserAccountData` from an Aave V3 Pool.
pub struct AaveSource<T: EthCall> {
    reader: AaveReader<T>,
    user: Address,
}

impl<T: EthCall> AccountSource for AaveSource<T> {
    fn protocol_id(&self) -> &str {
        PROTOCOL_ID
    }

    fn read(&self) -> Result<AggregateView, String> {
        let data = self
            .reader
            .account_data(&self.user)
            .map_err(|e| e.to_string())?;
        aggregate(&data)
    }
}

/// Turn on-chain totals into the aggregate view (micro-USD and the average threshold in bps).
pub fn aggregate(d: &AccountData) -> Result<AggregateView, String> {
    let micro = |name: &str, v: &crate::onchain::U256| -> Result<u64, String> {
        let base = v
            .to_u128()
            .ok_or_else(|| format!("{name} is too large to represent"))?;
        u64::try_from(base / BASE_PER_MICRO)
            .map_err(|_| format!("{name} is too large to represent"))
    };
    let collateral = micro("total collateral", &d.total_collateral_base)?;
    let debt = micro("total debt", &d.total_debt_base)?;
    let threshold = d
        .current_liquidation_threshold
        .to_u128()
        .and_then(|t| u32::try_from(t).ok())
        .filter(|t| *t <= 10_000)
        .ok_or("liquidation threshold is not a valid basis-point value")?;
    Ok(AggregateView {
        collateral_micro_usd: collateral,
        debt_micro_usd: debt,
        liq_threshold_bps: threshold,
    })
}

pub struct AaveChainRead<T: EthCall> {
    inner: ReadOnlyAdapter<AaveSource<T>>,
}

impl<T: EthCall> AaveChainRead<T> {
    /// Reads the account once, so a bad node, pool or address fails before anything runs.
    pub fn new(reader: AaveReader<T>, user: Address) -> Result<AaveChainRead<T>, String> {
        Ok(AaveChainRead {
            inner: ReadOnlyAdapter::new(AaveSource { reader, user })?,
        })
    }

    pub fn user(&self) -> &Address {
        &self.inner.source().user
    }
}

impl<T: EthCall> ProtocolAdapter for AaveChainRead<T> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn snapshot(&self, tick: u64) -> Result<Snapshot, String> {
        self.inner.snapshot(tick)
    }

    fn simulate(&self, tick: u64, action: &Action) -> Result<Snapshot, String> {
        self.inner.simulate(tick, action)
    }

    fn execute(&mut self, tick: u64, action: &Action) -> Result<(), String> {
        self.inner.execute(tick, action)
    }

    fn account(&self) -> &Account {
        self.inner.account()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onchain::ChainError;
    use protocol_core::amount::MICRO;
    use protocol_core::types::ActionKind;
    use std::cell::Cell;
    use std::rc::Rc;

    const POOL: &str = "0x2222222222222222222222222222222222222222";
    const USER: &str = "0x1111111111111111111111111111111111111111";

    fn word(n: u128) -> String {
        format!("{n:064x}")
    }

    /// Serves a fixed reply and counts calls.
    struct Fake {
        reply: String,
        calls: Rc<Cell<u32>>,
    }
    impl EthCall for Fake {
        fn eth_call(&self, _to: &Address, _data: &str) -> Result<String, ChainError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.reply.clone())
        }
    }

    fn adapter(words: [u128; 6]) -> AaveChainRead<Fake> {
        let reply = format!("0x{}", words.map(word).concat());
        let reader = AaveReader::new(
            Fake {
                reply,
                calls: Rc::new(Cell::new(0)),
            },
            Address::parse(POOL).unwrap(),
        );
        AaveChainRead::new(reader, Address::parse(USER).unwrap()).unwrap()
    }

    // 1500.25 USD collateral, 400 USD debt, avg threshold 82.5%, HF = 1500.25*0.825/400
    fn sample() -> [u128; 6] {
        let hf = 1500.25_f64 * 0.825 / 400.0;
        [
            150_025_000_000,
            40_000_000_000,
            5_000_000_000,
            8250,
            8000,
            (hf * 1e18) as u128,
        ]
    }

    #[test]
    fn snapshot_is_an_aggregate_usd_view_with_the_protocols_health_factor() {
        let a = adapter(sample());
        let s = a.snapshot(3).unwrap();
        assert_eq!(s.tick, 3);
        assert_eq!(s.account.supplied["USD"], 1_500_250_000);
        assert_eq!(s.account.borrowed["USD"], 400_000_000);
        assert!(s.account.wallet.is_empty());
        assert_eq!(s.markets["USD"].liq_threshold_bps, 8250);
        assert_eq!(s.markets["USD"].price_usd_micro, MICRO);
        let hf = s.health_factor().unwrap();
        let on_chain = sample()[5] as f64 / 1e18;
        assert!((hf - on_chain).abs() < 1e-6, "{hf} vs {on_chain}");
    }

    #[test]
    fn no_debt_means_no_health_factor() {
        let a = adapter([150_025_000_000, 0, 0, 8250, 8000, u128::MAX]);
        let s = a.snapshot(1).unwrap();
        assert!(!s.account.borrowed.contains_key("USD"));
        assert!(s.health_factor().is_none());
    }

    #[test]
    fn empty_account_is_valid() {
        let a = adapter([0, 0, 0, 0, 0, u128::MAX]);
        let s = a.snapshot(1).unwrap();
        assert!(s.account.supplied.is_empty() && s.account.borrowed.is_empty());
        assert!(s.health_factor().is_none());
    }

    #[test]
    fn new_reads_once_and_every_snapshot_reads_fresh_data() {
        let calls = Rc::new(Cell::new(0));
        let reply = format!("0x{}", sample().map(word).concat());
        let reader = AaveReader::new(
            Fake {
                reply,
                calls: calls.clone(),
            },
            Address::parse(POOL).unwrap(),
        );
        let a = AaveChainRead::new(reader, Address::parse(USER).unwrap()).unwrap();
        assert_eq!(calls.get(), 1, "construction reads once");
        a.snapshot(1).unwrap();
        a.snapshot(2).unwrap();
        assert_eq!(calls.get(), 3, "each snapshot reads again");
        assert_eq!(a.user().to_hex(), USER);
    }

    #[test]
    fn no_action_can_move_funds() {
        let mut a = adapter(sample());
        for kind in [ActionKind::Supply, ActionKind::Withdraw, ActionKind::Repay] {
            let act = Action::new(kind, "aave", "USD", MICRO);
            assert!(a.simulate(1, &act).unwrap_err().contains("read-only"));
            assert!(a.execute(1, &act).unwrap_err().contains("read-only"));
        }
        let alert = Action::new(ActionKind::Alert, "aave", "USD", 0);
        assert!(a.simulate(1, &alert).is_ok());
        // the cached account never changes
        assert_eq!(a.account().supplied["USD"], 1_500_250_000);
    }

    #[test]
    fn invalid_chain_data_is_an_error_not_a_guess() {
        // threshold above 100% is not basis points
        let reply = format!("0x{}", [1u128, 1, 1, 20_000, 8000, 1].map(word).concat());
        let reader = AaveReader::new(
            Fake {
                reply,
                calls: Rc::new(Cell::new(0)),
            },
            Address::parse(POOL).unwrap(),
        );
        let err = AaveChainRead::new(reader, Address::parse(USER).unwrap())
            .err()
            .unwrap();
        assert!(err.contains("basis-point"), "{err}");
        // collateral that does not fit in u64 micro-USD
        let huge = [u128::MAX / 2, 0, 0, 8250, 8000, u128::MAX]
            .map(word)
            .concat();
        let reader = AaveReader::new(
            Fake {
                reply: format!("0x{huge}"),
                calls: Rc::new(Cell::new(0)),
            },
            Address::parse(POOL).unwrap(),
        );
        assert!(AaveChainRead::new(reader, Address::parse(USER).unwrap()).is_err());
    }
}
