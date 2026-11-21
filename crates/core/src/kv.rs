//! Tiny `key=value` file format used for instance state, accounts and strategy state.
//! Chosen over JSON/TOML crates to keep the first release dependency-free.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

pub fn parse_kv(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

pub fn render_kv(map: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (k, v) in map {
        let k = k.replace(['\n', '\r', '='], "_");
        let v = v.replace(['\n', '\r'], " ");
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}

/// Read a kv file. A missing file yields an empty map.
pub fn read_kv_file(path: &Path) -> Result<BTreeMap<String, String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_kv(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write via a temporary file + rename so readers never see a half-written file.
///
/// The temporary name is unique per call (process id + a process-wide counter). Two writers to
/// the same file, in different processes or in different threads of one process, must never
/// share a temporary file, or one rename would find it already gone.
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}-{n}", std::process::id()));
    let result = (|| {
        let mut f =
            fs::File::create(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.write_all(contents.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.sync_all().ok();
        fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn write_kv_file(path: &Path, map: &BTreeMap<String, String>) -> Result<(), String> {
    atomic_write(path, &render_kv(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_sanitize() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), "1".to_string());
        m.insert("b".to_string(), "x=y\nz".to_string());
        let text = render_kv(&m);
        let back = parse_kv(&text);
        assert_eq!(back["a"], "1");
        assert_eq!(back["b"], "x=y z");
    }

    #[test]
    fn concurrent_writers_to_one_file_never_fail_or_leave_temp_files() {
        let dir = std::env::temp_dir().join(format!("proto-kv-conc-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("status");
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let target = target.clone();
                std::thread::spawn(move || {
                    for i in 0..200 {
                        atomic_write(&target, &format!("writer {t} round {i}\n")).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(fs::read_to_string(&target).unwrap().starts_with("writer "));
        let leftovers = fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 1, "only the target file remains");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let m = parse_kv("# c\n\nk = v\nbad line\n");
        assert_eq!(m.len(), 1);
        assert_eq!(m["k"], "v");
    }
}
