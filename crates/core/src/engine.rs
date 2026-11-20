//! Execution engine: routes every proposed action through the risk engine and then,
//! depending on the execution mode, executes, queues for approval, or only suggests.
//!
//! Modes (brief section 7):
//! - Autonomous: strategy proposals that pass risk are executed.
//! - Approval:   strategy proposals that pass risk wait in a pending queue for the user.
//! - Manual:     strategy proposals are recorded as suggestions only; the user acts.
//!
//! User-initiated actions (`Source::User`) execute immediately in every mode, but still
//! pass the same risk checks.

use crate::adapter::ProtocolAdapter;
use crate::risk::{Decision, RiskPolicy, RiskState};
use crate::types::{Action, ActionKind, Mode, Network};

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: u64,
    pub tick: u64,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Strategy,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Executed,
    Queued(u64),
    Suggested,
    Notified,
    Denied(String),
    Failed(String),
}

impl Outcome {
    pub fn describe(&self) -> String {
        match self {
            Outcome::Executed => "executed (simulated)".into(),
            Outcome::Queued(id) => format!("queued for approval as #{id}"),
            Outcome::Suggested => "suggested only (manual mode)".into(),
            Outcome::Notified => "alert raised".into(),
            Outcome::Denied(r) => format!("DENIED: {r}"),
            Outcome::Failed(r) => format!("FAILED: {r}"),
        }
    }
}

pub struct Engine {
    pub adapter: Box<dyn ProtocolAdapter>,
    pub policy: RiskPolicy,
    pub mode: Mode,
    pub network: Network,
    pub total_moved: u64,
    pub kill_switch: bool,
    pub pending: Vec<Pending>,
    pub next_id: u64,
    audit: Vec<String>,
}

impl Engine {
    pub fn new(
        adapter: Box<dyn ProtocolAdapter>,
        policy: RiskPolicy,
        mode: Mode,
        network: Network,
    ) -> Engine {
        Engine {
            adapter,
            policy,
            mode,
            network,
            total_moved: 0,
            kill_switch: false,
            pending: Vec::new(),
            next_id: 1,
            audit: Vec::new(),
        }
    }

    fn risk_state(&self) -> RiskState {
        RiskState {
            total_moved: self.total_moved,
            kill_switch: self.kill_switch,
            network: self.network,
        }
    }

    /// Static risk checks, then a dry-run through the adapter, then risk checks again
    /// against the simulated post-action state.
    fn check(&self, tick: u64, action: &Action) -> Result<(), String> {
        if let Decision::Deny(r) = self.policy.check(action, &self.risk_state(), None) {
            return Err(r);
        }
        if action.kind.moves_funds() {
            if action.protocol != self.adapter.id() {
                return Err(format!(
                    "adapter '{}' cannot execute actions for protocol '{}'",
                    self.adapter.id(),
                    action.protocol
                ));
            }
            let post = self
                .adapter
                .simulate(tick, action)
                .map_err(|e| format!("simulation failed: {e}"))?;
            if let Decision::Deny(r) = self.policy.check(action, &self.risk_state(), Some(&post)) {
                return Err(r);
            }
        }
        Ok(())
    }

    pub fn submit(&mut self, tick: u64, action: Action, source: Source) -> Outcome {
        if let Err(reason) = self.check(tick, &action) {
            self.log(tick, "denied", format!("{action} ({reason})"));
            return Outcome::Denied(reason);
        }
        if action.kind == ActionKind::Alert {
            self.log(tick, "alert", action.to_string());
            return Outcome::Notified;
        }
        match (source, self.mode) {
            (Source::User, _) | (Source::Strategy, Mode::Autonomous) => self.execute(tick, &action),
            (Source::Strategy, Mode::Approval) => {
                let id = self.next_id;
                self.next_id += 1;
                self.log(tick, "queued", format!("#{id} {action}"));
                self.pending.push(Pending { id, tick, action });
                Outcome::Queued(id)
            }
            (Source::Strategy, Mode::Manual) => {
                self.log(tick, "suggested", action.to_string());
                Outcome::Suggested
            }
        }
    }

    fn execute(&mut self, tick: u64, action: &Action) -> Outcome {
        match self.network {
            Network::Simulation => {}
            Network::Testnet => {
                let r = "testnet execution is not implemented in this release".to_string();
                self.log(tick, "failed", format!("{action} ({r})"));
                return Outcome::Failed(r);
            }
            Network::Mainnet => {
                let r = "mainnet execution is disabled in this release".to_string();
                self.log(tick, "failed", format!("{action} ({r})"));
                return Outcome::Failed(r);
            }
        }
        match self.adapter.execute(tick, action) {
            Ok(()) => {
                self.total_moved = self.total_moved.saturating_add(action.amount);
                self.log(tick, "executed", action.to_string());
                Outcome::Executed
            }
            Err(e) => {
                self.log(tick, "failed", format!("{action} ({e})"));
                Outcome::Failed(e)
            }
        }
    }

    /// Approve a queued action. Risk checks run again because state may have changed
    /// since the action was queued.
    pub fn approve(&mut self, tick: u64, id: u64) -> Result<Outcome, String> {
        let idx = self
            .pending
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| format!("no pending action with id {id}"))?;
        let p = self.pending.remove(idx);
        self.log(tick, "approved", format!("#{} {}", p.id, p.action));
        match self.check(tick, &p.action) {
            Err(r) => {
                self.log(tick, "denied", format!("{} ({r})", p.action));
                Ok(Outcome::Denied(r))
            }
            Ok(()) => Ok(self.execute(tick, &p.action)),
        }
    }

    pub fn reject(&mut self, tick: u64, id: u64) -> Result<(), String> {
        let idx = self
            .pending
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| format!("no pending action with id {id}"))?;
        let p = self.pending.remove(idx);
        self.log(tick, "rejected", format!("#{} {}", p.id, p.action));
        Ok(())
    }

    pub fn log(&mut self, tick: u64, event: &str, detail: String) {
        self.audit
            .push(format!("tick={tick} event={event} {detail}"));
    }

    pub fn drain_audit(&mut self) -> Vec<String> {
        std::mem::take(&mut self.audit)
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use crate::adapter::{Account, MarketInfo, Snapshot};
    use crate::amount::MICRO;
    use std::collections::BTreeMap;

    /// Minimal in-memory adapter used by core tests (the real simulated Aave adapter
    /// lives in its own crate).
    pub struct FakeAdapter {
        pub account: Account,
        pub weth_price: u64,
    }

    impl FakeAdapter {
        pub fn new() -> FakeAdapter {
            let mut account = Account::default();
            account.wallet.insert("USDC".into(), 1_000 * MICRO);
            account.supplied.insert("WETH".into(), 2 * MICRO);
            account.borrowed.insert("USDC".into(), 2_000 * MICRO);
            FakeAdapter {
                account,
                weth_price: 3_000 * MICRO,
            }
        }
    }

    impl ProtocolAdapter for FakeAdapter {
        fn id(&self) -> &str {
            "aave"
        }
        fn snapshot(&self, tick: u64) -> Result<Snapshot, String> {
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
                    price_usd_micro: self.weth_price,
                    liq_threshold_bps: 8000,
                },
            );
            Ok(Snapshot {
                tick,
                markets,
                account: self.account.clone(),
            })
        }
        fn simulate(&self, tick: u64, action: &Action) -> Result<Snapshot, String> {
            self.snapshot(tick)?.apply(action)
        }
        fn execute(&mut self, tick: u64, action: &Action) -> Result<(), String> {
            self.account = self.simulate(tick, action)?.account;
            Ok(())
        }
        fn account(&self) -> &Account {
            &self.account
        }
    }

    pub fn policy() -> RiskPolicy {
        RiskPolicy {
            allowed_protocols: vec!["aave".into()],
            allowed_assets: vec!["USDC".into(), "WETH".into()],
            allowed_actions: vec![ActionKind::Supply, ActionKind::Withdraw, ActionKind::Alert],
            max_amount_per_action: 100 * MICRO,
            max_total_exposure: 150 * MICRO,
            min_health_factor: 1.5,
        }
    }

    pub fn engine(mode: Mode) -> Engine {
        Engine::new(
            Box::new(FakeAdapter::new()),
            policy(),
            mode,
            Network::Simulation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::amount::MICRO;

    fn supply(n: u64) -> Action {
        Action::new(ActionKind::Supply, "aave", "USDC", n * MICRO)
    }

    #[test]
    fn autonomous_executes_and_tracks_exposure() {
        let mut e = engine(Mode::Autonomous);
        assert_eq!(
            e.submit(1, supply(100), Source::Strategy),
            Outcome::Executed
        );
        assert_eq!(e.total_moved, 100 * MICRO);
        assert_eq!(e.adapter.account().supplied["USDC"], 100 * MICRO);
        // 100 + 60 > 150 total cap
        assert!(
            matches!(e.submit(2, supply(60), Source::Strategy), Outcome::Denied(m) if m.contains("exposure"))
        );
        assert_eq!(e.adapter.account().supplied["USDC"], 100 * MICRO);
    }

    #[test]
    fn approval_mode_queues_then_approve_executes() {
        let mut e = engine(Mode::Approval);
        assert_eq!(
            e.submit(1, supply(50), Source::Strategy),
            Outcome::Queued(1)
        );
        assert_eq!(
            e.adapter.account().supplied.get("USDC"),
            None,
            "nothing executed yet"
        );
        assert_eq!(e.pending.len(), 1);
        assert_eq!(e.approve(2, 1).unwrap(), Outcome::Executed);
        assert!(e.pending.is_empty());
        assert_eq!(e.adapter.account().supplied["USDC"], 50 * MICRO);
    }

    #[test]
    fn reject_discards_without_executing() {
        let mut e = engine(Mode::Approval);
        e.submit(1, supply(50), Source::Strategy);
        e.reject(1, 1).unwrap();
        assert!(e.pending.is_empty());
        assert_eq!(e.total_moved, 0);
        assert!(e.reject(1, 1).is_err(), "cannot reject twice");
        assert!(e.approve(1, 99).is_err());
    }

    #[test]
    fn approval_rechecks_risk_at_approval_time() {
        let mut e = engine(Mode::Approval);
        e.submit(1, supply(100), Source::Strategy);
        e.submit(1, supply(100), Source::Strategy);
        assert_eq!(e.approve(2, 1).unwrap(), Outcome::Executed);
        // second one would now exceed the 150 total cap
        assert!(matches!(e.approve(2, 2).unwrap(), Outcome::Denied(m) if m.contains("exposure")));
    }

    #[test]
    fn manual_mode_only_suggests_but_user_actions_run() {
        let mut e = engine(Mode::Manual);
        assert_eq!(
            e.submit(1, supply(10), Source::Strategy),
            Outcome::Suggested
        );
        assert_eq!(e.total_moved, 0);
        assert_eq!(e.submit(1, supply(10), Source::User), Outcome::Executed);
        assert_eq!(e.total_moved, 10 * MICRO);
    }

    #[test]
    fn user_actions_still_pass_risk() {
        let mut e = engine(Mode::Manual);
        assert!(matches!(
            e.submit(1, supply(500), Source::User),
            Outcome::Denied(_)
        ));
    }

    #[test]
    fn alerts_never_queue_and_need_no_funds() {
        let mut e = engine(Mode::Approval);
        let a = Action::alert("aave", "hf low");
        assert_eq!(e.submit(1, a, Source::Strategy), Outcome::Notified);
        assert!(e.pending.is_empty());
    }

    #[test]
    fn withdraw_blocked_when_health_factor_would_drop() {
        let mut e = engine(Mode::Autonomous);
        // supplied 2 WETH: HF = 2*3000*0.8/2000 = 2.4. Withdrawing 0.5 WETH leaves 1.5 * ...
        // 1.5 WETH -> 3600/2000 = 1.8 (ok); 1.0 WETH removed -> 1 WETH -> 1.2 (< 1.5)
        let mut w = Action::new(ActionKind::Withdraw, "aave", "WETH", MICRO / 2);
        assert_eq!(e.submit(1, w.clone(), Source::Strategy), Outcome::Executed);
        w.amount = MICRO;
        // policy per-action cap is 100 so this is within caps; HF check must deny it
        assert!(
            matches!(e.submit(1, w, Source::Strategy), Outcome::Denied(m) if m.contains("health factor"))
        );
    }

    #[test]
    fn kill_switch_blocks_funds_immediately() {
        let mut e = engine(Mode::Autonomous);
        e.kill_switch = true;
        assert!(
            matches!(e.submit(1, supply(1), Source::User), Outcome::Denied(m) if m.contains("kill switch"))
        );
    }

    #[test]
    fn mainnet_and_testnet_do_not_execute() {
        let mut e = engine(Mode::Autonomous);
        e.network = Network::Mainnet;
        assert!(
            matches!(e.submit(1, supply(1), Source::Strategy), Outcome::Denied(m) if m.contains("mainnet"))
        );
        e.network = Network::Testnet;
        assert!(
            matches!(e.submit(1, supply(1), Source::Strategy), Outcome::Failed(m) if m.contains("not implemented"))
        );
        assert_eq!(e.total_moved, 0);
    }

    #[test]
    fn protocol_mismatch_and_simulation_failures_are_denied() {
        let mut e = engine(Mode::Autonomous);
        e.policy.allowed_protocols.push("compound".into());
        let mut a = supply(1);
        a.protocol = "compound".into();
        assert!(
            matches!(e.submit(1, a, Source::Strategy), Outcome::Denied(m) if m.contains("cannot execute"))
        );
        // wallet only has 1000 USDC but per-action cap is 100, so use an unknown asset instead
        e.policy.allowed_assets.push("DOGE".into());
        let d = Action::new(ActionKind::Supply, "aave", "DOGE", MICRO);
        assert!(
            matches!(e.submit(1, d, Source::Strategy), Outcome::Denied(m) if m.contains("simulation failed"))
        );
    }

    #[test]
    fn audit_log_records_decisions() {
        let mut e = engine(Mode::Approval);
        e.submit(3, supply(10), Source::Strategy);
        e.submit(3, supply(500), Source::Strategy);
        e.approve(4, 1).unwrap();
        let log = e.drain_audit().join("\n");
        assert!(log.contains("event=queued"));
        assert!(log.contains("event=denied"));
        assert!(log.contains("event=approved"));
        assert!(log.contains("event=executed"));
        assert!(e.drain_audit().is_empty());
    }
}
