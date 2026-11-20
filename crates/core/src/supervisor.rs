//! Strategy supervisor: lifecycle, continuous ticking, failure detection, restart.
//!
//! Control plane: `pause`/`resume`/`stop` (from any process) write the instance `status`
//! file; the supervisor re-reads it before every tick. A failing strategy is restarted from
//! a fresh instance up to `max_failures` consecutive times, then the instance is marked
//! `failed`. A failure never propagates beyond the supervised instance.

use std::thread::sleep;
use std::time::Duration;

use crate::engine::{Engine, Outcome, Source};
use crate::manifest::Manifest;
use crate::runtime::step;
use crate::store::{Instance, Store};
use crate::strategy::Strategy;
use crate::types::{Action, ActionKind, Status};

#[derive(Clone, Debug)]
pub struct RunConfig {
    /// Number of completed ticks to run; `0` means run until stopped.
    pub ticks: u64,
    pub interval: Duration,
    /// Consecutive failures tolerated (with restart) before the instance is marked failed.
    pub max_failures: u32,
    /// While paused, give up after this many polls; `0` waits indefinitely.
    pub max_idle_polls: u64,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            ticks: 10,
            interval: Duration::from_millis(500),
            max_failures: 3,
            max_idle_polls: 0,
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct RunSummary {
    pub ticks_run: u64,
    pub executed: u64,
    pub queued: u64,
    pub suggested: u64,
    pub alerts: u64,
    pub denied: u64,
    pub failures: u64,
    pub final_status: Option<Status>,
}

pub type StrategyFactory<'a> = &'a dyn Fn() -> Result<Box<dyn Strategy>, String>;

fn persist(
    store: &Store,
    meta: &mut Instance,
    engine: &mut Engine,
    state: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    meta.total_moved = engine.total_moved;
    meta.next_id = engine.next_id;
    store.save_account(&meta.name, engine.adapter.account())?;
    store.save_pending(&meta.name, &engine.pending)?;
    store.save_strategy_state(&meta.name, state)?;
    store.save_meta(meta)?;
    store.append_audit(&meta.name, &engine.drain_audit())
}

/// Apply commands (approve / reject / act) queued in the instance inbox against `engine`.
/// Used by the supervisor between ticks and by the CLI for instances that are not running.
/// The caller persists afterwards.
pub fn apply_commands(
    store: &Store,
    meta: &Instance,
    engine: &mut Engine,
    out: &mut dyn FnMut(&str),
) -> Result<usize, String> {
    let commands = store.take_commands(&meta.name)?;
    let tick = meta.tick;
    for cmd in &commands {
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd.as_str(), ""));
        match verb {
            "approve" => match rest.trim().parse::<u64>() {
                Ok(id) => match engine.approve(tick, id) {
                    Ok(o) => out(&format!("approve #{id}: {}", o.describe())),
                    Err(e) => out(&format!("approve #{id}: {e}")),
                },
                Err(_) => out(&format!("ignored malformed command '{cmd}'")),
            },
            "reject" => match rest.trim().parse::<u64>() {
                Ok(id) => match engine.reject(tick, id) {
                    Ok(()) => out(&format!("reject #{id}: rejected")),
                    Err(e) => out(&format!("reject #{id}: {e}")),
                },
                Err(_) => out(&format!("ignored malformed command '{cmd}'")),
            },
            "act" => match Action::from_line(rest) {
                Ok(a) if a.kind != ActionKind::Alert => {
                    let o = engine.submit(tick, a.clone(), Source::User);
                    out(&format!("manual {a}: {}", o.describe()));
                }
                Ok(_) => out("ignored manual alert action"),
                Err(e) => out(&format!("ignored malformed command: {e}")),
            },
            _ => out(&format!("ignored unknown command '{cmd}'")),
        }
    }
    Ok(commands.len())
}

pub fn run(
    store: &Store,
    meta: &mut Instance,
    engine: &mut Engine,
    manifest: &Manifest,
    factory: StrategyFactory,
    cfg: &RunConfig,
    out: &mut dyn FnMut(&str),
) -> Result<RunSummary, String> {
    let mut strategy = factory()?;
    let mut state = store.load_strategy_state(&meta.name)?;
    store.write_status(&meta.name, Status::Running)?;
    engine.log(
        meta.tick,
        "started",
        format!("mode={} network={}", engine.mode, engine.network),
    );

    let mut summary = RunSummary::default();
    let mut consecutive_failures: u32 = 0;
    let mut idle_polls: u64 = 0;
    let poll = cfg.interval.max(Duration::from_millis(10));

    loop {
        if apply_commands(store, meta, engine, out)? > 0 {
            persist(store, meta, engine, &state)?;
        }
        if cfg.ticks > 0 && summary.ticks_run >= cfg.ticks {
            break;
        }
        match store.read_status(&meta.name) {
            Status::Stopped | Status::Failed => break,
            Status::Paused => {
                idle_polls += 1;
                if cfg.max_idle_polls > 0 && idle_polls > cfg.max_idle_polls {
                    out("paused: giving up waiting for resume");
                    break;
                }
                sleep(poll);
                continue;
            }
            _ => {}
        }
        idle_polls = 0;
        engine.kill_switch = store.kill_switch();

        meta.tick += 1;
        let tick = meta.tick;
        let report = step(engine, strategy.as_mut(), manifest, tick, &mut state);
        for line in report.lines() {
            out(&line);
        }

        for (_, outcome) in &report.results {
            match outcome {
                Outcome::Executed => summary.executed += 1,
                Outcome::Queued(_) => summary.queued += 1,
                Outcome::Suggested => summary.suggested += 1,
                Outcome::Notified => summary.alerts += 1,
                Outcome::Denied(_) | Outcome::Failed(_) => summary.denied += 1,
            }
        }

        if let Some(err) = &report.error {
            consecutive_failures += 1;
            summary.failures += 1;
            meta.last_error = err.clone();
            engine.log(tick, "strategy_failed", err.clone());
            if consecutive_failures > cfg.max_failures {
                engine.log(
                    tick,
                    "marked_failed",
                    format!("{consecutive_failures} consecutive failures"),
                );
                out(&format!(
                    "tick {tick}: giving up after {consecutive_failures} consecutive failures; instance marked failed"
                ));
                summary.ticks_run += 1;
                persist(store, meta, engine, &state)?;
                store.write_status(&meta.name, Status::Failed)?;
                summary.final_status = Some(Status::Failed);
                return Ok(summary);
            }
            meta.restarts += 1;
            engine.log(tick, "restart", format!("restart #{}", meta.restarts));
            out(&format!(
                "tick {tick}: restarting strategy (attempt {consecutive_failures}/{})",
                cfg.max_failures
            ));
            strategy = factory()?;
        } else {
            consecutive_failures = 0;
        }

        summary.ticks_run += 1;
        persist(store, meta, engine, &state)?;

        if !cfg.interval.is_zero() && (cfg.ticks == 0 || summary.ticks_run < cfg.ticks) {
            sleep(cfg.interval);
        }
    }

    // commands that arrived while the last tick was running
    apply_commands(store, meta, engine, out)?;
    let mut status = store.read_status(&meta.name);
    if status == Status::Running {
        status = Status::Completed;
        store.write_status(&meta.name, status)?;
    }
    engine.log(meta.tick, "finished", format!("status={status}"));
    persist(store, meta, engine, &state)?;
    summary.final_status = Some(status);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::engine;
    use crate::store::temp_store;
    use crate::strategy::{TickInput, TickOutput};
    use crate::types::{Action, ActionKind, Mode, Network, Scenario};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn manifest() -> Manifest {
        Manifest::parse(
            "name=\"t\"\nversion=\"1\"\nlanguage=\"rust\"\nentry=\"builtin:t\"\nprotocols=\"aave\"\n",
        )
        .unwrap()
    }

    fn setup(tag: &str) -> (Store, Instance) {
        let store = temp_store(tag);
        let meta = Instance::new(
            "inst",
            "builtin",
            Mode::Autonomous,
            Network::Simulation,
            Scenario::Calm,
        );
        store.create(&meta).unwrap();
        (store, meta)
    }

    fn cfg(ticks: u64) -> RunConfig {
        RunConfig {
            ticks,
            interval: Duration::ZERO,
            max_failures: 2,
            max_idle_polls: 0,
        }
    }

    struct Counter;
    impl Strategy for Counter {
        fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
            let n: u64 = input
                .state
                .get("n")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
                + 1;
            let mut st = input.state.clone();
            st.insert("n".into(), n.to_string());
            Ok(TickOutput {
                logs: vec![format!("n={n}")],
                proposals: vec![],
                state: st,
            })
        }
    }

    #[test]
    fn runs_requested_ticks_and_completes() {
        let (store, mut meta) = setup("complete");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Counter)) };
        let mut lines = Vec::new();
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(3),
            &mut |l| lines.push(l.to_string()),
        )
        .unwrap();
        assert_eq!(s.ticks_run, 3);
        assert_eq!(s.final_status, Some(Status::Completed));
        assert_eq!(store.read_status("inst"), Status::Completed);
        assert_eq!(store.load_meta("inst").unwrap().tick, 3);
        assert_eq!(store.load_strategy_state("inst").unwrap()["n"], "3");
        assert!(lines.iter().any(|l| l.contains("n=3")));
        assert!(store
            .read_audit("inst")
            .unwrap()
            .iter()
            .any(|l| l.contains("event=finished")));
    }

    #[test]
    fn strategy_state_survives_a_restart_of_the_whole_run() {
        let (store, mut meta) = setup("persist-state");
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Counter)) };
        let mut e = engine(Mode::Autonomous);
        run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(2),
            &mut |_| {},
        )
        .unwrap();
        let mut meta2 = store.load_meta("inst").unwrap();
        let mut e2 = engine(Mode::Autonomous);
        run(
            &store,
            &mut meta2,
            &mut e2,
            &manifest(),
            &factory,
            &cfg(2),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(store.load_strategy_state("inst").unwrap()["n"], "4");
        assert_eq!(store.load_meta("inst").unwrap().tick, 4);
    }

    #[test]
    fn stop_from_outside_halts_the_loop() {
        let (store, mut meta) = setup("stop");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Counter)) };
        let ctl = store.clone();
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(100),
            &mut |l| {
                if l.contains("n=2") {
                    ctl.write_status("inst", Status::Stopped).unwrap();
                }
            },
        )
        .unwrap();
        assert_eq!(s.ticks_run, 2);
        assert_eq!(s.final_status, Some(Status::Stopped));
        assert_eq!(store.read_status("inst"), Status::Stopped);
    }

    #[test]
    fn pause_waits_and_resume_continues() {
        let (store, mut meta) = setup("pause");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Counter)) };
        let ctl = store.clone();
        let c = RunConfig {
            ticks: 4,
            interval: Duration::from_millis(5),
            max_failures: 2,
            max_idle_polls: 2000,
        };
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &c,
            &mut |l| {
                if l.contains("n=2") {
                    ctl.write_status("inst", Status::Paused).unwrap();
                    let resume = ctl.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(80));
                        resume.write_status("inst", Status::Running).unwrap();
                    });
                }
            },
        )
        .unwrap();
        assert_eq!(s.ticks_run, 4);
        assert_eq!(s.final_status, Some(Status::Completed));
    }

    #[test]
    fn paused_forever_gives_up_but_stays_paused() {
        let (store, mut meta) = setup("pause-forever");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Counter)) };
        let ctl = store.clone();
        let c = RunConfig {
            ticks: 10,
            interval: Duration::from_millis(1),
            max_failures: 2,
            max_idle_polls: 3,
        };
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &c,
            &mut |l| {
                if l.contains("n=1") {
                    ctl.write_status("inst", Status::Paused).unwrap();
                }
            },
        )
        .unwrap();
        assert_eq!(s.ticks_run, 1);
        assert_eq!(s.final_status, Some(Status::Paused));
    }

    struct FlakyThenGood {
        calls: Arc<AtomicU32>,
    }
    impl Strategy for FlakyThenGood {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err("transient".into())
            } else {
                Ok(TickOutput {
                    logs: vec!["recovered".into()],
                    ..Default::default()
                })
            }
        }
    }

    #[test]
    fn failing_strategy_is_restarted_then_recovers() {
        let (store, mut meta) = setup("recover");
        let mut e = engine(Mode::Autonomous);
        let calls = Arc::new(AtomicU32::new(0));
        let built = Arc::new(AtomicU32::new(0));
        let (c2, b2) = (calls.clone(), built.clone());
        let factory = move || -> Result<Box<dyn Strategy>, String> {
            b2.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FlakyThenGood { calls: c2.clone() }))
        };
        let mut lines = Vec::new();
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(4),
            &mut |l| lines.push(l.to_string()),
        )
        .unwrap();
        assert_eq!(s.failures, 2);
        assert_eq!(s.final_status, Some(Status::Completed));
        assert_eq!(built.load(Ordering::SeqCst), 3, "initial + 2 restarts");
        assert!(lines.iter().any(|l| l.contains("recovered")));
        assert_eq!(store.load_meta("inst").unwrap().restarts, 2);
    }

    struct AlwaysFails;
    impl Strategy for AlwaysFails {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            Err("permanent".into())
        }
    }

    #[test]
    fn repeated_failures_mark_instance_failed() {
        let (store, mut meta) = setup("fail");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(AlwaysFails)) };
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(50),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(s.final_status, Some(Status::Failed));
        assert_eq!(
            s.ticks_run, 3,
            "2 tolerated + the one that exceeds the limit"
        );
        assert_eq!(store.read_status("inst"), Status::Failed);
        assert_eq!(store.load_meta("inst").unwrap().last_error, "permanent");
    }

    struct Panics;
    impl Strategy for Panics {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            panic!("boom");
        }
    }

    #[test]
    fn panicking_strategy_does_not_crash_the_supervisor() {
        let (store, mut meta) = setup("panic");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Panics)) };
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(10),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(s.final_status, Some(Status::Failed));
    }

    struct Supplier;
    impl Strategy for Supplier {
        fn on_tick(&mut self, _: &TickInput) -> Result<TickOutput, String> {
            Ok(TickOutput {
                proposals: vec![Action::new(ActionKind::Supply, "aave", "USDC", 50_000_000)],
                ..Default::default()
            })
        }
    }

    #[test]
    fn approval_queue_and_account_persist_across_ticks() {
        let (store, mut meta) = setup("queue");
        meta.mode = Mode::Approval;
        let mut e = engine(Mode::Approval);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Supplier)) };
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(2),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(s.queued, 2);
        let pending = store.load_pending("inst").unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(store.load_meta("inst").unwrap().next_id, 3);
    }

    #[test]
    fn approve_and_manual_actions_reach_a_running_instance() {
        let (store, mut meta) = setup("inbox");
        meta.mode = Mode::Approval;
        let mut e = engine(Mode::Approval);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Supplier)) };
        let ctl = store.clone();
        let mut lines = Vec::new();
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(3),
            &mut |l| {
                lines.push(l.to_string());
                if l.contains("tick 1:") && l.contains("queued for approval as #1") {
                    ctl.push_command("inst", "approve 1").unwrap();
                    ctl.push_command("inst", "reject 2").unwrap(); // does not exist yet: reported, not fatal
                    ctl.push_command("inst", "bogus").unwrap();
                }
            },
        )
        .unwrap();
        assert_eq!(s.ticks_run, 3);
        assert!(lines.iter().any(|l| l.contains("approve #1: executed")));
        assert!(lines
            .iter()
            .any(|l| l.contains("reject #2: no pending action")));
        assert!(lines.iter().any(|l| l.contains("ignored unknown command")));
        let acc = store.load_account("inst").unwrap().unwrap();
        assert_eq!(acc.supplied["USDC"], 50_000_000);
        assert_eq!(store.load_meta("inst").unwrap().total_moved, 50_000_000);
        // ticks 2 and 3 queued two more
        assert_eq!(store.load_pending("inst").unwrap().len(), 2);
    }

    #[test]
    fn commands_apply_while_paused() {
        let (store, mut meta) = setup("inbox-paused");
        let mut e = engine(Mode::Approval);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Supplier)) };
        let ctl = store.clone();
        let c = RunConfig {
            ticks: 10,
            interval: Duration::from_millis(1),
            max_failures: 2,
            max_idle_polls: 30,
        };
        let mut lines = Vec::new();
        run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &c,
            &mut |l| {
                lines.push(l.to_string());
                if l.contains("queued for approval as #1") {
                    ctl.write_status("inst", Status::Paused).unwrap();
                    ctl.push_command("inst", "approve 1").unwrap();
                }
            },
        )
        .unwrap();
        assert!(lines.iter().any(|l| l.contains("approve #1: executed")));
        assert_eq!(store.read_status("inst"), Status::Paused);
    }

    #[test]
    fn kill_switch_is_honoured_between_ticks() {
        let (store, mut meta) = setup("kill");
        let mut e = engine(Mode::Autonomous);
        let factory = || -> Result<Box<dyn Strategy>, String> { Ok(Box::new(Supplier)) };
        let ctl = store.clone();
        let s = run(
            &store,
            &mut meta,
            &mut e,
            &manifest(),
            &factory,
            &cfg(4),
            &mut |l| {
                if l.contains("tick 1:") && l.contains("executed") {
                    ctl.set_kill_switch(true).unwrap();
                }
            },
        )
        .unwrap();
        assert_eq!(s.executed, 1);
        assert_eq!(s.denied, 3);
        let _ = std::fs::remove_dir_all(&store.home);
    }
}
