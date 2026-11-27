//! Content hash of a strategy directory, used for verification pinning.
//!
//! Covers source files only: build output directories (`target`, `build`), caches and VCS
//! metadata are skipped, and symlinks are never followed. Verification is therefore of
//! *source*; users rebuild from verified source.

use std::fs;
use std::path::Path;

use crate::sha256::{hex, Sha256};

pub const SKIP_DIRS: [&str; 5] = ["target", "build", "__pycache__", ".git", "node_modules"];

pub fn hash_dir(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for rel in &files {
        let data = fs::read(dir.join(rel)).map_err(|e| format!("cannot read {rel}: {e}"))?;
        h.update(rel.as_bytes());
        h.update(&[0]);
        h.update(&(data.len() as u64).to_le_bytes());
        h.update(&data);
    }
    Ok(hex(&h.finalize()))
}

fn collect(root: &Path, cur: &Path, out: &mut Vec<String>) -> Result<(), String> {
    let rd = fs::read_dir(cur).map_err(|e| format!("cannot read {}: {e}", cur.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| e.to_string())?;
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect(root, &entry.path(), out)?;
        } else if ft.is_file() {
            let rel = entry
                .path()
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("proto-hash-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn deterministic_and_content_sensitive() {
        let d = tmp("a");
        fs::write(d.join("strategy.toml"), "x").unwrap();
        fs::create_dir_all(d.join("src")).unwrap();
        fs::write(d.join("src/main.rs"), "fn main(){}").unwrap();
        let h1 = hash_dir(&d).unwrap();
        assert_eq!(h1, hash_dir(&d).unwrap());
        assert_eq!(h1.len(), 64);
        fs::write(d.join("src/main.rs"), "fn main(){ evil() }").unwrap();
        assert_ne!(h1, hash_dir(&d).unwrap());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn build_output_is_ignored_but_renames_are_not() {
        let d = tmp("b");
        fs::write(d.join("a.txt"), "1").unwrap();
        let h1 = hash_dir(&d).unwrap();
        fs::create_dir_all(d.join("target")).unwrap();
        fs::write(d.join("target/out"), "binary").unwrap();
        fs::create_dir_all(d.join("build")).unwrap();
        fs::write(d.join("build/strategy"), "binary").unwrap();
        assert_eq!(h1, hash_dir(&d).unwrap());
        fs::rename(d.join("a.txt"), d.join("b.txt")).unwrap();
        assert_ne!(h1, hash_dir(&d).unwrap());
        let _ = fs::remove_dir_all(&d);
    }
}
