//! End-to-end CLI tests. They call `protocol_cli::run` in-process with a private `--home`
//! per test, and use the real filesystem and real child processes for external strategies.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn unique(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    // Under the build's own tmp dir, not /tmp: bubblewrap mounts its own tmpfs on /tmp, and a
    // strategy directory inside /tmp may not be bindable into the sandbox.
    let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("proto-cli-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

struct Cli {
    home: PathBuf,
    work: PathBuf,
}

struct Out {
    code: i32,
    out: String,
    err: String,
}

impl Cli {
    fn new(tag: &str) -> Cli {
        Cli {
            home: unique(&format!("{tag}-home")),
            work: unique(&format!("{tag}-work")),
        }
    }

    fn run(&self, args: &[&str]) -> Out {
        // Most tests exercise strategy logic, not isolation; the sandbox has its own tests below.
        self.run_with(&["--sandbox", "off"], args)
    }

    fn run_with(&self, pre: &[&str], args: &[&str]) -> Out {
        let mut argv: Vec<String> = vec!["--home".into(), self.home.to_string_lossy().to_string()];
        argv.extend(pre.iter().map(|s| s.to_string()));
        argv.extend(args.iter().map(|s| s.to_string()));
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = protocol_cli::run(&argv, &mut o, &mut e);
        Out {
            code,
            out: String::from_utf8_lossy(&o).to_string(),
            err: String::from_utf8_lossy(&e).to_string(),
        }
    }

    fn ok(&self, args: &[&str]) -> String {
        let r = self.run(args);
        assert_eq!(r.code, 0, "`{}` failed: {}{}", args.join(" "), r.out, r.err);
        r.out
    }

    fn dir(&self, name: &str) -> String {
        self.work.join(name).to_string_lossy().to_string()
    }

    /// Write a shell-script strategy directory.
    fn script_strategy(&self, name: &str, manifest_extra: &str, script: &str) -> String {
        let d = self.work.join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(
            d.join("strategy.toml"),
            format!(
                "name = \"{name}\"\nversion = \"1.0.0\"\nlanguage = \"c\"\nentry = \"sh strategy.sh\"\nprotocols = [\"aave\"]\n{manifest_extra}\n"
            ),
        )
        .unwrap();
        fs::write(d.join("strategy.sh"), script).unwrap();
        d.to_string_lossy().to_string()
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
        let _ = fs::remove_dir_all(&self.work);
    }
}

const SUPPLIER: &str = "cat >/dev/null\necho 'PROPOSE supply aave USDC 50000000 test'\necho DONE\n";
const SUPPLIER_MANIFEST: &str =
    "assets = [\"USDC\"]\nallowed_actions = [\"supply\", \"alert\"]\nmax_amount_per_action = \"100\"\nmax_total_exposure = \"120\"\n";

#[test]
fn help_version_and_usage_errors() {
    let c = Cli::new("basic");
    assert!(c.ok(&["help"]).contains("Usage"));
    assert!(c.ok(&[]).contains("Usage"));
    assert!(c.ok(&["version"]).contains("simulation-only"));
    let r = c.run(&["frobnicate"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("frobnicate"));
    assert!(c.ok(&["--version"]).contains("protocol"));
    assert_eq!(c.run(&["run"]).code, 2, "missing strategy is a usage error");
    assert_eq!(c.run(&["test", "stable-lending", "--ticks", "abc"]).code, 2);
    assert_eq!(c.run(&["test", "stable-lending", "--bogus"]).code, 2);
    let r = c.run(&["strategies", "info", "does-not-exist"]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("unknown strategy"));
}

#[test]
fn lists_and_describes_official_strategies() {
    let c = Cli::new("list");
    let out = c.ok(&["strategies", "list"]);
    for n in ["stable-lending", "health-factor-monitor", "rate-monitor"] {
        assert!(out.contains(n), "{n} missing from list");
    }
    let info = c.ok(&["strategies", "info", "stable-lending"]);
    assert!(info.contains("official"));
    assert!(info.contains("caps:      100 per action, 500 total"));
    assert!(info.contains("min_apy_bps = 300"));
    assert!(info.contains("Risks"), "docs are shown for built-ins");
    let monitor = c.ok(&["strategies", "info", "health-factor-monitor"]);
    assert!(monitor.contains("cannot move funds"));
}

#[test]
fn test_command_passes_for_official_strategies() {
    let c = Cli::new("test");
    for n in ["stable-lending", "health-factor-monitor", "rate-monitor"] {
        let out = c.ok(&["test", n]);
        assert!(out.contains("RESULT: PASS"), "{n}: {out}");
        assert!(out.contains("[calm]") && out.contains("[stress]"));
        assert!(out.contains("does not mean") || out.contains("says nothing about whether it is"));
    }
}

#[test]
fn test_command_reports_denials_and_fails_on_errors() {
    let c = Cli::new("test-fail");
    // proposes a supply but only declared alert: risk engine denies, test still passes
    let over = c.script_strategy("overreach", "", SUPPLIER);
    let out = c.ok(&["test", &over, "--ticks", "2"]);
    assert!(out.contains("denied by risk engine"));
    assert!(out.contains("not permitted by the manifest"));
    // crashing strategy fails the test run with exit 1
    let bad = c.script_strategy("crashy", "", "exit 7\n");
    let r = c.run(&["test", &bad, "--ticks", "2"]);
    assert_eq!(r.code, 1);
    assert!(r.out.contains("RESULT: FAIL"));
}

#[test]
fn simulate_stress_shows_alerts_and_final_health_factor() {
    let c = Cli::new("sim");
    let out = c.ok(&[
        "simulate",
        "health-factor-monitor",
        "--scenario",
        "stress",
        "--ticks",
        "13",
    ]);
    assert!(out.contains("WARNING health factor"));
    assert!(out.contains("CRITICAL health factor"));
    assert!(out.contains("final health factor: 0.990"));
    assert!(out.contains("2 alerts"));
    // simulate never writes instance state
    assert!(c.ok(&["ps"]).contains("no instances"));
}

#[test]
fn simulate_modes_differ() {
    let c = Cli::new("sim-modes");
    let auto = c.ok(&[
        "simulate",
        "stable-lending",
        "--ticks",
        "4",
        "--mode",
        "autonomous",
    ]);
    assert!(auto.contains("executed"));
    assert!(auto.contains("funds moved (simulated): 400"));
    let approval = c.ok(&[
        "simulate",
        "stable-lending",
        "--ticks",
        "4",
        "--mode",
        "approval",
    ]);
    assert!(approval.contains("queued for approval"));
    assert!(approval.contains("funds moved (simulated): 0"));
    let manual = c.ok(&[
        "simulate",
        "stable-lending",
        "--ticks",
        "2",
        "--mode",
        "manual",
    ]);
    assert!(manual.contains("suggested only"));
}

#[test]
fn risk_caps_stop_autonomous_supplies() {
    let c = Cli::new("caps");
    // stable-lending caps total exposure at 500: 6 ticks of 100 -> only 5 execute
    let out = c.ok(&[
        "simulate",
        "stable-lending",
        "--ticks",
        "6",
        "--mode",
        "autonomous",
    ]);
    assert!(out.contains("funds moved (simulated): 500"));
    assert!(out.contains("total exposure cap 500 would be exceeded"));
}

#[test]
fn run_approval_flow_end_to_end() {
    let c = Cli::new("approval");
    let out = c.ok(&[
        "run",
        "stable-lending",
        "--ticks",
        "3",
        "--interval-ms",
        "0",
    ]);
    assert!(out.contains("3 action(s) await approval"));
    assert!(c.ok(&["ps"]).contains("completed"));

    let pending = c.ok(&["pending", "stable-lending"]);
    assert!(pending.contains("#1") && pending.contains("#3"));

    let approved = c.ok(&["approve", "stable-lending", "1"]);
    assert!(approved.contains("approve #1: executed"));
    let rejected = c.ok(&["reject", "stable-lending", "2"]);
    assert!(rejected.contains("reject #2: rejected"));

    let inspect = c.ok(&["inspect", "stable-lending"]);
    assert!(inspect.contains("pending approvals: 1"), "{inspect}");
    assert!(inspect.contains("funds moved (simulated): 100"));
    assert!(inspect.contains("event=approved"));
    assert!(inspect.contains("event=rejected"));
    assert!(inspect.contains("supplied: WETH 2, DAI 100") || inspect.contains("DAI 100"));

    // unknown id is reported, not fatal
    assert!(c
        .ok(&["approve", "stable-lending", "99"])
        .contains("no pending action"));
    assert_eq!(c.run(&["approve", "stable-lending", "x"]).code, 2);
    assert_eq!(c.run(&["approve", "ghost", "1"]).code, 1);
}

#[test]
fn rerunning_continues_the_same_instance() {
    let c = Cli::new("continue");
    c.ok(&["run", "rate-monitor", "--ticks", "2", "--interval-ms", "0"]);
    let out = c.ok(&["run", "rate-monitor", "--ticks", "2", "--interval-ms", "0"]);
    assert!(out.contains("continuing instance 'rate-monitor' from tick 2"));
    let inspect = c.ok(&["inspect", "rate-monitor"]);
    assert!(inspect.contains("tick:       4"));
    // a different strategy cannot hijack an existing instance name
    let r = c.run(&[
        "run",
        "stable-lending",
        "--instance",
        "rate-monitor",
        "--ticks",
        "1",
        "--interval-ms",
        "0",
    ]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("different strategy"));
}

#[test]
fn autonomous_manual_act_and_risk_denial() {
    let c = Cli::new("act");
    c.ok(&[
        "run",
        "stable-lending",
        "--mode",
        "manual",
        "--ticks",
        "2",
        "--interval-ms",
        "0",
    ]);
    let inspect = c.ok(&["inspect", "stable-lending"]);
    assert!(
        inspect.contains("funds moved (simulated): 0"),
        "manual mode never moves funds by itself"
    );

    let ok = c.ok(&[
        "act",
        "stable-lending",
        "--kind",
        "supply",
        "--asset",
        "usdc",
        "--amount",
        "40",
    ]);
    assert!(ok.contains("manual supply aave USDC 40: executed"), "{ok}");
    let denied = c.ok(&[
        "act",
        "stable-lending",
        "--kind",
        "supply",
        "--asset",
        "USDC",
        "--amount",
        "101",
    ]);
    assert!(
        denied.contains("DENIED") && denied.contains("per-action cap"),
        "{denied}"
    );
    let wrong_asset = c.ok(&[
        "act",
        "stable-lending",
        "--kind",
        "supply",
        "--asset",
        "WETH",
        "--amount",
        "1",
    ]);
    assert!(
        wrong_asset.contains("DENIED") && wrong_asset.contains("asset"),
        "{wrong_asset}"
    );
    let wrong_kind = c.ok(&[
        "act",
        "stable-lending",
        "--kind",
        "withdraw",
        "--asset",
        "USDC",
        "--amount",
        "1",
    ]);
    assert!(
        wrong_kind.contains("DENIED") && wrong_kind.contains("withdraw"),
        "{wrong_kind}"
    );

    assert_eq!(
        c.run(&[
            "act",
            "stable-lending",
            "--kind",
            "alert",
            "--asset",
            "USDC",
            "--amount",
            "1"
        ])
        .code,
        2
    );
    assert_eq!(
        c.run(&["act", "stable-lending", "--kind", "supply"]).code,
        2
    );
    assert_eq!(
        c.run(&[
            "act",
            "stable-lending",
            "--kind",
            "supply",
            "--asset",
            "USDC",
            "--amount",
            "1.0000001"
        ])
        .code,
        1
    );
}

#[test]
fn kill_switch_blocks_funds_across_commands() {
    let c = Cli::new("kill");
    assert!(c.ok(&["kill-switch", "status"]).contains("off"));
    c.ok(&["kill-switch", "on"]);
    assert!(c.ok(&["kill-switch", "status"]).contains("ON"));
    let out = c.ok(&["simulate", "stable-lending", "--ticks", "2"]);
    // simulate uses an in-memory engine with no kill switch; the persisted instance path honours it
    assert!(out.contains("executed"));
    let run = c.ok(&[
        "run",
        "stable-lending",
        "--mode",
        "autonomous",
        "--ticks",
        "3",
        "--interval-ms",
        "0",
    ]);
    assert!(run.contains("emergency kill switch is on"), "{run}");
    assert!(run.contains("0 executed"));
    assert!(c
        .ok(&["inspect", "stable-lending"])
        .contains("KILL SWITCH IS ON"));
    c.ok(&["kill-switch", "off"]);
    assert_eq!(c.run(&["kill-switch", "maybe"]).code, 2);
}

#[test]
fn mainnet_and_testnet_are_refused() {
    let c = Cli::new("net");
    let m = c.run(&[
        "run",
        "stable-lending",
        "--network",
        "mainnet",
        "--ticks",
        "1",
    ]);
    assert_eq!(m.code, 1);
    assert!(m.err.contains("mainnet execution is disabled"));
    let t = c.run(&[
        "run",
        "stable-lending",
        "--network",
        "testnet",
        "--ticks",
        "1",
    ]);
    assert_eq!(t.code, 1);
    assert!(t.err.contains("not implemented"));
    assert_eq!(
        c.run(&["run", "stable-lending", "--network", "moon"]).code,
        2
    );
    assert!(
        c.ok(&["ps"]).contains("no instances"),
        "refused runs create nothing"
    );
}

#[test]
fn control_commands_validate_state() {
    let c = Cli::new("control");
    assert_eq!(c.run(&["pause", "ghost"]).code, 1);
    c.ok(&["run", "rate-monitor", "--ticks", "1", "--interval-ms", "0"]);
    // finished instance: cannot pause or resume, stop is a no-op
    let p = c.run(&["pause", "rate-monitor"]);
    assert_eq!(p.code, 1);
    assert!(p.err.contains("not running"));
    assert_eq!(c.run(&["resume", "rate-monitor"]).code, 1);
    assert!(c
        .ok(&["stop", "rate-monitor"])
        .contains("already completed"));
}

#[test]
fn live_pause_resume_and_stop_on_a_running_instance() {
    let c = Cli::new("live");
    let home = c.home.to_string_lossy().to_string();
    let home2 = home.clone();
    // continuous run (ticks=0) in a background thread, 20 ms per tick
    let handle = std::thread::spawn(move || {
        let argv: Vec<String> = [
            "--home",
            &home2,
            "run",
            "rate-monitor",
            "--ticks",
            "0",
            "--interval-ms",
            "20",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = protocol_cli::run(&argv, &mut o, &mut e);
        (code, String::from_utf8_lossy(&o).to_string())
    });

    let wait_for = |what: &str, f: &dyn Fn() -> bool| {
        for _ in 0..400 {
            if f() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    };
    let status = |c: &Cli| c.ok(&["ps"]);
    wait_for("running", &|| status(&c).contains("running"));

    assert!(c.ok(&["pause", "rate-monitor"]).contains("paused"));
    let tick_of = |s: &str| {
        s.lines()
            .find(|l| l.starts_with("tick:"))
            .unwrap()
            .to_string()
    };
    // A tick that was already running when `pause` arrived may still finish and persist (several
    // fsyncs, which can be slow on a busy disk). Wait until the tick counter has settled, then
    // check that it really stays put.
    let mut last = tick_of(&c.ok(&["inspect", "rate-monitor"]));
    let mut stable = 0;
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = tick_of(&c.ok(&["inspect", "rate-monitor"]));
        if now == last {
            stable += 1;
            if stable >= 10 {
                break;
            }
        } else {
            stable = 0;
            last = now;
        }
    }
    assert!(stable >= 10, "tick counter never settled after pause");
    let tick_a = c.ok(&["inspect", "rate-monitor"]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let tick_b = c.ok(&["inspect", "rate-monitor"]);
    assert_eq!(tick_of(&tick_a), tick_of(&tick_b), "no ticks while paused");
    assert!(tick_a.contains("status:     paused"));

    assert!(c.ok(&["resume", "rate-monitor"]).contains("resumed"));
    wait_for("ticks to advance after resume", &|| {
        tick_of(&c.ok(&["inspect", "rate-monitor"])) != tick_of(&tick_b)
    });

    assert!(c.ok(&["stop", "rate-monitor"]).contains("stopped"));
    let (code, out) = handle.join().unwrap();
    assert_eq!(code, 0);
    assert!(out.contains("instance 'rate-monitor' stopped"));
    assert!(c.ok(&["ps"]).contains("stopped"));
}

#[test]
fn create_scaffolds_every_language() {
    let c = Cli::new("create");
    let base = c.work.to_string_lossy().to_string();
    for (lang, file) in [
        ("rust", "src/main.rs"),
        ("c", "strategy.c"),
        ("cpp", "strategy.cpp"),
        ("wasm", "src/main.rs"),
    ] {
        let name = format!("demo-{lang}");
        let out = c.ok(&["create", &name, "--lang", lang, "--dir", &base]);
        assert!(out.contains("alert-only by default"));
        assert!(
            Path::new(&base).join(&name).join(file).is_file(),
            "{lang}: {file}"
        );
        assert!(Path::new(&base).join(&name).join("strategy.toml").is_file());
    }
    assert_eq!(
        c.run(&["create", "x", "--lang", "go", "--dir", &base]).code,
        2
    );
    assert_eq!(c.run(&["create", "x", "--dir", &base]).code, 2);
    assert_eq!(
        c.run(&["create", "Bad Name", "--lang", "c", "--dir", &base])
            .code,
        1
    );
    assert_eq!(
        c.run(&["create", "../evil", "--lang", "c", "--dir", &base])
            .code,
        1
    );
    let reserved = c.run(&["create", "stable-lending", "--lang", "c", "--dir", &base]);
    assert_eq!(reserved.code, 1);
    assert!(reserved.err.contains("reserved"));
    let again = c.run(&["create", "demo-c", "--lang", "c", "--dir", &base]);
    assert_eq!(again.code, 1, "refuses to overwrite");
}

fn have(tool: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Scaffold -> build -> test -> simulate -> run for one language, using the real toolchain.
fn full_language_cycle(lang: &str, tools: &[&str]) {
    if let Some(missing) = tools.iter().find(|t| !have(t)) {
        eprintln!("skipping {lang}: {missing} not installed");
        return;
    }
    let c = Cli::new(&format!("lang-{lang}"));
    let base = c.work.to_string_lossy().to_string();
    let name = format!("hello-{lang}");
    c.ok(&["create", &name, "--lang", lang, "--dir", &base]);
    let dir = c.dir(&name);
    let built = c.ok(&["build", &dir]);
    assert!(built.contains("build ok"), "{lang}: {built}");
    let tested = c.ok(&["test", &dir, "--ticks", "3"]);
    assert!(tested.contains("RESULT: PASS"), "{lang}: {tested}");
    let sim = c.ok(&["simulate", &dir, "--ticks", "2"]);
    assert!(
        sim.contains("health_factor"),
        "{lang}: the strategy saw the engine's data: {sim}"
    );
    assert!(sim.contains("UNVERIFIED"));
    let run = c.ok(&["run", &dir, "--ticks", "2", "--interval-ms", "0"]);
    assert!(run.contains("completed"), "{lang}: {run}");
}

#[test]
fn rust_strategy_full_cycle() {
    full_language_cycle("rust", &["cargo"]);
}

#[test]
fn c_strategy_full_cycle() {
    full_language_cycle("c", &["cc"]);
}

#[test]
fn cpp_strategy_full_cycle() {
    full_language_cycle("cpp", &["c++"]);
}

#[test]
fn build_failure_is_reported() {
    if !have("cc") {
        return;
    }
    let c = Cli::new("buildfail");
    let base = c.work.to_string_lossy().to_string();
    c.ok(&["create", "broken", "--lang", "c", "--dir", &base]);
    fs::write(c.work.join("broken/strategy.c"), "int main( { nonsense").unwrap();
    let r = c.run(&["build", &c.dir("broken")]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("build failed"));
}

#[test]
fn build_is_sandboxed_and_cannot_see_your_files() {
    if !have("cc") {
        return;
    }
    let c = Cli::new("build-sandbox");
    let base = c.work.to_string_lossy().to_string();
    c.ok(&["create", "boxed-build", "--lang", "c", "--dir", &base]);
    // a file of "yours" outside the project; the C preprocessor fails the build if it can see it
    let secret_dir = c.work.join("your-files");
    fs::create_dir_all(&secret_dir).unwrap();
    let secret = secret_dir.join("keys.txt");
    fs::write(&secret, "not for builds").unwrap();
    fs::write(
        c.work.join("boxed-build/strategy.c"),
        format!(
            "#if __has_include(\"{}\")\n#error host file is readable from the build\n#endif\nint main(void) {{ return 0; }}\n",
            secret.display()
        ),
    )
    .unwrap();
    let dir = c.dir("boxed-build");

    // control: without the sandbox the build sees the file, so this must fail
    let off = c.run_with(&["--sandbox", "off"], &["build", &dir]);
    assert_eq!(off.code, 1, "control must leak: {}{}", off.out, off.err);
    assert!(off.out.contains("sandbox: OFF"), "{}", off.out);

    let r = c.run_with(&[], &["build", &dir]);
    let strict = std::env::var("PROTOCOL_REQUIRE_SANDBOX_TESTS").is_ok_and(|v| !v.is_empty());
    match r.code {
        0 => {
            assert!(r.out.contains("sandbox: on"), "{}", r.out);
            assert!(r.out.contains("build ok"), "{}", r.out);
            // output is written into the project directory
            assert!(c.work.join("boxed-build/build/strategy").is_file());
        }
        1 if strict => panic!("sandboxed build failed in CI: {}{}", r.out, r.err),
        1 => assert!(
            r.err.contains("sandbox") && r.err.contains("--sandbox off"),
            "must fail closed with a hint: {}{}",
            r.out,
            r.err
        ),
        other => panic!("unexpected exit {other}: {}{}", r.out, r.err),
    }
}

#[test]
fn verification_pinning_lifecycle() {
    let c = Cli::new("verify");
    let dir = c.script_strategy("pinme", "", "cat >/dev/null\necho 'LOG hi'\necho DONE\n");
    assert!(c.ok(&["strategies", "info", &dir]).contains("UNVERIFIED"));
    let v = c.ok(&["verify", &dir]);
    assert!(v.contains("content hash") && v.contains("UNVERIFIED"));

    let pinned = c.ok(&["verify", &dir, "--pin"]);
    assert!(pinned.contains("pinned as verified"));
    assert!(pinned.contains("reviewed, not safe or profitable"));
    let info = c.ok(&["strategies", "info", &dir]);
    assert!(info.contains("[verified (pinned)]"), "{info}");
    assert!(!info.contains("WARNING: UNVERIFIED"));
    let sim = c.ok(&["simulate", &dir, "--ticks", "1"]);
    assert!(sim.contains("trust: verified"));

    // any change to the source un-verifies it
    fs::write(
        c.work.join("pinme/strategy.sh"),
        "cat >/dev/null\necho 'LOG changed'\necho DONE\n",
    )
    .unwrap();
    assert!(c.ok(&["strategies", "info", &dir]).contains("UNVERIFIED"));

    fs::write(
        c.work.join("pinme/strategy.sh"),
        "cat >/dev/null\necho 'LOG hi'\necho DONE\n",
    )
    .unwrap();
    assert!(
        c.ok(&["strategies", "info", &dir])
            .contains("[verified (pinned)]"),
        "restoring the exact source restores it"
    );
    let toml = fs::read_to_string(c.work.join("pinme/strategy.toml")).unwrap();
    fs::write(
        c.work.join("pinme/strategy.toml"),
        toml.replace("1.0.0", "1.0.1"),
    )
    .unwrap();
    assert!(c.ok(&["strategies", "info", &dir]).contains("UNVERIFIED"));

    assert!(c.ok(&["verify", "rate-monitor"]).contains("official"));
}

#[test]
fn official_names_cannot_be_shadowed() {
    let c = Cli::new("shadow");
    let d = c.work.join("evil");
    fs::create_dir_all(&d).unwrap();
    fs::write(
        d.join("strategy.toml"),
        "name = \"stable-lending\"\nversion = \"9\"\nlanguage = \"c\"\nentry = \"sh x.sh\"\nprotocols = [\"aave\"]\n",
    )
    .unwrap();
    let r = c.run(&["strategies", "info", &d.to_string_lossy()]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("reserved"), "{}", r.err);
    assert!(c
        .ok(&["strategies", "info", "stable-lending"])
        .contains("official"));
}

#[test]
fn invalid_manifest_gives_a_clear_error() {
    let c = Cli::new("badmanifest");
    let d = c.work.join("bad");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("strategy.toml"), "name = \"bad\"\nversion = \"1\"\nlanguage = \"c\"\nentry = \"e\"\nprotocols = [\"aave\"]\nallowed_actoins = \"supply\"\n").unwrap();
    let r = c.run(&["simulate", &d.to_string_lossy()]);
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("unknown field `allowed_actoins`"),
        "{}",
        r.err
    );
}

#[test]
fn external_strategy_is_bound_by_its_manifest_caps() {
    let c = Cli::new("ext-caps");
    let dir = c.script_strategy("supplier", SUPPLIER_MANIFEST, SUPPLIER);
    // 50 per tick, 120 total cap: ticks 1 and 2 execute (100), tick 3 would be 150 > 120
    let out = c.ok(&["simulate", &dir, "--ticks", "3", "--mode", "autonomous"]);
    assert!(out.contains("2 executed"), "{out}");
    assert!(out.contains("1 denied"));
    assert!(out.contains("total exposure cap 120 would be exceeded"));
    assert!(out.contains("funds moved (simulated): 100"));
}

#[test]
fn external_strategy_cannot_exceed_declared_permissions() {
    let c = Cli::new("ext-perm");
    // declares alert-only but tries to supply, and tries a protocol it did not declare
    let script = "cat >/dev/null\necho 'PROPOSE supply aave USDC 1000000'\necho 'PROPOSE alert compound - 0 sneaky'\necho DONE\n";
    let dir = c.script_strategy("sneaky", "", script);
    let out = c.ok(&["simulate", &dir, "--ticks", "1", "--mode", "autonomous"]);
    assert!(out.contains("2 denied"), "{out}");
    assert!(out.contains("action 'supply' is not permitted by the manifest"));
    assert!(out.contains("protocol 'compound' is not permitted by the manifest"));
    assert!(out.contains("funds moved (simulated): 0"));
}

#[test]
fn external_strategy_state_and_input_are_delivered() {
    let c = Cli::new("ext-state");
    // counts ticks via SETSTATE, and echoes what it received for the account
    let script = "\
n=0; acc=''; hf=''\n\
while read -r line; do\n\
  case \"$line\" in\n\
    'STATE n '*) n=${line#STATE n } ;;\n\
    'HEALTH_FACTOR '*) hf=${line#HEALTH_FACTOR } ;;\n\
    'ACCOUNT wallet USDC '*) acc=${line#ACCOUNT wallet USDC } ;;\n\
    END) break ;;\n\
  esac\n\
done\n\
n=$((n+1))\n\
echo \"LOG count=$n usdc_wallet=$acc hf=$hf\"\n\
echo \"SETSTATE n $n\"\n\
echo DONE\n";
    let dir = c.script_strategy("counter", "", script);
    let out = c.ok(&["run", &dir, "--ticks", "3", "--interval-ms", "0"]);
    assert!(
        out.contains("count=1 usdc_wallet=1000000000 hf=2.4"),
        "{out}"
    );
    assert!(out.contains("count=3"), "state carried across ticks: {out}");
    let again = c.ok(&["run", &dir, "--ticks", "1", "--interval-ms", "0"]);
    assert!(again.contains("count=4"), "{again}");
}

#[test]
fn failing_external_strategy_is_restarted_then_marked_failed() {
    let c = Cli::new("ext-fail");
    let dir = c.script_strategy(
        "always-fails",
        "",
        "cat >/dev/null\necho oops >&2\nexit 1\n",
    );
    let r = c.run(&[
        "run",
        &dir,
        "--ticks",
        "20",
        "--interval-ms",
        "0",
        "--max-failures",
        "2",
    ]);
    assert_eq!(r.code, 1);
    assert!(
        r.out.contains("restarting strategy (attempt 1/2)"),
        "{}",
        r.out
    );
    assert!(r.out.contains("instance marked failed"));
    assert!(r.out.contains("oops"));
    let inspect = c.ok(&["inspect", "always-fails"]);
    assert!(inspect.contains("status:     failed"));
    assert!(inspect.contains("restarts: 2"));
    c.ok(&["run", "rate-monitor", "--ticks", "1", "--interval-ms", "0"]);
    assert!(c.ok(&["ps"]).contains("completed"));
}

#[test]
fn slow_external_strategy_times_out_without_hanging_the_cli() {
    let c = Cli::new("ext-slow");
    let dir = c.script_strategy("sleepy", "tick_timeout_ms = 150\n", "exec sleep 10\n");
    let started = std::time::Instant::now();
    let r = c.run(&[
        "run",
        &dir,
        "--ticks",
        "10",
        "--interval-ms",
        "0",
        "--max-failures",
        "1",
    ]);
    assert_eq!(r.code, 1);
    assert!(r.out.contains("timed out"), "{}", r.out);
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
}

#[test]
fn state_and_audit_log_are_persisted_on_disk() {
    let c = Cli::new("disk");
    c.ok(&[
        "run",
        "stable-lending",
        "--mode",
        "autonomous",
        "--ticks",
        "2",
        "--interval-ms",
        "0",
    ]);
    let inst = c.home.join("instances/stable-lending");
    for f in ["status", "instance.state", "account.state", "audit.log"] {
        assert!(inst.join(f).is_file(), "{f} missing");
    }
    let audit = fs::read_to_string(inst.join("audit.log")).unwrap();
    assert!(audit.contains("event=started"));
    assert!(audit.contains("event=executed supply aave"));
    assert!(audit.contains("event=finished status=completed"));
    let account = fs::read_to_string(inst.join("account.state")).unwrap();
    assert!(account.contains("supplied.USDC=") || account.contains("supplied.DAI="));
    // no private-key or secret material anywhere in the state directory
    fn scan(dir: &Path, hits: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, hits);
            } else if let Ok(t) = fs::read_to_string(&p) {
                let low = t.to_lowercase();
                if low.contains("private")
                    || low.contains("mnemonic")
                    || low.contains("seed phrase")
                {
                    hits.push(p.display().to_string());
                }
            }
        }
    }
    let mut hits = Vec::new();
    scan(&c.home, &mut hits);
    assert!(
        hits.is_empty(),
        "unexpected secret-like content in {hits:?}"
    );
}

fn spawn_run(home: &str, args: &[&str]) -> std::thread::JoinHandle<(i32, String)> {
    let mut argv: Vec<String> = vec![
        "--home".into(),
        home.to_string(),
        "--sandbox".into(),
        "off".into(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    std::thread::spawn(move || {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = protocol_cli::run(&argv, &mut o, &mut e);
        (code, String::from_utf8_lossy(&o).to_string())
    })
}

fn wait_until(what: &str, f: &dyn Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_second_run_of_a_live_instance_is_refused() {
    let c = Cli::new("race");
    let home = c.home.to_string_lossy().to_string();
    let first = spawn_run(
        &home,
        &["run", "rate-monitor", "--ticks", "0", "--interval-ms", "20"],
    );
    wait_until("first run to be live", &|| {
        c.ok(&["ps"]).contains("running")
    });

    let second = c.run(&["run", "rate-monitor", "--ticks", "1", "--interval-ms", "0"]);
    assert_eq!(second.code, 1);
    assert!(second.err.contains("already running"), "{}", second.err);
    c.ok(&[
        "run",
        "rate-monitor",
        "--instance",
        "other",
        "--ticks",
        "1",
        "--interval-ms",
        "0",
    ]);

    c.ok(&["stop", "rate-monitor"]);
    let (code, out) = first.join().unwrap();
    assert_eq!(code, 0);
    assert!(out.contains("stopped"));
    c.ok(&["run", "rate-monitor", "--ticks", "1", "--interval-ms", "0"]);
}

#[test]
fn stale_running_status_is_detected_and_recoverable() {
    let c = Cli::new("stale");
    c.ok(&["run", "rate-monitor", "--ticks", "1", "--interval-ms", "0"]);
    // simulate a crash / kill -9: status says running but no process holds the lock
    let status_file = c.home.join("instances/rate-monitor/status");
    fs::write(&status_file, "running\n").unwrap();

    let ps = c.ok(&["ps"]);
    assert!(ps.contains("stale"), "{ps}");
    assert!(ps.contains("process is gone"));
    let inspect = c.ok(&["inspect", "rate-monitor"]);
    assert!(inspect.contains("status:     stale"), "{inspect}");

    // pause/resume must not pretend to control a dead run
    let pause = c.run(&["pause", "rate-monitor"]);
    assert_eq!(pause.code, 1);
    assert!(pause.err.contains("stale"), "{}", pause.err);
    fs::write(&status_file, "paused\n").unwrap();
    let resume = c.run(&["resume", "rate-monitor"]);
    assert_eq!(resume.code, 1);
    assert!(resume.err.contains("not running"), "{}", resume.err);

    // stop clears the stale status and says so
    let stop = c.ok(&["stop", "rate-monitor"]);
    assert!(
        stop.contains("stale") && stop.contains("marked stopped"),
        "{stop}"
    );
    assert!(c.ok(&["ps"]).contains("stopped"));
    assert!(c
        .ok(&["inspect", "rate-monitor"])
        .contains("cleared stale status"));

    fs::write(&status_file, "running\n").unwrap();
    let rerun = c.ok(&["run", "rate-monitor", "--ticks", "1", "--interval-ms", "0"]);
    assert!(rerun.contains("did not shut down cleanly"), "{rerun}");
    assert!(rerun.contains("completed"));
    assert!(c.ok(&["ps"]).contains("completed"));
}

#[test]
fn commands_apply_directly_when_the_recorded_run_is_dead() {
    let c = Cli::new("stale-cmd");
    c.ok(&[
        "run",
        "stable-lending",
        "--ticks",
        "2",
        "--interval-ms",
        "0",
    ]);
    fs::write(c.home.join("instances/stable-lending/status"), "running\n").unwrap();
    let out = c.ok(&["approve", "stable-lending", "1"]);
    assert!(
        out.contains("approve #1: executed"),
        "applied right away, not queued forever: {out}"
    );
    assert!(!out.contains("sent to instance"));
    assert!(c
        .ok(&["inspect", "stable-lending"])
        .contains("funds moved (simulated): 100"));
}

#[test]
fn two_runs_starting_at_the_same_instant_cannot_both_win() {
    let c = Cli::new("simul");
    let home = c.home.to_string_lossy().to_string();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let (b, h) = (barrier.clone(), home.clone());
        handles.push(std::thread::spawn(move || {
            b.wait(); // both threads reach `run` together
            let argv: Vec<String> = [
                "--home",
                &h,
                "run",
                "rate-monitor",
                "--ticks",
                "0",
                "--interval-ms",
                "20",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            let (mut o, mut e) = (Vec::new(), Vec::new());
            let code = protocol_cli::run(&argv, &mut o, &mut e);
            (code, String::from_utf8_lossy(&e).to_string())
        }));
    }
    // the loser returns on its own; the winner keeps running until stopped
    wait_until("one of the two runs to be refused", &|| {
        handles.iter().any(|h| h.is_finished())
    });
    let finished = handles.iter().filter(|h| h.is_finished()).count();
    assert_eq!(finished, 1, "exactly one run must have been refused");
    let idx = handles.iter().position(|h| h.is_finished()).unwrap();
    let loser = handles.remove(idx).join().unwrap();
    assert_eq!(loser.0, 1);
    assert!(loser.1.contains("already running"), "{}", loser.1);

    // The lock is taken before the instance files are written, so the refused run can return
    // while the winner is still setting up. Wait until the winner is really running.
    wait_until("the winning run to be up", &|| {
        c.run(&["inspect", "rate-monitor"])
            .out
            .contains("status:     running")
    });
    c.ok(&["stop", "rate-monitor"]);
    let winner = handles.remove(0).join().unwrap();
    assert_eq!(winner.0, 0);
}

#[test]
fn sandbox_flag_is_validated_and_builtins_never_need_it() {
    let c = Cli::new("sandbox-flag");
    assert_eq!(c.run_with(&["--sandbox", "maybe"], &["ps"]).code, 2);
    // built-in strategies are in-process: the default (require) works without bubblewrap
    let r = c.run_with(&[], &["simulate", "rate-monitor", "--ticks", "2"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(!r.out.contains("sandbox:"), "no sandbox line for built-ins");
}

#[test]
fn external_strategies_are_sandboxed_by_default_or_refused() {
    let c = Cli::new("sandbox-default");
    let dir = c.script_strategy("boxed", "", "echo DONE\n");
    let r = c.run_with(&[], &["simulate", &dir, "--ticks", "1"]);
    // CI sets this on Linux so a sandbox that cannot start is a failure, not a quiet skip.
    let strict = std::env::var("PROTOCOL_REQUIRE_SANDBOX_TESTS").is_ok_and(|v| !v.is_empty());
    match r.code {
        0 => assert!(r.out.contains("sandbox: on"), "{}", r.out),
        1 if strict => panic!("sandbox required by CI but unavailable: {}", r.err),
        1 => assert!(
            r.err.contains("sandbox") && r.err.contains("--sandbox off"),
            "must fail closed with a hint: {}",
            r.err
        ),
        other => panic!("unexpected exit {other}: {}{}", r.out, r.err),
    }
    // explicit opt-out runs, and says so loudly
    let off = c.run_with(&["--sandbox", "off"], &["simulate", &dir, "--ticks", "1"]);
    assert_eq!(off.code, 0, "{}{}", off.out, off.err);
    assert!(off.out.contains("sandbox: OFF"), "{}", off.out);
}

#[test]
fn aave_account_rejects_a_bad_address_before_touching_env_or_network() {
    let c = Cli::new("aave-bad-addr");
    for bad in [
        "nonsense",
        "0x12",
        "0x0000000000000000000000000000000000000000",
    ] {
        let r = c.run(&["aave", "account", bad]);
        assert_eq!(r.code, 1, "{bad}: {}{}", r.out, r.err);
        assert!(r.err.contains("bad address"), "{bad}: {}", r.err);
    }
}

#[test]
fn aave_account_needs_an_address_and_is_documented_in_help() {
    let c = Cli::new("aave-usage");
    assert_eq!(c.run(&["aave", "account"]).code, 2);
    let help = c.ok(&["aave", "account", "--help"]);
    assert!(help.contains("PROTOCOL_RPC_URL"), "{help}");
    assert!(help.contains("PROTOCOL_AAVE_POOL"), "{help}");
    assert!(help.contains("mainnet"), "{help}");
}

#[test]
fn watch_validates_input_before_env_network_or_strategy() {
    let c = Cli::new("watch-validate");
    let good = "0x1111111111111111111111111111111111111111";
    assert_eq!(c.run(&["watch", "health-monitor"]).code, 2);
    let r = c.run(&["watch", "health-monitor", "--account", "nonsense"]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert!(r.err.contains("bad address"), "{}", r.err);
    // zero ticks and a too-fast interval are usage errors (exit 2)
    let r = c.run(&["watch", "health-monitor", "--account", good, "--ticks", "0"]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    let r = c.run(&[
        "watch",
        "health-monitor",
        "--account",
        good,
        "--interval-ms",
        "10",
    ]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(r.err.contains("1000"), "{}", r.err);
}

#[test]
fn aave_preflight_validates_input_before_env_or_network() {
    let c = Cli::new("aave-preflight");
    let good = "0x1111111111111111111111111111111111111111";
    let run = |action: &str, asset: &str, amount: &str, account: &str| {
        c.run(&[
            "aave",
            "preflight",
            action,
            "--asset",
            asset,
            &format!("--amount={amount}"),
            "--account",
            account,
        ])
    };
    let r = run("swap", good, "1", good);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(
        r.err.contains("supply, withdraw, borrow, repay"),
        "{}",
        r.err
    );
    let r = run("supply", "nonsense", "1", good);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("asset"), "{}", r.err);
    let r = run("supply", good, "1", "0x12");
    assert_eq!(r.code, 1);
    assert!(r.err.contains("account"), "{}", r.err);
    for bad in ["1.2.3", "-1", "1e3", ""] {
        let r = run("supply", good, bad, good);
        assert_eq!(r.code, 1, "amount {bad:?}: {}{}", r.out, r.err);
        assert!(r.err.contains("bad amount"), "{}", r.err);
    }
    let r = c.run(&["aave", "preflight", "supply"]);
    assert_eq!(r.code, 2);
}

#[test]
fn aave_preflight_is_documented_in_help() {
    let c = Cli::new("aave-preflight-help");
    let r = c.run(&["aave", "preflight", "--help"]);
    assert_eq!(r.code, 0);
    for needle in [
        "Nothing is signed or sent",
        "WOULD REVERT",
        "PROTOCOL_RPC_URL",
        "official address book",
    ] {
        assert!(r.out.contains(needle), "help lacks {needle:?}: {}", r.out);
    }
}

#[test]
fn watch_trigger_flags_are_validated_before_env_or_network() {
    let c = Cli::new("watch-trigger");
    let good = "0x1111111111111111111111111111111111111111";
    let r = c.run(&[
        "watch",
        "health-monitor",
        "--account",
        good,
        "--on",
        "sometimes",
    ]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(r.err.contains("interval, block or event"), "{}", r.err);
    let r = c.run(&[
        "watch",
        "health-monitor",
        "--account",
        good,
        "--on",
        "event",
        "--timeout-secs",
        "0",
    ]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(r.err.contains("--timeout-secs"), "{}", r.err);
    let help = c.ok(&["watch", "--help"]);
    for needle in ["--on", "block", "event", "eth_getLogs", "--timeout-secs"] {
        assert!(help.contains(needle), "help lacks {needle:?}: {help}");
    }
}

#[test]
fn watch_is_documented_in_help() {
    let c = Cli::new("watch-help");
    let help = c.ok(&["watch", "--help"]);
    assert!(help.contains("read-only"), "{help}");
    assert!(help.contains("PROTOCOL_RPC_URL"), "{help}");
    assert!(help.contains("never executed"), "{help}");
}

#[test]
fn compound_account_validates_input_before_env_or_network() {
    let c = Cli::new("compound-account");
    let r = c.run(&["compound", "account", "nonsense"]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert!(r.err.contains("bad address"), "{}", r.err);
    assert_eq!(c.run(&["compound", "account"]).code, 2);
    let r = c.run(&["compound", "account", "--help"]);
    assert_eq!(r.code, 0);
    for needle in [
        "Nothing is signed or sent",
        "PROTOCOL_COMPOUND_COMET",
        "PROTOCOL_RPC_URL",
        "mainnet",
    ] {
        assert!(r.out.contains(needle), "help lacks {needle:?}: {}", r.out);
    }
}

#[test]
fn morpho_account_validates_input_before_env_or_network() {
    let c = Cli::new("morpho-account");
    let r = c.run(&["morpho", "account", "nonsense"]);
    assert_eq!(r.code, 1, "{}{}", r.out, r.err);
    assert!(r.err.contains("bad address"), "{}", r.err);
    assert_eq!(c.run(&["morpho", "account"]).code, 2);
    let r = c.run(&["morpho", "account", "--help"]);
    assert_eq!(r.code, 0);
    for needle in [
        "Nothing is signed or sent",
        "PROTOCOL_MORPHO_ADDRESS",
        "PROTOCOL_MORPHO_MARKET_ID",
        "optimistic",
        "mainnet",
    ] {
        assert!(r.out.contains(needle), "help lacks {needle:?}: {}", r.out);
    }
}

#[test]
fn watch_protocol_is_validated_before_env_network_or_strategy() {
    let c = Cli::new("watch-protocol");
    let good = "0x1111111111111111111111111111111111111111";
    let r = c.run(&[
        "watch",
        "health-monitor",
        "--account",
        good,
        "--protocol",
        "nope",
    ]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(r.err.contains("aave, compound or morpho"), "{}", r.err);
    for p in ["compound", "morpho"] {
        let r = c.run(&[
            "watch",
            "health-monitor",
            "--account",
            good,
            "--protocol",
            p,
            "--on",
            "event",
        ]);
        assert_eq!(r.code, 2, "{p}: {}{}", r.out, r.err);
        assert!(r.err.contains("only available for aave"), "{}", r.err);
    }
    let r = c.run(&["watch", "--help"]);
    assert!(r.out.contains("--protocol"), "{}", r.out);
}

fn write_history(c: &Cli, name: &str, text: &str) -> String {
    std::fs::create_dir_all(&c.work).unwrap();
    let p = c.work.join(name);
    std::fs::write(&p, text).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn backtest_replays_history_and_reports_timing() {
    let c = Cli::new("backtest");
    let csv = "label,collateral_usd,debt_usd,liq_threshold_bps\n\
               d1,3000,1000,8500\nd2,1800,1000,8500\nd3,1700,1000,8500\n\
               d4,1400,1000,8500\nd5,1100,1000,8500\nd6,3000,1000,8500\n";
    let f = write_history(&c, "h.csv", csv);
    let r = c.run(&["backtest", "health-factor-monitor", "--data", &f]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    for needle in [
        "6 recorded ticks",
        "nothing is executed",
        "lowest health factor 0.935 at tick 5 (d5); 1 tick(s) below 1.0",
        "first tick below 1.0 is 5 (d5)",
        "2 alerts",
        "first proposal or alert at tick 3 (d3)",
        "spoke up 2 tick(s) before the first unsafe tick",
    ] {
        assert!(r.out.contains(needle), "missing {needle:?}:\n{}", r.out);
    }
}

#[test]
fn backtest_validates_everything_before_running() {
    let c = Cli::new("backtest-validate");
    let f = write_history(
        &c,
        "ok.csv",
        "label,collateral_usd,debt_usd,liq_threshold_bps\na,1,1,8000\n",
    );
    let r = c.run(&[
        "backtest",
        "health-factor-monitor",
        "--data",
        &f,
        "--protocol",
        "x",
    ]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    // missing file and bad content: failures that name the problem
    let r = c.run(&[
        "backtest",
        "health-factor-monitor",
        "--data",
        "/definitely/not/here.csv",
    ]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("cannot read"), "{}", r.err);
    let bad = write_history(
        &c,
        "bad.csv",
        "label,collateral_usd,debt_usd,liq_threshold_bps\na,1,1,99999\n",
    );
    let r = c.run(&["backtest", "health-factor-monitor", "--data", &bad]);
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("line 2") && r.err.contains("liq_threshold_bps"),
        "{}",
        r.err
    );
    assert_eq!(c.run(&["backtest", "health-factor-monitor"]).code, 2);
    let r = c.run(&["backtest", "no-such-strategy", "--data", &f]);
    assert_ne!(r.code, 0);
}

#[test]
fn backtest_is_documented_in_help() {
    let c = Cli::new("backtest-help");
    let r = c.run(&["backtest", "--help"]);
    assert_eq!(r.code, 0);
    for needle in [
        "label,collateral_usd,debt_usd,liq_threshold_bps",
        "Nothing is executed",
        "WOULD",
    ] {
        assert!(r.out.contains(needle), "help lacks {needle:?}: {}", r.out);
    }
}

#[test]
fn python_and_java_are_no_longer_supported_languages() {
    let c = Cli::new("no-py-java");
    let base = c.work.to_string_lossy().to_string();
    for lang in ["python", "py", "java"] {
        let r = c.run(&["create", "demo-x", "--lang", lang, "--dir", &base]);
        assert_eq!(r.code, 2, "{lang}: {}{}", r.out, r.err);
        assert!(r.err.contains("rust, c, cpp, wasm"), "{lang}: {}", r.err);
    }
}

fn have_wasm_target() -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg("rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn wasm_strategy_full_cycle() {
    if !have_wasm_target() {
        assert!(
            std::env::var_os("PROTOCOL_REQUIRE_WASM_TESTS").is_none(),
            "PROTOCOL_REQUIRE_WASM_TESTS is set but the wasm32-wasip1 target is not installed"
        );
        eprintln!("skipping wasm: rustup target wasm32-wasip1 not installed");
        return;
    }
    full_language_cycle("wasm", &["cargo"]);
}

/// A hand-built module (no toolchain needed) runs through the real CLI, with the OS sandbox
/// required: WebAssembly strategies do not need bubblewrap.
#[test]
fn prebuilt_wasm_module_runs_without_an_os_sandbox() {
    let c = Cli::new("wasm-prebuilt");
    let dir = c.work.join("echo-wasm");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("strategy.toml"),
        "name = \"echo-wasm\"\nversion = \"0.1.0\"\nlanguage = \"wasm\"\nentry = \"echo.wasm\"\n\
         description = \"fixture\"\nprotocols = [\"aave\"]\nallowed_actions = [\"alert\"]\n\
         max_amount_per_action = \"0\"\nmax_total_exposure = \"0\"\ntick_timeout_ms = 2000\n",
    )
    .unwrap();
    fs::write(
        dir.join("echo.wasm"),
        include_bytes!("../../wasm/tests/fixtures/echo.wasm"),
    )
    .unwrap();
    let d = dir.to_string_lossy().to_string();
    let r = c.run_with(&["--sandbox", "require"], &["simulate", &d, "--ticks", "2"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(r.out.contains("hello from wasm"), "{}", r.out);
    assert!(r.out.contains("sandbox: not needed"), "{}", r.out);
    // a module that is missing is a clear failure, not a crash
    fs::remove_file(dir.join("echo.wasm")).unwrap();
    let r = c.run(&["simulate", &d, "--ticks", "1"]);
    assert_ne!(r.code, 0);
    assert!(
        r.err.contains("protocol build") || r.out.contains("protocol build"),
        "{}{}",
        r.out,
        r.err
    );
}

#[test]
fn dashboard_is_documented_read_only_and_validates_its_port() {
    let c = Cli::new("dash-help");
    let h = c.ok(&["dashboard", "--help"]);
    assert!(h.contains("127.0.0.1") && h.contains("read-only") && h.contains("--port"));
    let r = c.run(&["dashboard", "--port", "99999"]);
    assert_eq!(r.code, 2);
}

const H64: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const C40: &str = "0123456789abcdef0123456789abcdef01234567";

fn market_index(c: &Cli) -> String {
    let line = |n: &str, v: &str, d: &str| {
        format!("{n} | {v} | rust | {H64} | https://github.com/a/b | {C40} | {d}")
    };
    let text = format!(
        "# protocol-market-index v1\n{}\n{}\n{}\n",
        line("watcher", "1.0.0", "Watches health"),
        line("watcher", "1.2.0", "Watches health"),
        line("stable-lending", "9.9.9", "Pretends to be official")
    );
    let p = c.work.join("index.txt");
    fs::write(&p, text).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn market_search_and_info_read_a_local_index() {
    let c = Cli::new("market-read");
    let idx = market_index(&c);
    let all = c.ok(&["market", "search", "--index", &idx]);
    assert!(
        all.contains("watcher")
            && all.contains("1.2.0")
            && all.contains("Listed does not mean reviewed")
    );
    assert!(c
        .ok(&["market", "search", "nothing-like-this", "--index", &idx])
        .contains("no matching"));
    let info = c.ok(&["market", "info", "watcher", "--index", &idx]);
    assert!(info.contains("1.2.0") && info.contains(H64) && info.contains(C40));
    assert!(info.contains("does not vouch"));
    let old = c.ok(&[
        "market",
        "info",
        "watcher",
        "--version",
        "1.0.0",
        "--index",
        &idx,
    ]);
    assert!(old.contains("1.0.0"));
    let r = c.run(&["market", "info", "ghost", "--index", &idx]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("not in this index"));
}

#[test]
fn market_refuses_bad_indexes_and_needs_one() {
    let c = Cli::new("market-bad");
    let bad = c.work.join("bad.txt");
    fs::write(&bad, "# protocol-market-index v1\nnot a valid line\n").unwrap();
    let r = c.run(&["market", "search", "--index", &bad.to_string_lossy()]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("line 2"), "{}", r.err);
    let nohdr = c.work.join("nohdr.txt");
    fs::write(&nohdr, "x\n").unwrap();
    assert_eq!(
        c.run(&["market", "search", "--index", &nohdr.to_string_lossy()])
            .code,
        1
    );
    // no --index and no environment variable: a usage error, not a guess
    if std::env::var("PROTOCOL_MARKET_INDEX").is_err() {
        assert_eq!(c.run(&["market", "search"]).code, 2);
    }
    assert_eq!(
        c.run(&["market", "search", "--index", "http://example.com/x"])
            .code,
        1
    );
}

#[test]
fn market_install_refuses_official_names_and_unknown_entries_without_fetching() {
    let c = Cli::new("market-install");
    let idx = market_index(&c);
    let dir = c.work.join("dest");
    let d = dir.to_string_lossy().to_string();
    let r = c.run(&[
        "market",
        "install",
        "stable-lending",
        "--index",
        &idx,
        "--dir",
        &d,
    ]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("reserved"), "{}", r.err);
    let r = c.run(&["market", "install", "ghost", "--index", &idx, "--dir", &d]);
    assert_eq!(r.code, 1);
    assert!(
        !dir.exists(),
        "nothing is created when nothing is installed"
    );
    let h = c.ok(&["market", "install", "--help"]);
    assert!(h.contains("UNVERIFIED") && h.contains("never overwritten"));
}
