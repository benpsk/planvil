//! Install the embedded plugin into Codex through `codex plugin` commands.
//!
//! Uses the same local marketplace folder as the Claude Code install; Codex reads its own
//! `.codex-plugin/plugin.json`, whose hooks call this binary directly. Codex runs plugin hooks
//! only after the user trusts them once, so the result says how.

use std::process::Command;

use serde_json::Value;

use crate::install::{MARKETPLACE, PLUGIN_ID, data_dir, write_marketplace};

fn codex_bin() -> String {
    std::env::var("PLANVIL_CODEX_BIN").unwrap_or_else(|_| "codex".into())
}

fn codex(args: &[&str]) -> Result<String, String> {
    let out = Command::new(codex_bin()).args(args).output().map_err(|e| {
        format!(
            "cannot run `{}` ({e}); is Codex CLI installed and on PATH?",
            codex_bin()
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
            "`codex {}` failed: {}",
            args.join(" "),
            text.trim()
        ))
    }
}

/// Whether `codex ... --json` lists an item whose `key` equals `value` under `list_key`.
fn lists(args: &[&str], list_key: &str, key: &str, value: &str) -> bool {
    let Ok(out) = codex(args) else { return false };
    let start = out.find('{').unwrap_or(0);
    serde_json::from_str::<Value>(&out[start..])
        .ok()
        .and_then(|v| v.get(list_key).and_then(Value::as_array).cloned())
        .is_some_and(|items| {
            items
                .iter()
                .any(|i| i.get(key).and_then(Value::as_str) == Some(value))
        })
}

pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    let dir = data_dir().ok_or("cannot find a data folder; set PLANVIL_HOME")?;
    write_marketplace(&dir, &exe)?;
    let dir_str = dir.display().to_string();
    let mut log = vec![format!(
        "wrote plugin to {dir_str} (Codex hooks call {})",
        exe.display()
    )];
    if lists(
        &["plugin", "marketplace", "list", "--json"],
        "marketplaces",
        "name",
        MARKETPLACE,
    ) {
        log.push("the planvil marketplace is already configured".into());
    } else {
        codex(&["plugin", "marketplace", "add", &dir_str])?;
        log.push("added the planvil marketplace".into());
    }
    if lists(
        &["plugin", "list", "--json"],
        "installed",
        "pluginId",
        PLUGIN_ID,
    ) {
        codex(&["plugin", "remove", PLUGIN_ID])?;
    }
    codex(&["plugin", "add", PLUGIN_ID])?;
    log.push(format!("installed {PLUGIN_ID} in Codex"));
    log.push(
        "start `codex` and, when it asks to review hooks, choose \"Trust all and continue\" \
         (or use /hooks). Until then planvil refuses to start a run."
            .into(),
    );
    Ok(log.join("\n"))
}

pub fn uninstall() -> Result<String, String> {
    let mut log = Vec::new();
    if lists(
        &["plugin", "list", "--json"],
        "installed",
        "pluginId",
        PLUGIN_ID,
    ) {
        codex(&["plugin", "remove", PLUGIN_ID])?;
        log.push(format!("removed {PLUGIN_ID} from Codex"));
    }
    if lists(
        &["plugin", "marketplace", "list", "--json"],
        "marketplaces",
        "name",
        MARKETPLACE,
    ) {
        codex(&["plugin", "marketplace", "remove", MARKETPLACE])?;
        log.push("removed the planvil marketplace from Codex".into());
    }
    log.push(
        "the shared plugin folder is kept for Claude Code; `planvil uninstall` removes it".into(),
    );
    Ok(log.join("\n"))
}
