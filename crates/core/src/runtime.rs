//! One strategy tick, shared by `simulate`, `test` and the supervisor.
//!
//! Strategy panics and errors are caught here so one faulty strategy can never take
//! the engine down (brief section 6: fault isolation).

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::engine::{Engine, Outcome, Source};
use crate::manifest::Manifest;
use crate::strategy::{Strategy, TickInput};
use crate::types::Action;
use crate::MAX_PROPOSALS_PER_TICK;

#[derive(Debug, Default)]
pub struct TickReport {
    pub tick: u64,
    pub health_factor: Option<f64>,
    pub logs: Vec<String>,
    pub results: Vec<(Action, Outcome)>,
    pub error: Option<String>,
}

impl TickReport {
    pub fn lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(e) = &self.error {
            v.push(format!("tick {}: ERROR {e}", self.tick));
        }
        for l in &self.logs {
            v.push(format!("tick {}: {l}", self.tick));
        }
        for (a, o) in &self.results {
            v.push(format!("tick {}: {a} -> {}", self.tick, o.describe()));
        }
        v
    }
}

fn panic_message(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

pub fn step(
    engine: &mut Engine,
    strategy: &mut dyn Strategy,
    manifest: &Manifest,
    tick: u64,
    state: &mut BTreeMap<String, String>,
) -> TickReport {
    let mut report = TickReport {
        tick,
        ..Default::default()
    };
    let snapshot = match engine.adapter.snapshot(tick) {
        Ok(s) => s,
        Err(e) => {
            report.error = Some(format!("adapter error: {e}"));
            return report;
        }
    };
    report.health_factor = snapshot.health_factor();

    let result = {
        let input = TickInput {
            tick,
            snapshot: &snapshot,
            params: &manifest.params,
            state: &*state,
        };
        catch_unwind(AssertUnwindSafe(|| strategy.on_tick(&input)))
    };

    match result {
        Err(p) => report.error = Some(format!("strategy panicked: {}", panic_message(p))),
        Ok(Err(e)) => report.error = Some(e),
        Ok(Ok(out)) => {
            if out.proposals.len() > MAX_PROPOSALS_PER_TICK {
                report.error = Some(format!(
                    "more than {MAX_PROPOSALS_PER_TICK} proposals in one tick"
                ));
                return report;
            }
            *state = out.state;
            report.logs = out.logs;
            for action in out.proposals {
                let outcome = engine.submit(tick, action.clone(), Source::Strategy);
                report.results.push((action, outcome));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::engine;
    use crate::strategy::TickOutput;
    use crate::types::{ActionKind, Mode};

    fn manifest() -> Manifest {
        Manifest::parse(
            "name=\"t\"\nversion=\"1\"\nlanguage=\"rust\"\nentry=\"builtin:t\"\nprotocols=\"aave\"\n",
        )
        .unwrap()
    }

    struct Proposer;
    impl Strategy for Proposer {
        fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
            let mut st = input.state.clone();
            let n: u64 = st.get("n").and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
            st.insert("n".into(), n.to_string());
            Ok(TickOutput {
                logs: vec![format!("n={n}")],
                proposals: vec![Action::new(ActionKind::Supply, "aave", "USDC", 1_000_000)],
                state: st,
            })
        }
    }

    struct Panicker;
    impl Strategy for Panicker {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            panic!("kaboom");
        }
    }

    struct Failer;
    impl Strategy for Failer {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            Err("bad input".into())
        }
    }

    struct Flooder;
    impl Strategy for Flooder {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            Ok(TickOutput {
                proposals: (0..(MAX_PROPOSALS_PER_TICK + 1))
                    .map(|_| Action::new(ActionKind::Supply, "aave", "USDC", 1))
                    .collect(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn state_flows_between_ticks_and_proposals_hit_risk() {
        let mut e = engine(Mode::Autonomous);
        let m = manifest();
        let mut state = BTreeMap::new();
        let r1 = step(&mut e, &mut Proposer, &m, 1, &mut state);
        let r2 = step(&mut e, &mut Proposer, &m, 2, &mut state);
        assert_eq!(r1.logs, vec!["n=1"]);
        assert_eq!(r2.logs, vec!["n=2"]);
        assert_eq!(state["n"], "2");
        assert_eq!(r1.results[0].1, Outcome::Executed);
        assert_eq!(r1.health_factor.map(|h| (h * 10.0).round()), Some(24.0));
        assert!(r1
            .lines()
            .iter()
            .any(|l| l.contains("supply aave USDC 1 -> executed")));
    }

    #[test]
    fn panics_are_contained() {
        let mut e = engine(Mode::Autonomous);
        let r = step(&mut e, &mut Panicker, &manifest(), 1, &mut BTreeMap::new());
        assert!(r.error.unwrap().contains("kaboom"));
    }

    #[test]
    fn errors_are_reported_and_state_kept() {
        let mut e = engine(Mode::Autonomous);
        let mut state = BTreeMap::new();
        state.insert("keep".to_string(), "me".to_string());
        let r = step(&mut e, &mut Failer, &manifest(), 1, &mut state);
        assert_eq!(r.error.as_deref(), Some("bad input"));
        assert_eq!(state["keep"], "me");
    }

    #[test]
    fn proposal_flood_is_rejected_before_any_action() {
        let mut e = engine(Mode::Autonomous);
        let r = step(&mut e, &mut Flooder, &manifest(), 1, &mut BTreeMap::new());
        assert!(r.error.unwrap().contains("proposals"));
        assert_eq!(e.total_moved, 0);
    }
}
