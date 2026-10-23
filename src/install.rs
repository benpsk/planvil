//! Install the embedded Claude Code plugin through Claude Code's own plugin commands.
//!
//! The plugin is written to planvil's data folder as a local marketplace, with the hooks
//! pointing at this binary's absolute path, then registered with `claude plugin`. Claude
//! Code settings files are never edited directly.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

pub const MARKETPLACE: &str = "planvil";
pub const PLUGIN_ID: &str = "planvil@planvil";

const MARKETPLACE_JSON: &str = include_str!("../.claude-plugin/marketplace.json");
const PLUGIN_JSON: &str = include_str!("../plugin/.claude-plugin/plugin.json");
const HOOKS_JSON: &str = include_str!("../plugin/hooks/hooks.json");
const SKILL_MD: &str = include_str!("../plugin/skills/planvil/SKILL.md");

/// Folder holding the generated marketplace. `PLANVIL_HOME` overrides it.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("PLANVIL_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("planvil"));
    }
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/planvil"));
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".local/share")))
        .map(|d| d.join("planvil"))
}

/// `hooks.json` with every launcher call (`sh .../planvil-hook <event>`) replaced by a direct
/// call to `exe hook claude <event>`, so a local install never depends on PATH.
pub fn hooks_for(exe: &Path) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(HOOKS_JSON).map_err(|e| e.to_string())?;
    point_at(&mut value, &exe.display().to_string());
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

fn point_at(value: &mut Value, exe: &str) {
    match value {
        Value::Object(map) => {
            let event = map
                .get("args")
                .and_then(Value::as_array)
                .filter(|args| {
                    args.first()
                        .and_then(Value::as_str)
                        .is_some_and(|a| a.ends_with("planvil-hook"))
                })
                .and_then(|args| args.get(1).cloned());
            if let Some(event) = event {
                map.insert("command".into(), Value::String(exe.into()));
                map.insert("args".into(), serde_json::json!(["hook", "claude", event]));
            }
            map.values_mut().for_each(|v| point_at(v, exe));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| point_at(v, exe)),
        _ => {}
    }
}

/// Write the marketplace and plugin files under `dir`.
pub fn write_marketplace(dir: &Path, exe: &Path) -> Result<(), String> {
    let files = [
        (
            ".claude-plugin/marketplace.json",
            MARKETPLACE_JSON.to_string(),
        ),
        ("plugin/.claude-plugin/plugin.json", PLUGIN_JSON.to_string()),
        ("plugin/hooks/hooks.json", hooks_for(exe)?),
        ("plugin/skills/planvil/SKILL.md", SKILL_MD.to_string()),
    ];
    for (rel, body) in files {
        let path = dir.join(rel);
        let parent = path.parent().ok_or("bad path")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn claude_bin() -> String {
    std::env::var("PLANVIL_CLAUDE_BIN").unwrap_or_else(|_| "claude".into())
}

fn claude(args: &[&str]) -> Result<String, String> {
    let out = Command::new(claude_bin())
        .args(args)
        .output()
        .map_err(|e| {
            format!(
                "cannot run `{}` ({e}); is Claude Code installed and on PATH?",
                claude_bin()
            )
        })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!(
            "`claude {}` failed: {}",
            args.join(" "),
            text.trim()
        ))
    }
}

pub fn install(scope: &str) -> Result<String, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    let dir = data_dir().ok_or("cannot find a data folder; set PLANVIL_HOME")?;
    write_marketplace(&dir, &exe)?;
    let dir_str = dir.display().to_string();
    let mut log = vec![format!(
        "wrote plugin to {dir_str} (hooks call {})",
        exe.display()
    )];

    if json_lists(
        &["plugin", "marketplace", "list", "--json"],
        "name",
        MARKETPLACE,
    ) {
        claude(&["plugin", "marketplace", "update", MARKETPLACE])?;
        log.push("updated the planvil marketplace".into());
    } else {
        claude(&["plugin", "marketplace", "add", &dir_str, "--scope", "user"])?;
        log.push("added the planvil marketplace".into());
    }
    if json_lists(&["plugin", "list", "--json"], "id", PLUGIN_ID) {
        claude(&["plugin", "update", PLUGIN_ID, "--scope", scope])?;
        log.push(format!("updated {PLUGIN_ID} ({scope} scope)"));
    } else {
        claude(&["plugin", "install", PLUGIN_ID, "--scope", scope])?;
        log.push(format!("installed {PLUGIN_ID} ({scope} scope)"));
    }
    log.push("restart Claude Code, then run /planvil in a repository".into());
    Ok(log.join("\n"))
}

/// Files `write_marketplace` creates, relative to the data folder.
const MARKETPLACE_FILES: [&str; 4] = [
    ".claude-plugin/marketplace.json",
    "plugin/.claude-plugin/plugin.json",
    "plugin/hooks/hooks.json",
    "plugin/skills/planvil/SKILL.md",
];

/// Whether the `claude ... list --json` output has an entry whose `key` equals `value`.
fn json_lists(args: &[&str], key: &str, value: &str) -> bool {
    let Ok(out) = claude(args) else { return false };
    let start = out.find('[').unwrap_or(0);
    serde_json::from_str::<Value>(&out[start..])
        .ok()
        .and_then(|v| v.as_array().cloned())
        .is_some_and(|items| {
            items
                .iter()
                .any(|i| i.get(key).and_then(Value::as_str) == Some(value))
        })
}

pub fn uninstall(scope: &str) -> Result<String, String> {
    let mut log = Vec::new();
    if json_lists(&["plugin", "list", "--json"], "id", PLUGIN_ID) {
        claude(&["plugin", "uninstall", PLUGIN_ID, "--scope", scope, "--yes"])?;
        log.push(format!("uninstalled {PLUGIN_ID}"));
    }
    if json_lists(
        &["plugin", "marketplace", "list", "--json"],
        "name",
        MARKETPLACE,
    ) {
        claude(&["plugin", "marketplace", "remove", MARKETPLACE])?;
        log.push("removed the planvil marketplace".into());
    }
    if let Some(dir) = data_dir() {
        let removed = remove_marketplace_files(&dir)?;
        if removed > 0 {
            log.push(format!(
                "removed {removed} plugin files from {}",
                dir.display()
            ));
        }
    }
    log.push("project files (.planvil.json, feature folders) were left untouched".into());
    Ok(log.join("\n"))
}

/// Remove only the files planvil wrote, then any folders left empty. Never removes
/// anything else under the data folder.
fn remove_marketplace_files(dir: &Path) -> Result<usize, String> {
    let mut removed = 0;
    for rel in MARKETPLACE_FILES {
        let path = dir.join(rel);
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        for parent in path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(dir) && *p != dir)
        {
            if std::fs::remove_dir(parent).is_err() {
                break;
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_point_at_the_given_binary() {
        let hooks = hooks_for(Path::new("/opt/bin/planvil")).unwrap();
        let v: Value = serde_json::from_str(&hooks).unwrap();
        let pre = &v["hooks"]["PreToolUse"][0]["hooks"][0];
        assert_eq!(pre["command"], "/opt/bin/planvil");
        assert_eq!(
            pre["args"],
            serde_json::json!(["hook", "claude", "pre-tool"])
        );
        assert!(!hooks.contains("planvil-hook"));
    }

    #[test]
    fn every_hook_event_is_registered() {
        let v: Value = serde_json::from_str(HOOKS_JSON).unwrap();
        for event in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
        ] {
            assert!(v["hooks"][event].is_array(), "{event}");
        }
    }

    #[test]
    fn writes_a_complete_marketplace() {
        let dir = tempfile::tempdir().unwrap();
        write_marketplace(dir.path(), Path::new("/x/planvil")).unwrap();
        for rel in [
            ".claude-plugin/marketplace.json",
            "plugin/.claude-plugin/plugin.json",
            "plugin/hooks/hooks.json",
            "plugin/skills/planvil/SKILL.md",
        ] {
            assert!(dir.path().join(rel).exists(), "{rel}");
        }
        let market: Value = serde_json::from_str(MARKETPLACE_JSON).unwrap();
        assert_eq!(market["plugins"][0]["source"], "./plugin");
    }

    #[test]
    fn uninstall_removes_only_planvil_files() {
        let dir = tempfile::tempdir().unwrap();
        write_marketplace(dir.path(), Path::new("/x/planvil")).unwrap();
        std::fs::write(dir.path().join("keep.txt"), "user file").unwrap();
        assert_eq!(remove_marketplace_files(dir.path()).unwrap(), 4);
        assert!(dir.path().join("keep.txt").exists());
        assert!(!dir.path().join("plugin").exists());
    }
}
