//! `protocol market`: find and install strategies from a git-based index.
//!
//! The index is a text file (see `protocol_core::market`). Nothing here runs a strategy and
//! nothing is pinned: install fetches an exact commit, checks the content hash against the index,
//! and leaves the strategy UNVERIFIED for you to read and pin yourself.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use protocol_core::manifest::Manifest;
use protocol_core::market::{self, Entry, Fetch};
use protocol_core::store::Store;

use crate::args::MarketCmd;

macro_rules! outln {
    ($w:expr, $($a:tt)*) => {{ let _ = writeln!($w, $($a)*); }};
}

const GIT_TIMEOUT: Duration = Duration::from_secs(180);
const INDEX_FILE: &str = "index.txt";

/// Run git with the transport and hook features that could execute or reach anything else turned
/// off: only https, no hooks, no symlink checkout, no submodules, never prompt.
fn git(args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.symlinks=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "submodule.recurse=false",
        "-c",
        "advice.detachedHead=false",
    ])
    .args(args)
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_ASKPASS", "true")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git is not installed (needed to fetch from an https index or repository)".to_string()
        } else {
            format!("cannot start git: {e}")
        }
    })?;
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.take(64 * 1024).read_to_string(&mut s);
        }
        s
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git took longer than {} s and was stopped",
                    GIT_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("git: {e}")),
        }
    };
    let err = reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let tail: String = err
            .trim()
            .chars()
            .rev()
            .take(300)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        Err(format!("git failed: {tail}"))
    }
}

pub struct GitFetch;

impl Fetch for GitFetch {
    fn fetch(&self, repo: &str, commit: &str, dest: &Path) -> Result<(), String> {
        if !market::valid_repo_url(repo) {
            return Err(format!("refused repository URL '{repo}'"));
        }
        let d = dest.to_str().ok_or("destination path is not valid UTF-8")?;
        git(&["clone", "--quiet", "--no-checkout", "--", repo, d])?;
        git(&["-C", d, "checkout", "--quiet", "--detach", commit])
    }
}

fn temp_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "protocol-market-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ))
}

fn read_index_file(path: &Path) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a plain file", path.display()));
    }
    if meta.len() > market::MAX_BYTES as u64 {
        return Err(format!(
            "{} is larger than {} bytes",
            path.display(),
            market::MAX_BYTES
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The index from a local file, or from `index.txt` in an https git repository.
pub fn load_index(source: &str) -> Result<Vec<Entry>, String> {
    let text = if source.starts_with("https://") {
        if !market::valid_repo_url(source) {
            return Err(format!("refused index URL '{source}'"));
        }
        let dir = temp_dir();
        let d = dir
            .to_str()
            .ok_or("temporary path is not valid UTF-8")?
            .to_string();
        let got = git(&["clone", "--quiet", "--depth", "1", "--", source, &d])
            .and_then(|()| read_index_file(&dir.join(INDEX_FILE)));
        let _ = std::fs::remove_dir_all(&dir);
        got?
    } else {
        read_index_file(Path::new(source))?
    };
    market::parse_index(&text)
}

fn source_of(index: Option<String>) -> Result<String, crate::E> {
    index
        .or_else(|| std::env::var("PROTOCOL_MARKET_INDEX").ok())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            crate::E::Usage(
                "give --index <index file or https git URL> (or set PROTOCOL_MARKET_INDEX)".into(),
            )
        })
}

/// The manifest inside the fetched source must say the same thing the index does.
fn manifest_check(entry: &Entry) -> impl Fn(&Path) -> Result<(), String> + '_ {
    move |dir| {
        let text = std::fs::read_to_string(dir.join("strategy.toml"))
            .map_err(|e| format!("refused: no readable strategy.toml in the download: {e}"))?;
        let m = Manifest::parse(&text).map_err(|e| format!("refused: strategy.toml: {e}"))?;
        if m.is_builtin() {
            return Err("refused: external strategies cannot use a builtin: entry".into());
        }
        if m.name != entry.name
            || m.version != entry.version
            || m.language.as_str() != entry.language
        {
            return Err(format!(
                "refused: the index says {} {} ({}) but strategy.toml says {} {} ({})",
                entry.name,
                entry.version,
                entry.language,
                m.name,
                m.version,
                m.language.as_str()
            ));
        }
        Ok(())
    }
}

fn print_entry(out: &mut dyn Write, e: &Entry) {
    outln!(
        out,
        "{:<24} {:<9} {:<5} {}",
        e.name,
        e.version,
        e.language,
        e.description
    );
}

pub fn cmd_market(sub: MarketCmd, store: &Store, out: &mut dyn Write) -> crate::R {
    match sub {
        MarketCmd::Search { term, index } => {
            let entries = load_index(&source_of(index)?)?;
            let found = market::search(&entries, term.as_deref().unwrap_or(""));
            if found.is_empty() {
                outln!(
                    out,
                    "no matching strategies ({} listed in this index)",
                    entries.len()
                );
            }
            for e in &found {
                print_entry(out, e);
            }
            if !found.is_empty() {
                outln!(out, "\n{} found. Listed does not mean reviewed: read the source before you pin anything.", found.len());
            }
            Ok(0)
        }
        MarketCmd::Info {
            name,
            version,
            index,
        } => {
            let entries = load_index(&source_of(index)?)?;
            let e = market::find(&entries, &name, version.as_deref())
                .ok_or_else(|| format!("'{name}' is not in this index"))?;
            outln!(out, "name:         {}", e.name);
            outln!(out, "version:      {}", e.version);
            outln!(out, "language:     {}", e.language);
            outln!(out, "description:  {}", e.description);
            outln!(out, "repository:   {}", e.repo);
            outln!(out, "commit:       {}", e.commit);
            outln!(out, "content hash: {}", e.hash);
            outln!(out, "\nThe index only lists where to fetch this and what the source must hash to. It does not vouch for it.");
            Ok(0)
        }
        MarketCmd::Install {
            name,
            version,
            index,
            dir,
        } => {
            let entries = load_index(&source_of(index)?)?;
            let e = market::find(&entries, &name, version.as_deref())
                .ok_or_else(|| format!("'{name}' is not in this index"))?;
            if protocol_strategies::BUILTIN_NAMES.contains(&e.name.as_str()) {
                return Err(format!("'{}' is reserved for an official strategy; it cannot be installed from an index", e.name).into());
            }
            let parent = dir.unwrap_or_else(|| PathBuf::from("custom-strategies"));
            outln!(
                out,
                "fetching {} {} at commit {} ...",
                e.name,
                e.version,
                &e.commit[..12]
            );
            let _ = out.flush();
            let dest = market::install(e, &GitFetch, &parent, &manifest_check(e))?;
            outln!(out, "installed to {}", dest.display());
            outln!(out, "content hash matches the index: {}", e.hash);
            outln!(out, "VCS data and any build output in the download were removed; you build from this source.");
            let r = crate::resolve::resolve(store, &dest.to_string_lossy())?;
            outln!(out, "declared permissions:");
            crate::print_permissions(&r, out);
            outln!(out, "status: UNVERIFIED. Nothing was pinned or run.");
            outln!(
                out,
                "Read the source in {} and the permissions above, then: protocol verify {} --pin",
                dest.display(),
                e.name
            );
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn entry(name: &str, version: &str, lang: &str) -> Entry {
        Entry {
            name: name.into(),
            version: version.into(),
            language: lang.into(),
            hash: "0".repeat(64),
            repo: "https://github.com/a/b".into(),
            commit: "1".repeat(40),
            description: String::new(),
        }
    }

    fn manifest_dir(text: &str) -> PathBuf {
        let d = temp_dir();
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("strategy.toml"), text).unwrap();
        d
    }

    const OK: &str = "name = \"demo\"\nversion = \"1.0.0\"\nlanguage = \"c\"\nentry = \"sh strategy.sh\"\nprotocols = [\"aave\"]\n";

    #[test]
    fn the_manifest_must_match_the_index() {
        let d = manifest_dir(OK);
        assert!(manifest_check(&entry("demo", "1.0.0", "c"))(&d).is_ok());
        for e in [
            entry("other", "1.0.0", "c"),
            entry("demo", "2.0.0", "c"),
            entry("demo", "1.0.0", "rust"),
        ] {
            let err = manifest_check(&e)(&d).unwrap_err();
            assert!(err.contains("the index says"), "{err}");
        }
        let b = manifest_dir(&OK.replace("sh strategy.sh", "builtin:stable-lending"));
        assert!(manifest_check(&entry("demo", "1.0.0", "c"))(&b).is_err());
        let missing = temp_dir();
        fs::create_dir_all(&missing).unwrap();
        assert!(manifest_check(&entry("demo", "1.0.0", "c"))(&missing).is_err());
    }

    #[test]
    fn only_https_and_local_files_load_as_an_index() {
        assert!(load_index("http://github.com/a/b").is_err());
        assert!(load_index("https://localhost/a/b")
            .unwrap_err()
            .contains("refused"));
        assert!(load_index("https://user:pw@github.com/a/b")
            .unwrap_err()
            .contains("refused"));
        let missing = temp_dir().join("nope.txt");
        assert!(load_index(&missing.to_string_lossy()).is_err());
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        assert!(load_index(&dir.to_string_lossy())
            .unwrap_err()
            .contains("plain file"));
    }

    #[test]
    fn git_urls_with_option_look_are_refused_before_git_runs() {
        let r = GitFetch.fetch("--upload-pack=touch /tmp/x", &"1".repeat(40), &temp_dir());
        assert!(r.unwrap_err().contains("refused"));
    }
}
