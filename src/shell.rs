//! Best-effort shell command classification.
//!
//! This is not a sandbox. It recognizes a conservative set of read-only commands and
//! refuses anything it cannot parse confidently: multi-line input, heredocs, comments,
//! command and process substitution, ANSI-C quoting, variable expansion in risky places,
//! and output redirection to files. Each allowed command has an option allowlist or
//! denylist for the flags that write files or run programs. `planvil doctor` reports this
//! limit.

const PLANVIL: &str = "planvil";
/// planvil subcommands the agent must never run itself.
const AGENT_FORBIDDEN_PLANVIL: [&str; 7] = [
    "pause",
    "resume",
    "cancel",
    "init",
    "install",
    "uninstall",
    "hook",
];
const AGENT_PLANVIL: [&str; 15] = [
    "setup",
    "rename",
    "--version",
    "-V",
    "start",
    "status",
    "advance",
    "ask",
    "answer",
    "verify",
    "report",
    "doctor",
    "next",
    "help",
    "--help",
];
/// Variables that may prefix a read-only command.
const HARMLESS_VARS: [&str; 6] = ["LANG", "TZ", "NO_COLOR", "TERM", "COLUMNS", "LINES"];
/// Flag fragments that commonly run programs or write files; refused on readonly_commands.
const RISKY_FLAG_FRAGMENTS: [&str; 7] = [
    "exec", "overlay", "output", "profile", "pre", "pager", "toolexec",
];

/// A word plus whether it had unquoted glob characters (which may expand into options).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    text: String,
    glob: bool,
    /// Contains `$` outside single quotes, so the shell expands it.
    expands: bool,
}

/// Split `cmd` into simple commands (lists of words).
pub fn segments(cmd: &str) -> Result<Vec<Vec<String>>, String> {
    Ok(tokenize(cmd)?
        .into_iter()
        .map(|seg| seg.into_iter().map(|w| w.text).collect())
        .collect())
}

/// Characters that end an unquoted operand after `<` or `>`.
const OPERAND_END: &str = " \t;|&()<>";

fn tokenize(cmd: &str) -> Result<Vec<Vec<Word>>, String> {
    if cmd.contains(['\n', '\r']) {
        return Err("multi-line commands are not classified as read-only".into());
    }
    for (pat, what) in [
        ("$'", "ANSI-C quoting `$'...'`"),
        ("$\"", "locale quoting `$\"...\"`"),
        ("<<", "heredocs"),
        ("`", "backticks"),
        ("$(", "command substitution `$(...)`"),
    ] {
        if cmd.contains(pat) {
            return Err(format!("{what} not allowed"));
        }
    }
    let chars: Vec<char> = cmd.chars().collect();
    let mut segs: Vec<Vec<Word>> = vec![Vec::new()];
    let mut word = String::new();
    let mut glob = false;
    let mut expands = false;
    let mut in_word = false;
    let mut i = 0;

    macro_rules! end_word {
        () => {
            if in_word {
                segs.last_mut().ok_or("parser state")?.push(Word {
                    text: std::mem::take(&mut word),
                    glob,
                    expands,
                });
                in_word = false;
                glob = false;
                expands = false;
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            '#' if !in_word => return Err("shell comments are not classified".into()),
            '\'' => {
                let close = chars[i + 1..]
                    .iter()
                    .position(|&ch| ch == '\'')
                    .ok_or("unclosed quote")?;
                word.extend(&chars[i + 1..i + 1 + close]);
                in_word = true;
                i += close + 2;
                continue;
            }
            '"' => {
                i += 1;
                in_word = true;
                loop {
                    match chars.get(i) {
                        None => return Err("unclosed quote".into()),
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(&next) = chars.get(i + 1) {
                                word.push(next);
                            }
                            i += 2;
                            continue;
                        }
                        Some(&ch) => {
                            expands |= ch == '$';
                            word.push(ch);
                        }
                    }
                    i += 1;
                }
            }
            '\\' => {
                if let Some(&next) = chars.get(i + 1) {
                    word.push(next);
                    in_word = true;
                }
                i += 2;
                continue;
            }
            ' ' | '\t' => end_word!(),
            ';' | '|' | '&' | '(' | ')' => {
                end_word!();
                if segs.last().is_some_and(|s| !s.is_empty()) {
                    segs.push(Vec::new());
                }
            }
            '>' => {
                // A pure-digit word right before `>` is a file descriptor, not an argument.
                if in_word && word.chars().all(|ch| ch.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                end_word!();
                i += 1;
                let dup = chars.get(i) == Some(&'&');
                if matches!(chars.get(i), Some('>') | Some('&') | Some('|')) {
                    i += 1;
                }
                let (target, next) = operand(&chars, i)?;
                i = next;
                let to_fd = dup && (target.chars().all(|ch| ch.is_ascii_digit()) || target == "-");
                if !(to_fd || target == "/dev/null") {
                    return Err(format!("output redirection to `{target}` writes a file"));
                }
                continue;
            }
            '<' => {
                end_word!();
                if chars.get(i + 1) == Some(&'(') {
                    return Err("process substitution `<(...)` is not allowed".into());
                }
                let (_input, next) = operand(&chars, i + 1)?;
                i = next;
                continue;
            }
            '$' => {
                word.push(c);
                expands = true;
                in_word = true;
            }
            '*' | '?' | '[' => {
                word.push(c);
                glob = true;
                in_word = true;
            }
            _ => {
                word.push(c);
                in_word = true;
            }
        }
        i += 1;
    }
    if in_word {
        segs.last_mut().ok_or("parser state")?.push(Word {
            text: word,
            glob,
            expands,
        });
    }
    Ok(segs.into_iter().filter(|s| !s.is_empty()).collect())
}

/// The plain word after a redirection operator. Anything that could expand or execute
/// (quotes, `$`, backticks) is refused.
fn operand(chars: &[char], mut i: usize) -> Result<(String, usize), String> {
    while matches!(chars.get(i), Some(' ') | Some('\t')) {
        i += 1;
    }
    let start = i;
    while i < chars.len() && !OPERAND_END.contains(chars[i]) {
        if matches!(chars[i], '$' | '`' | '\'' | '"' | '\\') {
            return Err("redirection targets must be plain paths".into());
        }
        i += 1;
    }
    if i == start {
        return Err("redirection without a target".into());
    }
    Ok((chars[start..i].iter().collect(), i))
}

fn basename(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

fn assignment_name(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .then_some(name)
}

fn harmless_var(name: &str) -> bool {
    HARMLESS_VARS.contains(&name) || name.starts_with("LC_")
}

/// Leading `VAR=value` words and transparent wrappers removed; errors if a wrapper or
/// assignment could change what runs.
fn strip_wrappers(words: &[Word]) -> Result<&[Word], String> {
    let mut rest = words;
    loop {
        let Some(first) = rest.first() else {
            return Ok(rest);
        };
        let w = first.text.as_str();
        if let Some(name) = assignment_name(w) {
            if !harmless_var(name) {
                return Err(format!("setting `{name}` can change what a command runs"));
            }
            rest = &rest[1..];
            continue;
        }
        match basename(w) {
            "time" => rest = &rest[1..],
            "env" if rest.len() > 1 => {
                if rest[1].text.starts_with('-') {
                    return Err("`env` options can run other commands".into());
                }
                rest = &rest[1..];
            }
            "nice" | "timeout" => {
                rest = &rest[1..];
                while let Some(opt) = rest.first().filter(|o| o.text.starts_with('-')) {
                    let takes_value = matches!(
                        opt.text.as_str(),
                        "-n" | "-s" | "-k" | "--signal" | "--kill-after"
                    );
                    rest = rest.get(if takes_value { 2 } else { 1 }..).unwrap_or(&[]);
                }
                if basename(w) == "timeout" {
                    rest = rest.get(1..).unwrap_or(&[]);
                }
            }
            _ => return Ok(rest),
        }
    }
}

/// Command words with assignments skipped, for detection (not classification).
fn command_words(seg: &[Word]) -> &[Word] {
    let skip = seg
        .iter()
        .take_while(|w| assignment_name(&w.text).is_some())
        .count();
    let rest = &seg[skip..];
    match rest.first().map(|w| basename(&w.text)) {
        Some("time" | "nice" | "command" | "exec" | "env" | "timeout" | "sudo") => {
            command_words(&rest[1..])
        }
        _ => rest,
    }
}

fn whole_words(cmd: &str) -> Vec<String> {
    cmd.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Why the agent may not run this command, if it may invoke a user-only planvil action.
pub fn forbidden_planvil(cmd: &str) -> Option<String> {
    let message = |sub: &str| {
        format!(
            "`planvil {sub}` is a user action. Ask the user to type `planvil {sub}` as their own message \
             (or run it in their own terminal)."
        )
    };
    let parsed = tokenize(cmd).ok();
    if let Some(segs) = &parsed {
        for seg in segs {
            let words = command_words(seg);
            let Some(first) = words.first() else { continue };
            if basename(&first.text) == PLANVIL || first.expands {
                let sub = words[1..].iter().find(|w| !w.text.starts_with('-'));
                if let Some(sub) =
                    sub.filter(|s| AGENT_FORBIDDEN_PLANVIL.contains(&s.text.as_str()))
                {
                    return Some(message(&sub.text));
                }
            }
        }
    }
    // Fallback for text the parser cannot follow (quoting tricks, `sh -c '...'`).
    let words = whole_words(cmd);
    let mentions_planvil = cmd.to_ascii_lowercase().contains(PLANVIL) || parsed.is_none();
    words
        .iter()
        .find(|w| {
            mentions_planvil
                && AGENT_FORBIDDEN_PLANVIL.contains(&w.as_str())
                && cmd.contains(w.as_str())
        })
        .filter(|w| {
            !matches!(w.as_str(), "init" | "install" | "uninstall")
                || cmd.to_ascii_lowercase().contains(PLANVIL)
        })
        .map(|w| message(w))
}

/// Git subcommands that discard work, move HEAD off the run's branch, or leave the worktree.
fn destructive_git_sub(sub: &str, args: &[String]) -> Option<&'static str> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let any = |flags: &[&str]| args.iter().any(|a| flags.contains(&a.as_str()));
    match sub {
        "push" => Some("pushing is outside planvil's scope; the user pushes"),
        "pull" | "merge" | "rebase" | "filter-branch" | "update-ref" | "init" => {
            Some("history-changing or merging")
        }
        "checkout" | "switch" => {
            Some("switching branches or discarding changes would leave the run")
        }
        "symbolic-ref" if args.len() > 2 => Some("moves HEAD"),
        "reset" if any(&["--hard", "--merge", "--keep"]) => Some("discards changes"),
        "clean" => Some("deletes untracked files"),
        "restore" if !has("--staged") || any(&["--worktree", "-W"]) => Some("discards changes"),
        "stash" if !matches!(args.get(1).map(String::as_str), Some("list" | "show")) => {
            Some("hides user changes")
        }
        "branch"
            if any(&[
                "-D", "-d", "--delete", "-m", "-M", "--move", "-c", "-C", "--copy", "-f", "--force",
            ]) =>
        {
            Some("deletes, renames or moves a branch")
        }
        "worktree"
            if matches!(
                args.get(1).map(String::as_str),
                Some("add" | "remove" | "move" | "prune")
            ) =>
        {
            Some("changes worktrees")
        }
        _ => None,
    }
}

const DESTRUCTIVE_WORDS: [&str; 15] = [
    "push",
    "pull",
    "merge",
    "rebase",
    "filter-branch",
    "update-ref",
    "init",
    "checkout",
    "switch",
    "symbolic-ref",
    "reset",
    "clean",
    "restore",
    "stash",
    "worktree",
];

/// Git operations that discard user work or move the run off its branch.
pub fn destructive_git(cmd: &str) -> Option<String> {
    let Ok(segs) = tokenize(cmd) else {
        // Cannot follow it: refuse if it looks like a destructive git call at all.
        let words = whole_words(cmd);
        let mentions_git = words.iter().any(|w| w == "git");
        let hit = words
            .iter()
            .find(|w| mentions_git && DESTRUCTIVE_WORDS.contains(&w.as_str()));
        return hit.map(|w| format!("`git {w}` is not allowed during a planvil run"));
    };
    segs.iter().find_map(|seg| {
        let words = command_words(seg);
        if words.first().map(|w| basename(&w.text)) != Some("git") {
            return None;
        }
        let texts: Vec<String> = words[1..].iter().map(|w| w.text.clone()).collect();
        let args = git_args(&texts).ok()?;
        let sub = args.first()?.as_str();
        destructive_git_sub(sub, args)
            .map(|r| format!("`git {sub}` is not allowed during a planvil run ({r})"))
    })
}

/// Skip git's global options. Errors on options that inject configuration or programs.
fn git_args(args: &[String]) -> Result<&[String], String> {
    let mut rest = args;
    loop {
        let Some(a) = rest.first().map(String::as_str) else {
            return Ok(rest);
        };
        match a {
            "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                rest = rest.get(2..).unwrap_or(&[])
            }
            "--no-pager" | "-P" | "--no-replace-objects" | "--literal-pathspecs" | "--bare" => {
                rest = &rest[1..]
            }
            "-c" | "--config-env" | "--exec-path" => {
                return Err(format!("git `{a}` can inject configuration or programs"));
            }
            _ if a.starts_with("-c")
                || a.starts_with("--config-env")
                || a.starts_with("--exec-path") =>
            {
                return Err(format!("git `{a}` can inject configuration or programs"));
            }
            _ if a.starts_with("-C")
                || a.starts_with("--git-dir=")
                || a.starts_with("--work-tree=")
                || a.starts_with("--namespace=") =>
            {
                rest = &rest[1..];
            }
            _ if a.starts_with('-') => {
                return Err(format!("git global option `{a}` is not classified"));
            }
            _ => return Ok(rest),
        }
    }
}

/// `Ok(())` when every part of `cmd` is a recognized read-only command.
pub fn read_only(cmd: &str, extra_prefixes: &[String]) -> Result<(), String> {
    for seg in tokenize(cmd)? {
        let words = strip_wrappers(&seg)?;
        if words.is_empty() {
            continue;
        }
        if words.iter().any(|w| w.expands) {
            return Err("variable expansion is not classified as read-only".into());
        }
        if matches_prefix(words, extra_prefixes)? {
            continue;
        }
        read_only_words(words)?;
    }
    Ok(())
}

/// A configured read-only prefix matches by exact command name (no paths) and without
/// flags that commonly run programs or write files.
fn matches_prefix(words: &[Word], prefixes: &[String]) -> Result<bool, String> {
    let hit = prefixes.iter().any(|p| {
        let want: Vec<&str> = p.split_whitespace().collect();
        !want.is_empty()
            && want.len() <= words.len()
            && want.iter().zip(words).all(|(w, got)| *w == got.text)
    });
    if !hit {
        return Ok(false);
    }
    let risky = words[1..].iter().find(|w| {
        let t = w.text.to_ascii_lowercase();
        t == "-o" || (t.starts_with('-') && RISKY_FLAG_FRAGMENTS.iter().any(|f| t.contains(f)))
    });
    match risky {
        Some(flag) => Err(format!("`{}` may run programs or write files", flag.text)),
        None => Ok(true),
    }
}

/// Commands that read files and have no writing or executing options worth worrying about.
const PLAIN_READERS: [&str; 32] = [
    "ls", "cat", "head", "tail", "wc", "cut", "tr", "nl", "grep", "egrep", "fgrep", "du", "df",
    "stat", "which", "type", "pwd", "echo", "printf", "true", "false", "whoami", "uname",
    "printenv", "basename", "dirname", "realpath", "readlink", "cmp", "comm", "test", "cd",
];
/// Readers that are safe only without arguments (with arguments they can change the system).
const BARE_ONLY: [&str; 4] = ["date", "hostname", "id", "env"];
/// Commands whose options can run programs or write files: glob words could expand into
/// such options, so they are refused.
const OPTION_SENSITIVE: [&str; 13] = [
    "sed", "sort", "uniq", "rg", "ag", "fd", "tree", "file", "find", "xargs", "git", "go", "cargo",
];

fn read_only_words(words: &[Word]) -> Result<(), String> {
    let cmd = basename(&words[0].text);
    let args: Vec<&str> = words[1..].iter().map(|w| w.text.as_str()).collect();
    let deny = |why: &str| Err(format!("`{cmd}` {why}"));
    if OPTION_SENSITIVE.contains(&cmd) && words[1..].iter().any(|w| w.glob) {
        return deny(
            "with unquoted glob patterns may expand into options; quote them or list paths",
        );
    }
    if words[0].text.contains('/') && cmd != PLANVIL {
        return deny("must be run by name, not by path");
    }
    let has = |pred: &dyn Fn(&str) -> bool| args.iter().any(|a| pred(a));
    let short_cluster_has = |c: char| {
        args.iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains(c))
    };
    match cmd {
        c if PLAIN_READERS.contains(&c) => Ok(()),
        c if BARE_ONLY.contains(&c) && args.is_empty() => Ok(()),
        "diff" | "jq" | "column" | "md5sum" | "sha256sum" | "sha1sum" | "[" => Ok(()),
        "sed" => sed_read_only(&args).or_else(|why| deny(&why)),
        "sort"
            if short_cluster_has('o')
                || has(&|a| a.starts_with("--output") || a.starts_with("--compress-program")) =>
        {
            deny("with -o or --compress-program writes files or runs programs")
        }
        "sort" => Ok(()),
        "uniq" if args.iter().filter(|a| !a.starts_with('-')).count() > 1 => {
            deny("with an output file writes it")
        }
        "uniq" => Ok(()),
        "rg" if short_cluster_has('z')
            || has(&|a| {
                a.starts_with("--pre")
                    || a.starts_with("--hostname-bin")
                    || a.starts_with("--search-zip")
            }) =>
        {
            deny("with --pre, --hostname-bin or -z runs programs")
        }
        "rg" => Ok(()),
        "fd" if short_cluster_has('x')
            || short_cluster_has('X')
            || has(&|a| a.starts_with("--exec")) =>
        {
            deny("with -x/-X/--exec runs programs")
        }
        "fd" => Ok(()),
        "ag" if has(&|a| a.starts_with("--pager")) => deny("with --pager runs a program"),
        "ag" => Ok(()),
        "tree" if short_cluster_has('o') || has(&|a| a.starts_with("--output")) => {
            deny("with -o writes a file")
        }
        "tree" => Ok(()),
        "file" if short_cluster_has('C') || has(&|a| a.starts_with("--compile")) => {
            deny("with -C writes a file")
        }
        "file" => Ok(()),
        "find"
            if has(&|a| {
                matches!(
                    a,
                    "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir" | "-fls"
                ) || a.starts_with("-fprint")
            }) =>
        {
            deny("with -delete/-exec/-fprint can modify files or run programs")
        }
        "find" => Ok(()),
        "xargs" => xargs_read_only(&words[1..]),
        "git" => {
            let texts: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            git_read_only(git_args(&texts)?)
        }
        "go" => go_read_only(&args).or_else(|why| deny(&why)),
        "cargo" if has(&|a| a.starts_with("--config") || a.starts_with("-Z")) => {
            deny("with --config or -Z can run programs")
        }
        "cargo" => match args.first() {
            Some(&("tree" | "metadata" | "--version" | "-V")) => Ok(()),
            _ => deny("subcommand may build or modify files; add it to readonly_commands if safe"),
        },
        PLANVIL => match args.first() {
            Some(sub) if AGENT_PLANVIL.contains(sub) => Ok(()),
            None => Ok(()),
            Some(sub) => deny(&format!("{sub} is not an agent command")),
        },
        _ => deny(
            "is not a recognized read-only command; add it to readonly_commands in .planvil.json if it is",
        ),
    }
}

/// sed is allowed only to print line ranges: `sed -n '1,20p' file`, `sed -n '$p' file`.
fn sed_read_only(args: &[&str]) -> Result<(), String> {
    let mut script = None;
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match *a {
            "-n" | "--quiet" | "--silent" | "-E" | "-r" | "--regexp-extended" => {}
            "-e" | "--expression" => script = iter.next().copied(),
            _ if a.starts_with('-') => {
                return Err(format!("option `{a}` is not allowed (only -n, -E, -e)"));
            }
            _ if script.is_none() => script = Some(*a),
            _ => {}
        }
    }
    let script = script.ok_or("needs a script")?;
    let address = |s: &str| s == "$" || (!s.is_empty() && s.chars().all(|c| c.is_ascii_digit()));
    let body = script
        .strip_suffix('p')
        .or_else(|| script.strip_suffix('q'))
        .ok_or("only line-printing scripts like `1,20p` are allowed")?;
    let ok = match body.split_once(',') {
        Some((a, b)) => address(a) && address(b),
        None => address(body),
    };
    if ok {
        Ok(())
    } else {
        Err("only line-printing scripts like `1,20p` are allowed".into())
    }
}

fn xargs_read_only(words: &[Word]) -> Result<(), String> {
    let mut rest = words;
    while let Some(a) = rest.first() {
        match a.text.as_str() {
            "-0" | "-r" | "--null" | "--no-run-if-empty" => rest = &rest[1..],
            "-n" | "-P" | "-I" | "-L" => rest = rest.get(2..).unwrap_or(&[]),
            t if t.starts_with('-') => return Err(format!("`xargs {t}` is not allowed")),
            _ => break,
        }
    }
    if rest.is_empty() {
        Ok(())
    } else {
        read_only_words(rest)
    }
}

fn go_read_only(args: &[&str]) -> Result<(), String> {
    const RISKY: [&str; 7] = [
        "-toolexec",
        "-exec",
        "-overlay",
        "-modfile",
        "-export",
        "-compiled",
        "-mod",
    ];
    if args.iter().any(|a| RISKY.iter().any(|r| a.starts_with(r))) {
        return Err("with -toolexec/-exec/-overlay/-mod… can run programs or modify go.mod".into());
    }
    match args.first() {
        Some(&("list" | "doc" | "version")) => Ok(()),
        Some(&"env") if !args.iter().any(|a| *a == "-w" || *a == "-u") => Ok(()),
        _ => {
            Err("subcommand may build or modify files; add it to readonly_commands if safe".into())
        }
    }
}

/// Options that make an otherwise read-only git subcommand write files or run programs.
fn git_option_risky(a: &str) -> bool {
    a.starts_with("--output")
        || a.starts_with("-O")
        || a.starts_with("--open-files-in-pager")
        || a == "--ext-diff"
        || a == "--textconv"
        || a.starts_with("--exec")
}

fn git_read_only(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    if let Some(bad) = rest.iter().find(|a| git_option_risky(a)) {
        return Err(format!("`git {sub} {bad}` can write files or run programs"));
    }
    let only = |allowed: &[&str]| rest.iter().all(|a| allowed.contains(a));
    let ok = match sub {
        "status" | "log" | "diff" | "show" | "rev-parse" | "ls-files" | "ls-tree" | "blame"
        | "grep" | "describe" | "shortlog" | "cat-file" | "merge-base" | "rev-list" => true,
        "branch" => only(&[
            "-a",
            "-r",
            "-v",
            "-vv",
            "--list",
            "--show-current",
            "--all",
            "--remotes",
        ]),
        "remote" => only(&["-v"]) || (rest.first() == Some(&"get-url") && rest.len() == 2),
        "tag" => only(&["-l", "--list"]),
        "stash" => matches!(rest.first(), Some(&("list" | "show"))),
        "worktree" => rest == ["list"],
        "config" => {
            const SAFE: [&str; 10] = [
                "--get",
                "--get-all",
                "--list",
                "-l",
                "--show-origin",
                "--global",
                "--local",
                "--system",
                "--null",
                "--name-only",
            ];
            let reads = rest
                .iter()
                .any(|a| matches!(*a, "--get" | "--get-all" | "--list" | "-l"));
            reads && rest.iter().all(|a| SAFE.contains(a) || !a.starts_with('-'))
        }
        "reflog" => rest
            .first()
            .is_none_or(|a| *a == "show" || a.starts_with('-') && !a.starts_with("--exec")),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("`git {sub}` is not a read-only git command"))
    }
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
