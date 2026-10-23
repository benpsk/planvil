//! Tests for `shell`.

use super::*;

fn ro(cmd: &str) -> bool {
    read_only(cmd, &[]).is_ok()
}

#[test]
fn plain_reads_are_read_only() {
    for cmd in [
        "ls -la",
        "cat src/main.rs | head -20",
        "grep -rn 'fn main' src && wc -l src/*.rs",
        "git status --short; git log --oneline -5",
        "git -C ../repo diff HEAD~1",
        "find . -name '*.go' | xargs grep -l Login",
        "rg Login 2>/dev/null",
        "cat file 2>&1 | sort | uniq -c",
        "LANG=C timeout 5 ls",
        "sed -n 1,20p file",
        "cd backend && go list ./...",
        "planvil status",
        "planvil advance",
    ] {
        assert!(
            ro(cmd),
            "expected read-only: {cmd} ({:?})",
            read_only(cmd, &[])
        );
    }
}

#[test]
fn writes_and_hidden_execution_are_not_read_only() {
    for cmd in [
        "echo x > src/main.rs",
        "echo x >> notes.txt",
        "cat a | tee b",
        "sed -i 's/a/b/' f",
        "sed --in-place s/a/b/ f",
        "rm -rf src",
        "find . -delete",
        "find . -exec rm {} \\;",
        "ls $(cat list)",
        "ls `pwd`",
        "diff <(ls a) <(ls b)",
        "git commit -m x",
        "git checkout -b x",
        "go build ./...",
        "cargo build",
        "python3 -c 'open(\"f\",\"w\")'",
        "awk '{print > \"out\"}' f",
        "sort -o out in",
        "xargs rm",
        "echo \"$(rm -rf /)\"",
        "cp a b",
        "mv a b",
        "touch x",
        "planvil pause",
    ] {
        assert!(!ro(cmd), "expected NOT read-only: {cmd}");
    }
}

#[test]
fn extra_prefixes_extend_the_allowlist() {
    let extra = vec!["go test".to_string(), "qmd search".to_string()];
    assert!(read_only("go test ./internal/...", &extra).is_ok());
    assert!(read_only("qmd search login", &extra).is_ok());
    assert!(read_only("go build", &extra).is_err());
}

#[test]
fn user_only_planvil_actions_are_forbidden_even_when_hidden() {
    for cmd in [
        "planvil resume",
        "./target/release/planvil cancel",
        "sh -c 'planvil pause'",
        "echo hi && planvil hook claude prompt",
        "planvil init",
    ] {
        assert!(forbidden_planvil(cmd).is_some(), "{cmd}");
    }
    assert!(forbidden_planvil("planvil status").is_none());
    assert!(forbidden_planvil("cat planvil/sp20-mcp/PLAN.md").is_none());
}

#[test]
fn destructive_git_is_flagged() {
    for cmd in [
        "git push origin main",
        "git reset --hard HEAD",
        "git clean -fd",
        "git checkout -- .",
        "git restore src/a.go",
        "git stash",
        "git rebase main",
        "git -C x push",
        "git branch -D old",
    ] {
        assert!(destructive_git(cmd).is_some(), "{cmd}");
    }
    for cmd in [
        "git status",
        "git commit -m x",
        "git restore --staged a",
        "git stash list",
        "git add .",
    ] {
        assert!(destructive_git(cmd).is_none(), "{cmd}");
    }
}

#[test]
fn segments_split_on_operators_and_respect_quotes() {
    let segs = segments("a 'b c' | d \"e;f\" && g").unwrap();
    assert_eq!(segs, vec![vec!["a", "b c"], vec!["d", "e;f"], vec!["g"]]);
}

#[test]
fn security_review_bypasses_are_not_read_only() {
    for cmd in [
        // Parser desync: ANSI-C quoting, heredocs, comments, substitution in redirects.
        "ls $'\\''\ntouch x\necho '",
        "cat <<E\nls '\nE\ntouch x # '",
        "cat <`touch${IFS}x`",
        "cat <<<`id`",
        "ls # ; touch x",
        "ls\ntouch x",
        // Read-only tools that write or execute.
        "sed -ni 's/a/b/p' f",
        "sed -Ei s/a/b/ f",
        "sed -n 'w src/x.go' f",
        "sed '1e touch x' f",
        "sed -f planvil/feat/x.sed f",
        "awk -f planvil/feat/x.awk f",
        "awk -i inplace '{print}' f",
        "sort -ofile in",
        "sort -uo file in",
        "sort --compress-program=sh in",
        "uniq /dev/null src/main.rs",
        "rg --pre sh pat planvil/feat/evil.txt",
        "rg --hostname-bin=sh pat",
        "rg -z pat",
        "fd -x rm",
        "fd --exec-batch rm",
        "ag --pager=sh pat",
        "tree -o out",
        "file -C",
        "xargs -a ls rm",
        "rg pat *",
        // git configuration and output injection.
        "git -c core.fsmonitor='touch /tmp/x' status",
        "git -ccore.pager=sh log",
        "git --config-env=core.pager=X log",
        "GIT_EXTERNAL_DIFF=sh git diff --no-index a b",
        "git diff --output=src/main.rs HEAD HEAD",
        "git log -p --output=f",
        "git grep -Osh pat",
        "git grep --open-files-in-pager=sh pat",
        "git diff --ext-diff",
        "git config -e",
        "git config --add x.y z",
        // Variables, wrappers and paths.
        "PATH=planvil/feat ls",
        "LD_PRELOAD=x ls",
        "env -S 'sh -c id'",
        "ls $X",
        "./evil/ls",
        "go list -toolexec=sh ./...",
        "go list -mod=mod ./...",
        "cargo --config x=y tree",
        "date -s now",
    ] {
        assert!(
            read_only(cmd, &[]).is_err(),
            "expected NOT read-only: {cmd:?}"
        );
    }
}

#[test]
fn line_printing_sed_is_still_allowed() {
    for cmd in [
        "sed -n 1,20p f",
        "sed -n '$p' f",
        "sed -n -e 5p f",
        "sed 3q f",
    ] {
        assert!(ro(cmd), "{cmd}");
    }
}

#[test]
fn readonly_prefixes_reject_exec_and_output_flags_and_paths() {
    let extra = vec!["go test".to_string()];
    assert!(read_only("go test ./...", &extra).is_ok());
    assert!(read_only("go test -exec 'sh x.sh' ./...", &extra).is_err());
    assert!(read_only("go test -toolexec=sh ./...", &extra).is_err());
    assert!(read_only("go test -coverprofile=src/x ./...", &extra).is_err());
    assert!(read_only("go test -o out ./...", &extra).is_err());
    assert!(read_only("./evil/go test ./...", &extra).is_err());
}

#[test]
fn planvil_evasions_are_forbidden() {
    for cmd in [
        "\"planvil\" resume",
        "'planvil' resume",
        "pl\"\"anvil resume",
        "p\\lanvil resume",
        "planvil \\\nresume",
        "p=planvil; $p hook claude prompt",
        "$(echo planvil) resume",
        "env planvil cancel",
        "planvil --quiet pause",
    ] {
        assert!(forbidden_planvil(cmd).is_some(), "{cmd:?}");
    }
    assert!(forbidden_planvil("planvil status").is_none());
    assert!(forbidden_planvil("grep -rn resume src").is_none());
}

#[test]
fn destructive_git_fails_closed_and_handles_attached_options() {
    for cmd in [
        "git push origin x $(true)",
        "git reset --hard `git rev-parse HEAD`",
        "git -C../x push",
        "git --namespace n push",
        "git checkout -b x",
        "git checkout --detach",
        "git switch -c x",
        "git branch -m new",
        "git symbolic-ref HEAD refs/heads/x",
        "git worktree add ../w",
        "git pull",
    ] {
        assert!(destructive_git(cmd).is_some(), "{cmd:?}");
    }
}
