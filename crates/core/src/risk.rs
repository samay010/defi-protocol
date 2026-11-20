//! Risk engine: an independent gate that every action must pass, regardless of mode,
//! source (strategy or user) or language the strategy was written in.
//!
//! Rules are deny-by-default: a manifest that declares nothing can only raise alerts.

use crate::adapter::Snapshot;
use crate::types::{Action, ActionKind, Network};

#[derive(Clone, Debug, PartialEq)]
pub struct RiskPolicy {
    pub allowed_protocols: Vec<String>,
    pub allowed_assets: Vec<String>,
    pub allowed_actions: Vec<ActionKind>,
    pub max_amount_per_action: u64,
    /// Cumulative cap on funds moved by this instance (micro-units).
    pub max_total_exposure: u64,
    pub min_health_factor: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct RiskState {
    pub total_moved: u64,
    pub kill_switch: bool,
    pub network: Network,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

impl RiskPolicy {
    /// `post` is the simulated post-action snapshot; pass `None` for the static checks.
    pub fn check(&self, action: &Action, state: &RiskState, post: Option<&Snapshot>) -> Decision {
        let deny = |m: String| Decision::Deny(m);

        if !self.allowed_protocols.iter().any(|p| p == &action.protocol) {
            return deny(format!(
                "protocol '{}' is not permitted by the manifest",
                action.protocol
            ));
        }
        if !self.allowed_actions.contains(&action.kind) {
            return deny(format!(
                "action '{}' is not permitted by the manifest",
                action.kind
            ));
        }
        if !action.kind.moves_funds() {
            return Decision::Allow;
        }

        // Fund-moving actions only from here.
        match state.network {
            Network::Simulation => {}
            Network::Testnet => {}
            Network::Mainnet => {
                return deny("mainnet execution is disabled in this release".into());
            }
        }
        if state.kill_switch {
            return deny("emergency kill switch is on".into());
        }
        if !self.allowed_assets.iter().any(|a| a == &action.asset) {
            return deny(format!(
                "asset '{}' is not permitted by the manifest",
                action.asset
            ));
        }
        if action.amount == 0 {
            return deny("amount must be greater than zero".into());
        }
        if action.amount > self.max_amount_per_action {
            return deny(format!(
                "amount {} exceeds per-action cap {}",
                crate::amount::format_amount(action.amount),
                crate::amount::format_amount(self.max_amount_per_action)
            ));
        }
        if state.total_moved.saturating_add(action.amount) > self.max_total_exposure {
            return deny(format!(
                "total exposure cap {} would be exceeded (already moved {})",
                crate::amount::format_amount(self.max_total_exposure),
                crate::amount::format_amount(state.total_moved)
            ));
        }
        if let (Some(post), ActionKind::Withdraw) = (post, action.kind) {
            if let Some(hf) = post.health_factor() {
                if hf < self.min_health_factor {
                    return deny(format!(
                        "health factor after withdrawal would be {hf:.2}, below the minimum {:.2}",
                        self.min_health_factor
                    ));
                }
            }
        }
        Decision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::MICRO;

    fn policy() -> RiskPolicy {
        RiskPolicy {
            allowed_protocols: vec!["aave".into()],
            allowed_assets: vec!["USDC".into()],
            allowed_actions: vec![ActionKind::Supply, ActionKind::Withdraw, ActionKind::Alert],
            max_amount_per_action: 100 * MICRO,
            max_total_exposure: 250 * MICRO,
            min_health_factor: 1.5,
        }
    }

    fn state() -> RiskState {
        RiskState {
            total_moved: 0,
            kill_switch: false,
            network: Network::Simulation,
        }
    }

    fn supply(amount: u64) -> Action {
        Action::new(ActionKind::Supply, "aave", "USDC", amount)
    }

    #[test]
    fn allows_in_policy_action() {
        assert_eq!(
            policy().check(&supply(50 * MICRO), &state(), None),
            Decision::Allow
        );
    }

    #[test]
    fn denies_wrong_protocol_asset_and_kind() {
        let p = policy();
        let mut a = supply(MICRO);
        a.protocol = "compound".into();
        assert!(matches!(p.check(&a, &state(), None), Decision::Deny(m) if m.contains("protocol")));
        let mut a = supply(MICRO);
        a.asset = "DAI".into();
        assert!(matches!(p.check(&a, &state(), None), Decision::Deny(m) if m.contains("asset")));
        let a = Action::new(ActionKind::Repay, "aave", "USDC", MICRO);
        assert!(matches!(p.check(&a, &state(), None), Decision::Deny(m) if m.contains("repay")));
    }

    #[test]
    fn enforces_caps() {
        let p = policy();
        assert!(
            matches!(p.check(&supply(101 * MICRO), &state(), None), Decision::Deny(m) if m.contains("per-action"))
        );
        let mut s = state();
        s.total_moved = 200 * MICRO;
        assert!(
            matches!(p.check(&supply(60 * MICRO), &s, None), Decision::Deny(m) if m.contains("exposure"))
        );
        // exactly at the cap is allowed
        assert_eq!(p.check(&supply(50 * MICRO), &s, None), Decision::Allow);
    }

    #[test]
    fn zero_amount_denied() {
        assert!(matches!(
            policy().check(&supply(0), &state(), None),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn kill_switch_and_mainnet_block_funds_but_not_alerts() {
        let p = policy();
        let mut s = state();
        s.kill_switch = true;
        assert!(
            matches!(p.check(&supply(MICRO), &s, None), Decision::Deny(m) if m.contains("kill switch"))
        );
        let alert = Action::alert("aave", "hi");
        assert_eq!(p.check(&alert, &s, None), Decision::Allow);
        let mut s = state();
        s.network = Network::Mainnet;
        assert!(
            matches!(p.check(&supply(MICRO), &s, None), Decision::Deny(m) if m.contains("mainnet"))
        );
        assert_eq!(p.check(&alert, &s, None), Decision::Allow);
    }

    #[test]
    fn default_manifest_policy_is_alert_only() {
        let p = RiskPolicy {
            allowed_protocols: vec!["aave".into()],
            allowed_assets: vec![],
            allowed_actions: vec![ActionKind::Alert],
            max_amount_per_action: 0,
            max_total_exposure: 0,
            min_health_factor: 1.5,
        };
        assert!(matches!(
            p.check(&supply(MICRO), &state(), None),
            Decision::Deny(_)
        ));
        assert_eq!(
            p.check(&Action::alert("aave", "x"), &state(), None),
            Decision::Allow
        );
    }
}
