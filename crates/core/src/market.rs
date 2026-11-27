//! The strategy index ("marketplace"): a plain-text list of where to fetch strategies and the
//! exact content each one must have.
//!
//! The index is only a list of claims. It never makes a strategy trusted: an install checks the
//! content hash against the index, and the user still reviews the source and pins it themselves.
//! Parsing is strict and bounded because the index may come from anywhere.
//!
//! Format, one entry per line (blank lines and `#` comments ignored):
//! `name | version | language | sha256 | repo | commit | description`

pub const HEADER: &str = "# protocol-market-index v1";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 10_000;
const LANGUAGES: [&str; 4] = ["rust", "c", "cpp", "wasm"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub version: String,
    pub language: String,
    /// Content hash of the strategy source, as `protocol verify` prints it.
    pub hash: String,
    /// `https://` git repository.
    pub repo: String,
    /// Full 40-character commit id: a branch or tag could move, a commit cannot.
    pub commit: String,
    pub description: String,
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !n.starts_with(['-', '_'])
}

fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 32
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

/// An `https://` URL made of plain characters only: no credentials, no query, no spaces, no
/// leading dash (so it can never be read as a git option), no local or private-looking host.
pub fn valid_repo_url(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else {
        return false;
    };
    if u.len() > 300 || rest.is_empty() {
        return false;
    }
    if !u
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~' | b'/' | b':'))
    {
        return false;
    }
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host_port.split(':').next().unwrap_or("");
    if host.is_empty() || path.is_empty() || path.contains("..") || path.contains("//") {
        return false;
    }
    let h = host.to_ascii_lowercase();
    let numeric = h.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    !(h == "localhost" || h.ends_with(".local") || h.ends_with(".internal") || numeric)
        && h.contains('.')
        && !h.starts_with(['.', '-'])
        && !h.ends_with(['.', '-'])
}

pub fn parse_line(line: &str) -> Result<Entry, String> {
    let parts: Vec<&str> = line.split('|').map(str::trim).collect();
    if parts.len() != 7 {
        return Err(format!(
            "expected 7 fields separated by '|', found {}",
            parts.len()
        ));
    }
    let e = Entry {
        name: parts[0].to_string(),
        version: parts[1].to_string(),
        language: parts[2].to_string(),
        hash: parts[3].to_string(),
        repo: parts[4].to_string(),
        commit: parts[5].to_string(),
        description: parts[6].to_string(),
    };
    if !valid_name(&e.name) {
        return Err(format!("bad name '{}'", e.name));
    }
    if !valid_version(&e.version) {
        return Err(format!("bad version '{}'", e.version));
    }
    if !LANGUAGES.contains(&e.language.as_str()) {
        return Err(format!(
            "bad language '{}' (rust, c, cpp or wasm)",
            e.language
        ));
    }
    if !is_hex(&e.hash, 64) {
        return Err("hash must be 64 lowercase hex characters".into());
    }
    if !valid_repo_url(&e.repo) {
        return Err(format!(
            "bad repo '{}' (plain https:// git URL on a public host)",
            e.repo
        ));
    }
    if !is_hex(&e.commit, 40) {
        return Err("commit must be a full 40-character lowercase hex commit id".into());
    }
    if e.description.chars().count() > 200 || e.description.chars().any(char::is_control) {
        return Err("description must be at most 200 characters, no control characters".into());
    }
    Ok(e)
}

/// Parse a whole index. Any bad line rejects the entire index, so a half-understood list is never
/// used.
pub fn parse_index(text: &str) -> Result<Vec<Entry>, String> {
    if text.len() > MAX_BYTES {
        return Err(format!("index is larger than {MAX_BYTES} bytes"));
    }
    if text.lines().next().map(str::trim) != Some(HEADER) {
        return Err(format!("first line must be exactly '{HEADER}'"));
    }
    let mut out: Vec<Entry> = Vec::new();
    for (i, line) in text.lines().enumerate().skip(1) {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let e = parse_line(t).map_err(|m| format!("line {}: {m}", i + 1))?;
        if out
            .iter()
            .any(|o| o.name == e.name && o.version == e.version)
        {
            return Err(format!(
                "line {}: duplicate {} {}",
                i + 1,
                e.name,
                e.version
            ));
        }
        out.push(e);
        if out.len() > MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} entries"));
        }
    }
    Ok(out)
}

/// Entries whose name or description contains `term` (case-insensitive); all when empty.
pub fn search<'a>(entries: &'a [Entry], term: &str) -> Vec<&'a Entry> {
    let t = term.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|e| {
            t.is_empty() || e.name.contains(&t) || e.description.to_ascii_lowercase().contains(&t)
        })
        .collect()
}

/// The entry for `name`; the highest listed version when `version` is `None`, by plain
/// version-string order of the numeric parts.
pub fn find<'a>(entries: &'a [Entry], name: &str, version: Option<&str>) -> Option<&'a Entry> {
    let key = |v: &str| -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    entries
        .iter()
        .filter(|e| e.name == name && version.is_none_or(|v| e.version == v))
        .max_by_key(|e| key(&e.version))
}

use std::fs;
use std::path::{Path, PathBuf};

use crate::hash::{hash_dir, SKIP_DIRS};

const MAX_FILES: usize = 5_000;
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_DEPTH: usize = 20;

/// Gets the files of `repo` at exactly `commit` into the (not yet existing) directory `dest`.
pub trait Fetch {
    fn fetch(&self, repo: &str, commit: &str, dest: &Path) -> Result<(), String>;
}

/// Remove what the content hash does not cover (VCS data, build output, caches) and refuse
/// anything that is not a plain file or directory. Build output must never be taken from a
/// download: it would run without ever being part of the reviewed source.
fn sanitize(dir: &Path, depth: usize, files: &mut usize, bytes: &mut u64) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("directories nested deeper than {MAX_DEPTH}"));
    }
    let rd = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| e.to_string())?;
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if ft.is_symlink() {
            return Err(format!("refused: '{name}' is a symbolic link"));
        }
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                fs::remove_dir_all(&path)
                    .map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
            } else {
                sanitize(&path, depth + 1, files, bytes)?;
            }
        } else if ft.is_file() {
            *files += 1;
            *bytes += entry.metadata().map_err(|e| e.to_string())?.len();
            if *files > MAX_FILES || *bytes > MAX_TOTAL_BYTES {
                return Err(format!(
                    "refused: more than {MAX_FILES} files or {} MiB",
                    MAX_TOTAL_BYTES / 1024 / 1024
                ));
            }
        } else {
            return Err(format!(
                "refused: '{name}' is not a plain file or directory"
            ));
        }
    }
    Ok(())
}

fn stage_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        ".market-stage-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

/// Fetch, sanitize, verify against the index and only then move into `parent/<name>`.
/// Nothing is installed unless every check passes. `check` runs on the sanitized directory
/// (the caller validates the manifest there). The result is never pinned: that stays a human step.
pub fn install(
    entry: &Entry,
    fetch: &dyn Fetch,
    parent: &Path,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let dest = parent.join(&entry.name);
    if fs::symlink_metadata(&dest).is_ok() {
        return Err(format!(
            "{} already exists; remove or rename it first (nothing was changed)",
            dest.display()
        ));
    }
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let stage = parent.join(stage_name());
    fs::create_dir(&stage).map_err(|e| format!("cannot create {}: {e}", stage.display()))?;
    let result = (|| {
        let src = stage.join("src");
        fetch.fetch(&entry.repo, &entry.commit, &src)?;
        let (mut files, mut bytes) = (0usize, 0u64);
        sanitize(&src, 0, &mut files, &mut bytes)?;
        let got = hash_dir(&src)?;
        if got != entry.hash {
            return Err(format!(
                "refused: content hash does not match the index\n  index: {}\n  fetched: {got}",
                entry.hash
            ));
        }
        check(&src)?;
        fs::rename(&src, &dest).map_err(|e| format!("cannot move into {}: {e}", dest.display()))
    })();
    let _ = fs::remove_dir_all(&stage);
    result.map(|()| dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    fn line(name: &str, ver: &str) -> String {
        format!("{name} | {ver} | rust | {H} | https://github.com/a/b | {C} | Watches things")
    }

    fn index(lines: &[String]) -> String {
        format!("{HEADER}\n# comment\n\n{}\n", lines.join("\n"))
    }

    #[test]
    fn a_good_index_parses() {
        let e = parse_index(&index(&[line("one", "1.0.0"), line("two", "0.2.0")])).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].name, "one");
        assert_eq!(e[0].hash, H);
        assert_eq!(e[1].repo, "https://github.com/a/b");
    }

    #[test]
    fn header_is_required_and_bad_lines_reject_everything() {
        assert!(parse_index("").is_err());
        assert!(parse_index(&line("one", "1.0.0")).is_err());
        let bad = index(&[line("one", "1.0.0"), "garbage".to_string()]);
        assert!(parse_index(&bad).unwrap_err().contains("line"));
    }

    #[test]
    fn every_field_is_validated() {
        let ok = line("one", "1.0.0");
        for (from, to) in [
            ("one |", "One |"),
            ("one |", "-one |"),
            ("one |", " |"),
            ("1.0.0", "1 0"),
            ("rust", "python"),
            (H, &H[..63]),
            (H, &H.to_uppercase()),
            (C, &C[..39]),
            ("https://github.com/a/b", "http://github.com/a/b"),
            ("https://github.com/a/b", "--upload-pack=x"),
            ("https://github.com/a/b", "https://user:pw@github.com/a/b"),
            ("https://github.com/a/b", "https://github.com/a/b?x=1"),
            ("https://github.com/a/b", "https://github.com/a/../b"),
            ("https://github.com/a/b", "https://localhost/a/b"),
            ("https://github.com/a/b", "https://127.0.0.1/a/b"),
            ("https://github.com/a/b", "https://10.0.0.1/a/b"),
            ("https://github.com/a/b", "https://nodot/a/b"),
            ("https://github.com/a/b", "https://github.com"),
            ("Watches things", "a\u{7}b"),
        ] {
            let bad = ok.replacen(from, to, 1);
            assert!(parse_line(&bad).is_err(), "should reject: {bad}");
        }
        let long = ok.replace("Watches things", &"x".repeat(201));
        assert!(parse_line(&long).is_err());
        assert!(parse_line(&format!("{ok} | extra")).is_err());
    }

    #[test]
    fn duplicates_and_size_are_limited() {
        let dup = index(&[line("one", "1.0.0"), line("one", "1.0.0")]);
        assert!(parse_index(&dup).unwrap_err().contains("duplicate"));
        assert!(parse_index(&index(&[line("one", "1.0.0"), line("one", "1.1.0")])).is_ok());
        let huge = format!("{HEADER}\n{}", "#".repeat(MAX_BYTES));
        assert!(parse_index(&huge).is_err());
    }

    #[test]
    fn search_and_find() {
        let e = parse_index(&index(&[
            line("alpha", "1.2.0"),
            line("alpha", "1.10.0"),
            line("beta", "0.1.0"),
        ]))
        .unwrap();
        assert_eq!(search(&e, "").len(), 3);
        assert_eq!(search(&e, "ALPHA").len(), 2);
        assert_eq!(search(&e, "watches").len(), 3);
        assert!(search(&e, "zzz").is_empty());
        assert_eq!(find(&e, "alpha", None).unwrap().version, "1.10.0");
        assert_eq!(find(&e, "alpha", Some("1.2.0")).unwrap().version, "1.2.0");
        assert!(find(&e, "alpha", Some("9.9.9")).is_none());
        assert!(find(&e, "gamma", None).is_none());
    }

    use std::cell::RefCell;

    fn tmp(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "proto-market-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    struct Fake {
        extra: Box<dyn Fn(&Path)>,
        seen: RefCell<Vec<(String, String)>>,
    }

    impl Fake {
        fn new(extra: impl Fn(&Path) + 'static) -> Fake {
            Fake {
                extra: Box::new(extra),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Fetch for Fake {
        fn fetch(&self, repo: &str, commit: &str, dest: &Path) -> Result<(), String> {
            self.seen.borrow_mut().push((repo.into(), commit.into()));
            fs::create_dir_all(dest).unwrap();
            fs::write(dest.join("strategy.toml"), "name = \"x\"\n").unwrap();
            fs::write(dest.join("main.rs"), "fn main() {}\n").unwrap();
            fs::create_dir_all(dest.join(".git")).unwrap();
            fs::write(dest.join(".git/config"), "secret").unwrap();
            (self.extra)(dest);
            Ok(())
        }
    }

    fn entry_for(fake: &Fake) -> Entry {
        let probe = tmp("probe");
        fake.fetch("https://github.com/a/b", C, &probe.join("s"))
            .unwrap();
        let src = probe.join("s");
        let mut f = 0;
        let mut b = 0;
        sanitize(&src, 0, &mut f, &mut b).unwrap();
        let hash = hash_dir(&src).unwrap();
        let _ = fs::remove_dir_all(&probe);
        Entry {
            name: "x".into(),
            version: "1.0.0".into(),
            language: "rust".into(),
            hash,
            repo: "https://github.com/a/b".into(),
            commit: C.into(),
            description: String::new(),
        }
    }

    fn leftovers(parent: &Path) -> Vec<String> {
        fs::read_dir(parent)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn install_moves_verified_source_and_strips_vcs_and_build_output() {
        // the fetched tree also carries a prebuilt binary the hash does not cover
        let fake = Fake::new(|d| {
            fs::create_dir_all(d.join("target/release")).unwrap();
            fs::write(d.join("target/release/evil"), "binary").unwrap();
        });
        let e = entry_for(&fake);
        let parent = tmp("ok");
        let dest = install(&e, &fake, &parent, &|_| Ok(())).unwrap();
        assert_eq!(dest, parent.join("x"));
        assert!(dest.join("main.rs").is_file());
        assert!(!dest.join(".git").exists() && !dest.join("target").exists());
        assert_eq!(hash_dir(&dest).unwrap(), e.hash);
        assert_eq!(leftovers(&parent), vec!["x".to_string()]);
        assert_eq!(
            fake.seen.borrow()[0],
            ("https://github.com/a/b".to_string(), C.to_string())
        );
    }

    #[test]
    fn a_hash_mismatch_installs_nothing() {
        let fake = Fake::new(|_| {});
        let mut e = entry_for(&fake);
        e.hash = "0".repeat(64);
        let parent = tmp("mismatch");
        let err = install(&e, &fake, &parent, &|_| Ok(())).unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(leftovers(&parent).is_empty());
    }

    #[test]
    fn a_failed_check_installs_nothing() {
        let fake = Fake::new(|_| {});
        let e = entry_for(&fake);
        let parent = tmp("check");
        let err = install(&e, &fake, &parent, &|_| {
            Err("manifest says another name".into())
        })
        .unwrap_err();
        assert!(err.contains("another name"));
        assert!(leftovers(&parent).is_empty());
    }

    #[test]
    fn an_existing_destination_is_never_touched() {
        let fake = Fake::new(|_| {});
        let e = entry_for(&fake);
        fake.seen.borrow_mut().clear();
        let parent = tmp("exists");
        fs::create_dir_all(parent.join("x")).unwrap();
        fs::write(parent.join("x/mine.txt"), "keep").unwrap();
        let err = install(&e, &fake, &parent, &|_| Ok(())).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(
            fs::read_to_string(parent.join("x/mine.txt")).unwrap(),
            "keep"
        );
        assert!(fake.seen.borrow().is_empty(), "nothing was even fetched");
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_refused() {
        let fake =
            Fake::new(|d| std::os::unix::fs::symlink("/etc/passwd", d.join("link")).unwrap());
        let parent = tmp("link");
        let mut e = entry_for(&Fake::new(|_| {}));
        e.hash = "1".repeat(64);
        let err = install(&e, &fake, &parent, &|_| Ok(())).unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");
        assert!(leftovers(&parent).is_empty());
    }

    #[test]
    fn oversized_trees_are_refused() {
        let fake = Fake::new(|d| {
            for i in 0..=MAX_FILES {
                fs::write(d.join(format!("f{i}")), "x").unwrap();
            }
        });
        let parent = tmp("many");
        let mut e = entry_for(&Fake::new(|_| {}));
        e.hash = "2".repeat(64);
        let err = install(&e, &fake, &parent, &|_| Ok(())).unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert!(leftovers(&parent).is_empty());
    }
}
