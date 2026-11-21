//! OS sandbox for external strategy processes, built on `bubblewrap` (`bwrap`).
//!
//! `bwrap` is the unprivileged namespace sandbox used by Flatpak. We do not reimplement
//! isolation; we only build its command line. A sandboxed strategy:
//! - has **no network** (new network namespace with no interfaces),
//! - sees **no home directory and none of your files**: only a read-only view of the system
//!   directories (`/usr`, `/bin`, `/lib*`, a few `/etc` entries), its own directory (read-only),
//!   a private empty `/tmp`, and `/proc` + `/dev` of its own namespaces,
//! - runs in its own PID, IPC, UTS and user namespaces and dies with the supervisor.
//!
//! Policy is fail-closed: when the sandbox is required but cannot be started, the strategy does
//! not run. Linux only.
//!
//! This is defence for *untrusted strategy code*. It does not make a strategy correct, and a
//! kernel or `bwrap` vulnerability would defeat it. The risk engine still checks every action.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    /// Run strategies inside `bwrap`; refuse to run them if it is unavailable.
    Require,
    /// No sandbox: the strategy runs with the user's own file and network access.
    Off,
}

impl SandboxMode {
    pub fn parse(s: &str) -> Option<SandboxMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "require" | "on" => Some(SandboxMode::Require),
            "off" => Some(SandboxMode::Off),
            _ => None,
        }
    }
}

/// Where `bwrap` may live. A fixed list, not `$PATH`, so a planted binary cannot be picked up.
const BWRAP_LOCATIONS: [&str; 3] = ["/usr/bin/bwrap", "/bin/bwrap", "/usr/local/bin/bwrap"];

pub fn find_bwrap() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    BWRAP_LOCATIONS
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

pub const INSTALL_HINT: &str = "install it with `sudo apt install bubblewrap` (Debian/Ubuntu) or your distribution's package, \
or pass `--sandbox off` to run the strategy with your user's full file and network access";

/// The `bwrap` options (everything before `--`) for a strategy living in `dir`.
/// `dir` must be an absolute, canonical path.
pub fn bwrap_args(dir: &Path) -> Vec<String> {
    let s = |x: &str| x.to_string();
    let mut a = vec![
        s("--die-with-parent"),
        s("--new-session"),
        s("--unshare-all"),
    ];

    // System directories: symlinks (merged /usr) are recreated, real directories bound read-only.
    for p in [
        "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32",
    ] {
        match fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                if let Ok(target) = fs::read_link(p) {
                    a.extend([s("--symlink"), target.to_string_lossy().into_owned(), s(p)]);
                }
            }
            Ok(m) if m.is_dir() => a.extend([s("--ro-bind"), s(p), s(p)]),
            _ => {}
        }
    }

    // The few /etc entries a dynamic loader or a JVM needs. Never all of /etc.
    let mut etc: Vec<String> = ["ld.so.cache", "ld.so.conf", "ld.so.conf.d", "alternatives"]
        .iter()
        .map(|n| format!("/etc/{n}"))
        .collect();
    if let Ok(rd) = fs::read_dir("/etc") {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("java") {
                etc.push(format!("/etc/{name}"));
            }
        }
    }
    etc.sort();
    for p in etc {
        if Path::new(&p).exists() {
            a.extend([s("--ro-bind"), p.clone(), p]);
        }
    }

    a.extend([
        s("--proc"),
        s("/proc"),
        s("--dev"),
        s("/dev"),
        s("--tmpfs"),
        s("/tmp"),
    ]);
    let d = dir.to_string_lossy().into_owned();
    a.extend([s("--ro-bind"), d.clone(), d.clone(), s("--chdir"), d]);
    a
}

/// Locate `bwrap` and prove it works here (user namespaces can be disabled by the host).
pub fn prepare(mode: SandboxMode, dir: &Path) -> Result<Option<PathBuf>, String> {
    match mode {
        SandboxMode::Off => Ok(None),
        SandboxMode::Require => {
            let bwrap = find_bwrap().ok_or_else(|| {
                format!("sandbox required but `bwrap` (bubblewrap) was not found: {INSTALL_HINT}")
            })?;
            let out = Command::new(&bwrap)
                .args(bwrap_args(dir))
                .args(["--", "/bin/sh", "-c", "exit 0"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("cannot start {}: {e}", bwrap.display()))?;
            if !out.status.success() {
                return Err(format!(
                    "the sandbox could not start ({}): {}. The host may forbid unprivileged user namespaces; \
or pass `--sandbox off` to run strategies without a sandbox",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(Some(bwrap))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing() {
        assert_eq!(SandboxMode::parse("require"), Some(SandboxMode::Require));
        assert_eq!(SandboxMode::parse("ON"), Some(SandboxMode::Require));
        assert_eq!(SandboxMode::parse("off"), Some(SandboxMode::Off));
        assert_eq!(SandboxMode::parse("maybe"), None);
    }

    #[test]
    fn args_isolate_network_and_hide_the_home() {
        let args = bwrap_args(Path::new("/tmp/strategies/demo"));
        for must in ["--die-with-parent", "--unshare-all", "--new-session"] {
            assert!(args.iter().any(|a| a == must), "missing {must}");
        }
        assert!(
            !args.iter().any(|a| a == "--share-net"),
            "network must stay unshared"
        );
        for forbidden in ["/home", "/root", "/etc/shadow", "/var", "/"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "{forbidden} must not be exposed"
            );
        }
        // the strategy directory is bound read-only, at the same path, and is the working directory
        let i = args.iter().position(|a| a == "--ro-bind").and_then(|_| {
            args.windows(3).position(|w| {
                w[0] == "--ro-bind"
                    && w[1] == "/tmp/strategies/demo"
                    && w[2] == "/tmp/strategies/demo"
            })
        });
        assert!(i.is_some(), "{args:?}");
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--chdir" && w[1] == "/tmp/strategies/demo"));
        // the private /tmp comes before the directory bind so the bind is not hidden
        let tmpfs = args.iter().position(|a| a == "--tmpfs").unwrap();
        assert!(tmpfs < i.unwrap());
    }

    #[test]
    fn only_a_few_etc_entries_are_exposed() {
        let args = bwrap_args(Path::new("/tmp/x"));
        for a in args.iter().filter(|a| a.starts_with("/etc")) {
            assert!(
                a.starts_with("/etc/ld.so")
                    || a.starts_with("/etc/alternatives")
                    || a.starts_with("/etc/java"),
                "unexpected /etc exposure: {a}"
            );
        }
    }

    #[test]
    fn off_needs_nothing_and_require_fails_closed_without_bwrap() {
        assert_eq!(prepare(SandboxMode::Off, Path::new("/tmp")), Ok(None));
        if find_bwrap().is_none() {
            let e = prepare(SandboxMode::Require, Path::new("/tmp")).unwrap_err();
            assert!(
                e.contains("bubblewrap") && e.contains("--sandbox off"),
                "{e}"
            );
        }
    }
}
