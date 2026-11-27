//! Officially maintained built-in strategies.
//!
//! Each one implements the same `Strategy` contract that out-of-process strategies in other
//! languages speak over the line protocol. Built-ins are in-process because they are
//! reviewed, compiled-in project code; anything else runs as a separate process.

use std::collections::BTreeMap;

use protocol_core::amount::{format_amount, parse_amount};
use protocol_core::manifest::Manifest;
use protocol_core::strategy::{Strategy, TickInput, TickOutput};
use protocol_core::types::{Action, ActionKind};

const STABLE_LENDING: &str = include_str!("../manifests/stable-lending.toml");
const HEALTH_FACTOR_MONITOR: &str = include_str!("../manifests/health-factor-monitor.toml");
const RATE_MONITOR: &str = include_str!("../manifests/rate-monitor.toml");

const DOC_STABLE_LENDING: &str = include_str!("../docs/stable-lending.md");
const DOC_HEALTH_FACTOR_MONITOR: &str = include_str!("../docs/health-factor-monitor.md");
const DOC_RATE_MONITOR: &str = include_str!("../docs/rate-monitor.md");

pub const BUILTIN_NAMES: [&str; 3] = ["stable-lending", "health-factor-monitor", "rate-monitor"];

pub fn manifest(name: &str) -> Option<Manifest> {
    let text = match name {
        "stable-lending" => STABLE_LENDING,
        "health-factor-monitor" => HEALTH_FACTOR_MONITOR,
        "rate-monitor" => RATE_MONITOR,
        _ => return None,
    };
    Some(Manifest::parse(text).expect("built-in manifests are validated by tests"))
}

pub fn docs(name: &str) -> Option<&'static str> {
    match name {
        "stable-lending" => Some(DOC_STABLE_LENDING),
        "health-factor-monitor" => Some(DOC_HEALTH_FACTOR_MONITOR),
        "rate-monitor" => Some(DOC_RATE_MONITOR),
        _ => None,
    }
}

pub fn create(name: &str) -> Option<Box<dyn Strategy>> {
    match name {
        "stable-lending" => Some(Box::new(StableLending)),
        "health-factor-monitor" => Some(Box::new(HealthFactorMonitor)),
        "rate-monitor" => Some(Box::new(RateMonitor)),
        _ => None,
    }
}

fn param_f64(input: &TickInput, key: &str, default: f64) -> Result<f64, String> {
    match input.params.get(key) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| format!("param '{key}' must be a number, got '{v}'")),
    }
}

pub struct HealthFactorMonitor;

impl Strategy for HealthFactorMonitor {
    fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
        let warn = param_f64(input, "warn_below", 1.5)?;
        let critical = param_f64(input, "critical_below", 1.2)?;
        if critical > warn {
            return Err("critical_below must not exceed warn_below".into());
        }
        let mut out = TickOutput {
            state: input.state.clone(),
            ..Default::default()
        };
        let prev = input
            .state
            .get("level")
            .map(|s| s.as_str())
            .unwrap_or("ok")
            .to_string();

        let Some(hf) = input.snapshot.health_factor() else {
            out.logs.push("no debt: nothing to monitor".into());
            out.state.insert("level".into(), "ok".into());
            return Ok(out);
        };
        let level = if hf < critical {
            "critical"
        } else if hf < warn {
            "warning"
        } else {
            "ok"
        };
        out.logs.push(format!("health factor {hf:.3} ({level})"));
        if level != prev {
            match level {
                "ok" => out.logs.push(format!("health factor recovered to {hf:.3}")),
                _ => out.proposals.push(Action::alert(
                    "aave",
                    &format!("{} health factor {hf:.3}", level.to_uppercase()),
                )),
            }
        }
        out.state.insert("level".into(), level.into());
        Ok(out)
    }
}

pub struct RateMonitor;

impl Strategy for RateMonitor {
    fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
        let min_change = param_f64(input, "min_change_bps", 25.0)?;
        let mut out = TickOutput::default();
        for (asset, m) in &input.snapshot.markets {
            let key = format!("apy.{asset}");
            let now = m.supply_apy_bps as f64;
            if let Some(prev) = input.state.get(&key).and_then(|v| v.parse::<f64>().ok()) {
                let delta = now - prev;
                out.logs
                    .push(format!("{asset} supply APY {now:.0} bps ({delta:+.0})"));
                if delta.abs() >= min_change {
                    out.proposals.push(Action::alert(
                        "aave",
                        &format!("{asset} APY moved {delta:+.0} bps to {now:.0}"),
                    ));
                }
            } else {
                out.logs
                    .push(format!("{asset} supply APY {now:.0} bps (baseline)"));
            }
            out.state.insert(key, format!("{now:.0}"));
        }
        Ok(out)
    }
}

pub struct StableLending;

impl Strategy for StableLending {
    fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
        let min_apy = param_f64(input, "min_apy_bps", 300.0)?;
        let chunk = parse_amount(
            input
                .params
                .get("chunk")
                .map(|s| s.as_str())
                .unwrap_or("100"),
        )
        .map_err(|e| format!("param 'chunk': {e}"))?;
        let assets: Vec<String> = input
            .params
            .get("assets")
            .map(|s| s.as_str())
            .unwrap_or("USDC,DAI")
            .split(',')
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| !s.is_empty())
            .collect();

        let mut out = TickOutput {
            state: input.state.clone(),
            ..Default::default()
        };

        // best APY among configured assets (ties resolved alphabetically for determinism)
        let mut best: Option<(&String, u32)> = None;
        let candidates: BTreeMap<&String, u32> = assets
            .iter()
            .filter_map(|a| input.snapshot.markets.get(a).map(|m| (a, m.supply_apy_bps)))
            .collect();
        for (a, apy) in &candidates {
            if best.map(|(_, b)| *apy > b).unwrap_or(true) {
                best = Some((a, *apy));
            }
        }
        let Some((asset, apy)) = best else {
            out.logs
                .push("none of the configured assets is available in this market".into());
            return Ok(out);
        };
        out.logs
            .push(format!("best stablecoin yield: {asset} at {apy} bps"));

        if (apy as f64) < min_apy {
            out.logs
                .push(format!("below the {min_apy:.0} bps floor, not supplying"));
            return Ok(out);
        }
        let wallet = input
            .snapshot
            .account
            .wallet
            .get(asset)
            .copied()
            .unwrap_or(0);
        if wallet < chunk {
            out.logs.push(format!(
                "wallet holds {} {asset}, less than the {} chunk",
                format_amount(wallet),
                format_amount(chunk)
            ));
            return Ok(out);
        }
        let mut action = Action::new(ActionKind::Supply, "aave", asset, chunk);
        action.note = format!("best yield {apy} bps");
        out.proposals.push(action);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol_adapter_aave_shim::*;

    // The strategies crate does not depend on the adapter crate; tests build snapshots by hand.
    mod protocol_adapter_aave_shim {
        use protocol_core::adapter::{Account, MarketInfo, Snapshot};
        use protocol_core::amount::MICRO;
        use std::collections::BTreeMap;

        pub fn snapshot(
            usdc_apy: u32,
            dai_apy: u32,
            hf_weth_price: u64,
            wallet_usdc: u64,
        ) -> Snapshot {
            let mut markets = BTreeMap::new();
            let m = |apy, price| MarketInfo {
                supply_apy_bps: apy,
                price_usd_micro: price,
                liq_threshold_bps: 8000,
            };
            markets.insert("USDC".to_string(), m(usdc_apy, MICRO));
            markets.insert("DAI".to_string(), m(dai_apy, MICRO));
            markets.insert("WETH".to_string(), m(100, hf_weth_price));
            let mut account = Account::default();
            account.wallet.insert("USDC".into(), wallet_usdc);
            account.wallet.insert("DAI".into(), 500 * MICRO);
            account.supplied.insert("WETH".into(), 2 * MICRO);
            account.borrowed.insert("USDC".into(), 2_000 * MICRO);
            Snapshot {
                tick: 1,
                markets,
                account,
            }
        }
    }

    fn run(
        name: &str,
        snap: &protocol_core::adapter::Snapshot,
        state: &BTreeMap<String, String>,
    ) -> TickOutput {
        let manifest = manifest(name).unwrap();
        let mut s = create(name).unwrap();
        s.on_tick(&TickInput {
            tick: 1,
            snapshot: snap,
            params: &manifest.params,
            state,
        })
        .unwrap()
    }

    #[test]
    fn all_builtin_manifests_parse_and_have_docs() {
        for name in BUILTIN_NAMES {
            let m = manifest(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(m.name, name);
            assert!(m.is_builtin());
            assert_eq!(m.entry, format!("builtin:{name}"));
            assert!(create(name).is_some());
            let d = docs(name).unwrap();
            for heading in ["Purpose", "Risks"] {
                assert!(d.contains(heading), "{name} docs missing '{heading}'");
            }
        }
        assert!(manifest("nope").is_none() && create("nope").is_none() && docs("nope").is_none());
    }

    #[test]
    fn monitors_cannot_move_funds_by_manifest() {
        for name in ["health-factor-monitor", "rate-monitor"] {
            let m = manifest(name).unwrap();
            assert_eq!(m.allowed_actions, vec![ActionKind::Alert]);
            assert_eq!(m.max_total_exposure, 0);
        }
        let lending = manifest("stable-lending").unwrap();
        assert!(
            lending.max_amount_per_action > 0
                && lending.max_total_exposure >= lending.max_amount_per_action
        );
    }

    #[test]
    fn stable_lending_picks_best_yield() {
        let out = run(
            "stable-lending",
            &snapshot(380, 450, 3_000_000_000, 1_000_000_000),
            &BTreeMap::new(),
        );
        assert_eq!(out.proposals.len(), 1);
        let p = &out.proposals[0];
        assert_eq!(
            (p.kind, p.asset.as_str(), p.amount),
            (ActionKind::Supply, "DAI", 100_000_000)
        );
        // DAI wallet is 500, so DAI is fundable
        let out = run(
            "stable-lending",
            &snapshot(450, 380, 3_000_000_000, 1_000_000_000),
            &BTreeMap::new(),
        );
        assert_eq!(out.proposals[0].asset, "USDC");
    }

    #[test]
    fn stable_lending_respects_floor_and_wallet() {
        let low = run(
            "stable-lending",
            &snapshot(250, 200, 3_000_000_000, 1_000_000_000),
            &BTreeMap::new(),
        );
        assert!(low.proposals.is_empty());
        assert!(low.logs.iter().any(|l| l.contains("floor")));
        let broke = run(
            "stable-lending",
            &snapshot(450, 380, 3_000_000_000, 10_000_000),
            &BTreeMap::new(),
        );
        assert!(broke.proposals.is_empty());
        assert!(broke.logs.iter().any(|l| l.contains("wallet holds")));
    }

    #[test]
    fn stable_lending_rejects_bad_params() {
        let snap = snapshot(400, 400, 3_000_000_000, 1_000_000_000);
        let mut params = manifest("stable-lending").unwrap().params;
        params.insert("chunk".into(), "lots".into());
        let state = BTreeMap::new();
        let err = StableLending
            .on_tick(&TickInput {
                tick: 1,
                snapshot: &snap,
                params: &params,
                state: &state,
            })
            .unwrap_err();
        assert!(err.contains("chunk"));
    }

    #[test]
    fn health_monitor_alerts_only_on_level_change() {
        let calm = snapshot(380, 340, 3_000_000_000, 0); // HF 2.4
        let o1 = run("health-factor-monitor", &calm, &BTreeMap::new());
        assert!(o1.proposals.is_empty());
        assert_eq!(o1.state["level"], "ok");

        let warn = snapshot(380, 340, 1_850_000_000, 0); // 2*1850*0.8/2000 = 1.48
        let o2 = run("health-factor-monitor", &warn, &o1.state);
        assert_eq!(o2.proposals.len(), 1);
        assert!(o2.proposals[0].note.contains("WARNING"));
        // same level again: quiet
        let o3 = run("health-factor-monitor", &warn, &o2.state);
        assert!(o3.proposals.is_empty());

        let crit = snapshot(380, 340, 1_300_000_000, 0); // 1.04
        let o4 = run("health-factor-monitor", &crit, &o3.state);
        assert!(o4.proposals[0].note.contains("CRITICAL"));

        let back = run("health-factor-monitor", &calm, &o4.state);
        assert!(back.proposals.is_empty());
        assert!(back.logs.iter().any(|l| l.contains("recovered")));
        assert_eq!(back.state["level"], "ok");
    }

    #[test]
    fn health_monitor_handles_no_debt_and_bad_config() {
        let mut snap = snapshot(380, 340, 3_000_000_000, 0);
        snap.account.borrowed.clear();
        let out = run("health-factor-monitor", &snap, &BTreeMap::new());
        assert!(out.proposals.is_empty());
        assert!(out.logs[0].contains("no debt"));

        let mut params = BTreeMap::new();
        params.insert("warn_below".to_string(), "1.1".to_string());
        params.insert("critical_below".to_string(), "1.4".to_string());
        let state = BTreeMap::new();
        let err = HealthFactorMonitor
            .on_tick(&TickInput {
                tick: 1,
                snapshot: &snap,
                params: &params,
                state: &state,
            })
            .unwrap_err();
        assert!(err.contains("critical_below"));
    }

    #[test]
    fn rate_monitor_baselines_then_alerts_on_big_moves() {
        let s1 = snapshot(380, 340, 3_000_000_000, 0);
        let o1 = run("rate-monitor", &s1, &BTreeMap::new());
        assert!(o1.proposals.is_empty());
        assert!(o1.logs.iter().all(|l| l.contains("baseline")));
        // small move: quiet
        let s2 = snapshot(390, 345, 3_000_000_000, 0);
        let o2 = run("rate-monitor", &s2, &o1.state);
        assert!(o2.proposals.is_empty());
        // 60 bps jump on USDC only
        let s3 = snapshot(450, 345, 3_000_000_000, 0);
        let o3 = run("rate-monitor", &s3, &o2.state);
        assert_eq!(o3.proposals.len(), 1);
        assert!(o3.proposals[0].note.starts_with("USDC APY moved +60"));
        assert_eq!(o3.state["apy.USDC"], "450");
    }
}
