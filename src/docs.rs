//! Feature documents: templates and the content gates that check them.
//!
//! A gate checks substance, not existence: every required `##` section must hold real
//! content once template comments and placeholders are stripped.

use std::collections::BTreeSet;
use std::path::Path;

pub const FEATURE: &str = "FEATURE.md";
pub const AUDIT: &str = "AUDIT.md";
pub const PLAN: &str = "PLAN.md";
pub const DECISIONS: &str = "DECISIONS.md";
pub const HANDOFF: &str = "HANDOFF.md";
pub const REPORT: &str = "REPORT.md";
pub const VERIFY: &str = "verify.json";

/// Templates written into a new feature folder: (file name, body).
const TEMPLATES: [(&str, &str); 6] = [
    (FEATURE, include_str!("../templates/FEATURE.md")),
    (AUDIT, include_str!("../templates/AUDIT.md")),
    (PLAN, include_str!("../templates/PLAN.md")),
    (DECISIONS, include_str!("../templates/DECISIONS.md")),
    (HANDOFF, include_str!("../templates/HANDOFF.md")),
    (VERIFY, include_str!("../templates/verify.json")),
];

const FEATURE_SECTIONS: [&str; 5] = [
    "Goal",
    "Scope",
    "Out of scope",
    "Behavior",
    "Acceptance criteria",
];
const AUDIT_SECTIONS: [&str; 5] = [
    "Affected code",
    "Current behavior",
    "Reuse",
    "Constraints",
    "Gaps",
];
const PLAN_SECTIONS: [&str; 4] = ["Status", "Steps", "Verification", "Risks"];
const DECISIONS_SECTIONS: [&str; 2] = ["Decided", "Open questions"];

/// Write any missing template into `feature_dir`. Existing files are never touched.
/// Returns the names of files created.
pub fn write_missing_templates(feature_dir: &Path, branch: &str) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(feature_dir)?;
    let mut created = Vec::new();
    for (name, body) in TEMPLATES {
        let path = feature_dir.join(name);
        if path.exists() {
            continue;
        }
        std::fs::write(&path, body.replace("{{branch}}", branch))?;
        created.push(name.to_string());
    }
    Ok(created)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub body: String,
}

/// Split Markdown into `##` sections. Comments are removed first, and headings inside
/// fenced code blocks (backtick or tilde fences) are ignored.
pub fn sections(md: &str) -> Vec<Section> {
    let text = strip_comments(md);
    let mut out: Vec<Section> = Vec::new();
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        for marker in ["```", "~~~"] {
            if trimmed.starts_with(marker) {
                fence = match fence {
                    Some(open) if open == marker => None,
                    None => Some(marker),
                    other => other,
                };
            }
        }
        let heading = fence.is_none().then(|| line.strip_prefix("## ")).flatten();
        match (heading, out.last_mut()) {
            (Some(h), _) => out.push(Section {
                heading: h.trim().to_string(),
                body: String::new(),
            }),
            (None, Some(current)) => {
                current.body.push_str(line);
                current.body.push('\n');
            }
            (None, None) => {}
        }
    }
    out
}

fn find<'a>(sections: &'a [Section], heading: &str) -> Option<&'a Section> {
    sections
        .iter()
        .find(|s| s.heading.eq_ignore_ascii_case(heading))
}

/// Remove `<!-- ... -->` comments (possibly multi-line).
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("-->") {
            Some(end) => &rest[start + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn is_table_line(line: &str) -> bool {
    line.starts_with('|')
}

fn is_table_separator(line: &str) -> bool {
    is_table_line(line) && line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

const PLACEHOLDERS: [&str; 6] = ["todo", "tbd", "...", "-", "n/a?", "fill me"];

/// True when a section body holds real content: not just comments, placeholders,
/// or a table with no data rows.
pub fn has_content(body: &str) -> bool {
    let text = strip_comments(body);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let prose = lines
        .iter()
        .any(|l| !is_table_line(l) && !PLACEHOLDERS.contains(&l.to_ascii_lowercase().as_str()));
    // A table needs a header, a separator, and at least one row that is not placeholders.
    let table_rows = lines
        .iter()
        .filter(|l| is_table_line(l) && !is_table_separator(l))
        .skip(1)
        .any(|row| {
            row.split('|').map(str::trim).any(|cell| {
                !cell.is_empty() && !PLACEHOLDERS.contains(&cell.to_ascii_lowercase().as_str())
            })
        });
    prose || table_rows
}

/// Acceptance-criterion IDs (`AC-01`, `AC-123`) in order of first appearance.
pub fn ac_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    let mut i = 0;
    while let Some(pos) = text[i..].find("AC-") {
        let start = i + pos;
        let digits_end = text[start + 3..]
            .find(|c: char| !c.is_ascii_digit())
            .map_or(text.len(), |n| start + 3 + n);
        let boundary_before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        if digits_end > start + 3 && boundary_before {
            let id = text[start..digits_end].to_string();
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
        i = start + 3;
    }
    ids
}

fn required_sections(md: &str, required: &[&str], doc: &str) -> (Vec<Section>, Vec<String>) {
    let secs = sections(md);
    let problems = required
        .iter()
        .filter_map(|name| match find(&secs, name) {
            None => Some(format!("{doc}: missing section `## {name}`")),
            Some(s) if !has_content(&s.body) => {
                Some(format!("{doc}: section `## {name}` is empty"))
            }
            Some(_) => None,
        })
        .collect();
    (secs, problems)
}

/// FEATURE.md: all sections filled and at least one acceptance criterion.
pub fn check_feature(md: &str) -> Vec<String> {
    let (secs, mut problems) = required_sections(md, &FEATURE_SECTIONS, FEATURE);
    if find(&secs, "Acceptance criteria").is_some() && feature_criteria(md).is_empty() {
        problems.push(format!(
            "{FEATURE}: no acceptance criteria with IDs like AC-01"
        ));
    }
    problems
}

/// Acceptance-criterion IDs declared in FEATURE.md's criteria section: the ID that starts a
/// table row or list item. IDs mentioned in prose ("supersedes AC-09") are not criteria.
pub fn feature_criteria(md: &str) -> Vec<String> {
    let secs = sections(md);
    let Some(section) = find(&secs, "Acceptance criteria") else {
        return Vec::new();
    };
    let mut ids: Vec<String> = Vec::new();
    for line in section.body.lines() {
        let lead = line
            .trim_start()
            .trim_start_matches(['|', '-', '*', '+', ' ', '[', ']', 'x'])
            .trim_start();
        if let Some(id) = ac_ids(lead)
            .into_iter()
            .next()
            .filter(|id| lead.starts_with(id.as_str()))
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

/// AUDIT.md: all sections filled, and grounded in at least one existing repo path.
pub fn check_audit(md: &str, repo_root: &Path) -> Vec<String> {
    let (secs, mut problems) = required_sections(md, &AUDIT_SECTIONS, AUDIT);
    if let Some(affected) = find(&secs, "Affected code") {
        let grounded = backticked(&strip_comments(&affected.body))
            .iter()
            .any(|token| existing_repo_path(repo_root, token));
        if !grounded {
            problems.push(format!(
                "{AUDIT}: `## Affected code` names no existing repository path in backticks"
            ));
        }
    }
    problems
}

/// PLAN.md: all sections filled and every criterion mapped under `## Verification`.
pub fn check_plan(md: &str, criteria: &[String]) -> Vec<String> {
    let (secs, mut problems) = required_sections(md, &PLAN_SECTIONS, PLAN);
    if let Some(verification) = find(&secs, "Verification") {
        let mapped: BTreeSet<String> = ac_ids(&strip_comments(&verification.body))
            .into_iter()
            .collect();
        problems.extend(
            criteria
                .iter()
                .filter(|id| !mapped.contains(*id))
                .map(|id| format!("{PLAN}: {id} is not mapped under `## Verification`")),
        );
    }
    problems
}

/// After a failed verification, AUDIT.md must explain each failure under
/// `## Failure analysis` before the loop continues.
pub fn check_failure_analysis(md: &str, failing: &[String]) -> Vec<String> {
    let secs = sections(md);
    let Some(section) = find(&secs, "Failure analysis").filter(|s| has_content(&s.body)) else {
        return vec![format!(
            "{AUDIT}: verification failed; add `## Failure analysis` with the root cause of each failure"
        )];
    };
    failing
        .iter()
        .filter(|name| !section.body.contains(name.as_str()))
        .map(|name| {
            format!("{AUDIT}: `## Failure analysis` does not explain the failure of `{name}`")
        })
        .collect()
}

/// DECISIONS.md: decisions recorded and no unchecked open question.
pub fn check_decisions(md: &str) -> Vec<String> {
    let (secs, mut problems) = required_sections(md, &DECISIONS_SECTIONS[..1], DECISIONS);
    match find(&secs, "Open questions") {
        None => problems.push(format!("{DECISIONS}: missing section `## Open questions`")),
        Some(open) => {
            let unchecked = strip_comments(&open.body)
                .lines()
                .filter(|l| l.trim_start().starts_with("- [ ]"))
                .count();
            if unchecked > 0 {
                problems.push(format!(
                    "{DECISIONS}: {unchecked} open question(s) still unchecked"
                ));
            }
        }
    }
    problems
}

fn backticked(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// `token` (optionally with `:line` or `::symbol`) names an existing file in the repo.
fn existing_repo_path(repo_root: &Path, token: &str) -> bool {
    let path = token.split("::").next().unwrap_or("");
    let path = path.split(':').next().unwrap_or("").trim();
    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|seg| seg == ".." || seg == ".")
    {
        return false;
    }
    repo_root.join(path).is_file()
}

#[cfg(test)]
#[path = "docs_tests.rs"]
mod tests;
