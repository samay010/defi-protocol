//! Strategy manifest (`strategy.toml`): identity, entry point and *declared permissions*.
//!
//! A deliberately small TOML subset (top-level `key = "value"` pairs plus a `[params]`
//! table; `#` comment lines only). Unknown keys are errors so typos in permission names
//! cannot silently widen or narrow what a strategy may do.
//!
//! Defaults are least-privilege: no declared actions beyond `alert`, zero caps.

use std::collections::BTreeMap;

use crate::amount::parse_amount;
use crate::risk::RiskPolicy;
use crate::types::ActionKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    C,
    Cpp,
    Python,
    Java,
}

impl Language {
    pub fn parse(s: &str) -> Option<Language> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rust" => Some(Language::Rust),
            "c" => Some(Language::C),
            "cpp" | "c++" => Some(Language::Cpp),
            "python" | "py" => Some(Language::Python),
            "java" => Some(Language::Java),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Python => "python",
            Language::Java => "java",
        }
    }

    pub fn all() -> [Language; 5] {
        [
            Language::Rust,
            Language::C,
            Language::Cpp,
            Language::Python,
            Language::Java,
        ]
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub language: Language,
    /// `builtin:<name>` for in-process official strategies, otherwise a command line
    /// (program + args) run inside the strategy directory.
    pub entry: String,
    pub description: String,
    pub protocols: Vec<String>,
    pub assets: Vec<String>,
    pub allowed_actions: Vec<ActionKind>,
    pub max_amount_per_action: u64,
    pub max_total_exposure: u64,
    pub min_health_factor: f64,
    pub tick_timeout_ms: u64,
    pub params: BTreeMap<String, String>,
}

pub const DEFAULT_TICK_TIMEOUT_MS: u64 = 2_000;
pub const MAX_TICK_TIMEOUT_MS: u64 = 60_000;

/// Names are used in file paths, so keep them strictly safe.
pub fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let mut top: BTreeMap<String, String> = BTreeMap::new();
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        let mut in_params = false;

        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                if line == "[params]" {
                    in_params = true;
                    continue;
                }
                return Err(format!(
                    "line {n}: unknown section {line} (only [params] is supported)"
                ));
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("line {n}: expected key = value"))?;
            let key = k.trim().to_string();
            let mut val = v.trim().to_string();
            if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                val = val[1..val.len() - 1].to_string();
            }
            if key.is_empty() {
                return Err(format!("line {n}: empty key"));
            }
            let target = if in_params { &mut params } else { &mut top };
            if target.insert(key.clone(), val).is_some() {
                return Err(format!("line {n}: duplicate key '{key}'"));
            }
        }

        const KNOWN: [&str; 12] = [
            "name",
            "version",
            "language",
            "entry",
            "description",
            "protocols",
            "assets",
            "allowed_actions",
            "max_amount_per_action",
            "max_total_exposure",
            "min_health_factor",
            "tick_timeout_ms",
        ];
        for k in top.keys() {
            if !KNOWN.contains(&k.as_str()) {
                return Err(format!("unknown manifest key '{k}'"));
            }
        }

        let req = |k: &str| -> Result<String, String> {
            top.get(k)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| format!("missing required key '{k}'"))
        };

        let name = req("name")?;
        if !valid_name(&name) {
            return Err(format!(
                "invalid name '{name}': use lowercase letters, digits, '-' or '_', starting with a letter (max 64)"
            ));
        }
        let version = req("version")?;
        let lang_s = req("language")?;
        let language = Language::parse(&lang_s)
            .ok_or_else(|| format!("unknown language '{lang_s}' (rust, c, cpp, python, java)"))?;
        let entry = req("entry")?;
        let protocols: Vec<String> = split_list(&req("protocols")?)
            .into_iter()
            .map(|p| p.to_ascii_lowercase())
            .collect();
        if protocols.is_empty() {
            return Err("'protocols' must list at least one protocol".into());
        }
        let assets: Vec<String> = top
            .get("assets")
            .map(|v| split_list(v))
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.to_ascii_uppercase())
            .collect();

        let allowed_actions = match top.get("allowed_actions") {
            None => vec![ActionKind::Alert],
            Some(v) => {
                let mut out = Vec::new();
                for item in split_list(v) {
                    let kind = ActionKind::parse(&item)
                        .ok_or_else(|| format!("unknown action '{item}' in allowed_actions"))?;
                    if !out.contains(&kind) {
                        out.push(kind);
                    }
                }
                if out.is_empty() {
                    return Err("'allowed_actions' is empty".into());
                }
                out
            }
        };

        let amount = |k: &str| -> Result<u64, String> {
            match top.get(k) {
                None => Ok(0),
                Some(v) => parse_amount(v).map_err(|e| format!("{k}: {e}")),
            }
        };
        let max_amount_per_action = amount("max_amount_per_action")?;
        let max_total_exposure = amount("max_total_exposure")?;
        if max_amount_per_action > max_total_exposure {
            return Err("max_amount_per_action cannot exceed max_total_exposure".into());
        }

        let min_health_factor = match top.get("min_health_factor") {
            None => 1.5,
            Some(v) => {
                let f: f64 = v
                    .parse()
                    .map_err(|_| format!("min_health_factor '{v}' is not a number"))?;
                if !(f.is_finite() && f >= 1.0) {
                    return Err("min_health_factor must be a number >= 1.0".into());
                }
                f
            }
        };

        let tick_timeout_ms = match top.get("tick_timeout_ms") {
            None => DEFAULT_TICK_TIMEOUT_MS,
            Some(v) => {
                let t: u64 = v
                    .parse()
                    .map_err(|_| format!("tick_timeout_ms '{v}' is not an integer"))?;
                if t == 0 || t > MAX_TICK_TIMEOUT_MS {
                    return Err(format!("tick_timeout_ms must be 1..={MAX_TICK_TIMEOUT_MS}"));
                }
                t
            }
        };

        let funds_declared = allowed_actions.iter().any(|a| a.moves_funds());
        if funds_declared && (assets.is_empty() || max_amount_per_action == 0) {
            return Err(
                "fund-moving actions are declared but 'assets' or 'max_amount_per_action' is missing/zero".into(),
            );
        }

        Ok(Manifest {
            name,
            version,
            language,
            entry,
            description: top.get("description").cloned().unwrap_or_default(),
            protocols,
            assets,
            allowed_actions,
            max_amount_per_action,
            max_total_exposure,
            min_health_factor,
            tick_timeout_ms,
            params,
        })
    }

    pub fn is_builtin(&self) -> bool {
        self.entry.starts_with("builtin:")
    }

    pub fn policy(&self) -> RiskPolicy {
        RiskPolicy {
            allowed_protocols: self.protocols.clone(),
            allowed_assets: self.assets.clone(),
            allowed_actions: self.allowed_actions.clone(),
            max_amount_per_action: self.max_amount_per_action,
            max_total_exposure: self.max_total_exposure,
            min_health_factor: self.min_health_factor,
        }
    }

    /// Human-readable permission summary shown before a strategy is run.
    pub fn permission_summary(&self) -> Vec<String> {
        let actions: Vec<&str> = self.allowed_actions.iter().map(|a| a.as_str()).collect();
        let mut v = vec![
            format!("protocols: {}", self.protocols.join(", ")),
            format!("actions:   {}", actions.join(", ")),
        ];
        if self.allowed_actions.iter().any(|a| a.moves_funds()) {
            v.push(format!("assets:    {}", self.assets.join(", ")));
            v.push(format!(
                "caps:      {} per action, {} total",
                crate::amount::format_amount(self.max_amount_per_action),
                crate::amount::format_amount(self.max_total_exposure)
            ));
            v.push(format!(
                "min health factor for withdrawals: {:.2}",
                self.min_health_factor
            ));
        } else {
            v.push("funds:     cannot move funds (alert-only)".to_string());
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
# a comment
name = "my-strategy"
version = "0.1.0"
language = "python"
entry = "python3 strategy.py"
description = "demo"
protocols = "aave"
assets = "usdc, dai"
allowed_actions = "supply, alert"
max_amount_per_action = "100"
max_total_exposure = "500.5"
min_health_factor = "1.8"
tick_timeout_ms = "1500"

[params]
threshold = "42"
"#;

    #[test]
    fn parses_full_manifest() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!(m.name, "my-strategy");
        assert_eq!(m.language, Language::Python);
        assert_eq!(m.assets, vec!["USDC", "DAI"]);
        assert_eq!(
            m.allowed_actions,
            vec![ActionKind::Supply, ActionKind::Alert]
        );
        assert_eq!(m.max_amount_per_action, 100_000_000);
        assert_eq!(m.max_total_exposure, 500_500_000);
        assert_eq!(m.tick_timeout_ms, 1500);
        assert_eq!(m.params["threshold"], "42");
        assert!(!m.is_builtin());
    }

    #[test]
    fn minimal_manifest_is_alert_only() {
        let m = Manifest::parse(
            "name=\"x\"\nversion=\"1\"\nlanguage=\"c\"\nentry=\"./build/strategy\"\nprotocols=\"aave\"\n",
        )
        .unwrap();
        assert_eq!(m.allowed_actions, vec![ActionKind::Alert]);
        assert_eq!(m.max_total_exposure, 0);
        assert_eq!(m.tick_timeout_ms, DEFAULT_TICK_TIMEOUT_MS);
        assert!((m.min_health_factor - 1.5).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_manifests() {
        let base = |extra: &str| {
            format!("name=\"x\"\nversion=\"1\"\nlanguage=\"rust\"\nentry=\"e\"\nprotocols=\"aave\"\n{extra}")
        };
        assert!(Manifest::parse(&base("typo_key=\"1\""))
            .unwrap_err()
            .contains("unknown manifest key"));
        assert!(Manifest::parse(&base("allowed_actions=\"fly\""))
            .unwrap_err()
            .contains("unknown action"));
        assert!(Manifest::parse(&base("allowed_actions=\"supply\""))
            .unwrap_err()
            .contains("fund-moving"));
        assert!(Manifest::parse(&base(
            "max_amount_per_action=\"5\"\nmax_total_exposure=\"1\""
        ))
        .unwrap_err()
        .contains("cannot exceed"));
        assert!(Manifest::parse(&base("min_health_factor=\"0.5\"")).is_err());
        assert!(Manifest::parse(&base("tick_timeout_ms=\"0\"")).is_err());
        assert!(Manifest::parse(&base("[other]"))
            .unwrap_err()
            .contains("unknown section"));
        assert!(Manifest::parse(&base("name=\"dup\""))
            .unwrap_err()
            .contains("duplicate"));
        assert!(Manifest::parse("version=\"1\"")
            .unwrap_err()
            .contains("missing required key"));
    }

    #[test]
    fn rejects_path_traversal_names() {
        for bad in ["../evil", "A", "1abc", "a/b", "", "-x", "a b"] {
            assert!(!valid_name(bad), "{bad}");
            let t = format!(
                "name=\"{bad}\"\nversion=\"1\"\nlanguage=\"rust\"\nentry=\"e\"\nprotocols=\"aave\"\n"
            );
            assert!(Manifest::parse(&t).is_err());
        }
        assert!(valid_name("ok-name_1"));
    }

    #[test]
    fn policy_reflects_manifest() {
        let m = Manifest::parse(GOOD).unwrap();
        let p = m.policy();
        assert_eq!(p.allowed_assets, vec!["USDC", "DAI"]);
        assert_eq!(p.max_amount_per_action, 100_000_000);
        assert!(m.permission_summary().iter().any(|l| l.contains("caps")));
    }

    #[test]
    fn language_aliases() {
        assert_eq!(Language::parse("C++"), Some(Language::Cpp));
        assert_eq!(Language::parse("py"), Some(Language::Python));
        assert_eq!(Language::parse("go"), None);
        assert_eq!(Language::all().len(), 5);
    }
}
