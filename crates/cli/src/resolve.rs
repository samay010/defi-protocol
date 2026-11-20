//! Turn a user-supplied strategy reference (built-in name or directory) into a manifest,
//! a trust tier and a runnable strategy.

use std::fs;
use std::path::{Path, PathBuf};

use protocol_core::external::ExternalStrategy;
use protocol_core::hash::hash_dir;
use protocol_core::manifest::Manifest;
use protocol_core::store::Store;
use protocol_core::strategy::Strategy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Built in and maintained by the project.
    Official,
    /// External, and its exact (name, version, content hash) has been pinned as reviewed.
    Verified,
    /// External and not reviewed (or changed since review).
    Unverified,
}

impl Tier {
    pub fn label(&self) -> &'static str {
        match self {
            Tier::Official => "official",
            Tier::Verified => "verified (pinned)",
            Tier::Unverified => "UNVERIFIED",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Source {
    Builtin,
    External(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub manifest: Manifest,
    pub source: Source,
    pub tier: Tier,
    pub hash: Option<String>,
}

impl Resolved {
    /// Stable reference stored in instance metadata.
    pub fn strategy_ref(&self) -> String {
        match &self.source {
            Source::Builtin => self.manifest.name.clone(),
            Source::External(dir) => dir.to_string_lossy().to_string(),
        }
    }
}

pub fn resolve(store: &Store, reference: &str) -> Result<Resolved, String> {
    // Built-in names win, so a local directory can never shadow an official strategy.
    if let Some(manifest) = protocol_strategies::manifest(reference) {
        return Ok(Resolved {
            manifest,
            source: Source::Builtin,
            tier: Tier::Official,
            hash: None,
        });
    }

    let candidates = [
        PathBuf::from(reference),
        Path::new("custom-strategies").join(reference),
    ];
    let dir = candidates.iter().find(|d| d.join("strategy.toml").is_file()).ok_or_else(|| {
        format!(
            "unknown strategy '{reference}': not a built-in ({}) and no strategy.toml in ./{reference} or ./custom-strategies/{reference}",
            protocol_strategies::BUILTIN_NAMES.join(", ")
        )
    })?;
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;

    let text = fs::read_to_string(dir.join("strategy.toml"))
        .map_err(|e| format!("cannot read {}/strategy.toml: {e}", dir.display()))?;
    let manifest =
        Manifest::parse(&text).map_err(|e| format!("{}/strategy.toml: {e}", dir.display()))?;
    if manifest.is_builtin() {
        return Err("external strategies cannot use a builtin: entry".into());
    }
    if protocol_strategies::BUILTIN_NAMES.contains(&manifest.name.as_str()) {
        return Err(format!(
            "'{}' is reserved for an official strategy; choose another name",
            manifest.name
        ));
    }
    let hash = hash_dir(&dir)?;
    let tier = if store.is_pinned(&manifest.name, &manifest.version, &hash) {
        Tier::Verified
    } else {
        Tier::Unverified
    };
    Ok(Resolved {
        manifest,
        source: Source::External(dir),
        tier,
        hash: Some(hash),
    })
}

pub fn make_strategy(res: &Resolved) -> Result<Box<dyn Strategy>, String> {
    match &res.source {
        Source::Builtin => protocol_strategies::create(&res.manifest.name)
            .ok_or_else(|| format!("no built-in strategy '{}'", res.manifest.name)),
        Source::External(dir) => Ok(Box::new(ExternalStrategy::from_manifest(
            &res.manifest,
            dir,
        )?)),
    }
}
