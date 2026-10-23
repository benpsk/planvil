//! Branch name → feature folder mapping.
//!
//! One flat folder per branch under the docs root, with `/` written as `-`:
//! `sp20/mcp` → `planvil/sp20-mcp/`.

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutError {
    #[error("branch name is empty")]
    EmptyBranch,
    #[error("branch name `{0}` contains characters outside [A-Za-z0-9._/-]")]
    InvalidChars(String),
    #[error("branch name `{0}` has an empty, `.` or `..` segment, or starts with `-`")]
    InvalidSegment(String),
    #[error("docs_root `{0}` must be a relative path inside the repository")]
    InvalidDocsRoot(String),
}

/// Folder name for a branch: every `/` becomes `-`.
pub fn branch_to_folder(branch: &str) -> Result<String, LayoutError> {
    if branch.is_empty() {
        return Err(LayoutError::EmptyBranch);
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-');
    if !branch.chars().all(allowed) {
        return Err(LayoutError::InvalidChars(branch.to_string()));
    }
    let bad_segment = |s: &str| s.is_empty() || s == "." || s == ".." || s.starts_with('-');
    if branch.split('/').any(bad_segment) {
        return Err(LayoutError::InvalidSegment(branch.to_string()));
    }
    Ok(branch.replace('/', "-"))
}

/// `<repo_root>/<docs_root>/<branch-folder>`. `docs_root` must stay inside the repo.
pub fn feature_dir(
    repo_root: &Path,
    docs_root: &str,
    branch: &str,
) -> Result<PathBuf, LayoutError> {
    validate_docs_root(docs_root)?;
    Ok(repo_root.join(docs_root).join(branch_to_folder(branch)?))
}

/// `docs_root` must be non-empty and made only of plain relative segments.
pub fn validate_docs_root(docs_root: &str) -> Result<(), LayoutError> {
    let path = Path::new(docs_root);
    let plain = path.components().all(|c| matches!(c, Component::Normal(_)));
    if docs_root.is_empty() || !plain {
        return Err(LayoutError::InvalidDocsRoot(docs_root.to_string()));
    }
    // Never write run files into git internals, host config, or a run folder.
    let first = path
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .unwrap_or("");
    if first.starts_with(".git") || first == ".claude" || first == ".planvil" {
        return Err(LayoutError::InvalidDocsRoot(docs_root.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_slash_branches_to_flat_folder() {
        assert_eq!(branch_to_folder("sp20/mcp").unwrap(), "sp20-mcp");
        assert_eq!(branch_to_folder("feat/hrm-mcp").unwrap(), "feat-hrm-mcp");
        assert_eq!(
            branch_to_folder("feat/mailbox/phase2").unwrap(),
            "feat-mailbox-phase2"
        );
    }

    #[test]
    fn keeps_branch_without_slash() {
        assert_eq!(branch_to_folder("hotfix").unwrap(), "hotfix");
    }

    #[test]
    fn rejects_empty_branch() {
        assert_eq!(branch_to_folder(""), Err(LayoutError::EmptyBranch));
    }

    #[test]
    fn rejects_traversal_and_bad_segments() {
        for bad in ["../x", "a/../b", "a//b", "a/", "/a", "./a", "-x"] {
            assert!(
                matches!(branch_to_folder(bad), Err(LayoutError::InvalidSegment(_))),
                "expected InvalidSegment for {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_characters() {
        for bad in ["a b", "a\\b", "a;rm", "a\nb", "ä"] {
            assert!(
                matches!(branch_to_folder(bad), Err(LayoutError::InvalidChars(_))),
                "expected InvalidChars for {bad:?}"
            );
        }
    }

    #[test]
    fn builds_feature_dir_under_docs_root() {
        let dir = feature_dir(Path::new("/repo"), "planvil", "sp20/mcp").unwrap();
        assert_eq!(dir, PathBuf::from("/repo/planvil/sp20-mcp"));
    }

    #[test]
    fn allows_nested_relative_docs_root() {
        let dir = feature_dir(Path::new("/repo"), "docs/planvil", "hotfix").unwrap();
        assert_eq!(dir, PathBuf::from("/repo/docs/planvil/hotfix"));
    }

    #[test]
    fn rejects_docs_root_escaping_repo() {
        for bad in [
            "",
            "/abs",
            "../out",
            "a/../../out",
            ".",
            ".git/hooks",
            ".github",
            ".claude",
            ".planvil",
        ] {
            assert!(
                matches!(
                    feature_dir(Path::new("/repo"), bad, "x"),
                    Err(LayoutError::InvalidDocsRoot(_))
                ),
                "expected InvalidDocsRoot for {bad:?}"
            );
        }
    }
}
