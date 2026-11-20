//! File-based persistence under a home directory (default `~/.protocol`):
//!
//! ```text
//! <home>/KILL                    emergency kill-switch marker
//! <home>/verified.list           pinned (name, version, sha256) of reviewed strategies
//! <home>/instances/<name>/
//!     status                     single word; the control plane (pause/resume/stop)
//!     instance.state             metadata (tick, mode, counters)
//!     account.state              simulated account
//!     pending.state              actions waiting for approval
//!     strategy.state             strategy-owned state
//!     audit.log                  append-only audit trail
//! ```
//!
//! `status` is its own file so `pause`/`resume`/`stop` from another process never race
//! with the supervisor rewriting metadata every tick.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::adapter::Account;
use crate::engine::Pending;
use crate::kv::{atomic_write, read_kv_file, write_kv_file};
use crate::manifest::valid_name;
use crate::types::{Action, Mode, Network, Scenario, Status};

#[derive(Clone, Debug)]
pub struct Store {
    pub home: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub name: String,
    pub strategy_ref: String,
    pub mode: Mode,
    pub network: Network,
    pub scenario: Scenario,
    pub tick: u64,
    pub total_moved: u64,
    pub next_id: u64,
    pub restarts: u32,
    pub last_error: String,
}

impl Instance {
    pub fn new(
        name: &str,
        strategy_ref: &str,
        mode: Mode,
        network: Network,
        scenario: Scenario,
    ) -> Instance {
        Instance {
            name: name.to_string(),
            strategy_ref: strategy_ref.to_string(),
            mode,
            network,
            scenario,
            tick: 0,
            total_moved: 0,
            next_id: 1,
            restarts: 0,
            last_error: String::new(),
        }
    }

    fn to_kv(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("strategy_ref".into(), self.strategy_ref.clone());
        m.insert("mode".into(), self.mode.to_string());
        m.insert("network".into(), self.network.to_string());
        m.insert("scenario".into(), self.scenario.to_string());
        m.insert("tick".into(), self.tick.to_string());
        m.insert("total_moved".into(), self.total_moved.to_string());
        m.insert("next_id".into(), self.next_id.to_string());
        m.insert("restarts".into(), self.restarts.to_string());
        m.insert("last_error".into(), self.last_error.clone());
        m
    }

    fn from_kv(name: &str, m: &BTreeMap<String, String>) -> Result<Instance, String> {
        let get = |k: &str| {
            m.get(k)
                .ok_or_else(|| format!("instance '{name}': missing '{k}'"))
        };
        let num = |k: &str| -> Result<u64, String> {
            get(k)?
                .parse()
                .map_err(|_| format!("instance '{name}': bad number for '{k}'"))
        };
        Ok(Instance {
            name: name.to_string(),
            strategy_ref: get("strategy_ref")?.clone(),
            mode: Mode::parse(get("mode")?).ok_or("bad mode")?,
            network: Network::parse(get("network")?).ok_or("bad network")?,
            scenario: Scenario::parse(get("scenario")?).ok_or("bad scenario")?,
            tick: num("tick")?,
            total_moved: num("total_moved")?,
            next_id: num("next_id")?,
            restarts: num("restarts")? as u32,
            last_error: m.get("last_error").cloned().unwrap_or_default(),
        })
    }
}

impl Store {
    pub fn new(home: impl Into<PathBuf>) -> Store {
        Store { home: home.into() }
    }

    fn instances_dir(&self) -> PathBuf {
        self.home.join("instances")
    }

    pub fn instance_dir(&self, name: &str) -> Result<PathBuf, String> {
        if !valid_name(name) {
            return Err(format!("invalid instance name '{name}'"));
        }
        Ok(self.instances_dir().join(name))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.instance_dir(name)
            .map(|d| d.join("instance.state").is_file())
            .unwrap_or(false)
    }

    pub fn list(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(rd) = fs::read_dir(self.instances_dir()) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if valid_name(&n) && e.path().join("instance.state").is_file() {
                    names.push(n);
                }
            }
        }
        names.sort();
        names
    }

    pub fn create(&self, meta: &Instance) -> Result<(), String> {
        let dir = self.instance_dir(&meta.name)?;
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        self.save_meta(meta)?;
        self.write_status(&meta.name, Status::Created)
    }

    pub fn load_meta(&self, name: &str) -> Result<Instance, String> {
        if !self.exists(name) {
            return Err(format!("no such instance '{name}' (see `protocol ps`)"));
        }
        let m = read_kv_file(&self.instance_dir(name)?.join("instance.state"))?;
        Instance::from_kv(name, &m)
    }

    pub fn save_meta(&self, meta: &Instance) -> Result<(), String> {
        write_kv_file(
            &self.instance_dir(&meta.name)?.join("instance.state"),
            &meta.to_kv(),
        )
    }

    pub fn read_status(&self, name: &str) -> Status {
        self.instance_dir(name)
            .ok()
            .and_then(|d| fs::read_to_string(d.join("status")).ok())
            .and_then(|s| Status::parse(&s))
            .unwrap_or(Status::Created)
    }

    pub fn write_status(&self, name: &str, status: Status) -> Result<(), String> {
        atomic_write(
            &self.instance_dir(name)?.join("status"),
            &format!("{status}\n"),
        )
    }

    pub fn load_account(&self, name: &str) -> Result<Option<Account>, String> {
        let path = self.instance_dir(name)?.join("account.state");
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Account::from_kv(&read_kv_file(&path)?)))
    }

    pub fn save_account(&self, name: &str, account: &Account) -> Result<(), String> {
        write_kv_file(
            &self.instance_dir(name)?.join("account.state"),
            &account.to_kv(),
        )
    }

    pub fn load_pending(&self, name: &str) -> Result<Vec<Pending>, String> {
        let path = self.instance_dir(name)?.join("pending.state");
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let mut out = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let p: Vec<&str> = line.splitn(3, '|').collect();
            if p.len() != 3 {
                return Err(format!("corrupt pending entry '{line}'"));
            }
            out.push(Pending {
                id: p[0].parse().map_err(|_| "corrupt pending id")?,
                tick: p[1].parse().map_err(|_| "corrupt pending tick")?,
                action: Action::from_line(p[2])?,
            });
        }
        Ok(out)
    }

    pub fn save_pending(&self, name: &str, pending: &[Pending]) -> Result<(), String> {
        let mut text = String::new();
        for p in pending {
            text.push_str(&format!("{}|{}|{}\n", p.id, p.tick, p.action.to_line()));
        }
        atomic_write(&self.instance_dir(name)?.join("pending.state"), &text)
    }

    pub fn load_strategy_state(&self, name: &str) -> Result<BTreeMap<String, String>, String> {
        read_kv_file(&self.instance_dir(name)?.join("strategy.state"))
    }

    pub fn save_strategy_state(
        &self,
        name: &str,
        state: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        write_kv_file(&self.instance_dir(name)?.join("strategy.state"), state)
    }

    pub fn append_audit(&self, name: &str, lines: &[String]) -> Result<(), String> {
        if lines.is_empty() {
            return Ok(());
        }
        let path = self.instance_dir(name)?.join("audit.log");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        for l in lines {
            writeln!(f, "{}", l.replace(['\n', '\r'], " ")).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn read_audit(&self, name: &str) -> Result<Vec<String>, String> {
        let path = self.instance_dir(name)?.join("audit.log");
        match fs::read_to_string(&path) {
            Ok(t) => Ok(t.lines().map(|l| l.to_string()).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    pub fn kill_switch(&self) -> bool {
        self.home.join("KILL").exists()
    }

    pub fn set_kill_switch(&self, on: bool) -> Result<(), String> {
        let path = self.home.join("KILL");
        if on {
            fs::create_dir_all(&self.home).map_err(|e| e.to_string())?;
            atomic_write(&path, "on\n")
        } else {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }

    // ---- command inbox (approve / reject / act sent to a live instance) -------------

    pub fn push_command(&self, name: &str, line: &str) -> Result<(), String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = self.instance_dir(name)?.join("commands");
        fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let file = dir.join(format!(
            "{nanos:025}-{:06}.cmd",
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        atomic_write(&file, &format!("{}\n", line.replace(['\n', '\r'], " ")))
    }

    /// Returns queued commands oldest-first and removes them from the inbox.
    pub fn take_commands(&self, name: &str) -> Result<Vec<String>, String> {
        let dir = self.instance_dir(name)?.join("commands");
        let mut files: Vec<PathBuf> = match fs::read_dir(&dir) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "cmd").unwrap_or(false))
                .collect(),
            Err(_) => return Ok(Vec::new()),
        };
        files.sort();
        let mut out = Vec::new();
        for f in files {
            if let Ok(text) = fs::read_to_string(&f) {
                if fs::remove_file(&f).is_ok() {
                    out.extend(
                        text.lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty()),
                    );
                }
            }
        }
        Ok(out)
    }

    // ---- verification pins -------------------------------------------------------

    fn verified_path(&self) -> PathBuf {
        self.home.join("verified.list")
    }

    pub fn is_pinned(&self, name: &str, version: &str, hash: &str) -> bool {
        let text = fs::read_to_string(self.verified_path()).unwrap_or_default();
        text.lines().any(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            p.len() >= 3 && p[0] == name && p[1] == version && p[2] == hash
        })
    }

    pub fn pin(&self, name: &str, version: &str, hash: &str) -> Result<(), String> {
        if self.is_pinned(name, version, hash) {
            return Ok(());
        }
        fs::create_dir_all(&self.home).map_err(|e| e.to_string())?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.verified_path())
            .map_err(|e| e.to_string())?;
        writeln!(f, "{name} {version} {hash}").map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub fn temp_store(tag: &str) -> Store {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let home = std::env::temp_dir().join(format!("proto-store-{tag}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    Store::new(home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ActionKind;

    fn meta(name: &str) -> Instance {
        Instance::new(
            name,
            "builtin",
            Mode::Approval,
            Network::Simulation,
            Scenario::Calm,
        )
    }

    #[test]
    fn instance_round_trip_and_listing() {
        let s = temp_store("rt");
        let mut m = meta("alpha");
        m.tick = 9;
        m.total_moved = 123;
        m.last_error = "x y".into();
        s.create(&m).unwrap();
        s.create(&meta("beta")).unwrap();
        assert_eq!(s.load_meta("alpha").unwrap(), m);
        assert_eq!(s.list(), vec!["alpha", "beta"]);
        assert_eq!(s.read_status("alpha"), Status::Created);
        s.write_status("alpha", Status::Paused).unwrap();
        assert_eq!(s.read_status("alpha"), Status::Paused);
        assert!(s.load_meta("nope").is_err());
        let _ = fs::remove_dir_all(&s.home);
    }

    #[test]
    fn rejects_unsafe_instance_names() {
        let s = temp_store("names");
        for bad in ["../x", "a/b", "", "A"] {
            assert!(s.instance_dir(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn pending_account_state_audit_persist() {
        let s = temp_store("persist");
        s.create(&meta("p")).unwrap();
        let pending = vec![Pending {
            id: 3,
            tick: 2,
            action: Action::new(ActionKind::Supply, "aave", "USDC", 5),
        }];
        s.save_pending("p", &pending).unwrap();
        let back = s.load_pending("p").unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, 3);
        assert_eq!(back[0].action.amount, 5);
        let mut acc = Account::default();
        acc.wallet.insert("USDC".into(), 7);
        s.save_account("p", &acc).unwrap();
        assert_eq!(s.load_account("p").unwrap().unwrap(), acc);
        let mut st = BTreeMap::new();
        st.insert("k".to_string(), "v".to_string());
        s.save_strategy_state("p", &st).unwrap();
        assert_eq!(s.load_strategy_state("p").unwrap(), st);
        s.append_audit("p", &["a".into(), "b\nc".into()]).unwrap();
        s.append_audit("p", &["d".into()]).unwrap();
        assert_eq!(s.read_audit("p").unwrap(), vec!["a", "b c", "d"]);
        let _ = fs::remove_dir_all(&s.home);
    }

    #[test]
    fn kill_switch_toggles() {
        let s = temp_store("kill");
        assert!(!s.kill_switch());
        s.set_kill_switch(true).unwrap();
        assert!(s.kill_switch());
        s.set_kill_switch(false).unwrap();
        assert!(!s.kill_switch());
        s.set_kill_switch(false).unwrap();
        let _ = fs::remove_dir_all(&s.home);
    }

    #[test]
    fn command_inbox_is_fifo_and_drains() {
        let s = temp_store("inbox");
        s.create(&meta("c")).unwrap();
        assert!(s.take_commands("c").unwrap().is_empty());
        s.push_command("c", "approve 1").unwrap();
        s.push_command("c", "reject 2").unwrap();
        s.push_command("c", "act a|b").unwrap();
        assert_eq!(
            s.take_commands("c").unwrap(),
            vec!["approve 1", "reject 2", "act a|b"]
        );
        assert!(s.take_commands("c").unwrap().is_empty());
        let _ = fs::remove_dir_all(&s.home);
    }

    #[test]
    fn pinning_is_exact() {
        let s = temp_store("pin");
        assert!(!s.is_pinned("x", "1", "abc"));
        s.pin("x", "1", "abc").unwrap();
        s.pin("x", "1", "abc").unwrap();
        assert!(s.is_pinned("x", "1", "abc"));
        assert!(!s.is_pinned("x", "2", "abc"), "new version is not verified");
        assert!(
            !s.is_pinned("x", "1", "abd"),
            "changed content is not verified"
        );
        let _ = fs::remove_dir_all(&s.home);
    }
}
