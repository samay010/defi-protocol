//! The `dashboard` command: a read-only local web page over the instance store.
//!
//! This file only reads. The server, its checks and the page live in `protocol-dashboard`.

use std::io::Write;
use std::time::Duration;

use protocol_core::amount::format_amount;
use protocol_core::store::Store;
use protocol_dashboard::json::{arr, Obj};
use protocol_dashboard::{Config, Data};

macro_rules! outln {
    ($w:expr, $($a:tt)*) => {{ let _ = writeln!($w, $($a)*); }};
}

/// Keep responses small however long an instance has been running.
const LOG_LINES: usize = 100;
const LOG_LINE_CHARS: usize = 300;
const STATE_ENTRIES: usize = 100;
const FIELD_CHARS: usize = 300;

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

pub struct StoreData<'a> {
    pub store: &'a Store,
}

impl StoreData<'_> {
    fn summary(&self, name: &str) -> Option<Obj> {
        let meta = self.store.load_meta(name).ok()?;
        let pending = self.store.load_pending(name).map(|p| p.len()).unwrap_or(0);
        Some(
            Obj::new()
                .str("name", name)
                .str("status", &crate::display_status(self.store, name))
                .str("mode", meta.mode.as_str())
                .str("network", meta.network.as_str())
                .num("tick", meta.tick)
                .num("pending", pending as u64)
                .str("strategy", &clip(&meta.strategy_ref, FIELD_CHARS))
                .str("last_error", &clip(&meta.last_error, FIELD_CHARS)),
        )
    }
}

impl Data for StoreData<'_> {
    fn overview(&self) -> String {
        let instances: Vec<String> = self
            .store
            .list()
            .iter()
            .filter_map(|n| self.summary(n))
            .map(Obj::finish)
            .collect();
        let strategies: Vec<String> = protocol_strategies::BUILTIN_NAMES
            .iter()
            .filter_map(|n| protocol_strategies::manifest(n))
            .map(|m| {
                let actions: Vec<String> = m
                    .allowed_actions
                    .iter()
                    .map(|a| protocol_dashboard::json::esc(a.as_str()))
                    .collect();
                Obj::new()
                    .str("name", &m.name)
                    .str("version", &m.version)
                    .raw("actions", &arr(&actions))
                    .str("description", &clip(&m.description, FIELD_CHARS))
                    .finish()
            })
            .collect();
        Obj::new()
            .bool("kill_switch", self.store.kill_switch())
            .raw("instances", &arr(&instances))
            .raw("strategies", &arr(&strategies))
            .finish()
    }

    fn instance(&self, name: &str) -> Option<String> {
        if !self.store.exists(name) {
            return None;
        }
        let meta = self.store.load_meta(name).ok()?;
        let mut account = Vec::new();
        if let Ok(Some(a)) = self.store.load_account(name) {
            for (kind, map) in [
                ("supplied", &a.supplied),
                ("borrowed", &a.borrowed),
                ("wallet", &a.wallet),
            ] {
                for (asset, v) in map {
                    account.push(
                        Obj::new()
                            .str("kind", kind)
                            .str("asset", &clip(asset, 40))
                            .str("amount", &format_amount(*v))
                            .finish(),
                    );
                }
            }
        }
        let pending: Vec<String> = self
            .store
            .load_pending(name)
            .unwrap_or_default()
            .iter()
            .map(|p| {
                Obj::new()
                    .num("id", p.id)
                    .num("tick", p.tick)
                    .str("action", &clip(&p.action.to_string(), FIELD_CHARS))
                    .finish()
            })
            .collect();
        let state: Vec<String> = self
            .store
            .load_strategy_state(name)
            .unwrap_or_default()
            .iter()
            .take(STATE_ENTRIES)
            .map(|(k, v)| {
                Obj::new()
                    .str("key", &clip(k, 80))
                    .str("value", &clip(v, FIELD_CHARS))
                    .finish()
            })
            .collect();
        let audit = self.store.read_audit(name).unwrap_or_default();
        let from = audit.len().saturating_sub(LOG_LINES);
        let log: Vec<String> = audit[from..]
            .iter()
            .map(|l| protocol_dashboard::json::esc(&clip(l, LOG_LINE_CHARS)))
            .collect();
        Some(
            Obj::new()
                .str("name", name)
                .str("status", &crate::display_status(self.store, name))
                .str("mode", meta.mode.as_str())
                .str("network", meta.network.as_str())
                .str("scenario", meta.scenario.as_str())
                .num("tick", meta.tick)
                .num("restarts", u64::from(meta.restarts))
                .num("total_moved", meta.total_moved)
                .str("strategy", &clip(&meta.strategy_ref, FIELD_CHARS))
                .str("last_error", &clip(&meta.last_error, FIELD_CHARS))
                .raw("account", &arr(&account))
                .raw("pending", &arr(&pending))
                .raw("state", &arr(&state))
                .raw("log", &arr(&log))
                .finish(),
        )
    }
}

/// 128 random bits from the operating system, hex encoded.
fn new_token() -> Result<String, String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("cannot read random bytes for the access token: {e} (the dashboard needs /dev/urandom, so it is unix-only)"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

pub fn cmd_dashboard(port: u16, store: &Store, out: &mut dyn Write) -> crate::R {
    let token = new_token()?;
    let listener = protocol_dashboard::bind(port).map_err(|e| {
        format!("cannot listen on 127.0.0.1:{port}: {e} (is another dashboard running? try --port)")
    })?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    outln!(
        out,
        "Dashboard (read-only) listening on 127.0.0.1:{port} only."
    );
    outln!(out, "Open this link in a browser on this device:");
    outln!(out, "  http://127.0.0.1:{port}/?t={token}");
    outln!(out, "The link is secret and changes every run. It cannot change anything: no approvals, no kill switch, no actions. Press Ctrl-C to stop.");
    let _ = out.flush();
    let cfg = Config {
        token,
        port,
        read_timeout: Duration::from_secs(5),
    };
    protocol_dashboard::serve(listener, &cfg, &StoreData { store }, None)
        .map_err(|e| format!("dashboard stopped: {e}"))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let home =
            std::env::temp_dir().join(format!("proto-dash-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        Store::new(home)
    }
    use protocol_core::store::Instance;
    use protocol_core::types::{Mode, Network, Scenario};
    use protocol_dashboard::json::is_valid;

    #[test]
    fn data_is_valid_json_and_inert() {
        let store = temp_store("dash-data");
        let evil = "<script>alert(1)</script>\"\\\u{2028}";
        let mut meta = Instance::new(
            "alpha",
            evil,
            Mode::Manual,
            Network::Simulation,
            Scenario::Calm,
        );
        meta.last_error = evil.to_string();
        store.create(&meta).unwrap();
        store
            .append_audit("alpha", &[evil.to_string(), "line two".into()])
            .unwrap();
        let d = StoreData { store: &store };
        let o = d.overview();
        assert!(is_valid(&o), "{o}");
        assert!(!o.contains('<') && !o.contains('\u{2028}'));
        assert!(o.contains("\"name\":\"alpha\"") && o.contains("stable-lending"));
        let i = d.instance("alpha").unwrap();
        assert!(is_valid(&i), "{i}");
        assert!(!i.contains('<') && !i.contains('\u{2028}'));
        assert!(i.contains("line two"));
        assert!(d.instance("ghost").is_none());
    }

    #[test]
    fn the_log_is_capped() {
        let store = temp_store("dash-cap");
        store
            .create(&Instance::new(
                "busy",
                "x",
                Mode::Manual,
                Network::Simulation,
                Scenario::Calm,
            ))
            .unwrap();
        let lines: Vec<String> = (0..500).map(|i| format!("entry {i}")).collect();
        store.append_audit("busy", &lines).unwrap();
        let i = StoreData { store: &store }.instance("busy").unwrap();
        assert!(i.contains("entry 499") && !i.contains("entry 399\""));
    }

    #[test]
    fn tokens_are_random_hex() {
        let a = new_token().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, new_token().unwrap());
    }
}
