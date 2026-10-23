//! Project configuration loaded from `.planvil.json` at the repository root.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::layout;
use crate::state::Limits;

pub const CONFIG_FILE: &str = ".planvil.json";
/// Upper bound for both repair limits, so a config edit cannot make the loop unbounded.
pub const MAX_LIMIT: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Branch new feature branches are cut from. `None` → ask once and save.
    pub base_branch: Option<String>,
    pub docs_root: String,
    pub docs_gitignored: bool,
    pub require_plan_approval: bool,
    pub max_repair_cycles: u32,
    pub max_equivalent_failures: u32,
    pub max_stop_continuations: u32,
    pub max_run_minutes: Option<u32>,
    /// Extra tools allowed before Implement (e.g. read-only MCP tools).
    pub allowed_tools: Vec<String>,
    /// Extra shell command prefixes treated as read-only before Implement
    /// (e.g. `"go test"`, `"qmd search"`).
    pub readonly_commands: Vec<String>,
    /// Project-wide verification; see `verify::ProjectVerify`.
    pub verify: Option<crate::verify::ProjectVerify>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_branch: None,
            docs_root: "planvil".into(),
            docs_gitignored: true,
            require_plan_approval: false,
            max_repair_cycles: 5,
            max_equivalent_failures: 3,
            max_stop_continuations: 3,
            max_run_minutes: None,
            allowed_tools: Vec::new(),
            readonly_commands: Vec::new(),
            verify: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {CONFIG_FILE}: {0}")]
    Io(#[from] std::io::Error),
    #[error("{CONFIG_FILE} is not valid: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("{CONFIG_FILE}: {0}")]
    Invalid(String),
}

/// Set one top-level key in `.planvil.json`, keeping every other key. Only the planvil
/// binary calls this; the agent cannot edit the file directly.
pub fn save_key(repo_root: &Path, key: &str, value: serde_json::Value) -> Result<(), ConfigError> {
    let path = repo_root.join(CONFIG_FILE);
    let mut doc: serde_json::Value = match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| ConfigError::Invalid("the file must be a JSON object".into()))?;
    obj.insert(key.to_string(), value);
    std::fs::write(&path, serde_json::to_string_pretty(&doc)? + "\n")?;
    Ok(())
}

/// Load `.planvil.json` from `repo_root`. A missing file yields the defaults.
pub fn load(repo_root: &Path) -> Result<Config, ConfigError> {
    let raw = match std::fs::read_to_string(repo_root.join(CONFIG_FILE)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e.into()),
    };
    let cfg: Config = serde_json::from_str(&raw)?;
    cfg.validate()?;
    Ok(cfg)
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |e: layout::LayoutError| ConfigError::Invalid(e.to_string());
        layout::validate_docs_root(&self.docs_root).map_err(invalid)?;
        if let Some(base) = &self.base_branch {
            layout::branch_to_folder(base).map_err(invalid)?;
        }
        for (name, value) in [
            ("max_repair_cycles", self.max_repair_cycles),
            ("max_equivalent_failures", self.max_equivalent_failures),
            ("max_stop_continuations", self.max_stop_continuations),
        ] {
            if !(1..=MAX_LIMIT).contains(&value) {
                return Err(ConfigError::Invalid(format!(
                    "{name} must be between 1 and {MAX_LIMIT}"
                )));
            }
        }
        if self.max_run_minutes == Some(0) {
            return Err(ConfigError::Invalid(
                "max_run_minutes must be at least 1 when set".into(),
            ));
        }
        if let Some(verify) = &self.verify {
            verify
                .validate()
                .map_err(|e| ConfigError::Invalid(format!("verify: {e}")))?;
        }
        Ok(())
    }

    /// Limits to snapshot into a new run.
    pub fn limits(&self) -> Limits {
        Limits {
            max_repair_cycles: self.max_repair_cycles,
            max_equivalent_failures: self.max_equivalent_failures,
            max_stop_continuations: self.max_stop_continuations,
            max_run_minutes: self.max_run_minutes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_config(dir: &Path, body: &str) {
        fs::write(dir.join(CONFIG_FILE), body).unwrap();
    }

    #[test]
    fn missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.docs_root, "planvil");
        assert!(cfg.docs_gitignored);
        assert_eq!(cfg.max_repair_cycles, 5);
        assert_eq!(cfg.max_equivalent_failures, 3);
        assert_eq!(cfg.base_branch, None);
    }

    #[test]
    fn partial_file_fills_remaining_defaults() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            r#"{"base_branch":"production","docs_root":"project-docs"}"#,
        );
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.base_branch.as_deref(), Some("production"));
        assert_eq!(cfg.docs_root, "project-docs");
        assert_eq!(cfg.max_repair_cycles, 5);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"docs_rot":"typo"}"#);
        assert!(matches!(load(dir.path()), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn malformed_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "{not json");
        assert!(matches!(load(dir.path()), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn escaping_docs_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"docs_root":"../elsewhere"}"#);
        assert!(matches!(load(dir.path()), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn zero_limits_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"max_repair_cycles":0}"#);
        assert!(matches!(load(dir.path()), Err(ConfigError::Invalid(_))));
        write_config(dir.path(), r#"{"max_equivalent_failures":0}"#);
        assert!(matches!(load(dir.path()), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn invalid_base_branch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"base_branch":"../main"}"#);
        assert!(matches!(load(dir.path()), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn limits_above_cap_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), r#"{"max_repair_cycles":4294967295}"#);
        assert!(matches!(load(dir.path()), Err(ConfigError::Invalid(_))));
        write_config(
            dir.path(),
            &format!(r#"{{"max_repair_cycles":{MAX_LIMIT}}}"#),
        );
        assert!(load(dir.path()).is_ok());
    }
}
