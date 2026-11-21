//! Strategy manifest (`strategy.toml`): identity, entry point and *declared permissions*.
//!
//! Real TOML, parsed by the `toml` crate through `serde`. Top-level keys describe the strategy
//! and its permissions; `[params]` holds strategy parameters. Unknown keys and duplicate keys
//! are errors, so typos in permission names cannot silently widen or narrow what a strategy
//! may do. Lists are TOML arrays (`protocols = ["aave"]`). Amounts are whole numbers or quoted
//! decimal strings (`"100.5"`), never floats, so money never passes through floating point.
//!
//! Defaults are least-privilege: no declared actions beyond `alert`, zero caps.

use std::collections::BTreeMap;

use serde::Deserialize;
use toml::Value;

use crate::amount::{parse_amount, MICRO};
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
    /// Address-space limit (`ulimit -v`) for external strategy processes, in MiB.
    /// `0` means no memory limit. Opt-in because some runtimes (the JVM) reserve far more
    /// address space than they use and fail to start under a tight limit.
    pub max_memory_mb: u64,
    pub params: BTreeMap<String, String>,
}

pub const DEFAULT_TICK_TIMEOUT_MS: u64 = 2_000;
pub const MAX_TICK_TIMEOUT_MS: u64 = 60_000;
pub const MIN_MEMORY_MB: u64 = 16;
pub const MAX_MEMORY_MB: u64 = 1_048_576;

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

/// The file exactly as written, before any validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: String,
    version: String,
    language: String,
    entry: String,
    #[serde(default)]
    description: String,
    protocols: Vec<String>,
    #[serde(default)]
    assets: Vec<String>,
    allowed_actions: Option<Vec<String>>,
    max_amount_per_action: Option<Value>,
    max_total_exposure: Option<Value>,
    min_health_factor: Option<Value>,
    tick_timeout_ms: Option<u64>,
    max_memory_mb: Option<u64>,
    #[serde(default)]
    params: BTreeMap<String, Value>,
}

fn amount_value(key: &str, v: Option<Value>) -> Result<u64, String> {
    match v {
        None => Ok(0),
        Some(Value::String(s)) => parse_amount(&s).map_err(|e| format!("{key}: {e}")),
        Some(Value::Integer(i)) => u64::try_from(i)
            .ok()
            .and_then(|n| n.checked_mul(MICRO))
            .ok_or_else(|| format!("{key}: must be a non-negative amount that fits in range")),
        Some(_) => Err(format!(
            "{key}: write the amount as a whole number or a quoted decimal string such as \"100.5\" (floats lose precision)"
        )),
    }
}

fn param_text(key: &str, v: Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(f.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        _ => Err(format!("params.{key} must be a string, number or boolean")),
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let raw: Raw = toml::from_str(text).map_err(|e| e.to_string())?;

        let name = raw.name;
        if !valid_name(&name) {
            return Err(format!(
                "invalid name '{name}': use lowercase letters, digits, '-' or '_', starting with a letter (max 64)"
            ));
        }
        let version = raw.version;
        if version.trim().is_empty() {
            return Err("'version' must not be empty".into());
        }
        let language = Language::parse(&raw.language).ok_or_else(|| {
            format!(
                "unknown language '{}' (rust, c, cpp, python, java)",
                raw.language
            )
        })?;
        let entry = raw.entry;
        if entry.trim().is_empty() {
            return Err("'entry' must not be empty".into());
        }

        let clean = |v: Vec<String>| -> Vec<String> {
            v.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let protocols: Vec<String> = clean(raw.protocols)
            .into_iter()
            .map(|p| p.to_ascii_lowercase())
            .collect();
        if protocols.is_empty() {
            return Err("'protocols' must list at least one protocol".into());
        }
        let assets: Vec<String> = clean(raw.assets)
            .into_iter()
            .map(|a| a.to_ascii_uppercase())
            .collect();

        let allowed_actions = match raw.allowed_actions {
            None => vec![ActionKind::Alert],
            Some(list) => {
                let mut out = Vec::new();
                for item in clean(list) {
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

        let max_amount_per_action =
            amount_value("max_amount_per_action", raw.max_amount_per_action)?;
        let max_total_exposure = amount_value("max_total_exposure", raw.max_total_exposure)?;
        if max_amount_per_action > max_total_exposure {
            return Err("max_amount_per_action cannot exceed max_total_exposure".into());
        }

        let min_health_factor = match raw.min_health_factor {
            None => 1.5,
            Some(v) => {
                let f = match v {
                    Value::Float(f) => f,
                    Value::Integer(i) => i as f64,
                    _ => return Err("min_health_factor must be a number, e.g. 1.5".into()),
                };
                if !(f.is_finite() && f >= 1.0) {
                    return Err("min_health_factor must be a number >= 1.0".into());
                }
                f
            }
        };

        let tick_timeout_ms = match raw.tick_timeout_ms {
            None => DEFAULT_TICK_TIMEOUT_MS,
            Some(t) => {
                if t == 0 || t > MAX_TICK_TIMEOUT_MS {
                    return Err(format!("tick_timeout_ms must be 1..={MAX_TICK_TIMEOUT_MS}"));
                }
                t
            }
        };

        let max_memory_mb = match raw.max_memory_mb {
            None => 0,
            Some(m) => {
                if !(MIN_MEMORY_MB..=MAX_MEMORY_MB).contains(&m) {
                    return Err(format!("max_memory_mb must be {MIN_MEMORY_MB}..={MAX_MEMORY_MB} (or omitted for no limit)"));
                }
                m
            }
        };

        let funds_declared = allowed_actions.iter().any(|a| a.moves_funds());
        if funds_declared && (assets.is_empty() || max_amount_per_action == 0) {
            return Err(
                "fund-moving actions are declared but 'assets' or 'max_amount_per_action' is missing/zero".into(),
            );
        }

        let mut params = BTreeMap::new();
        for (k, v) in raw.params {
            let text = param_text(&k, v)?;
            params.insert(k, text);
        }

        Ok(Manifest {
            name,
            version,
            language,
            entry,
            description: raw.description,
            protocols,
            assets,
            allowed_actions,
            max_amount_per_action,
            max_total_exposure,
            min_health_factor,
            tick_timeout_ms,
            max_memory_mb,
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
protocols = ["aave"]
assets = ["usdc", "dai"]
allowed_actions = ["supply", "alert"]
max_amount_per_action = 100
max_total_exposure = "500.5"
min_health_factor = 1.8
tick_timeout_ms = 1500
max_memory_mb = 256

[params]
threshold = "42"
count = 7
"#;

    fn base(extra: &str) -> String {
        format!("name = \"x\"\nversion = \"1\"\nlanguage = \"rust\"\nentry = \"e\"\nprotocols = [\"aave\"]\n{extra}")
    }

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
        assert_eq!(m.max_memory_mb, 256);
        assert_eq!(m.params["threshold"], "42");
        assert_eq!(m.params["count"], "7", "numeric params are kept as text");
        assert!(!m.is_builtin());
    }

    #[test]
    fn minimal_manifest_is_alert_only() {
        let m = Manifest::parse(&base("")).unwrap();
        assert_eq!(m.allowed_actions, vec![ActionKind::Alert]);
        assert_eq!(m.max_total_exposure, 0);
        assert_eq!(m.tick_timeout_ms, DEFAULT_TICK_TIMEOUT_MS);
        assert_eq!(m.max_memory_mb, 0, "no memory limit unless declared");
        assert!((m.min_health_factor - 1.5).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_manifests() {
        let err = |extra: &str| Manifest::parse(&base(extra)).unwrap_err();
        assert!(err("typo_key = 1").contains("unknown field"));
        assert!(err("allowed_actions = [\"fly\"]").contains("unknown action"));
        assert!(err("allowed_actions = [\"supply\"]").contains("fund-moving"));
        assert!(err("max_amount_per_action = 5\nmax_total_exposure = 1").contains("cannot exceed"));
        assert!(err("min_health_factor = 0.5").contains("min_health_factor"));
        assert!(err("min_health_factor = \"1.5\"").contains("min_health_factor"));
        assert!(err("tick_timeout_ms = 0").contains("tick_timeout_ms"));
        assert!(err("tick_timeout_ms = \"1500\"").contains("tick_timeout_ms"));
        assert!(err("max_memory_mb = 1").contains("max_memory_mb"));
        assert!(err("max_memory_mb = \"lots\"").contains("max_memory_mb"));
        assert!(err("max_memory_mb = 99999999").contains("max_memory_mb"));
        assert!(err("max_total_exposure = 1.5").contains("floats lose precision"));
        assert!(err("max_total_exposure = -1").contains("max_total_exposure"));
        assert!(err("protocols = \"aave\"").contains("protocols"));
        assert!(err("[other]\nx = 1").contains("other"));
        assert!(err("name = \"dup\"").contains("duplicate"));
        assert!(err("[params]\nnested = [1, 2]").contains("params.nested"));
        assert!(Manifest::parse("version = \"1\"")
            .unwrap_err()
            .contains("missing field"));
        assert!(Manifest::parse("this is not toml").is_err());
    }

    #[test]
    fn rejects_path_traversal_names() {
        for bad in ["../evil", "A", "1abc", "a/b", "", "-x", "a b"] {
            assert!(!valid_name(bad), "{bad}");
            let t = format!(
                "name = \"{bad}\"\nversion = \"1\"\nlanguage = \"rust\"\nentry = \"e\"\nprotocols = [\"aave\"]\n"
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
