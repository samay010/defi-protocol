//! Robustness and property tests using only the standard library.
//!
//! Two kinds of checks:
//! 1. Mutation "fuzz-lite": the parsers that read untrusted input (strategy stdout, manifests,
//!    action lines, amounts) are fed hundreds of thousands of mutated and random inputs. They must
//!    never panic, and whenever they accept an input the result must satisfy the invariants the
//!    rest of the engine relies on.
//! 2. Property tests: money and permission invariants hold for random action sequences.
//!
//! The PRNG is seeded, so failures are reproducible. For coverage-guided fuzzing see `fuzz/`.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use protocol_core::adapter::{Account, MarketInfo, ProtocolAdapter, Snapshot};
use protocol_core::amount::{format_amount, parse_amount, MICRO};
use protocol_core::engine::{Engine, Outcome, Source};
use protocol_core::external::parse_output;
use protocol_core::manifest::{
    valid_name, Manifest, MAX_MEMORY_MB, MAX_TICK_TIMEOUT_MS, MIN_MEMORY_MB,
};
use protocol_core::risk::{Decision, RiskPolicy, RiskState};
use protocol_core::types::{Action, ActionKind, Mode, Network};
use protocol_core::MAX_PROPOSALS_PER_TICK;

// ---- tiny deterministic PRNG (xorshift64*) -------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const INTERESTING: &[&[u8]] = &[
    b"=",
    b"\"",
    b"\n",
    b"\r\n",
    b"|",
    b" ",
    b"-",
    b"0",
    b".",
    b"#",
    b"[params]",
    b"[other]",
    b"PROPOSE ",
    b"DONE\n",
    b"SETSTATE ",
    b"999999999999999999999999",
    b"18446744073709551615",
    b"\xc3\xa9",
    b"\xe2\x82\xac",
    b"\x00",
    b",",
];

fn mutate(seed: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..1 + rng.below(4) {
        match rng.below(6) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = (rng.next() & 0xff) as u8;
            }
            1 => {
                let i = rng.below(v.len() + 1);
                let ins = *rng.pick(INTERESTING);
                v.splice(i..i, ins.iter().copied());
            }
            2 if !v.is_empty() => {
                let a = rng.below(v.len());
                let b = (a + 1 + rng.below(8)).min(v.len());
                v.drain(a..b);
            }
            3 if !v.is_empty() => {
                let a = rng.below(v.len());
                let b = (a + 1 + rng.below(16)).min(v.len());
                let dup: Vec<u8> = v[a..b].to_vec();
                v.splice(a..a, dup);
            }
            4 if !v.is_empty() => {
                let n = rng.below(v.len());
                v.truncate(n);
            }
            _ => {
                let n = rng.below(24);
                for _ in 0..n {
                    v.push((rng.next() & 0xff) as u8);
                }
            }
        }
    }
    v
}

fn as_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// Run `f`, turning a panic into a test failure that shows the offending input.
fn no_panic<T>(what: &str, input: &str, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("{what} panicked on input: {input:?}"),
    }
}

const ITERATIONS: usize = 40_000;

// ---- parser robustness -----------------------------------------------------------------------------

#[test]
fn strategy_output_parser_never_panics_and_keeps_its_promises() {
    let seeds: Vec<&[u8]> = vec![
        b"LOG hello\nPROPOSE supply aave USDC 1000000 note here\nPROPOSE alert aave - 0 careful\nSETSTATE k v\nDONE\n",
        b"DONE\n",
        b"PROPOSE withdraw aave WETH 500000\nDONE\n",
        b"SETSTATE a 1\nSETSTATE b 2\nLOG x\nDONE\n",
    ];
    let mut rng = Rng::new(0xC0FFEE);
    let mut accepted = 0;
    for i in 0..ITERATIONS {
        let input = if i % 5 == 0 {
            let n = rng.below(200);
            (0..n).map(|_| (rng.next() & 0xff) as u8).collect()
        } else {
            mutate(rng.pick(&seeds), &mut rng)
        };
        let text = as_text(&input);
        let result = no_panic("parse_output", &text, || parse_output(&text));
        if let Ok(out) = result {
            accepted += 1;
            assert!(
                out.proposals.len() <= MAX_PROPOSALS_PER_TICK,
                "too many proposals: {text:?}"
            );
            assert!(
                text.lines().any(|l| l.trim_end() == "DONE"),
                "accepted without DONE: {text:?}"
            );
            for p in &out.proposals {
                assert!(
                    p.asset == p.asset.to_ascii_uppercase(),
                    "asset not normalised: {text:?}"
                );
                assert!(
                    p.protocol == p.protocol.to_ascii_lowercase(),
                    "protocol not normalised: {text:?}"
                );
            }
        }
    }
    assert!(
        accepted > 100,
        "mutations should still produce some valid inputs ({accepted})"
    );
}

#[test]
fn manifest_parser_never_panics_and_only_accepts_consistent_manifests() {
    let base = r#"
name = "my-strategy"
version = "0.1.0"
language = "python"
entry = "python3 strategy.py"
description = "demo"
protocols = "aave"
assets = "usdc, dai"
allowed_actions = "supply, alert"
max_amount_per_action = "100"
max_total_exposure = "500.5"
min_health_factor = "1.8"
tick_timeout_ms = "1500"
max_memory_mb = "256"

[params]
threshold = "42"
"#;
    let seeds: Vec<&[u8]> = vec![
        base.as_bytes(),
        b"name=\"x\"\nversion=\"1\"\nlanguage=\"c\"\nentry=\"./b\"\nprotocols=\"aave\"\n",
    ];
    let mut rng = Rng::new(0xBADC0DE);
    let mut accepted = 0;
    for i in 0..ITERATIONS {
        let input = if i % 7 == 0 {
            let n = rng.below(300);
            (0..n).map(|_| (rng.next() & 0xff) as u8).collect()
        } else {
            mutate(rng.pick(&seeds), &mut rng)
        };
        let text = as_text(&input);
        let result = no_panic("Manifest::parse", &text, || Manifest::parse(&text));
        let Ok(m) = result else { continue };
        accepted += 1;
        assert!(
            valid_name(&m.name),
            "accepted unsafe name {:?} from {text:?}",
            m.name
        );
        assert!(!m.protocols.is_empty(), "no protocols: {text:?}");
        assert!(
            m.max_amount_per_action <= m.max_total_exposure,
            "caps inconsistent: {text:?}"
        );
        assert!(
            m.min_health_factor.is_finite() && m.min_health_factor >= 1.0,
            "bad hf: {text:?}"
        );
        assert!(
            m.tick_timeout_ms >= 1 && m.tick_timeout_ms <= MAX_TICK_TIMEOUT_MS,
            "bad timeout: {text:?}"
        );
        assert!(
            m.max_memory_mb == 0 || (MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&m.max_memory_mb),
            "bad memory limit: {text:?}"
        );
        assert!(!m.allowed_actions.is_empty());
        if m.allowed_actions.iter().any(|a| a.moves_funds()) {
            assert!(
                !m.assets.is_empty() && m.max_amount_per_action > 0,
                "fund-moving without caps: {text:?}"
            );
        }
        // permissions in the manifest must be exactly what the risk engine will enforce
        let p = m.policy();
        assert_eq!(p.allowed_protocols, m.protocols);
        assert_eq!(p.allowed_assets, m.assets);
        assert_eq!(p.max_total_exposure, m.max_total_exposure);
    }
    assert!(
        accepted > 100,
        "mutations should still produce some valid manifests ({accepted})"
    );
}

#[test]
fn action_line_parser_never_panics_and_round_trips() {
    let seeds: Vec<&[u8]> = vec![
        b"supply|aave|USDC|5000000|a note",
        b"alert|aave||0|hf low",
        b"withdraw|aave|WETH|1|x|y|z",
    ];
    let mut rng = Rng::new(0xFACADE);
    for _ in 0..ITERATIONS {
        let text = as_text(&mutate(rng.pick(&seeds), &mut rng));
        let result = no_panic("Action::from_line", &text, || Action::from_line(&text));
        if let Ok(a) = result {
            let again = Action::from_line(&a.to_line()).expect("serialised action must parse");
            assert_eq!(
                (again.kind, &again.protocol, &again.asset, again.amount),
                (a.kind, &a.protocol, &a.asset, a.amount),
                "{text:?}"
            );
        }
    }
}

#[test]
fn amount_parser_never_panics_and_round_trips() {
    let seeds: Vec<&[u8]> = vec![b"100", b"100.5", b"0.000001", b".5", b"123456789.123456"];
    let mut rng = Rng::new(0xABCDEF);
    for _ in 0..ITERATIONS {
        let text = as_text(&mutate(rng.pick(&seeds), &mut rng));
        if let Ok(v) = no_panic("parse_amount", &text, || parse_amount(&text)) {
            assert_eq!(
                parse_amount(&format_amount(v)),
                Ok(v),
                "round trip failed for {text:?}"
            );
        }
    }
    // exhaustive-ish: every power of ten boundary and the extremes
    for v in [
        0u64,
        1,
        999_999,
        1_000_000,
        1_000_001,
        u64::MAX / 2,
        u64::MAX - 1,
        u64::MAX,
    ] {
        assert_eq!(parse_amount(&format_amount(v)), Ok(v), "{v}");
    }
    for _ in 0..ITERATIONS {
        let v = rng.next() >> rng.below(64);
        assert_eq!(parse_amount(&format_amount(v)), Ok(v), "{v}");
    }
}

// ---- ledger and risk properties --------------------------------------------------------------------

fn markets() -> BTreeMap<String, MarketInfo> {
    let mut m = BTreeMap::new();
    m.insert(
        "USDC".into(),
        MarketInfo {
            supply_apy_bps: 400,
            price_usd_micro: MICRO,
            liq_threshold_bps: 8000,
        },
    );
    m.insert(
        "WETH".into(),
        MarketInfo {
            supply_apy_bps: 100,
            price_usd_micro: 3_000 * MICRO,
            liq_threshold_bps: 8250,
        },
    );
    m
}

fn start_account() -> Account {
    let mut a = Account::default();
    a.wallet.insert("USDC".into(), 1_000 * MICRO);
    a.supplied.insert("WETH".into(), 2 * MICRO);
    a.borrowed.insert("USDC".into(), 2_000 * MICRO);
    a
}

fn total(acc: &Account, asset: &str) -> u128 {
    acc.wallet.get(asset).copied().unwrap_or(0) as u128
        + acc.supplied.get(asset).copied().unwrap_or(0) as u128
}

fn random_action(rng: &mut Rng) -> Action {
    let kind = *rng.pick(&[
        ActionKind::Supply,
        ActionKind::Withdraw,
        ActionKind::Repay,
        ActionKind::Alert,
    ]);
    let asset = *rng.pick(&["USDC", "WETH", "DOGE", ""]);
    let amount = match rng.below(6) {
        0 => 0,
        1 => 1,
        2 => rng.next() % (50 * MICRO),
        3 => 100 * MICRO,
        4 => 100 * MICRO + 1,
        _ => rng.next(),
    };
    let protocol = if rng.chance(10) { "compound" } else { "aave" };
    Action {
        kind,
        protocol: protocol.into(),
        asset: asset.into(),
        amount,
        note: String::new(),
    }
}

#[test]
fn apply_conserves_balances_for_random_actions() {
    let mut rng = Rng::new(0x5EED);
    let snap = Snapshot {
        tick: 1,
        markets: markets(),
        account: start_account(),
    };
    for _ in 0..ITERATIONS {
        let a = random_action(&mut rng);
        match snap.apply(&a) {
            Err(_) => {}
            Ok(after) => {
                for asset in ["USDC", "WETH"] {
                    match a.kind {
                        ActionKind::Supply | ActionKind::Withdraw => {
                            assert_eq!(
                                total(&after.account, asset),
                                total(&snap.account, asset),
                                "{a:?}"
                            )
                        }
                        ActionKind::Repay => assert!(
                            total(&after.account, asset) <= total(&snap.account, asset),
                            "{a:?}"
                        ),
                        ActionKind::Alert => assert_eq!(after.account, snap.account),
                    }
                }
                if a.kind == ActionKind::Alert {
                    continue;
                }
                assert!(a.amount > 0, "zero-amount fund action accepted: {a:?}");
                assert!(
                    snap.markets.contains_key(&a.asset),
                    "unknown asset accepted: {a:?}"
                );
            }
        }
        // the input snapshot is never modified
        assert_eq!(snap.account, start_account());
    }
}

fn policy() -> RiskPolicy {
    RiskPolicy {
        allowed_protocols: vec!["aave".into()],
        allowed_assets: vec!["USDC".into(), "WETH".into()],
        allowed_actions: vec![ActionKind::Supply, ActionKind::Withdraw, ActionKind::Alert],
        max_amount_per_action: 100 * MICRO,
        max_total_exposure: 250 * MICRO,
        min_health_factor: 1.5,
    }
}

#[test]
fn risk_allow_implies_every_declared_limit() {
    let mut rng = Rng::new(0xD1CE);
    let p = policy();
    for _ in 0..ITERATIONS {
        let a = random_action(&mut rng);
        let state = RiskState {
            total_moved: *rng.pick(&[0, 100 * MICRO, 250 * MICRO, u64::MAX]),
            kill_switch: rng.chance(4),
            network: *rng.pick(&[Network::Simulation, Network::Testnet, Network::Mainnet]),
        };
        if p.check(&a, &state, None) != Decision::Allow {
            continue;
        }
        assert!(p.allowed_protocols.contains(&a.protocol), "{a:?}");
        assert!(p.allowed_actions.contains(&a.kind), "{a:?}");
        if a.kind.moves_funds() {
            assert!(p.allowed_assets.contains(&a.asset), "{a:?}");
            assert!(a.amount > 0 && a.amount <= p.max_amount_per_action, "{a:?}");
            assert!(
                state.total_moved.saturating_add(a.amount) <= p.max_total_exposure,
                "{a:?} {state:?}"
            );
            assert!(!state.kill_switch, "{a:?}");
            assert_ne!(state.network, Network::Mainnet, "{a:?}");
        }
    }
}

struct SimAdapter {
    account: Account,
}

impl ProtocolAdapter for SimAdapter {
    fn id(&self) -> &str {
        "aave"
    }
    fn snapshot(&self, tick: u64) -> Result<Snapshot, String> {
        Ok(Snapshot {
            tick,
            markets: markets(),
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

#[test]
fn engine_never_breaks_exposure_mode_network_or_health_factor_rules() {
    for seed in 1..=40u64 {
        let mut rng = Rng::new(seed * 7919);
        let p = policy();
        let mode = *rng.pick(&[Mode::Autonomous, Mode::Approval, Mode::Manual]);
        let network = *rng.pick(&[
            Network::Simulation,
            Network::Simulation,
            Network::Testnet,
            Network::Mainnet,
        ]);
        let mut engine = Engine::new(
            Box::new(SimAdapter {
                account: start_account(),
            }),
            p.clone(),
            mode,
            network,
        );
        let initial = start_account();

        for step in 0..400u64 {
            let before_moved = engine.total_moved;
            engine.kill_switch = rng.chance(8);
            let action = random_action(&mut rng);
            let source = if rng.chance(3) {
                Source::User
            } else {
                Source::Strategy
            };
            let outcome = match rng.below(5) {
                0 if !engine.pending.is_empty() => {
                    let id = engine.pending[rng.below(engine.pending.len())].id;
                    engine
                        .approve(step, id)
                        .unwrap_or(Outcome::Denied("gone".into()))
                }
                1 if !engine.pending.is_empty() => {
                    let id = engine.pending[rng.below(engine.pending.len())].id;
                    let _ = engine.reject(step, id);
                    continue;
                }
                _ => engine.submit(step, action.clone(), source),
            };

            // invariant 1: cumulative exposure never exceeds the cap
            assert!(
                engine.total_moved <= p.max_total_exposure,
                "seed {seed} step {step}: exposure {}",
                engine.total_moved
            );
            // invariant 2: only a successful execution moves the counter, and never by more than the per-action cap
            let moved = engine.total_moved - before_moved;
            assert!(
                moved <= p.max_amount_per_action,
                "seed {seed}: moved {moved}"
            );
            if outcome != Outcome::Executed {
                assert_eq!(moved, 0, "seed {seed}: {outcome:?} must not move funds");
            }
            // invariant 3: nothing executes on mainnet or testnet in this release, nor while the kill switch is on
            if network != Network::Simulation {
                assert_eq!(engine.total_moved, 0, "seed {seed}: executed on {network}");
            }
            if outcome == Outcome::Executed {
                assert!(
                    !engine.kill_switch,
                    "seed {seed}: executed with kill switch on"
                );
            }
            // invariant 4: strategy proposals in manual mode never execute by themselves
            if mode == Mode::Manual && source == Source::Strategy && outcome == Outcome::Executed {
                panic!("seed {seed}: manual mode executed a strategy proposal");
            }
            // invariant 5: balances are conserved and an executed withdrawal respects the health-factor floor
            let acc = engine.adapter.account();
            for asset in ["USDC", "WETH"] {
                assert!(
                    total(acc, asset) <= total(&initial, asset),
                    "seed {seed}: value appeared from nowhere"
                );
            }
            if outcome == Outcome::Executed && action.kind == ActionKind::Withdraw {
                if let Some(hf) = engine.adapter.snapshot(step).unwrap().health_factor() {
                    assert!(
                        hf >= p.min_health_factor,
                        "seed {seed}: executed withdrawal left hf {hf}"
                    );
                }
            }
        }
    }
}
