//! The `protocol` command-line interface.
//!
//! `run` takes arguments and two writers so the whole CLI is testable in-process.
//! Exit codes: 0 success, 1 failure (including failed tests), 2 usage error.

mod args;
mod resolve;
mod templates;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use protocol_adapter_aave::chain::AaveChainRead;
use protocol_adapter_aave::events::AccountWatcher;
use protocol_adapter_aave::onchain::{AaveReader, AccountData, Address, EthCall};
use protocol_adapter_aave::simulate;
use protocol_adapter_aave::AaveSim;
use protocol_adapter_compound::{CompoundReader, CompoundSource, MarketConfig, Position};
use protocol_adapter_morpho::{MarketId, MorphoReader, MorphoSource};
use protocol_core::adapter::{Account, ProtocolAdapter};
use protocol_core::amount::{format_amount, parse_amount};
use protocol_core::engine::{Engine, Outcome};
use protocol_core::manifest::{valid_name, Language};
use protocol_core::readonly::ReadOnlyAdapter;
use protocol_core::runtime::step;
use protocol_core::sandbox::{self, SandboxMode};
use protocol_core::store::{Instance, Store};
use protocol_core::supervisor::{self, RunConfig};
use protocol_core::types::{Action, ActionKind, Mode, Network, Scenario, Status};

use args::{AaveCmd, Cli, Cmd, CompoundCmd, MorphoCmd, StrategiesCmd};
use clap::{CommandFactory, Parser};
use resolve::{make_strategy, resolve, Resolved, Source, Tier};

macro_rules! outln {
    ($w:expr) => {{ let _ = writeln!($w); }};
    ($w:expr, $($a:tt)*) => {{ let _ = writeln!($w, $($a)*); }};
}

enum E {
    Usage(String),
    Fail(String),
}

impl From<String> for E {
    fn from(s: String) -> E {
        E::Fail(s)
    }
}

impl From<&str> for E {
    fn from(s: &str) -> E {
        E::Fail(s.to_string())
    }
}

type R = Result<i32, E>;

fn usage<T>(m: &str) -> Result<T, E> {
    Err(E::Usage(m.to_string()))
}

pub fn default_home() -> PathBuf {
    if let Ok(h) = std::env::var("PROTOCOL_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return PathBuf::from(h).join(".protocol");
        }
    }
    PathBuf::from(".protocol")
}

pub fn run(argv: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let cli = match Cli::try_parse_from(
        std::iter::once("protocol".to_string()).chain(argv.iter().cloned()),
    ) {
        Ok(c) => c,
        Err(e) => {
            // help and --version print to stdout with exit 0; real usage errors go to stderr, exit 2
            if e.use_stderr() {
                let _ = write!(err, "{e}");
                return 2;
            }
            let _ = write!(out, "{e}");
            return 0;
        }
    };
    let store = Store::new(cli.home.clone().unwrap_or_else(default_home));
    let Some(cmd) = cli.cmd else {
        let _ = write!(out, "{}", Cli::command().render_help());
        return 0;
    };
    let Some(sandbox) = SandboxMode::parse(&cli.sandbox) else {
        outln!(
            err,
            "error: --sandbox must be require or off\n(see `protocol --help`)"
        );
        return 2;
    };
    match dispatch(cmd, sandbox, &store, out) {
        Ok(code) => code,
        Err(E::Usage(m)) => {
            outln!(err, "error: {m}\n(see `protocol --help`)");
            2
        }
        Err(E::Fail(m)) => {
            outln!(err, "error: {m}");
            1
        }
    }
}

fn dispatch(cmd: Cmd, sandbox: SandboxMode, store: &Store, out: &mut dyn Write) -> R {
    match cmd {
        Cmd::Version => {
            outln!(
                out,
                "protocol {} (simulation-only)",
                env!("CARGO_PKG_VERSION")
            );
            Ok(0)
        }
        Cmd::Strategies(sub) => cmd_strategies(sub, store, out),
        Cmd::Create { name, lang, dir } => cmd_create(name, lang, dir, out),
        Cmd::Build { strategy } => cmd_build(strategy, sandbox, store, out),
        Cmd::Test { strategy, ticks } => cmd_test(strategy, ticks, sandbox, store, out),
        Cmd::Simulate {
            strategy,
            ticks,
            scenario,
            mode,
        } => cmd_simulate(strategy, ticks, scenario, mode, sandbox, store, out),
        Cmd::Verify { dir, pin } => cmd_verify(dir, pin, store, out),
        Cmd::Run {
            strategy,
            mode,
            network,
            scenario,
            ticks,
            interval_ms,
            max_failures,
            instance,
        } => cmd_run(
            RunArgs {
                strategy,
                mode,
                network,
                scenario,
                ticks,
                interval_ms,
                max_failures,
                instance,
            },
            sandbox,
            store,
            out,
        ),
        Cmd::Ps => cmd_ps(store, out),
        Cmd::Inspect { instance } => cmd_inspect(instance, store, out),
        Cmd::Pause { instance } => cmd_control(instance, store, out, Control::Pause),
        Cmd::Resume { instance } => cmd_control(instance, store, out, Control::Resume),
        Cmd::Stop { instance } => cmd_control(instance, store, out, Control::Stop),
        Cmd::Pending { instance } => cmd_pending(instance, store, out),
        Cmd::Approve { instance, id } => {
            send_command(store, &instance, &format!("approve {id}"), out)
        }
        Cmd::Reject { instance, id } => {
            send_command(store, &instance, &format!("reject {id}"), out)
        }
        Cmd::Act {
            instance,
            kind,
            asset,
            amount,
            protocol,
        } => cmd_act(instance, kind, asset, amount, protocol, store, out),
        Cmd::KillSwitch { which } => cmd_kill_switch(which, store, out),
        Cmd::Aave(AaveCmd::Account { address }) => cmd_aave_account(address, out),
        Cmd::Compound(CompoundCmd::Account { address }) => cmd_compound_account(address, out),
        Cmd::Morpho(MorphoCmd::Account { address }) => cmd_morpho_account(address, out),
        Cmd::Aave(AaveCmd::Preflight {
            action,
            asset,
            amount,
            account,
        }) => cmd_aave_preflight(action, asset, amount, account, out),
        Cmd::Watch {
            strategy,
            account,
            ticks,
            interval_ms,
            on,
            timeout_secs,
            protocol,
        } => cmd_watch(
            WatchArgs {
                protocol,
                strategy,
                account,
                ticks,
                interval_ms,
                on,
                timeout_secs,
            },
            sandbox,
            store,
            out,
        ),
    }
}

struct RunArgs {
    strategy: String,
    mode: Option<String>,
    network: Option<String>,
    scenario: Option<String>,
    ticks: u64,
    interval_ms: u64,
    max_failures: u32,
    instance: Option<String>,
}

// ---- small helpers ---------------------------------------------------------------------

fn parse_mode(v: Option<&str>, default: Mode) -> Result<Mode, E> {
    match v {
        None => Ok(default),
        Some(v) => Mode::parse(v).map_or_else(
            || usage("--mode must be autonomous, approval or manual"),
            Ok,
        ),
    }
}

fn parse_scenario(v: Option<&str>) -> Result<Scenario, E> {
    match v {
        None => Ok(Scenario::Calm),
        Some(v) => {
            Scenario::parse(v).map_or_else(|| usage("--scenario must be calm or stress"), Ok)
        }
    }
}

fn print_permissions(res: &Resolved, out: &mut dyn Write) {
    for l in res.manifest.permission_summary() {
        outln!(out, "  {l}");
    }
}

fn print_trust_banner(res: &Resolved, out: &mut dyn Write) {
    match res.tier {
        Tier::Official => outln!(out, "trust: official built-in strategy"),
        Tier::Verified => outln!(
            out,
            "trust: verified. This exact name, version and content hash was pinned as reviewed."
        ),
        Tier::Unverified => {
            outln!(
                out,
                "WARNING: UNVERIFIED strategy. Nobody has pinned this exact source as reviewed."
            );
            outln!(
                out,
                "  Read the source before running it (see docs/security.md)."
            );
        }
    }
}

fn print_sandbox_line(res: &Resolved, sandbox: SandboxMode, out: &mut dyn Write) {
    if !matches!(res.source, Source::External(_)) {
        return;
    }
    match sandbox {
        SandboxMode::Require => outln!(
            out,
            "sandbox: on (no network, no access to your files, read-only system; needs bubblewrap)"
        ),
        SandboxMode::Off => outln!(
            out,
            "sandbox: OFF. This strategy runs with your user's full file and network access."
        ),
    }
}

fn account_lines(acc: &Account) -> Vec<String> {
    let mut v = Vec::new();
    for (label, map) in [
        ("supplied", &acc.supplied),
        ("borrowed", &acc.borrowed),
        ("wallet", &acc.wallet),
    ] {
        let items: Vec<String> = map
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(a, n)| format!("{a} {}", format_amount(*n)))
            .collect();
        v.push(format!(
            "{label}: {}",
            if items.is_empty() {
                "-".to_string()
            } else {
                items.join(", ")
            }
        ));
    }
    v
}

fn build_engine(store: &Store, meta: &Instance, res: &Resolved) -> Result<Engine, String> {
    let account = store
        .load_account(&meta.name)?
        .unwrap_or_else(AaveSim::demo_account);
    let adapter = Box::new(AaveSim::new(meta.scenario, account));
    let mut e = Engine::new(adapter, res.manifest.policy(), meta.mode, meta.network);
    e.total_moved = meta.total_moved;
    e.next_id = meta.next_id;
    e.pending = store.load_pending(&meta.name)?;
    e.kill_switch = store.kill_switch();
    Ok(e)
}

fn persist_engine(store: &Store, meta: &mut Instance, engine: &mut Engine) -> Result<(), String> {
    meta.total_moved = engine.total_moved;
    meta.next_id = engine.next_id;
    store.save_account(&meta.name, engine.adapter.account())?;
    store.save_pending(&meta.name, &engine.pending)?;
    store.save_meta(meta)?;
    store.append_audit(&meta.name, &engine.drain_audit())
}

// ---- in-memory simulation (used by `simulate` and `test`) ----------------------------------

#[derive(Default)]
struct SimResult {
    executed: u64,
    queued: u64,
    suggested: u64,
    alerts: u64,
    denied: Vec<String>,
    errors: Vec<String>,
    total_moved: u64,
    final_hf: Option<f64>,
    account: Account,
}

fn simulate_core(
    res: &Resolved,
    scenario: Scenario,
    mode: Mode,
    ticks: u64,
    sandbox: SandboxMode,
    emit: &mut dyn FnMut(&str),
) -> Result<SimResult, String> {
    let adapter = Box::new(AaveSim::new(scenario, AaveSim::demo_account()));
    let engine = Engine::new(adapter, res.manifest.policy(), mode, Network::Simulation);
    drive(engine, res, ticks, Duration::ZERO, sandbox, emit)
}

/// Run `ticks` ticks of a strategy against an already built engine (simulated or live
/// read-only), waiting `interval` between ticks.
/// What to do before a tick: run it (with lines to show first) or stop early.
enum Wait {
    Tick(Vec<String>),
    Stop(Vec<String>),
}

fn drive(
    engine: Engine,
    res: &Resolved,
    ticks: u64,
    interval: Duration,
    sandbox: SandboxMode,
    emit: &mut dyn FnMut(&str),
) -> Result<SimResult, String> {
    drive_with(engine, res, ticks, sandbox, emit, &mut |tick| {
        if tick > 1 && !interval.is_zero() {
            std::thread::sleep(interval);
        }
        Ok(Wait::Tick(Vec::new()))
    })
}

/// Run up to `ticks` ticks; `before` decides, before each one, whether and when it runs.
fn drive_with(
    mut engine: Engine,
    res: &Resolved,
    ticks: u64,
    sandbox: SandboxMode,
    emit: &mut dyn FnMut(&str),
    before: &mut dyn FnMut(u64) -> Result<Wait, String>,
) -> Result<SimResult, String> {
    let mut strategy = make_strategy(res, sandbox)?;
    let mut state = BTreeMap::new();
    let mut r = SimResult::default();
    let mut done = 0;
    for tick in 1..=ticks {
        match before(tick)? {
            Wait::Tick(lines) => lines.iter().for_each(|l| emit(l)),
            Wait::Stop(lines) => {
                lines.iter().for_each(|l| emit(l));
                break;
            }
        }
        done = tick;
        let rep = step(
            &mut engine,
            strategy.as_mut(),
            &res.manifest,
            tick,
            &mut state,
        );
        for l in rep.lines() {
            emit(&l);
        }
        if let Some(e) = &rep.error {
            r.errors.push(format!("tick {tick}: {e}"));
        }
        for (_, o) in &rep.results {
            match o {
                Outcome::Executed => r.executed += 1,
                Outcome::Queued(_) => r.queued += 1,
                Outcome::Suggested => r.suggested += 1,
                Outcome::Notified => r.alerts += 1,
                Outcome::Denied(m) => r.denied.push(m.clone()),
                Outcome::Failed(m) => r.denied.push(format!("failed: {m}")),
            }
        }
    }
    r.total_moved = engine.total_moved;
    r.final_hf = engine
        .adapter
        .snapshot(done)
        .ok()
        .and_then(|s| s.health_factor());
    r.account = engine.adapter.account().clone();
    Ok(r)
}

// ---- commands -------------------------------------------------------------------------------

fn cmd_strategies(sub: StrategiesCmd, store: &Store, out: &mut dyn Write) -> R {
    match sub {
        StrategiesCmd::List => {
            outln!(out, "Official strategies (built in, simulation only):");
            for name in protocol_strategies::BUILTIN_NAMES {
                let m = protocol_strategies::manifest(name).expect("built-in manifest");
                let actions: Vec<&str> = m.allowed_actions.iter().map(|a| a.as_str()).collect();
                outln!(
                    out,
                    "  {:<22} {:<6} [{}] {}",
                    m.name,
                    m.version,
                    actions.join(","),
                    m.description
                );
            }
            let local = Path::new("custom-strategies");
            if local.is_dir() {
                let mut dirs: Vec<PathBuf> = std::fs::read_dir(local)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.path())
                            .filter(|p| p.join("strategy.toml").is_file())
                            .collect()
                    })
                    .unwrap_or_default();
                dirs.sort();
                if !dirs.is_empty() {
                    outln!(out, "\nLocal strategies (./custom-strategies):");
                    for d in dirs {
                        match resolve(store, &d.to_string_lossy()) {
                            Ok(r) => outln!(
                                out,
                                "  {:<22} {:<6} {:<7} {}",
                                r.manifest.name,
                                r.manifest.version,
                                r.manifest.language,
                                r.tier.label()
                            ),
                            Err(e) => outln!(out, "  {}: invalid ({e})", d.display()),
                        }
                    }
                }
            }
            Ok(0)
        }
        StrategiesCmd::Info { strategy: name } => {
            let r = resolve(store, &name)?;
            let m = &r.manifest;
            outln!(out, "{} {} [{}]", m.name, m.version, r.tier.label());
            outln!(out, "language: {}", m.language.as_str());
            outln!(out, "entry:    {}", m.entry);
            if !m.description.is_empty() {
                outln!(out, "about:    {}", m.description);
            }
            outln!(out, "permissions:");
            print_permissions(&r, out);
            if m.params.is_empty() {
                outln!(out, "parameters: none");
            } else {
                outln!(out, "parameters:");
                for (k, v) in &m.params {
                    outln!(out, "  {k} = {v}");
                }
            }
            if let Some(h) = &r.hash {
                outln!(out, "content hash: {h}");
            }
            print_trust_banner(&r, out);
            if let Some(doc) =
                protocol_strategies::docs(&m.name).filter(|_| matches!(r.source, Source::Builtin))
            {
                outln!(out, "\n{doc}");
            }
            Ok(0)
        }
    }
}

fn cmd_create(name: String, lang: Option<String>, dir: Option<PathBuf>, out: &mut dyn Write) -> R {
    let Some(lang_s) = lang else {
        return usage("--lang is required (rust, c, cpp, python, java)");
    };
    let Some(lang) = Language::parse(&lang_s) else {
        return usage(&format!(
            "unknown language '{lang_s}' (rust, c, cpp, python, java)"
        ));
    };
    let base = dir.unwrap_or_else(|| PathBuf::from("custom-strategies"));
    if !valid_name(&name) {
        return Err(E::Fail(format!(
            "invalid name '{name}': lowercase letters, digits, '-' or '_', starting with a letter"
        )));
    }
    if protocol_strategies::BUILTIN_NAMES.contains(&name.as_str()) {
        return Err(E::Fail(format!(
            "'{name}' is reserved for an official strategy"
        )));
    }
    let (dir, files) = templates::scaffold(&name, lang, &base)?;
    outln!(
        out,
        "created {} strategy '{name}' in {}",
        lang.as_str(),
        dir.display()
    );
    for f in files {
        outln!(out, "  {f}");
    }
    outln!(
        out,
        "\nIt is alert-only by default (no fund movements). Next:"
    );
    outln!(out, "  protocol build {}", dir.display());
    outln!(out, "  protocol test {}", dir.display());
    outln!(out, "  protocol simulate {}", dir.display());
    Ok(0)
}

fn cmd_build(name: String, sandbox: SandboxMode, store: &Store, out: &mut dyn Write) -> R {
    let r = resolve(store, &name)?;
    let Source::External(dir) = &r.source else {
        outln!(
            out,
            "'{}' is a built-in strategy; nothing to build.",
            r.manifest.name
        );
        return Ok(0);
    };
    let cmd = templates::build_command(r.manifest.language);
    outln!(
        out,
        "building {} ({}) in {}",
        r.manifest.name,
        r.manifest.language.as_str(),
        dir.display()
    );
    outln!(out, "$ {cmd}");
    // Keeps the sandbox (and its seccomp filter file) alive until the build has finished.
    let mut _sandbox_guard: Option<sandbox::Sandbox> = None;
    let mut command = match sandbox {
        SandboxMode::Off => {
            outln!(
                out,
                "sandbox: OFF. The build runs with your user's full file and network access; \
only build code you trust."
            );
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd).current_dir(dir);
            c
        }
        SandboxMode::Require => {
            let canon = dir
                .canonicalize()
                .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let cargo = std::env::var_os("CARGO_HOME").map(PathBuf::from);
            let rustup = std::env::var_os("RUSTUP_HOME").map(PathBuf::from);
            let be =
                sandbox::build_environment(home.as_deref(), cargo.as_deref(), rustup.as_deref());
            let sb = sandbox::prepare_build(sandbox, &canon, &be)
                .map_err(E::Fail)?
                .ok_or_else(|| E::Fail("sandbox unavailable".into()))?;
            outln!(
                out,
                "sandbox: on (network allowed for dependencies; no access to your files except this \
project directory; needs bubblewrap)"
            );
            let mut c = sb.command();
            c.args(sandbox::bwrap_build_args(&canon, &be))
                .args(["--", "/bin/sh", "-c", cmd]);
            _sandbox_guard = Some(sb);
            c
        }
    };
    let output = command
        .output()
        .map_err(|e| format!("cannot run build: {e}"))?;
    let _ = out.write_all(&output.stdout);
    let _ = out.write_all(&output.stderr);
    if !output.status.success() {
        return Err(E::Fail(format!("build failed ({})", output.status)));
    }
    outln!(out, "build ok");
    // Hash is computed over source only (build output directories are excluded).
    if let Some(h) = &r.hash {
        outln!(out, "source content hash: {h}");
    }
    Ok(0)
}

fn cmd_test(
    name: String,
    ticks: u64,
    sandbox: SandboxMode,
    store: &Store,
    out: &mut dyn Write,
) -> R {
    if ticks == 0 {
        return usage("--ticks must be at least 1");
    }
    let r = resolve(store, &name)?;
    outln!(
        out,
        "testing {} {} over {ticks} ticks per scenario (simulation, autonomous mode)",
        r.manifest.name,
        r.manifest.version
    );
    let mut failed = false;
    for scenario in [Scenario::Calm, Scenario::Stress] {
        match simulate_core(&r, scenario, Mode::Autonomous, ticks, sandbox, &mut |_| {}) {
            Err(e) => {
                outln!(out, "  [{scenario}] FAIL: cannot run strategy: {e}");
                failed = true;
            }
            Ok(s) => {
                let verdict = if s.errors.is_empty() { "ok" } else { "FAIL" };
                outln!(
                    out,
                    "  [{scenario}] {verdict}: {} executed, {} alerts, {} denied by risk engine, {} errors",
                    s.executed,
                    s.alerts,
                    s.denied.len(),
                    s.errors.len()
                );
                for e in s.errors.iter().take(5) {
                    outln!(out, "      error: {e}");
                    failed = true;
                }
                let mut reasons: Vec<&String> = s.denied.iter().collect();
                reasons.dedup();
                for d in reasons.iter().take(3) {
                    outln!(out, "      note: proposal denied: {d}");
                }
            }
        }
    }
    outln!(
        out,
        "{}",
        if failed {
            "RESULT: FAIL"
        } else {
            "RESULT: PASS"
        }
    );
    outln!(
        out,
        "Passing only means the strategy ran without errors. It says nothing about whether it is"
    );
    outln!(
        out,
        "profitable or safe, and risk-engine denials above show where it exceeds its permissions."
    );
    Ok(if failed { 1 } else { 0 })
}

fn cmd_simulate(
    name: String,
    ticks: u64,
    scenario: Option<String>,
    mode: Option<String>,
    sandbox: SandboxMode,
    store: &Store,
    out: &mut dyn Write,
) -> R {
    let scenario = parse_scenario(scenario.as_deref())?;
    let mode = parse_mode(mode.as_deref(), Mode::Autonomous)?;
    let r = resolve(store, &name)?;
    outln!(
        out,
        "simulating {} ({scenario} scenario, {mode} mode, {ticks} ticks). Nothing is saved.",
        r.manifest.name
    );
    print_trust_banner(&r, out);
    print_sandbox_line(&r, sandbox, out);
    let s = simulate_core(&r, scenario, mode, ticks, sandbox, &mut |l| {
        outln!(out, "{l}")
    })?;
    outln!(
        out,
        "\nsummary: {} executed, {} queued, {} suggested, {} alerts, {} denied, {} errors",
        s.executed,
        s.queued,
        s.suggested,
        s.alerts,
        s.denied.len(),
        s.errors.len()
    );
    outln!(
        out,
        "funds moved (simulated): {}",
        format_amount(s.total_moved)
    );
    match s.final_hf {
        Some(hf) => outln!(out, "final health factor: {hf:.3}"),
        None => outln!(out, "final health factor: n/a (no debt)"),
    }
    for l in account_lines(&s.account) {
        outln!(out, "  {l}");
    }
    outln!(
        out,
        "Simulated results are not predictions and do not show a strategy is profitable or safe."
    );
    Ok(if s.errors.is_empty() { 0 } else { 1 })
}

fn cmd_verify(name: String, pin: bool, store: &Store, out: &mut dyn Write) -> R {
    let r = resolve(store, &name)?;
    if matches!(r.source, Source::Builtin) {
        outln!(out, "'{}' is an official built-in strategy; it is part of the project source and needs no pin.", r.manifest.name);
        return Ok(0);
    }
    let hash = r.hash.clone().unwrap_or_default();
    outln!(out, "{} {}", r.manifest.name, r.manifest.version);
    outln!(out, "content hash (source files only): {hash}");
    outln!(out, "declared permissions:");
    print_permissions(&r, out);
    if pin {
        store.pin(&r.manifest.name, &r.manifest.version, &hash)?;
        outln!(
            out,
            "pinned as verified. Pin only after reading the source AND these permissions."
        );
        outln!(out, "'verified' means reviewed, not safe or profitable. Any change to the source or version un-verifies it.");
    } else if r.tier == Tier::Verified {
        outln!(out, "status: verified (this exact source is pinned)");
    } else {
        outln!(
            out,
            "status: UNVERIFIED. After review, run: protocol verify {name} --pin"
        );
    }
    Ok(0)
}

fn cmd_run(a: RunArgs, sandbox: SandboxMode, store: &Store, out: &mut dyn Write) -> R {
    let mode = parse_mode(a.mode.as_deref(), Mode::Approval)?;
    let network = match a.network {
        None => Network::Simulation,
        Some(v) => Network::parse(&v).map_or_else(
            || usage("--network must be simulation, testnet or mainnet"),
            Ok,
        )?,
    };
    let scenario = parse_scenario(a.scenario.as_deref())?;
    let (ticks, interval_ms, max_failures) = (a.ticks, a.interval_ms, a.max_failures);
    let instance = a.instance;
    let reference = a.strategy;

    match network {
        Network::Simulation => {}
        Network::Testnet => {
            return Err(E::Fail("testnet execution is not implemented in this release; use --network simulation".into()))
        }
        Network::Mainnet => {
            return Err(E::Fail("mainnet execution is disabled; real-asset execution stays off until audits and reviews are done".into()))
        }
    }

    let res = resolve(store, &reference)?;
    let name = instance.unwrap_or_else(|| res.manifest.name.clone());
    if !valid_name(&name) {
        return Err(E::Fail(format!("invalid instance name '{name}'")));
    }

    outln!(
        out,
        "strategy {} {} ({})",
        res.manifest.name,
        res.manifest.version,
        res.tier.label()
    );
    print_trust_banner(&res, out);
    print_sandbox_line(&res, sandbox, out);
    outln!(out, "permissions (enforced by the risk engine):");
    print_permissions(&res, out);

    // The lock (released by the OS even if a run is killed) decides whether the instance is
    // running; the status file may be stale after a crash. Held until this function returns.
    let Some(_instance_lock) = store.lock_instance_retry(&name)? else {
        return Err(E::Fail(format!(
            "instance '{name}' is already running (another process holds its lock)"
        )));
    };
    if matches!(store.read_status(&name), Status::Running | Status::Paused) {
        outln!(
            out,
            "note: the previous run of '{name}' did not shut down cleanly (its status was still '{}'); starting a fresh run",
            store.read_status(&name)
        );
    }

    let mut meta = if store.exists(&name) {
        let mut m = store.load_meta(&name)?;
        if m.strategy_ref != res.strategy_ref() {
            return Err(E::Fail(format!(
                "instance '{name}' belongs to a different strategy ({}); pick another --instance name",
                m.strategy_ref
            )));
        }
        m.mode = mode;
        m.network = network;
        m.scenario = scenario;
        store.save_meta(&m)?;
        outln!(out, "continuing instance '{name}' from tick {}", m.tick);
        m
    } else {
        let m = Instance::new(&name, &res.strategy_ref(), mode, network, scenario);
        store.create(&m)?;
        m
    };

    let mut engine = build_engine(store, &meta, &res)?;
    let factory = || make_strategy(&res, sandbox);
    let cfg = RunConfig {
        ticks,
        interval: Duration::from_millis(interval_ms),
        max_failures,
        max_idle_polls: 0,
    };
    outln!(
        out,
        "running instance '{name}': {mode} mode, {scenario} scenario, {} (simulation only)",
        if ticks == 0 {
            "until stopped".to_string()
        } else {
            format!("{ticks} ticks")
        }
    );
    let summary = supervisor::run(
        store,
        &mut meta,
        &mut engine,
        &res.manifest,
        &factory,
        &cfg,
        &mut |l| outln!(out, "{l}"),
    )?;
    let status = summary.final_status.unwrap_or(Status::Completed);
    outln!(
        out,
        "\ninstance '{name}' {status}: {} ticks, {} executed, {} queued, {} suggested, {} alerts, {} denied, {} failures",
        summary.ticks_run, summary.executed, summary.queued, summary.suggested, summary.alerts, summary.denied, summary.failures
    );
    if !engine.pending.is_empty() {
        outln!(
            out,
            "{} action(s) await approval: protocol pending {name}",
            engine.pending.len()
        );
    }
    Ok(if status == Status::Failed { 1 } else { 0 })
}

fn cmd_ps(store: &Store, out: &mut dyn Write) -> R {
    let names = store.list();
    if names.is_empty() {
        outln!(out, "no instances. Start one with: protocol run <strategy>");
        return Ok(0);
    }
    outln!(
        out,
        "{:<24} {:<10} {:<11} {:>5} {:>8}  {}",
        "NAME",
        "STATUS",
        "MODE",
        "TICK",
        "PENDING",
        "STRATEGY"
    );
    for n in names {
        match store.load_meta(&n) {
            Ok(m) => {
                let pending = store.load_pending(&n).map(|p| p.len()).unwrap_or(0);
                let strat = Path::new(&m.strategy_ref)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| m.strategy_ref.clone());
                outln!(
                    out,
                    "{:<24} {:<10} {:<11} {:>5} {:>8}  {}",
                    n,
                    display_status(store, &n),
                    m.mode,
                    m.tick,
                    pending,
                    strat
                );
            }
            Err(e) => outln!(out, "{n:<24} unreadable: {e}"),
        }
    }
    if store
        .list()
        .iter()
        .any(|n| display_status(store, n) == "stale")
    {
        outln!(out, "\nstale = the run process is gone (crashed or killed). Run it again, or `protocol stop <name>` to clear.");
    }
    Ok(0)
}

/// Status as the user should see it: `running`/`paused` only while a process really holds the
/// instance lock, otherwise `stale`.
fn display_status(store: &Store, name: &str) -> String {
    let s = store.read_status(name);
    if matches!(s, Status::Running | Status::Paused) && !store.is_running(name) {
        "stale".to_string()
    } else {
        s.to_string()
    }
}

fn cmd_inspect(name: String, store: &Store, out: &mut dyn Write) -> R {
    let meta = store.load_meta(&name)?;
    outln!(out, "instance:   {name}");
    outln!(out, "strategy:   {}", meta.strategy_ref);
    let shown = display_status(store, &name);
    if shown == "stale" {
        outln!(
            out,
            "status:     stale (last recorded '{}', but no process is running it)",
            store.read_status(&name)
        );
    } else {
        outln!(out, "status:     {shown}");
    }
    outln!(
        out,
        "mode:       {}   network: {}   scenario: {}",
        meta.mode,
        meta.network,
        meta.scenario
    );
    outln!(
        out,
        "tick:       {}   restarts: {}",
        meta.tick,
        meta.restarts
    );
    outln!(
        out,
        "funds moved (simulated): {}",
        format_amount(meta.total_moved)
    );
    if !meta.last_error.is_empty() {
        outln!(out, "last error: {}", meta.last_error);
    }
    if store.kill_switch() {
        outln!(out, "KILL SWITCH IS ON: fund-moving actions are blocked");
    }
    let account = store
        .load_account(&name)?
        .unwrap_or_else(AaveSim::demo_account);
    let sim = AaveSim::new(meta.scenario, account.clone());
    if let Ok(snap) = sim.snapshot(meta.tick.max(1)) {
        match snap.health_factor() {
            Some(hf) => outln!(out, "health factor (at tick {}): {hf:.3}", meta.tick.max(1)),
            None => outln!(out, "health factor: n/a (no debt)"),
        }
    }
    outln!(out, "account:");
    for l in account_lines(&account) {
        outln!(out, "  {l}");
    }
    let pending = store.load_pending(&name)?;
    outln!(out, "pending approvals: {}", pending.len());
    for p in &pending {
        outln!(out, "  #{} (tick {}) {}", p.id, p.tick, p.action);
    }
    let state = store.load_strategy_state(&name)?;
    if !state.is_empty() {
        outln!(out, "strategy state:");
        for (k, v) in &state {
            outln!(out, "  {k} = {v}");
        }
    }
    let audit = store.read_audit(&name)?;
    outln!(out, "recent audit log ({} entries total):", audit.len());
    for l in audit.iter().rev().take(10).rev() {
        outln!(out, "  {l}");
    }
    Ok(0)
}

enum Control {
    Pause,
    Resume,
    Stop,
}

fn cmd_control(name: String, store: &Store, out: &mut dyn Write, what: Control) -> R {
    let meta = store.load_meta(&name)?;
    let current = store.read_status(&name);
    let (target, event) = match what {
        Control::Pause => {
            if current != Status::Running {
                return Err(E::Fail(format!(
                    "cannot pause: instance is {current}, not running"
                )));
            }
            if !store.is_running(&name) {
                return Err(E::Fail(format!(
                    "cannot pause: instance is not running (stale status from a run that died); use `protocol stop {name}` to clear it"
                )));
            }
            (Status::Paused, "paused")
        }
        Control::Resume => {
            if current != Status::Paused {
                return Err(E::Fail(format!(
                    "cannot resume: instance is {current}, not paused"
                )));
            }
            if !store.is_running(&name) {
                return Err(E::Fail(format!(
                    "cannot resume: instance is not running (stale status from a run that died); run it again or `protocol stop {name}`"
                )));
            }
            (Status::Running, "resumed")
        }
        Control::Stop => {
            if matches!(
                current,
                Status::Stopped | Status::Completed | Status::Failed
            ) {
                outln!(out, "instance '{name}' is already {current}");
                return Ok(0);
            }
            if !store.is_running(&name) {
                store.write_status(&name, Status::Stopped)?;
                store.append_audit(
                    &name,
                    &[format!(
                        "tick={} event=stopped by=user note=cleared stale status",
                        meta.tick
                    )],
                )?;
                outln!(out, "instance '{name}' had a stale '{current}' status (its process is gone); marked stopped");
                return Ok(0);
            }
            (Status::Stopped, "stopped")
        }
    };
    store.write_status(&name, target)?;
    store.append_audit(
        &name,
        &[format!("tick={} event={event} by=user", meta.tick)],
    )?;
    outln!(out, "instance '{name}' {event}");
    Ok(0)
}

fn cmd_pending(name: String, store: &Store, out: &mut dyn Write) -> R {
    store.load_meta(&name)?;
    let pending = store.load_pending(&name)?;
    if pending.is_empty() {
        outln!(out, "no actions awaiting approval");
        return Ok(0);
    }
    outln!(
        out,
        "{} action(s) awaiting approval (risk is re-checked at approval time):",
        pending.len()
    );
    for p in &pending {
        outln!(
            out,
            "  #{} (tick {}) {}  {}",
            p.id,
            p.tick,
            p.action,
            p.action.note
        );
    }
    outln!(
        out,
        "approve with: protocol approve {name} <id>   reject with: protocol reject {name} <id>"
    );
    Ok(0)
}

/// Queue a command for the instance. A live instance applies it at its next poll; otherwise
/// it is applied right here against the persisted state.
fn send_command(store: &Store, name: &str, line: &str, out: &mut dyn Write) -> R {
    let meta = store.load_meta(name)?;
    store.push_command(name, line)?;
    // If we can take the instance lock nobody is running it, so apply the command here (holding
    // the lock so a `run` cannot start halfway through). Otherwise a live run picks it up.
    match store.lock_instance_retry(name)? {
        None => {
            outln!(
                out,
                "sent to instance '{name}'. It applies commands at its next tick or poll."
            );
            outln!(out, "check the result with: protocol inspect {name}");
        }
        Some(_lock) => {
            let res = resolve(store, &meta.strategy_ref)?;
            let mut engine = build_engine(store, &meta, &res)?;
            supervisor::apply_commands(store, &meta, &mut engine, &mut |l| outln!(out, "{l}"))?;
            let mut meta = meta;
            persist_engine(store, &mut meta, &mut engine)?;
        }
    }
    Ok(0)
}

fn cmd_act(
    name: String,
    kind_s: Option<String>,
    asset: Option<String>,
    amount_s: Option<String>,
    protocol: Option<String>,
    store: &Store,
    out: &mut dyn Write,
) -> R {
    let (Some(kind_s), Some(asset), Some(amount_s)) = (kind_s, asset, amount_s) else {
        return usage("act needs --kind, --asset and --amount");
    };
    let kind = match ActionKind::parse(&kind_s) {
        Some(k) if k.moves_funds() => k,
        _ => return usage("--kind must be supply, withdraw or repay"),
    };
    let amount = parse_amount(&amount_s)?;
    let meta = store.load_meta(&name)?;
    let res = resolve(store, &meta.strategy_ref)?;
    let protocol = protocol.unwrap_or_else(|| res.manifest.protocols[0].clone());
    let action = Action::new(
        kind,
        &protocol.to_ascii_lowercase(),
        &asset.to_ascii_uppercase(),
        amount,
    );
    send_command(store, &name, &format!("act {}", action.to_line()), out)
}

fn cmd_kill_switch(which: String, store: &Store, out: &mut dyn Write) -> R {
    match which.as_str() {
        "on" => {
            store.set_kill_switch(true)?;
            outln!(
                out,
                "kill switch ON: all fund-moving actions are blocked; alerts still work"
            );
        }
        "off" => {
            store.set_kill_switch(false)?;
            outln!(out, "kill switch off");
        }
        "status" => outln!(
            out,
            "kill switch is {}",
            if store.kill_switch() { "ON" } else { "off" }
        ),
        _ => return usage("kill-switch takes on, off or status"),
    }
    Ok(0)
}

fn env_required(name: &str) -> Result<String, E> {
    // never echo the value: the RPC URL carries an API key
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(E::Fail(format!(
            "{name} is not set (see `protocol aave account --help`)"
        ))),
    }
}

/// Connect to the testnet node and Pool named by the environment (read-only).
fn aave_reader_from_env() -> Result<AaveReader<impl EthCall>, E> {
    let url = env_required(protocol_rpc::ENV_RPC_URL)?;
    let pool_text = env_required(protocol_rpc::ENV_POOL)?;
    let pool = Address::parse(&pool_text)
        .map_err(|e| E::Fail(format!("{}: {e}", protocol_rpc::ENV_POOL)))?;
    let transport = protocol_rpc::connect(&url).map_err(|e| E::Fail(e.to_string()))?;
    Ok(AaveReader::new(transport, pool))
}

struct WatchArgs {
    protocol: String,
    strategy: String,
    account: String,
    ticks: u64,
    interval_ms: u64,
    on: String,
    timeout_secs: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Interval,
    Block,
    Event,
}

fn parse_trigger(s: &str) -> Option<Trigger> {
    match s.trim().to_ascii_lowercase().as_str() {
        "interval" => Some(Trigger::Interval),
        "block" => Some(Trigger::Block),
        "event" => Some(Trigger::Event),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Proto {
    Aave,
    Compound,
    Morpho,
}

fn parse_proto(s: &str) -> Option<Proto> {
    match s.trim().to_ascii_lowercase().as_str() {
        "aave" => Some(Proto::Aave),
        "compound" => Some(Proto::Compound),
        "morpho" => Some(Proto::Morpho),
        _ => None,
    }
}

/// What a watched protocol's data means, for the banner.
fn proto_banner(p: Proto) -> (&'static str, &'static str) {
    match p {
        Proto::Aave => (
            "Aave V3 Sepolia",
            "view: aggregate USD totals (pseudo-asset USD, no per-token rates); health factor follows Aave's definition.",
        ),
        Proto::Compound => (
            "Compound III Sepolia",
            "view: aggregate USD totals (pseudo-asset USD, no per-token rates); health factor = collateral x liquidation factors / debt (Compound's rule; slightly conservative).",
        ),
        Proto::Morpho => (
            "Morpho Blue Sepolia",
            "view: totals in LOAN-TOKEN units scaled to 6 decimals, labelled USD only because the strategy interface has one unit. They are NOT dollars unless the loan token is a dollar stablecoin. Health = max borrow / debt (Morpho's rule; slightly optimistic: interest is not accrued).",
        ),
    }
}

fn watch_adapter(p: Proto, user: Address) -> Result<Box<dyn ProtocolAdapter>, E> {
    match p {
        Proto::Aave => Ok(Box::new(AaveChainRead::new(aave_reader_from_env()?, user)?)),
        Proto::Compound => {
            let url = env_required(protocol_rpc::ENV_RPC_URL)?;
            let comet = Address::parse(&env_required(ENV_COMET)?)
                .map_err(|e| E::Fail(format!("{ENV_COMET}: {e}")))?;
            let t = protocol_rpc::connect(&url).map_err(|e| E::Fail(e.to_string()))?;
            let src = CompoundSource::new(CompoundReader::new(t, comet), user).map_err(E::Fail)?;
            Ok(Box::new(ReadOnlyAdapter::new(src).map_err(E::Fail)?))
        }
        Proto::Morpho => {
            let url = env_required(protocol_rpc::ENV_RPC_URL)?;
            let morpho = Address::parse(&env_required(ENV_MORPHO)?)
                .map_err(|e| E::Fail(format!("{ENV_MORPHO}: {e}")))?;
            let id = MarketId::parse(&env_required(ENV_MORPHO_MARKET)?)
                .map_err(|e| E::Fail(format!("{ENV_MORPHO_MARKET}: {e}")))?;
            let t = protocol_rpc::connect(&url).map_err(|e| E::Fail(e.to_string()))?;
            let src = MorphoSource::new(MorphoReader::new(t, morpho), id, user).map_err(E::Fail)?;
            Ok(Box::new(ReadOnlyAdapter::new(src).map_err(E::Fail)?))
        }
    }
}

/// One look at the chain for the block/event triggers.
struct Look {
    block: u64,
    advanced: bool,
    events: Vec<String>,
    skipped: u64,
}

fn watch_poller(p: Proto, user: Address) -> Result<Box<dyn FnMut() -> Result<Look, String>>, E> {
    if p == Proto::Aave {
        let mut w = AccountWatcher::new(aave_reader_from_env()?, user);
        return Ok(Box::new(move || {
            let p = w.poll().map_err(|e| e.to_string())?;
            Ok(Look {
                block: p.block,
                advanced: p.advanced,
                events: p.events.iter().map(|e| e.describe()).collect(),
                skipped: p.skipped_blocks,
            })
        }));
    }
    // Compound and Morpho: new blocks only (their events are not followed yet)
    let t = protocol_rpc::connect(&env_required(protocol_rpc::ENV_RPC_URL)?)
        .map_err(|e| E::Fail(e.to_string()))?;
    let mut last: Option<u64> = None;
    Ok(Box::new(move || {
        let b = t.block_number().map_err(|e| e.to_string())?;
        let advanced = last.is_none_or(|l| b > l);
        last = Some(last.map_or(b, |l| l.max(b)));
        Ok(Look {
            block: b,
            advanced,
            events: Vec::new(),
            skipped: 0,
        })
    }))
}

fn cmd_watch(a: WatchArgs, sandbox: SandboxMode, store: &Store, out: &mut dyn Write) -> R {
    let WatchArgs {
        protocol,
        strategy: name,
        account,
        ticks,
        interval_ms,
        on,
        timeout_secs,
    } = a;
    // validate all input before anything touches the environment, the network or a strategy
    let user = Address::parse(&account).map_err(|e| E::Fail(e.to_string()))?;
    if ticks == 0 {
        return usage("--ticks must be at least 1");
    }
    if interval_ms < 1_000 {
        return usage("--interval-ms must be at least 1000 (be gentle with RPC providers)");
    }
    let Some(trigger) = parse_trigger(&on) else {
        return usage("--on must be interval, block or event");
    };
    if timeout_secs == 0 {
        return usage("--timeout-secs must be at least 1");
    }
    let Some(proto) = parse_proto(&protocol) else {
        return usage("--protocol must be aave, compound or morpho");
    };
    if proto != Proto::Aave && trigger == Trigger::Event {
        return usage(
            "--on event is only available for aave; use --on block or interval for this protocol",
        );
    }
    let r = resolve(store, &name)?;
    let chain = watch_adapter(proto, user)?;
    let (label, view_line) = proto_banner(proto);
    outln!(
        out,
        "watching {} on {label} (live, read-only): account {}",
        r.manifest.name,
        user.to_hex()
    );
    outln!(
        out,
        "manual mode: fund-moving proposals are only suggested, never executed. Nothing is saved."
    );
    outln!(out, "{view_line}");
    print_trust_banner(&r, out);
    print_sandbox_line(&r, sandbox, out);
    let engine = Engine::new(chain, r.manifest.policy(), Mode::Manual, Network::Testnet);
    let interval = Duration::from_millis(interval_ms);
    let s = if trigger == Trigger::Interval {
        drive(engine, &r, ticks, interval, sandbox, &mut |l| {
            outln!(out, "{l}")
        })?
    } else {
        outln!(
            out,
            "trigger: {} (polling every {} ms; gives up after {} s without one)",
            if trigger == Trigger::Block {
                "each new block"
            } else {
                "an Aave Pool event touching the account (the first tick runs at once)"
            },
            interval_ms,
            timeout_secs
        );
        let mut poll = watch_poller(proto, user)?;
        let timeout = Duration::from_secs(timeout_secs);
        let mut deadline = std::time::Instant::now() + timeout;
        let mut first = true;
        let mut pending: Vec<String> = Vec::new();
        drive_with(
            engine,
            &r,
            ticks,
            sandbox,
            &mut |l| outln!(out, "{l}"),
            &mut |_tick| loop {
                let p = poll()?;
                if p.skipped > 0 {
                    pending.push(format!(
                        "note: {} blocks were not searched for events (gap too large)",
                        p.skipped
                    ));
                }
                pending.extend(p.events.iter().map(|e| format!("event: {e}")));
                let was_first = first;
                first = false;
                let go = if was_first {
                    pending.push(format!("following from block {}", p.block));
                    true
                } else if trigger == Trigger::Block {
                    p.advanced
                } else {
                    !p.events.is_empty()
                };
                if go {
                    if !was_first {
                        pending.push(format!("block {}", p.block));
                    }
                    deadline = std::time::Instant::now() + timeout;
                    return Ok(Wait::Tick(std::mem::take(&mut pending)));
                }
                if std::time::Instant::now() >= deadline {
                    pending.push(format!("no trigger for {timeout_secs} s: stopping"));
                    return Ok(Wait::Stop(std::mem::take(&mut pending)));
                }
                std::thread::sleep(interval);
            },
        )?
    };
    outln!(
        out,
        "\nsummary: {} suggested, {} alerts, {} denied, {} errors (nothing executed: read-only)",
        s.suggested,
        s.alerts,
        s.denied.len(),
        s.errors.len()
    );
    match s.final_hf {
        Some(hf) => outln!(out, "final health factor: {hf:.3}"),
        None => outln!(out, "final health factor: n/a (no debt)"),
    }
    Ok(if s.errors.is_empty() { 0 } else { 1 })
}

fn cmd_aave_account(address: String, out: &mut dyn Write) -> R {
    // validate the input first: a typo must not need the network or any secret
    let user = Address::parse(&address).map_err(|e| E::Fail(e.to_string()))?;
    let reader = aave_reader_from_env()?;
    let data = reader
        .account_data(&user)
        .map_err(|e| E::Fail(e.to_string()))?;
    print_aave_account(out, &user, reader.pool(), &data);
    Ok(0)
}

/// Environment variable naming the Comet (market) contract.
const ENV_COMET: &str = "PROTOCOL_COMPOUND_COMET";

fn cmd_compound_account(address: String, out: &mut dyn Write) -> R {
    // validate the input first: a typo must not need the network or any secret
    let user = Address::parse(&address).map_err(|e| E::Fail(e.to_string()))?;
    let url = env_required(protocol_rpc::ENV_RPC_URL)?;
    let comet_text = env_required(ENV_COMET)?;
    let comet = Address::parse(&comet_text).map_err(|e| E::Fail(format!("{ENV_COMET}: {e}")))?;
    let transport = protocol_rpc::connect(&url).map_err(|e| E::Fail(e.to_string()))?;
    let reader = CompoundReader::new(transport, comet);
    let fail = |e: protocol_adapter_compound::ChainError| E::Fail(e.to_string());
    let cfg = reader.market().map_err(fail)?;
    let pos = reader.position(&cfg, &user).map_err(fail)?;
    let chain = reader.is_liquidatable_on_chain(&user).map_err(fail)?;
    print_compound_account(out, &user, reader.comet(), &cfg, &pos, chain);
    Ok(0)
}

fn print_compound_account(
    out: &mut dyn Write,
    user: &Address,
    comet: &Address,
    cfg: &MarketConfig,
    p: &Position,
    on_chain_liquidatable: bool,
) {
    let usd = |v: u128| v as f64 / 1e8;
    outln!(out, "Compound III account (read-only, testnet)");
    outln!(out, "  address            {}", user.to_hex());
    outln!(out, "  market (comet)     {}", comet.to_hex());
    outln!(out, "  collateral assets  {}", cfg.assets.len());
    outln!(
        out,
        "  collateral         {:.2} USD",
        usd(p.collateral_usd8)
    );
    outln!(out, "  debt               {:.2} USD", usd(p.debt_usd8));
    outln!(
        out,
        "  borrow capacity    {:.2} USD",
        usd(p.borrow_capacity_usd8)
    );
    outln!(
        out,
        "  liquidation limit  {:.2} USD",
        usd(p.liquidation_capacity_usd8)
    );
    match p.health_factor() {
        Some(hf) => outln!(out, "  health factor      {hf:.3}"),
        None => outln!(out, "  health factor      n/a (no debt)"),
    }
    outln!(
        out,
        "  liquidatable       {}",
        if on_chain_liquidatable { "yes" } else { "no" }
    );
    if p.liquidatable() != on_chain_liquidatable {
        outln!(
            out,
            "  note: the contract and this reader disagree (interest accrual between the stored index and now); trust the contract."
        );
    }
}

/// Exact decimal text of a raw token amount (`d` decimals), trailing zeros trimmed.
fn format_units(v: u128, d: u8) -> String {
    let scale = 10u128.pow(u32::from(d));
    let (int, frac) = (v / scale, v % scale);
    if frac == 0 {
        return int.to_string();
    }
    let f = format!("{frac:0>width$}", width = usize::from(d));
    format!("{int}.{}", f.trim_end_matches('0'))
}

const ENV_MORPHO: &str = "PROTOCOL_MORPHO_ADDRESS";
const ENV_MORPHO_MARKET: &str = "PROTOCOL_MORPHO_MARKET_ID";

fn cmd_morpho_account(address: String, out: &mut dyn Write) -> R {
    // validate the input first: a typo must not need the network or any secret
    let user = Address::parse(&address).map_err(|e| E::Fail(e.to_string()))?;
    let url = env_required(protocol_rpc::ENV_RPC_URL)?;
    let morpho_text = env_required(ENV_MORPHO)?;
    let morpho = Address::parse(&morpho_text).map_err(|e| E::Fail(format!("{ENV_MORPHO}: {e}")))?;
    let id_text = env_required(ENV_MORPHO_MARKET)?;
    let id = MarketId::parse(&id_text).map_err(|e| E::Fail(format!("{ENV_MORPHO_MARKET}: {e}")))?;
    let transport = protocol_rpc::connect(&url).map_err(|e| E::Fail(e.to_string()))?;
    let reader = MorphoReader::new(transport, morpho);
    let fail = |e: protocol_adapter_morpho::ChainError| E::Fail(e.to_string());
    let params = reader.params(&id).map_err(fail)?;
    let loan_dec = reader.decimals(&params.loan_token).map_err(fail)?;
    let coll_dec = reader.decimals(&params.collateral_token).map_err(fail)?;
    let pos = reader.position(&id, &params, &user).map_err(fail)?;
    let amt = format_units;
    outln!(out, "Morpho Blue account (read-only, testnet)");
    outln!(out, "  address            {}", user.to_hex());
    outln!(out, "  morpho             {}", reader.morpho().to_hex());
    outln!(out, "  market id          {}", id.to_hex());
    outln!(out, "  loan token         {}", params.loan_token.to_hex());
    outln!(
        out,
        "  collateral token   {}",
        params.collateral_token.to_hex()
    );
    outln!(
        out,
        "  liquidation LLTV   {:.2}%",
        params.lltv as f64 / 1e16
    );
    outln!(
        out,
        "  collateral         {} (collateral token)",
        amt(pos.raw.collateral, coll_dec)
    );
    outln!(
        out,
        "  collateral value   {} (loan token)",
        amt(pos.collateral_value, loan_dec)
    );
    outln!(
        out,
        "  debt               {} (loan token)",
        amt(pos.borrowed_assets, loan_dec)
    );
    outln!(
        out,
        "  max borrow         {} (loan token)",
        amt(pos.max_borrow, loan_dec)
    );
    outln!(
        out,
        "  supplied           {} (loan token)",
        amt(pos.supplied_assets, loan_dec)
    );
    match pos.health_factor() {
        Some(hf) => outln!(out, "  health factor      {hf:.3}"),
        None => outln!(out, "  health factor      n/a (no debt)"),
    }
    outln!(
        out,
        "  liquidatable       {}",
        if pos.healthy() { "no" } else { "yes" }
    );
    Ok(0)
}

fn cmd_aave_preflight(
    action: String,
    asset: String,
    amount: String,
    account: String,
    out: &mut dyn Write,
) -> R {
    // validate everything that needs no network first: a typo must not need a key or a node
    let Some(op) = simulate::Operation::parse(&action) else {
        return usage("action must be one of: supply, withdraw, borrow, repay");
    };
    let asset = Address::parse(&asset).map_err(|e| E::Fail(format!("asset: {e}")))?;
    let account = Address::parse(&account).map_err(|e| E::Fail(format!("account: {e}")))?;
    simulate::check_amount_syntax(&amount).map_err(E::Fail)?;

    let reader = aave_reader_from_env()?;
    let decimals =
        simulate::token_decimals(&reader, &asset).map_err(|e| E::Fail(format!("asset: {e}")))?;
    let raw = simulate::parse_amount(&amount, decimals).map_err(E::Fail)?;

    outln!(
        out,
        "Aave V3 preflight (eth_call at the latest block; nothing is signed or sent)"
    );
    outln!(
        out,
        "  action    {} {} ({} decimals)",
        op.as_str(),
        amount,
        decimals
    );
    outln!(out, "  asset     {}", asset.to_hex());
    outln!(out, "  account   {}", account.to_hex());
    outln!(out, "  pool      {}", reader.pool().to_hex());
    let outcome = simulate::simulate(&reader, op, &asset, raw, &account)
        .map_err(|e| E::Fail(e.to_string()))?;
    match outcome {
        simulate::Outcome::WouldSucceed { .. } => {
            outln!(
                out,
                "\nWOULD SUCCEED: the Pool accepts this call right now."
            );
            outln!(
                out,
                "This is a simulation, not a guarantee: state can change before a real transaction, \
and it says nothing about whether the action is a good idea."
            );
            Ok(0)
        }
        simulate::Outcome::WouldRevert { reason } => {
            outln!(out, "\nWOULD REVERT: {reason}");
            outln!(
                out,
                "Common causes: not enough tokens or allowance to the Pool (supply, repay), not \
enough collateral or liquidity (withdraw, borrow), or a paused or capped market. Aave's own \
error codes are explained in its documentation."
            );
            Ok(1)
        }
    }
}

fn print_aave_account(out: &mut dyn Write, user: &Address, pool: &Address, d: &AccountData) {
    outln!(out, "Aave V3 account (read-only, testnet)");
    outln!(out, "  address            {}", user.to_hex());
    outln!(out, "  pool               {}", pool.to_hex());
    outln!(
        out,
        "  collateral         {:.2} USD",
        d.total_collateral_usd()
    );
    outln!(out, "  debt               {:.2} USD", d.total_debt_usd());
    outln!(
        out,
        "  available borrows  {:.2} USD",
        d.available_borrows_usd()
    );
    outln!(
        out,
        "  liquidation limit  {:.2}%",
        d.liquidation_threshold_percent()
    );
    outln!(out, "  ltv                {:.2}%", d.ltv_percent());
    match d.health_factor_f64() {
        Some(hf) => outln!(out, "  health factor      {hf:.3}"),
        None => outln!(out, "  health factor      n/a (no debt)"),
    }
}

#[cfg(test)]
mod aave_tests {
    use super::*;
    use protocol_adapter_aave::onchain::decode_account_data;

    #[test]
    fn format_units_is_exact() {
        assert_eq!(format_units(0, 6), "0");
        assert_eq!(format_units(1_500_000, 6), "1.5");
        assert_eq!(format_units(1, 18), "0.000000000000000001");
        assert_eq!(format_units(7, 0), "7");
        assert_eq!(
            format_units(u128::MAX, 36),
            "340.282366920938463463374607431768211455"
        );
    }

    fn words(w: [u128; 6]) -> String {
        format!("0x{}", w.map(|n| format!("{n:064x}")).concat())
    }

    fn render(reply: &str) -> String {
        let d = decode_account_data(reply).unwrap();
        let user = Address::parse("0x1111111111111111111111111111111111111111").unwrap();
        let pool = Address::parse("0x2222222222222222222222222222222222222222").unwrap();
        let mut out = Vec::new();
        print_aave_account(&mut out, &user, &pool, &d);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn report_shows_usd_percent_and_health_factor() {
        let text = render(&words([
            150_025_000_000,
            40_000_000_000,
            5_000_000_000,
            8250,
            8000,
            2_500_000_000_000_000_000,
        ]));
        assert!(text.contains("collateral         1500.25 USD"), "{text}");
        assert!(text.contains("debt               400.00 USD"), "{text}");
        assert!(text.contains("available borrows  50.00 USD"), "{text}");
        assert!(text.contains("liquidation limit  82.50%"), "{text}");
        assert!(text.contains("ltv                80.00%"), "{text}");
        assert!(text.contains("health factor      2.500"), "{text}");
    }

    #[test]
    fn report_says_no_debt_instead_of_a_huge_number() {
        let w = |n: u128| format!("{n:064x}");
        let reply = format!(
            "0x{}{}{}{}{}{}",
            w(1_000),
            w(0),
            w(800),
            w(8250),
            w(8000),
            "f".repeat(64)
        );
        let text = render(&reply);
        assert!(text.contains("health factor      n/a (no debt)"), "{text}");
    }
}
