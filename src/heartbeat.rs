//! Proof that planvil's hooks are actually running.
//!
//! A host can install the plugin yet not run its hooks: Codex skips hooks until the user
//! trusts them, and a disabled or broken plugin does the same. Without hooks a run would
//! be unenforced, silently. So the pre-tool hook touches a marker whenever the agent runs a
//! `planvil` command, and `planvil start`, run from the agent's shell, refuses unless the
//! marker was touched moments before by the hook for that very command.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// How recent the marker must be. The hook runs immediately before the command it allows.
pub const FRESH: Duration = Duration::from_secs(15);

/// Environment markers set by agent hosts in their shell tool.
const AGENT_SHELL_MARKERS: [&str; 2] = ["CLAUDECODE", "CODEX_CI"];

fn marker() -> PathBuf {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into());
    let user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    std::env::temp_dir().join(format!("planvil-hook-heartbeat-{user}"))
}

/// Called by the pre-tool hook. Best effort: a failure only makes `start` refuse.
pub fn touch() {
    let _ = std::fs::write(marker(), b"planvil hook ran\n");
}

/// Whether this process runs inside an agent host's shell tool.
pub fn in_agent_shell() -> bool {
    AGENT_SHELL_MARKERS
        .iter()
        .any(|m| std::env::var_os(m).is_some())
}

/// Whether a hook touched the marker within `within`.
pub fn fresh(within: Duration) -> bool {
    std::fs::metadata(marker())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age <= within)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touched_marker_is_fresh() {
        touch();
        assert!(fresh(FRESH));
    }
}
