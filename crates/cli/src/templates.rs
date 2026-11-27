//! `protocol create`: per-language strategy project templates.
//!
//! Every template speaks the same line protocol (docs/strategy-protocol.md) and by default
//! only logs. Permissions in the generated manifest start at "alert only, zero caps".

use std::fs;
use std::path::{Path, PathBuf};

use protocol_core::manifest::Language;

const MANIFEST: &str = r#"name = "__NAME__"
version = "0.1.0"
language = "__LANG__"
entry = "__ENTRY__"
description = "Describe what this strategy does."
protocols = ["aave"]
assets = ["USDC"]
# Least privilege: alert-only by default. To let the strategy propose fund movements, add
# e.g. allowed_actions = ["supply", "alert"] together with real caps below. Every proposal is still
# checked by the risk engine against exactly what you declare here.
allowed_actions = ["alert"]
max_amount_per_action = "0"
max_total_exposure = "0"
min_health_factor = 1.5
tick_timeout_ms = 2000
# Optional memory cap in MiB for this strategy process.
# max_memory_mb = 512

[params]
"#;

const README: &str = r#"# __NAME__

A `__LANG__` strategy for the DeFi strategy execution protocol (simulation-only release).

Protocol in one paragraph: on every tick the engine starts your program, writes lines such as
`TICK 3`, `MARKET USDC apy_bps=391 price_usd_micro=1000000`, `ACCOUNT wallet USDC 1000000000`,
`HEALTH_FACTOR 2.475000`, `PARAM key value`, `STATE key value`, then `END`. You answer with `LOG <text>`,
`PROPOSE <kind> <protocol> <asset|-> <amount_micro> [note]`, `SETSTATE <key> <value>` and finally `DONE`.
Amounts are integer micro-units (1 token = 1000000). You never sign or send anything: proposals are
checked by the risk engine against the permissions declared in `strategy.toml`.

Workflow:

    protocol build __NAME__       # compile / check
    protocol test __NAME__        # run in both simulated scenarios
    protocol simulate __NAME__    # see every tick
    protocol verify __NAME__      # content hash for review pinning

Build output goes to `build/` or `target/`, which are excluded from the verification hash.
"#;

const WASM_NOTE: &str = r#"
## WebAssembly notes

This strategy is compiled to `wasm32-wasip1` and runs inside the protocol's built-in WebAssembly
interpreter, not as an operating-system process. It can read the tick input on stdin and write
to stdout/stderr; it has no files, no network, no clock and no environment, and each tick has a
CPU and memory budget. Install the compiler target once (outside the build sandbox):

    rustup target add wasm32-wasip1

The built `.wasm` file lives under `target/`, which is excluded from the verification hash, like
every other build output: review and pin the source, build it yourself.
"#;

const RUST_CARGO: &str = r#"[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

[dependencies]

# Keeps this strategy a standalone project even when its folder sits inside another Cargo
# workspace (for example a checkout of this repository).
[workspace]
"#;

const RUST_MAIN: &str = r#"use std::io::{self, BufRead};

fn main() {
    let stdin = io::stdin();
    let mut tick = 0u64;
    let mut health_factor: Option<f64> = None;
    for line in stdin.lock().lines() {
        let line = line.expect("read stdin");
        let mut parts = line.splitn(3, ' ');
        match parts.next() {
            Some("TICK") => tick = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            Some("HEALTH_FACTOR") => health_factor = parts.next().and_then(|v| v.parse().ok()),
            Some("END") => break,
            _ => {}
        }
    }
    println!("LOG tick {} health_factor {:?}", tick, health_factor);
    // Example proposal (needs matching permissions in strategy.toml):
    // println!("PROPOSE supply aave USDC 100000000 example");
    println!("DONE");
}
"#;

const C_SRC: &str = r#"#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    char line[1024];
    long tick = 0;
    char hf[64] = "none";
    while (fgets(line, sizeof line, stdin)) {
        line[strcspn(line, "\n")] = 0;
        if (strncmp(line, "TICK ", 5) == 0) {
            tick = atol(line + 5);
        } else if (strncmp(line, "HEALTH_FACTOR ", 14) == 0) {
            strncpy(hf, line + 14, sizeof hf - 1);
            hf[sizeof hf - 1] = 0;
        } else if (strcmp(line, "END") == 0) {
            break;
        }
    }
    printf("LOG tick %ld health_factor %s\n", tick, hf);
    /* Example proposal (needs matching permissions in strategy.toml):
       printf("PROPOSE supply aave USDC 100000000 example\n"); */
    printf("DONE\n");
    return 0;
}
"#;

const CPP_SRC: &str = r#"#include <iostream>
#include <string>

int main() {
    std::string line;
    long tick = 0;
    std::string hf = "none";
    while (std::getline(std::cin, line)) {
        if (line.rfind("TICK ", 0) == 0) {
            tick = std::stol(line.substr(5));
        } else if (line.rfind("HEALTH_FACTOR ", 0) == 0) {
            hf = line.substr(14);
        } else if (line == "END") {
            break;
        }
    }
    std::cout << "LOG tick " << tick << " health_factor " << hf << "\n";
    // Example proposal (needs matching permissions in strategy.toml):
    // std::cout << "PROPOSE supply aave USDC 100000000 example\n";
    std::cout << "DONE\n";
    return 0;
}
"#;

pub fn entry_for(lang: Language, name: &str) -> String {
    match lang {
        Language::Rust => format!("./target/release/{name}"),
        Language::Wasm => format!("target/wasm32-wasip1/release/{name}.wasm"),
        Language::C | Language::Cpp => "./build/strategy".to_string(),
    }
}

/// Fixed per-language build commands. They are chosen here, never read from the manifest,
/// so a downloaded manifest cannot make `protocol build` run arbitrary shell lines.
/// (Building still executes the project's own toolchain: build only code you trust.)
pub fn build_command(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "cargo build --release",
        Language::Wasm => "cargo build --release --target wasm32-wasip1",
        Language::C => "mkdir -p build && cc -O2 -Wall -o build/strategy strategy.c",
        Language::Cpp => {
            "mkdir -p build && c++ -O2 -Wall -std=c++17 -o build/strategy strategy.cpp"
        }
    }
}

fn fill(template: &str, name: &str, lang: Language) -> String {
    template
        .replace("__NAME__", name)
        .replace("__LANG__", lang.as_str())
        .replace("__ENTRY__", &entry_for(lang, name))
}

/// Create `<base>/<name>/` with a manifest, README and a language template.
/// Returns the project directory and the relative paths written.
pub fn scaffold(name: &str, lang: Language, base: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let dir = base.join(name);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let mut readme = fill(README, name, lang);
    if lang == Language::Wasm {
        readme.push_str(WASM_NOTE);
    }
    let mut files: Vec<(&str, String)> = vec![
        ("strategy.toml", fill(MANIFEST, name, lang)),
        ("README.md", readme),
        (".gitignore", "target/\nbuild/\n".to_string()),
    ];
    match lang {
        Language::Rust | Language::Wasm => {
            files.push(("Cargo.toml", fill(RUST_CARGO, name, lang)));
            files.push(("src/main.rs", RUST_MAIN.to_string()));
        }
        Language::C => files.push(("strategy.c", C_SRC.to_string())),
        Language::Cpp => files.push(("strategy.cpp", CPP_SRC.to_string())),
    }
    let mut written = Vec::new();
    for (rel, content) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        fs::write(&path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        written.push(rel.to_string());
    }
    Ok((dir, written))
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol_core::manifest::Manifest;

    #[test]
    fn every_language_scaffolds_a_valid_alert_only_manifest() {
        for lang in Language::all() {
            let base = std::env::temp_dir().join(format!(
                "proto-tpl-{}-{}",
                lang.as_str(),
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&base);
            let (dir, files) = scaffold("demo-strategy", lang, &base).unwrap();
            assert!(files.contains(&"strategy.toml".to_string()));
            let m =
                Manifest::parse(&fs::read_to_string(dir.join("strategy.toml")).unwrap()).unwrap();
            assert_eq!(m.name, "demo-strategy");
            assert_eq!(m.language, lang);
            assert_eq!(m.max_total_exposure, 0);
            assert!(!m.allowed_actions.iter().any(|a| a.moves_funds()));
            assert!(
                scaffold("demo-strategy", lang, &base).is_err(),
                "refuses to overwrite"
            );
            let _ = fs::remove_dir_all(&base);
        }
    }

    #[test]
    fn entries_and_build_commands_are_defined() {
        assert_eq!(entry_for(Language::Rust, "x"), "./target/release/x");
        assert!(build_command(Language::C).contains("build/strategy"));
        assert!(build_command(Language::Cpp).contains("c++"));
        assert!(build_command(Language::Wasm).contains("wasm32-wasip1"));
        assert_eq!(
            entry_for(Language::Wasm, "x"),
            "target/wasm32-wasip1/release/x.wasm"
        );
    }
}
