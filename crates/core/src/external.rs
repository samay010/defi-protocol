//! Out-of-process strategies: the same strategy contract for any language.
//!
//! One short-lived process is spawned per tick. The engine writes a request on stdin and
//! reads a response from stdout (line protocol, documented in `docs/strategy-protocol.md`).
//! State is carried by the engine (`STATE` in, `SETSTATE` out), which makes restart after a
//! crash trivial and keeps strategies stateless processes.
//!
//! IMPORTANT: this provides a timeout, an output-size cap, a cleared environment and no
//! key material. It is NOT an OS sandbox: a malicious strategy process can still read user
//! files and use the network as the user. See `docs/security.md`.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::amount::format_amount;
use crate::manifest::Manifest;
use crate::strategy::{Strategy, TickInput, TickOutput};
use crate::types::{Action, ActionKind};
use crate::MAX_PROPOSALS_PER_TICK;

pub const MAX_OUTPUT_BYTES: u64 = 64 * 1024;
const SAFE_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

pub struct ExternalStrategy {
    program: PathBuf,
    args: Vec<String>,
    dir: PathBuf,
    timeout: Duration,
}

impl ExternalStrategy {
    pub fn from_manifest(m: &Manifest, dir: &Path) -> Result<ExternalStrategy, String> {
        if m.is_builtin() {
            return Err("manifest entry is a builtin strategy, not a process".into());
        }
        let mut parts = m.entry.split_whitespace();
        let prog = parts.next().ok_or("manifest entry is empty")?;
        let dir = dir
            .canonicalize()
            .map_err(|e| format!("strategy directory {}: {e}", dir.display()))?;
        // Relative program paths (e.g. ./build/strategy) are resolved against the strategy dir.
        let program = if prog.contains('/') && !Path::new(prog).is_absolute() {
            dir.join(prog)
        } else {
            PathBuf::from(prog)
        };
        Ok(ExternalStrategy {
            program,
            args: parts.map(|s| s.to_string()).collect(),
            dir,
            timeout: Duration::from_millis(m.tick_timeout_ms),
        })
    }

    fn run_process(&self, input: &str) -> Result<String, String> {
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .current_dir(&self.dir)
            .env_clear()
            .env("PATH", SAFE_PATH)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start '{}': {e}", self.program.display()))?;

        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let data = input.to_string();
        thread::spawn(move || {
            let _ = stdin.write_all(data.as_bytes());
        });

        let mut stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx_out, rx_out) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut stdout)
                .take(MAX_OUTPUT_BYTES + 1)
                .read_to_end(&mut buf);
            // keep draining so a chatty child cannot block on a full pipe
            let _ = io::copy(&mut stdout, &mut io::sink());
            let _ = tx_out.send(buf);
        });

        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let (tx_err, rx_err) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = (&mut stderr).take(4096).read_to_end(&mut buf);
            let _ = io::copy(&mut stderr, &mut io::sink());
            let _ = tx_err.send(buf);
        });

        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {
                    if start.elapsed() >= self.timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "strategy timed out after {} ms",
                            self.timeout.as_millis()
                        ));
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(e) => return Err(format!("cannot wait for strategy: {e}")),
            }
        };

        let grace = Duration::from_millis(500);
        let out = rx_out
            .recv_timeout(grace)
            .map_err(|_| "strategy left its output pipe open after exiting".to_string())?;
        let err_text = rx_err.recv_timeout(grace).unwrap_or_default();

        if !status.success() {
            let msg = String::from_utf8_lossy(&err_text);
            return Err(format!("strategy exited with {status}: {}", msg.trim()));
        }
        if out.len() as u64 > MAX_OUTPUT_BYTES {
            return Err(format!("strategy output exceeded {MAX_OUTPUT_BYTES} bytes"));
        }
        String::from_utf8(out).map_err(|_| "strategy output is not valid UTF-8".to_string())
    }
}

impl Strategy for ExternalStrategy {
    fn on_tick(&mut self, input: &TickInput) -> Result<TickOutput, String> {
        let request = encode_input(input);
        let response = self.run_process(&request)?;
        parse_output(&response)
    }
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

pub fn encode_input(input: &TickInput) -> String {
    let mut s = String::new();
    s.push_str(&format!("TICK {}\n", input.tick));
    for (k, v) in input.params {
        s.push_str(&format!("PARAM {} {}\n", one_line(k), one_line(v)));
    }
    for (k, v) in input.state {
        s.push_str(&format!("STATE {} {}\n", one_line(k), one_line(v)));
    }
    for (asset, m) in &input.snapshot.markets {
        s.push_str(&format!(
            "MARKET {asset} apy_bps={} price_usd_micro={}\n",
            m.supply_apy_bps, m.price_usd_micro
        ));
    }
    let acc = &input.snapshot.account;
    for (a, v) in &acc.supplied {
        s.push_str(&format!("ACCOUNT supplied {a} {v}\n"));
    }
    for (a, v) in &acc.borrowed {
        s.push_str(&format!("ACCOUNT borrowed {a} {v}\n"));
    }
    for (a, v) in &acc.wallet {
        s.push_str(&format!("ACCOUNT wallet {a} {v}\n"));
    }
    match input.snapshot.health_factor() {
        Some(hf) => s.push_str(&format!("HEALTH_FACTOR {hf:.6}\n")),
        None => s.push_str("HEALTH_FACTOR none\n"),
    }
    s.push_str("END\n");
    s
}

/// Strict parser: any unknown or malformed line is an error, and `DONE` is mandatory so a
/// truncated response is never mistaken for "no proposals".
pub fn parse_output(text: &str) -> Result<TickOutput, String> {
    let mut out = TickOutput::default();
    let mut done = false;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if done {
            return Err(format!("line {n}: output after DONE"));
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "LOG" => out.logs.push(rest.to_string()),
            "SETSTATE" => {
                let (k, v) = rest.split_once(' ').unwrap_or((rest, ""));
                if k.is_empty() {
                    return Err(format!("line {n}: SETSTATE needs a key"));
                }
                out.state.insert(k.to_string(), v.to_string());
            }
            "PROPOSE" => {
                let p: Vec<&str> = rest.splitn(5, ' ').collect();
                if p.len() < 4 {
                    return Err(format!(
                        "line {n}: PROPOSE <kind> <protocol> <asset> <amount_micro> [note]"
                    ));
                }
                let kind = ActionKind::parse(p[0])
                    .ok_or_else(|| format!("line {n}: unknown kind '{}'", p[0]))?;
                let amount: u64 = p[3].parse().map_err(|_| {
                    format!(
                        "line {n}: amount must be integer micro-units, got '{}'",
                        p[3]
                    )
                })?;
                out.proposals.push(Action {
                    kind,
                    protocol: p[1].to_ascii_lowercase(),
                    asset: if p[2] == "-" {
                        String::new()
                    } else {
                        p[2].to_ascii_uppercase()
                    },
                    amount,
                    note: p.get(4).unwrap_or(&"").to_string(),
                });
                if out.proposals.len() > MAX_PROPOSALS_PER_TICK {
                    return Err(format!(
                        "more than {MAX_PROPOSALS_PER_TICK} proposals in one tick"
                    ));
                }
            }
            "DONE" => done = true,
            other => return Err(format!("line {n}: unknown response command '{other}'")),
        }
    }
    if !done {
        return Err("response missing DONE (truncated output?)".into());
    }
    Ok(out)
}

/// Used by tests and docs to show the wire format of an action.
pub fn describe_proposal(a: &Action) -> String {
    format!(
        "{} {} {} {}",
        a.kind,
        a.protocol,
        if a.asset.is_empty() { "-" } else { &a.asset },
        format_amount(a.amount)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Account, MarketInfo, Snapshot};
    use crate::amount::MICRO;
    use std::collections::BTreeMap;
    use std::fs;

    fn snapshot() -> Snapshot {
        let mut markets = BTreeMap::new();
        markets.insert(
            "USDC".to_string(),
            MarketInfo {
                supply_apy_bps: 400,
                price_usd_micro: MICRO,
                liq_threshold_bps: 8000,
            },
        );
        let mut account = Account::default();
        account.wallet.insert("USDC".into(), 5 * MICRO);
        Snapshot {
            tick: 7,
            markets,
            account,
        }
    }

    #[test]
    fn encodes_expected_lines() {
        let snap = snapshot();
        let mut params = BTreeMap::new();
        params.insert("k".to_string(), "v w".to_string());
        let state = BTreeMap::new();
        let text = encode_input(&TickInput {
            tick: 7,
            snapshot: &snap,
            params: &params,
            state: &state,
        });
        assert!(text.starts_with("TICK 7\n"));
        assert!(text.contains("PARAM k v w\n"));
        assert!(text.contains("MARKET USDC apy_bps=400 price_usd_micro=1000000\n"));
        assert!(text.contains("ACCOUNT wallet USDC 5000000\n"));
        assert!(text.contains("HEALTH_FACTOR none\n"));
        assert!(text.ends_with("END\n"));
    }

    #[test]
    fn parses_valid_output() {
        let out = parse_output(
            "LOG hello world\nPROPOSE supply AAVE usdc 1000000 because yield\nPROPOSE alert aave - 0 careful\nSETSTATE last 5\nDONE\n",
        )
        .unwrap();
        assert_eq!(out.logs, vec!["hello world"]);
        assert_eq!(out.proposals.len(), 2);
        assert_eq!(out.proposals[0].protocol, "aave");
        assert_eq!(out.proposals[0].asset, "USDC");
        assert_eq!(out.proposals[0].note, "because yield");
        assert_eq!(out.proposals[1].asset, "");
        assert_eq!(out.state["last"], "5");
    }

    #[test]
    fn rejects_malformed_output() {
        assert!(parse_output("LOG x\n")
            .unwrap_err()
            .contains("missing DONE"));
        assert!(parse_output("DONE\nLOG late\n")
            .unwrap_err()
            .contains("after DONE"));
        assert!(parse_output("HACK x\nDONE\n")
            .unwrap_err()
            .contains("unknown response command"));
        assert!(parse_output("PROPOSE supply aave USDC\nDONE\n").is_err());
        assert!(parse_output("PROPOSE supply aave USDC 1.5\nDONE\n")
            .unwrap_err()
            .contains("integer"));
        assert!(parse_output("PROPOSE fly aave USDC 1\nDONE\n")
            .unwrap_err()
            .contains("unknown kind"));
        let many: String = (0..21)
            .map(|_| "PROPOSE supply aave USDC 1\n")
            .collect::<String>()
            + "DONE\n";
        assert!(parse_output(&many).unwrap_err().contains("proposals"));
    }

    fn script_dir(tag: &str, script: &str, timeout_ms: u64) -> (PathBuf, Manifest) {
        let d = std::env::temp_dir().join(format!("proto-ext-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("strategy.sh"), script).unwrap();
        let m = Manifest::parse(&format!(
            "name=\"sh-strategy\"\nversion=\"1\"\nlanguage=\"c\"\nentry=\"sh strategy.sh\"\nprotocols=\"aave\"\ntick_timeout_ms=\"{timeout_ms}\"\n"
        ))
        .unwrap();
        (d, m)
    }

    fn tick(strategy: &mut ExternalStrategy) -> Result<TickOutput, String> {
        let snap = snapshot();
        let params = BTreeMap::new();
        let state = BTreeMap::new();
        strategy.on_tick(&TickInput {
            tick: 1,
            snapshot: &snap,
            params: &params,
            state: &state,
        })
    }

    #[test]
    fn runs_a_shell_strategy_end_to_end() {
        let script = "while read line; do [ \"$line\" = END ] && break; case \"$line\" in TICK*) t=\"${line#TICK }\";; esac; done\necho \"LOG saw tick $t\"\necho \"PROPOSE supply aave USDC 1000000\"\necho DONE\n";
        let (d, m) = script_dir("ok", script, 2000);
        let mut s = ExternalStrategy::from_manifest(&m, &d).unwrap();
        let out = tick(&mut s).unwrap();
        assert_eq!(out.logs, vec!["saw tick 1"]);
        assert_eq!(out.proposals[0].amount, MICRO);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn times_out_and_kills_slow_strategies() {
        let (d, m) = script_dir("slow", "exec sleep 5\n", 200);
        let mut s = ExternalStrategy::from_manifest(&m, &d).unwrap();
        let started = Instant::now();
        let err = tick(&mut s).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn nonzero_exit_and_bad_output_are_errors() {
        let (d, m) = script_dir("fail", "echo boom >&2\nexit 3\n", 2000);
        let mut s = ExternalStrategy::from_manifest(&m, &d).unwrap();
        let err = tick(&mut s).unwrap_err();
        assert!(err.contains("boom"), "{err}");
        let (d2, m2) = script_dir("garbage", "echo nonsense\n", 2000);
        let mut s2 = ExternalStrategy::from_manifest(&m2, &d2).unwrap();
        assert!(tick(&mut s2).is_err());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn environment_is_cleared() {
        std::env::set_var("PROTO_TEST_SECRET", "hunter2");
        let script = "echo \"LOG secret=[$PROTO_TEST_SECRET]\"\necho DONE\n";
        let (d, m) = script_dir("env", script, 2000);
        let mut s = ExternalStrategy::from_manifest(&m, &d).unwrap();
        let out = tick(&mut s).unwrap();
        assert_eq!(out.logs, vec!["secret=[]"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn oversized_output_is_rejected() {
        let script = "i=0; while [ $i -lt 3000 ]; do echo \"LOG aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"; i=$((i+1)); done\necho DONE\n";
        let (d, m) = script_dir("big", script, 5000);
        let mut s = ExternalStrategy::from_manifest(&m, &d).unwrap();
        assert!(tick(&mut s).unwrap_err().contains("exceeded"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_program_is_a_clear_error() {
        let d = std::env::temp_dir().join(format!("proto-ext-missing-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let m = Manifest::parse(
            "name=\"x\"\nversion=\"1\"\nlanguage=\"c\"\nentry=\"./build/nope\"\nprotocols=\"aave\"\n",
        )
        .unwrap();
        let mut s = ExternalStrategy::from_manifest(&m, &d).unwrap();
        assert!(tick(&mut s).unwrap_err().contains("cannot start"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn describe_proposal_formats() {
        let a = Action::new(ActionKind::Supply, "aave", "USDC", 2_500_000);
        assert_eq!(describe_proposal(&a), "supply aave USDC 2.5");
    }
}
