//! Simulated Aave adapter.
//!
//! IMPORTANT: this is a *simulation*. Market data is deterministic synthetic data and the
//! numbers are NOT real Aave parameters. There is no network access, no RPC, no signing.
//! It exists so strategies, the risk engine and the CLI can be developed and tested end to
//! end (brief section 15: simulation-only first). A real read-only adapter is Phase 2 work,
//! see `docs/roadmap.md`.

pub mod chain;
pub mod onchain;

use std::collections::BTreeMap;

use protocol_core::adapter::{Account, MarketInfo, ProtocolAdapter, Snapshot};
use protocol_core::amount::MICRO;
use protocol_core::types::{Action, ActionKind, Scenario};

pub const PROTOCOL_ID: &str = "aave";

/// (asset, base supply APY bps, price in micro-USD, liquidation threshold bps)
const MARKETS: [(&str, u32, u64, u32); 3] = [
    ("USDC", 380, 1_000_000, 8000),
    ("DAI", 340, 1_000_000, 8000),
    ("WETH", 120, 3_000_000_000, 8250),
];

/// Protocol rule: a withdrawal that leaves health factor below this is rejected outright.
const PROTOCOL_MIN_HEALTH_FACTOR: f64 = 1.0;

pub struct AaveSim {
    scenario: Scenario,
    account: Account,
}

impl AaveSim {
    pub fn new(scenario: Scenario, account: Account) -> AaveSim {
        AaveSim { scenario, account }
    }

    /// Demo account: 2 WETH supplied as collateral, 2000 USDC borrowed (HF 2.475 at calm
    /// prices), plus 1000 USDC and 500 DAI idle in the wallet.
    pub fn demo_account() -> Account {
        let mut a = Account::default();
        a.supplied.insert("WETH".into(), 2 * MICRO);
        a.borrowed.insert("USDC".into(), 2_000 * MICRO);
        a.wallet.insert("USDC".into(), 1_000 * MICRO);
        a.wallet.insert("DAI".into(), 500 * MICRO);
        a
    }

    pub fn markets(&self, tick: u64) -> BTreeMap<String, MarketInfo> {
        let mut out = BTreeMap::new();
        for (idx, (asset, base_apy, base_price, threshold)) in MARKETS.iter().enumerate() {
            // Deterministic +-60 bps wobble so rate monitors have something to see.
            let wobble = ((tick * 37 + idx as u64 * 53) % 121) as i64 - 60;
            let apy = (*base_apy as i64 + wobble).max(0) as u32;
            let price = match self.scenario {
                Scenario::Calm => *base_price,
                // WETH loses 5% of its start price per tick, floor at -60%.
                Scenario::Stress if *asset == "WETH" => {
                    let drop = (5 * tick).min(60);
                    base_price * (100 - drop) / 100
                }
                Scenario::Stress => *base_price,
            };
            out.insert(
                asset.to_string(),
                MarketInfo {
                    supply_apy_bps: apy,
                    price_usd_micro: price,
                    liq_threshold_bps: *threshold,
                },
            );
        }
        out
    }
}

impl ProtocolAdapter for AaveSim {
    fn id(&self) -> &str {
        PROTOCOL_ID
    }

    fn snapshot(&self, tick: u64) -> Result<Snapshot, String> {
        Ok(Snapshot {
            tick,
            markets: self.markets(tick),
            account: self.account.clone(),
        })
    }

    fn simulate(&self, tick: u64, action: &Action) -> Result<Snapshot, String> {
        let post = self.snapshot(tick)?.apply(action)?;
        if action.kind == ActionKind::Withdraw {
            if let Some(hf) = post.health_factor() {
                if hf < PROTOCOL_MIN_HEALTH_FACTOR {
                    return Err(format!(
                        "aave would reject this withdrawal: health factor {hf:.2} < {PROTOCOL_MIN_HEALTH_FACTOR:.1}"
                    ));
                }
            }
        }
        Ok(post)
    }

    fn execute(&mut self, tick: u64, action: &Action) -> Result<(), String> {
        self.account = self.simulate(tick, action)?.account;
        Ok(())
    }

    fn account(&self) -> &Account {
        &self.account
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim(scenario: Scenario) -> AaveSim {
        AaveSim::new(scenario, AaveSim::demo_account())
    }

    #[test]
    fn identity() {
        assert_eq!(sim(Scenario::Calm).id(), "aave");
    }

    #[test]
    fn markets_are_deterministic() {
        let a = sim(Scenario::Calm).markets(5);
        let b = sim(Scenario::Calm).markets(5);
        assert_eq!(a, b);
        assert_ne!(
            sim(Scenario::Calm).markets(5),
            sim(Scenario::Calm).markets(6)
        );
    }

    #[test]
    fn apy_stays_in_a_sane_band() {
        let s = sim(Scenario::Calm);
        for tick in 0..500 {
            let m = s.markets(tick);
            assert!(
                (320..=440).contains(&m["USDC"].supply_apy_bps),
                "tick {tick}"
            );
            assert!(
                (280..=400).contains(&m["DAI"].supply_apy_bps),
                "tick {tick}"
            );
        }
    }

    #[test]
    fn calm_health_factor_is_constant() {
        let s = sim(Scenario::Calm);
        let hf1 = s.snapshot(1).unwrap().health_factor().unwrap();
        let hf9 = s.snapshot(9).unwrap().health_factor().unwrap();
        assert!((hf1 - 2.475).abs() < 1e-6);
        assert!((hf1 - hf9).abs() < 1e-12);
    }

    #[test]
    fn stress_drops_health_factor_below_one() {
        let s = sim(Scenario::Stress);
        let hf = |t| s.snapshot(t).unwrap().health_factor().unwrap();
        assert!(hf(1) > hf(4) && hf(4) > hf(8));
        assert!(
            hf(8) < 1.5 && hf(8) > 1.2,
            "tick 8 is the warning zone: {}",
            hf(8)
        );
        assert!(hf(11) < 1.2, "tick 11 is the critical zone: {}", hf(11));
        assert!(hf(12) < 1.0, "tick 12 is liquidatable");
        assert!((hf(12) - hf(40)).abs() < 1e-12, "price drop is floored");
    }

    #[test]
    fn supply_executes_against_the_account() {
        let mut s = sim(Scenario::Calm);
        let a = Action::new(ActionKind::Supply, PROTOCOL_ID, "USDC", 100 * MICRO);
        s.execute(1, &a).unwrap();
        assert_eq!(s.account().wallet["USDC"], 900 * MICRO);
        assert_eq!(s.account().supplied["USDC"], 100 * MICRO);
    }

    #[test]
    fn simulate_never_mutates() {
        let s = sim(Scenario::Calm);
        let a = Action::new(ActionKind::Supply, PROTOCOL_ID, "USDC", 100 * MICRO);
        let post = s.simulate(1, &a).unwrap();
        assert_eq!(post.account.supplied["USDC"], 100 * MICRO);
        assert!(!s.account().supplied.contains_key("USDC"));
    }

    #[test]
    fn protocol_rejects_withdrawals_that_liquidate() {
        let s = sim(Scenario::Calm);
        // withdrawing 1.5 of 2 WETH leaves 0.5 WETH: 0.5*3000*.825/2000 = 0.62
        let a = Action::new(ActionKind::Withdraw, PROTOCOL_ID, "WETH", 1_500_000);
        assert!(s.simulate(1, &a).unwrap_err().contains("health factor"));
        // withdrawing 0.2 WETH is fine: 1.8*3000*.825/2000 = 2.2275
        let ok = Action::new(ActionKind::Withdraw, PROTOCOL_ID, "WETH", 200_000);
        assert!(s.simulate(1, &ok).is_ok());
    }

    #[test]
    fn overdraw_is_an_error() {
        let mut s = sim(Scenario::Calm);
        let a = Action::new(ActionKind::Supply, PROTOCOL_ID, "USDC", 5_000 * MICRO);
        assert!(s.execute(1, &a).unwrap_err().contains("insufficient"));
        assert_eq!(
            s.account().wallet["USDC"],
            1_000 * MICRO,
            "failed execution changes nothing"
        );
    }
}
