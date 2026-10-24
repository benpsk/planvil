//! File paths touched by a Codex `apply_patch` body.
//!
//! Codex edits files with one patch that can add, update, delete and move several files:
//!
//! ```text
//! *** Begin Patch
//! *** Update File: src/a.rs
//! *** Move to: src/b.rs
//! *** Add File: src/c.rs
//! *** Delete File: src/d.rs
//! *** End Patch
//! ```

const HEADERS: [&str; 4] = [
    "*** Add File: ",
    "*** Update File: ",
    "*** Delete File: ",
    "*** Move to: ",
];

/// Every path the patch writes, in order, without duplicates. Empty when the text has no
/// recognizable file header, which callers must treat as "cannot tell what this writes".
pub fn touched_paths(patch: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in patch.lines() {
        let line = line.trim_end_matches('\r');
        let Some(path) = HEADERS.iter().find_map(|h| line.strip_prefix(h)) else {
            continue;
        };
        let path = path.trim().to_string();
        if !path.is_empty() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_added_updated_deleted_and_moved_file() {
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Move to: src/b.rs\n\
                     *** Add File: src/c.rs\n+new\n*** Delete File: src/d.rs\n*** End Patch\n";
        assert_eq!(
            touched_paths(patch),
            vec!["src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs"]
        );
    }

    #[test]
    fn ignores_header_text_inside_hunks_and_dedupes() {
        let patch = "*** Begin Patch\n*** Update File: a.md\n+ the line *** Add File: x is text\n\
                     *** Update File: a.md\n*** End Patch\r\n";
        assert_eq!(touched_paths(patch), vec!["a.md"]);
    }

    #[test]
    fn unrecognizable_text_yields_no_paths() {
        assert!(touched_paths("rm -rf /").is_empty());
    }
}
