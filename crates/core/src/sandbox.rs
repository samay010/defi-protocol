//! OS sandbox for external strategy processes, built on `bubblewrap` (`bwrap`).
//!
//! `bwrap` is the unprivileged namespace sandbox used by Flatpak. We do not reimplement
//! isolation; we only build its command line. A sandboxed strategy:
//! - has **no network** (new network namespace with no interfaces),
//! - sees **no home directory and none of your files**: only a read-only view of the system
//!   directories (`/usr`, `/bin`, `/lib*`, a few `/etc` entries), its own directory (read-only),
//!   a private empty `/tmp`, and `/proc` + `/dev` of its own namespaces,
//! - runs in its own PID, IPC, UTS and user namespaces and dies with the supervisor,
//! - cannot call a deny-list of dangerous syscalls (ptrace, mount, bpf, io_uring, ...), see
//!   [`crate::seccomp`].
//!
//! A **build** (`protocol build`) gets the same file isolation but keeps the network (compilers
//! fetch dependencies) and may write to its own directory; see [`bwrap_build_args`].
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

fn s(x: &str) -> String {
    x.to_string()
}

/// Read-only system directories: symlinks (merged /usr) are recreated, real directories bound.
fn push_system_mounts(a: &mut Vec<String>) {
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
}

/// The few /etc entries a dynamic loader or a JVM needs (plus `extra`). Never all of /etc.
fn push_etc_mounts(a: &mut Vec<String>, extra: &[&str]) {
    let mut etc: Vec<String> = ["ld.so.cache", "ld.so.conf", "ld.so.conf.d", "alternatives"]
        .iter()
        .chain(extra.iter())
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
    etc.dedup();
    for p in etc {
        if Path::new(&p).exists() {
            a.extend([s("--ro-bind"), p.clone(), p]);
        }
    }
}

fn push_proc_dev_tmp(a: &mut Vec<String>) {
    a.extend([
        s("--proc"),
        s("/proc"),
        s("--dev"),
        s("/dev"),
        s("--tmpfs"),
        s("/tmp"),
    ]);
}

/// The `bwrap` options (everything before `--`) for a strategy living in `dir`.
/// `dir` must be an absolute, canonical path.
pub fn bwrap_args(dir: &Path) -> Vec<String> {
    let mut a = vec![
        s("--die-with-parent"),
        s("--new-session"),
        s("--unshare-all"),
    ];
    push_system_mounts(&mut a);
    push_etc_mounts(&mut a, &[]);
    push_proc_dev_tmp(&mut a);
    let d = dir.to_string_lossy().into_owned();
    a.extend([s("--ro-bind"), d.clone(), d.clone(), s("--chdir"), d]);
    a
}

/// /etc entries a build needs to resolve names and verify TLS certificates (package downloads).
const BUILD_ETC: [&str; 6] = [
    "resolv.conf",
    "hosts",
    "nsswitch.conf",
    "host.conf",
    "ssl",
    "ca-certificates",
];

/// What a sandboxed build needs besides the system directories.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildEnv {
    /// Toolchain directories bound read-only at the same path (e.g. `~/.cargo/bin`, `~/.rustup`).
    pub toolchain_ro: Vec<PathBuf>,
    /// The complete environment of the build (the host environment is not inherited).
    pub env: Vec<(String, String)>,
}

/// Where the build's private home lives: inside the sandbox's own `/tmp`, gone afterwards.
const BUILD_HOME: &str = "/tmp/home";

/// Work out the toolchain mounts and environment for a build. Pure: the caller passes the host
/// values. Only the Rust toolchain lives outside the system directories; compilers for the other
/// languages come from `/usr`.
pub fn build_environment(
    home: Option<&Path>,
    cargo_home: Option<&Path>,
    rustup_home: Option<&Path>,
) -> BuildEnv {
    let cargo = cargo_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".cargo")));
    let rustup = rustup_home
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".rustup")));
    let mut toolchain_ro = Vec::new();
    let mut path = vec![];
    if let Some(c) = cargo.map(|c| c.join("bin")).filter(|c| c.is_dir()) {
        path.push(c.to_string_lossy().into_owned());
        toolchain_ro.push(c);
    }
    let mut env = vec![
        (s("HOME"), s(BUILD_HOME)),
        (s("CARGO_HOME"), format!("{BUILD_HOME}/.cargo")),
        (s("TMPDIR"), s("/tmp")),
    ];
    if let Some(r) = rustup.filter(|r| r.is_dir()) {
        env.push((s("RUSTUP_HOME"), r.to_string_lossy().into_owned()));
        toolchain_ro.push(r);
    }
    path.extend(["/usr/local/bin", "/usr/bin", "/bin"].map(String::from));
    env.push((s("PATH"), path.join(":")));
    BuildEnv { toolchain_ro, env }
}

/// `bwrap` options for a **build** of the strategy in `dir`. Differences from a run: the network
/// is shared (compilers fetch dependencies), the strategy directory is writable (build output),
/// and the toolchain is visible read-only. Still no view of your home or any other file.
pub fn bwrap_build_args(dir: &Path, be: &BuildEnv) -> Vec<String> {
    let mut a = vec![
        s("--die-with-parent"),
        s("--new-session"),
        s("--unshare-all"),
        s("--share-net"),
    ];
    push_system_mounts(&mut a);
    push_etc_mounts(&mut a, &BUILD_ETC);
    push_proc_dev_tmp(&mut a);
    a.extend([s("--dir"), s(BUILD_HOME)]);
    for t in &be.toolchain_ro {
        let t = t.to_string_lossy().into_owned();
        a.extend([s("--ro-bind"), t.clone(), t]);
    }
    let d = dir.to_string_lossy().into_owned();
    a.extend([s("--bind"), d.clone(), d.clone(), s("--chdir"), d]);
    a.push(s("--clearenv"));
    for (k, v) in &be.env {
        a.extend([s("--setenv"), k.clone(), v.clone()]);
    }
    a
}

/// A usable sandbox: the `bwrap` binary plus the seccomp filter file it loads. The filter file
/// is removed when this value is dropped.
#[derive(Debug)]
pub struct Sandbox {
    bwrap: PathBuf,
    filter: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.filter);
    }
}

/// `sh` helper that opens the filter file as fd 3 and then execs the rest of its arguments.
const FD3_LAUNCHER: &str = "f=$1; shift; exec \"$@\" 3<\"$f\"";

impl Sandbox {
    /// A command that starts `bwrap` with the seccomp filter attached (`--seccomp 3`). The caller
    /// adds the isolation options, `--`, and the program to run.
    pub fn command(&self) -> Command {
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(FD3_LAUNCHER)
            .arg("sh")
            .arg(&self.filter)
            .arg(&self.bwrap)
            .args(["--seccomp", "3"]);
        c
    }

    pub fn bwrap(&self) -> &Path {
        &self.bwrap
    }
}

#[cfg(target_os = "linux")]
fn write_filter() -> Result<PathBuf, String> {
    crate::seccomp::write_filter_file()
}

#[cfg(not(target_os = "linux"))]
fn write_filter() -> Result<PathBuf, String> {
    Err("the sandbox is only available on Linux".into())
}

/// Shown instead of the install hint on systems where bubblewrap cannot exist.
const NO_SANDBOX_HERE: &str = "the OS sandbox (bubblewrap) is Linux-only and this system has none yet, so the strategy was not run; \
pass `--sandbox off` to run it with your user's full file and network access, knowingly (see docs/security.md)";

fn probe(args: Vec<String>) -> Result<Sandbox, String> {
    if !cfg!(target_os = "linux") {
        return Err(NO_SANDBOX_HERE.to_string());
    }
    let bwrap = find_bwrap().ok_or_else(|| {
        format!("sandbox required but `bwrap` (bubblewrap) was not found: {INSTALL_HINT}")
    })?;
    let filter = write_filter()?;
    let sb = Sandbox { bwrap, filter };
    let out = sb
        .command()
        .args(args)
        .args(["--", "/bin/sh", "-c", "exit 0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot start {}: {e}", sb.bwrap.display()))?;
    if !out.status.success() {
        return Err(format!(
            "the sandbox could not start ({}): {}. The host may forbid unprivileged user namespaces; \
or pass `--sandbox off` to run without a sandbox",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(sb)
}

/// Locate `bwrap` and prove it works here (user namespaces can be disabled by the host).
pub fn prepare(mode: SandboxMode, dir: &Path) -> Result<Option<Sandbox>, String> {
    match mode {
        SandboxMode::Off => Ok(None),
        SandboxMode::Require => probe(bwrap_args(dir)).map(Some),
    }
}

/// Like [`prepare`], for a build (network on, strategy directory writable).
pub fn prepare_build(
    mode: SandboxMode,
    dir: &Path,
    be: &BuildEnv,
) -> Result<Option<Sandbox>, String> {
    match mode {
        SandboxMode::Off => Ok(None),
        SandboxMode::Require => probe(bwrap_build_args(dir, be)).map(Some),
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
        assert!(matches!(
            prepare(SandboxMode::Off, Path::new("/tmp")),
            Ok(None)
        ));
        if find_bwrap().is_none() {
            let e = prepare(SandboxMode::Require, Path::new("/tmp")).unwrap_err();
            assert!(
                e.contains("bubblewrap") && e.contains("--sandbox off"),
                "{e}"
            );
        }
    }

    #[test]
    fn build_shares_network_but_nothing_else() {
        let be = BuildEnv {
            toolchain_ro: vec![PathBuf::from("/nonexistent-home/.cargo/bin")],
            env: vec![("HOME".into(), "/tmp/home".into())],
        };
        let args = bwrap_build_args(Path::new("/work/strat"), &be);
        let pos = |x: &str| args.iter().position(|a| a == x).unwrap();
        // network is re-shared after everything is unshared
        assert!(pos("--share-net") > pos("--unshare-all"));
        // the strategy dir is writable (--bind, never --ro-bind) and is the working directory
        assert!(args
            .windows(3)
            .any(|w| w[0] == "--bind" && w[1] == "/work/strat" && w[2] == "/work/strat"));
        assert!(!args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == "/work/strat"));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--chdir" && w[1] == "/work/strat"));
        // the toolchain is read-only
        assert!(args
            .windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == "/nonexistent-home/.cargo/bin"));
        // the host environment is dropped before the build's own is set
        assert!(pos("--clearenv") < pos("--setenv"));
        // a private home inside the private /tmp, created after the tmpfs
        let home = args.iter().position(|a| a == "/tmp/home").unwrap();
        assert!(pos("--tmpfs") < home);
        for forbidden in ["/home", "/root", "/etc/shadow", "/var", "/"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "{forbidden} must not be exposed"
            );
        }
    }

    #[test]
    fn build_etc_entries_are_a_short_allowlist() {
        let args = bwrap_build_args(Path::new("/work/x"), &BuildEnv::default());
        for a in args.iter().filter(|a| a.starts_with("/etc")) {
            assert!(
                a.starts_with("/etc/ld.so")
                    || a.starts_with("/etc/alternatives")
                    || a.starts_with("/etc/java")
                    || BUILD_ETC.iter().any(|n| *a == format!("/etc/{n}")),
                "unexpected /etc exposure: {a}"
            );
        }
        // a run never gets the network files
        let run = bwrap_args(Path::new("/work/x"));
        assert!(!run.iter().any(|a| a == "/etc/resolv.conf"));
    }

    #[test]
    fn build_environment_is_explicit_and_private() {
        let d = std::env::temp_dir().join(format!("bwrap-env-{}", std::process::id()));
        fs::create_dir_all(d.join(".cargo/bin")).unwrap();
        fs::create_dir_all(d.join(".rustup")).unwrap();
        let be = build_environment(Some(&d), None, None);
        assert_eq!(
            be.toolchain_ro,
            vec![d.join(".cargo/bin"), d.join(".rustup")]
        );
        let get = |k: &str| {
            be.env
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("HOME"), "/tmp/home");
        assert_eq!(get("CARGO_HOME"), "/tmp/home/.cargo");
        assert_eq!(get("RUSTUP_HOME"), d.join(".rustup").to_string_lossy());
        assert!(get("PATH").starts_with(&*d.join(".cargo/bin").to_string_lossy()));
        // nothing from the host environment leaks in
        assert!(be.env.iter().all(|(k, _)| matches!(
            k.as_str(),
            "HOME" | "CARGO_HOME" | "TMPDIR" | "RUSTUP_HOME" | "PATH"
        )));
        // missing toolchain directories are simply not mounted
        let none = build_environment(Some(Path::new("/definitely/not/here")), None, None);
        assert!(none.toolchain_ro.is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
