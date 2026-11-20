//! Protocol adapter interface and the snapshot data model strategies observe.
//!
//! The engine only talks to `ProtocolAdapter`; adding Compound or Morpho means writing a
//! new adapter crate, not touching the engine. See `docs/adr/0004-protocol-adapters.md`.

use std::collections::BTreeMap;

use crate::amount::MICRO;
use crate::types::{Action, ActionKind};

#[derive(Clone, Debug, PartialEq)]
pub struct MarketInfo {
    pub supply_apy_bps: u32,
    /// USD price of one whole token, in micro-USD.
    pub price_usd_micro: u64,
    pub liq_threshold_bps: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub supplied: BTreeMap<String, u64>,
    pub borrowed: BTreeMap<String, u64>,
    pub wallet: BTreeMap<String, u64>,
}

impl Account {
    pub fn to_kv(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (a, v) in &self.supplied {
            m.insert(format!("supplied.{a}"), v.to_string());
        }
        for (a, v) in &self.borrowed {
            m.insert(format!("borrowed.{a}"), v.to_string());
        }
        for (a, v) in &self.wallet {
            m.insert(format!("wallet.{a}"), v.to_string());
        }
        m
    }

    pub fn from_kv(map: &BTreeMap<String, String>) -> Account {
        let mut acc = Account::default();
        for (k, v) in map {
            if let (Some((section, asset)), Ok(n)) = (k.split_once('.'), v.parse::<u64>()) {
                match section {
                    "supplied" => {
                        acc.supplied.insert(asset.to_string(), n);
                    }
                    "borrowed" => {
                        acc.borrowed.insert(asset.to_string(), n);
                    }
                    "wallet" => {
                        acc.wallet.insert(asset.to_string(), n);
                    }
                    _ => {}
                }
            }
        }
        acc
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub tick: u64,
    pub markets: BTreeMap<String, MarketInfo>,
    pub account: Account,
}

impl Snapshot {
    /// Health factor = (sum of supplied value * liquidation threshold) / debt value.
    /// `None` when there is no debt.
    pub fn health_factor(&self) -> Option<f64> {
        let mut collateral: u128 = 0;
        let mut debt: u128 = 0;
        for (asset, amt) in &self.account.supplied {
            if let Some(m) = self.markets.get(asset) {
                let usd = (*amt as u128) * (m.price_usd_micro as u128) / MICRO as u128;
                collateral += usd * (m.liq_threshold_bps as u128) / 10_000;
            }
        }
        for (asset, amt) in &self.account.borrowed {
            if let Some(m) = self.markets.get(asset) {
                debt += (*amt as u128) * (m.price_usd_micro as u128) / MICRO as u128;
            }
        }
        if debt == 0 {
            None
        } else {
            Some(collateral as f64 / debt as f64)
        }
    }

    /// Pure state transition for an action. Returns the post-action snapshot.
    /// This is the generic ledger logic; protocol-specific rules live in adapters.
    pub fn apply(&self, action: &Action) -> Result<Snapshot, String> {
        let mut next = self.clone();
        if action.kind == ActionKind::Alert {
            return Ok(next);
        }
        if !self.markets.contains_key(&action.asset) {
            return Err(format!("unknown asset '{}'", action.asset));
        }
        if action.amount == 0 {
            return Err("amount must be greater than zero".into());
        }
        let asset = action.asset.clone();
        let amt = action.amount;
        match action.kind {
            ActionKind::Supply => {
                take(&mut next.account.wallet, &asset, amt, "wallet balance")?;
                *next.account.supplied.entry(asset).or_insert(0) += amt;
            }
            ActionKind::Withdraw => {
                take(&mut next.account.supplied, &asset, amt, "supplied balance")?;
                *next.account.wallet.entry(asset).or_insert(0) += amt;
            }
            ActionKind::Repay => {
                take(&mut next.account.borrowed, &asset, amt, "outstanding debt")?;
                take(&mut next.account.wallet, &asset, amt, "wallet balance")
                    .map_err(|e| format!("cannot repay: {e}"))?;
            }
            ActionKind::Alert => {}
        }
        Ok(next)
    }
}

fn take(map: &mut BTreeMap<String, u64>, asset: &str, amt: u64, what: &str) -> Result<(), String> {
    let have = map.get(asset).copied().unwrap_or(0);
    if have < amt {
        return Err(format!(
            "insufficient {what} for {asset}: have {}, need {}",
            crate::amount::format_amount(have),
            crate::amount::format_amount(amt)
        ));
    }
    map.insert(asset.to_string(), have - amt);
    Ok(())
}

/// What the engine needs from a DeFi protocol integration.
pub trait ProtocolAdapter {
    /// Stable protocol id, e.g. `"aave"`. Matched against manifest `protocols`.
    fn id(&self) -> &str;
    /// Observed state at `tick`.
    fn snapshot(&self, tick: u64) -> Result<Snapshot, String>;
    /// Pure dry-run of an action including protocol-specific rules. Never mutates.
    fn simulate(&self, tick: u64, action: &Action) -> Result<Snapshot, String>;
    /// Apply an action to the (simulated) account. Real chain execution is not implemented.
    fn execute(&mut self, tick: u64, action: &Action) -> Result<(), String>;
    fn account(&self) -> &Account;
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> Snapshot {
        let mut markets = BTreeMap::new();
        markets.insert(
            "USDC".to_string(),
            MarketInfo {
                supply_apy_bps: 400,
                price_usd_micro: MICRO,
                liq_threshold_bps: 8000,
            },
        );
        markets.insert(
            "WETH".to_string(),
            MarketInfo {
                supply_apy_bps: 100,
                price_usd_micro: 3_000 * MICRO,
                liq_threshold_bps: 8000,
            },
        );
        let mut account = Account::default();
        account.wallet.insert("USDC".into(), 1_000 * MICRO);
        account.supplied.insert("WETH".into(), 2 * MICRO);
        account.borrowed.insert("USDC".into(), 2_000 * MICRO);
        Snapshot {
            tick: 1,
            markets,
            account,
        }
    }

    #[test]
    fn health_factor_math() {
        // 2 WETH * 3000 * 0.8 = 4800 / 2000 debt = 2.4
        let hf = sample().health_factor().unwrap();
        assert!((hf - 2.4).abs() < 1e-9);
    }

    #[test]
    fn no_debt_means_no_health_factor() {
        let mut s = sample();
        s.account.borrowed.clear();
        assert!(s.health_factor().is_none());
    }

    #[test]
    fn supply_and_withdraw_move_balances() {
        let s = sample();
        let a = Action::new(ActionKind::Supply, "aave", "USDC", 100 * MICRO);
        let after = s.apply(&a).unwrap();
        assert_eq!(after.account.wallet["USDC"], 900 * MICRO);
        assert_eq!(after.account.supplied["USDC"], 100 * MICRO);
        let w = Action::new(ActionKind::Withdraw, "aave", "USDC", 40 * MICRO);
        let after2 = after.apply(&w).unwrap();
        assert_eq!(after2.account.supplied["USDC"], 60 * MICRO);
        assert_eq!(after2.account.wallet["USDC"], 940 * MICRO);
        // the original snapshot is untouched
        assert_eq!(s.account.wallet["USDC"], 1_000 * MICRO);
    }

    #[test]
    fn apply_rejects_overdraw_and_unknown_asset() {
        let s = sample();
        let too_much = Action::new(ActionKind::Supply, "aave", "USDC", 5_000 * MICRO);
        assert!(s.apply(&too_much).unwrap_err().contains("insufficient"));
        let unknown = Action::new(ActionKind::Supply, "aave", "DOGE", MICRO);
        assert!(s.apply(&unknown).unwrap_err().contains("unknown asset"));
        let zero = Action::new(ActionKind::Supply, "aave", "USDC", 0);
        assert!(s.apply(&zero).is_err());
    }

    #[test]
    fn repay_reduces_debt() {
        let s = sample();
        let r = Action::new(ActionKind::Repay, "aave", "USDC", 500 * MICRO);
        let after = s.apply(&r).unwrap();
        assert_eq!(after.account.borrowed["USDC"], 1_500 * MICRO);
        assert_eq!(after.account.wallet["USDC"], 500 * MICRO);
    }

    #[test]
    fn account_kv_round_trip() {
        let acc = sample().account;
        assert_eq!(Account::from_kv(&acc.to_kv()), acc);
    }
}
