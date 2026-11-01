---
name: planvil
description: Run an enforced feature workflow with planvil (setup → feature → audit → plan → decide → implement → validate → verify → done, looping on failure). Use when the user invokes planvil (/planvil:planvil in Claude Code, $planvil:planvil in Codex), asks to build a feature "with planvil", or wants to resume or check a planvil run.
argument-hint: "[branch] [spec.md] | init | status | report | doctor | help"
---

# planvil

You are driving a planvil run. The `planvil` binary owns the run state and the stage gates.
Hooks enforce them: before the plan gates pass, edits outside the feature folder are denied,
and you cannot finish while the run is incomplete. Follow the `next:` line that every
planvil command prints. These instructions guide you; they are not the enforcement.

Arguments: `$ARGUMENTS`

## Asking the user

Every question to the user goes through the host's question tool, never as plain text at
the end of a message: **AskUserQuestion** in Claude Code, **request_user_input** in Codex.
If the tool is not available (Codex offers it only in some modes), ask in chat with the
same numbered options and end your turn. Whenever the next step needs the user to choose something, offer the
choice as a prompt and then carry out the chosen option yourself. Don't tell the user to go
do it and come back. Give 2–4 concrete options, put your recommendation first and mark it
"(Recommended)". The user can always pick "Other" and type an answer. Use a short `header`
(for example "Branch", "Base", "Setup", "Feature"). Typical options:

| Question | Options |
|---|---|
| Feature branch name | 2–3 names following the repo's existing branch pattern (see `git branch -a`), e.g. `sp21/stock-adjustments`, `feat/stock-adjustments` |
| Base branch | The likely base branches from `git branch -a` (`main`, `production`, `dev`, ...) |
| Feature description, when none was given | Ask what the feature is: offer "I'll describe it" and "Use a spec file" |
| Confirm FEATURE.md | "Approve", "Change something" (put the summary in the question) |
| Approve setup | "Approve", "Change something" |
| Material decision (Decide stage) | The real alternatives, each with its consequence in `description` |
| Install the planvil program | "Install it now (Recommended)", "Not now" |
| Uncommitted changes block a branch switch | "Commit them (Recommended)", "I'll handle it myself", "Cancel" |
| Rename the run's branch | 2–3 better names following the repo's pattern, plus "Keep the current name" |

An answer through the question tool counts as the user's reply for `planvil answer`, so you
can keep working in the same turn. After asking in chat, the user's next message is the
reply.

## Rules

- Never edit `.planvil.json`, the project's verify script (its `verify.script`), anything
  under a `.planvil/` folder, or hook settings. Never hand-edit run state. Only planvil
  commands change it. You may run the verify script.
- `planvil pause`, `planvil resume` and `planvil cancel` are user actions. If the user wants one,
  ask them to type it as their own message (for example `planvil cancel`). Never run them yourself.
- Never weaken, delete or renumber acceptance criteria, and never weaken or delete existing
  checks to get a pass. If a requirement seems wrong, ask the user.
- Never claim something works unless `planvil validate` and `planvil verify` printed PASSED
  for it. Build success,
  an HTTP 200 from a health check, or your own reading of the code is not proof.
- Preserve the user's uncommitted work. Do not stash, reset, clean, push, merge, or switch
  branches during a run.
- Before Implement, the shell accepts only simple read-only commands: no loops, no `$(...)`,
  no redirection into files, no heredocs. Run one plain command at a time (`cat a b`,
  `grep -rn x src`, `git log -5`), or use the Read, Grep and Glob tools.
- Decide routine technical choices yourself, following the repository's conventions, and
  record them in DECISIONS.md. Ask the user only about business rules, destructive scope, or
  materially different user-visible outcomes.

## 0. Make sure planvil is installed

Run `planvil --version`. If the command is not found, try `~/.cargo/bin/planvil --version`;
if that works, use `~/.cargo/bin/planvil` in place of `planvil` for the rest of this session.

If neither works, the binary is missing. Tell the user that the planvil plugin needs its
`planvil` program, and ask with AskUserQuestion whether to install it now. If they agree, run
the official installer from the planvil GitHub releases:

- macOS / Linux: `curl -LsSf https://github.com/benpsk/planvil/releases/latest/download/planvil-installer.sh | sh`
- Windows (PowerShell): `irm https://github.com/benpsk/planvil/releases/latest/download/planvil-installer.ps1 | iex`

Then use `~/.cargo/bin/planvil`. The hooks find it there automatically, so no restart is
needed. If the user declines, stop.

## 1. Arguments

| Argument | What to do |
|---|---|
| *(none)* | Resume this branch's run (`planvil start`), or ask for a branch name |
| `<branch> [spec.md]` | Start or resume a feature on its own branch (section 2) |
| `<feature> --here [spec.md]` | Start a feature on the current branch, without creating or switching branches (section 2) |
| `init` | Discover the project setup without a feature: `planvil start --setup`, then do the **Setup** stage. Running it again re-discovers the setup |
| `rename <new-branch>` | Rename this run's branch: `planvil rename <new-branch>` |
| `status` | Run `planvil status` and show the output |
| `report` | Run `planvil report` and show it; if the run is not finished, show why |
| `doctor` | Run `planvil doctor`; explain any ✗ lines and how to fix them |
| `help` | Briefly list these arguments and the user controls below, then stop |
| `pause`, `resume`, `cancel` | These run only when the user types `planvil <action>` as a message. Tell them so, then stop |

## 2. Start or resume

Run `planvil start <branch>`, or `planvil start <branch> --spec <path>` when a spec file was
given. With no branch argument, run `planvil start` to resume this branch's run.

When the user adds `--here`, run `planvil start <feature> --here` (plus `--spec <path>` if
given). The run stays on the current branch: no branch is created or switched, uncommitted
work is left as it is, and the feature folder is named after the feature
(`planvil/<feature>/`). There is one unfinished run per branch.

If it fails with one of these codes, resolve it and rerun:

- `NEEDS_BASE_BRANCH`: list candidates with `git branch -a` and ask with AskUserQuestion
  which branch feature branches are cut from; rerun with `--base <branch>`.
- `NEEDS_FEATURE_NAME`: ask with AskUserQuestion for a short feature name (for `--here`),
  for example `payment-retry`.
- `NEEDS_BRANCH`: ask with AskUserQuestion for the feature branch name (and what the feature
  is, if the user has not said), offering names that follow the repo's branch pattern.
- `GIT_SANDBOXED`: the host's sandbox keeps `.git` read-only (Codex does by default), so
  planvil cannot create, switch or rename branches. Rerun the same command with escalated
  permissions, so the user approves it once; in Codex, request to run it outside the sandbox.
  Never work around it in another way.
- `HOOKS_NOT_ACTIVE`: the host is not running planvil's hooks, so the run would not be
  enforced. Tell the user exactly what the message says. In Codex: restart `codex` and choose
  "Trust all and continue" when it asks to review hooks, or use `/hooks`. Do not continue
  without the hooks.
- `NEEDS_CLEAN_WORKTREE`: there are uncommitted changes. Ask with AskUserQuestion, listing
  the files in the question: "Commit them (Recommended)", "I'll handle it myself", "Cancel".
  - **Commit them:** stage exactly the listed files (`git add <files>`), commit with a short
    conventional message describing them, and rerun `planvil start` with the same arguments.
  - **I'll handle it myself:** stop and wait.
  - **Cancel:** stop.

  Only commit when the user chose it. Never stash, reset or discard their work.

**Renaming.** If the user wants a different branch name at any point (or the name no longer
fits the feature), offer names with AskUserQuestion and run `planvil rename <new-branch>`. It
renames the git branch, moves the feature folder with all docs and evidence, and keeps the
stage, confirmations and approvals. Never cancel and restart a run just to rename it, and
never rename with git directly. A `--here` run does not own its branch, so it cannot be
renamed.

When resuming, run `planvil status` and re-read the feature folder and the relevant code
before acting. Do not assume an interrupted verification passed.

## 3. Stages

Every document has fixed `##` headings. Keep them exactly; the gates check that each one has
real content (template comments, TODO and empty tables do not count).

**Setup** (once per repository, only when `.planvil.json` has no `verify`). Work out how
this project is run and checked. Do not assume a language or framework. Read what the
project already has, best sources first:

1. CI workflows (`.github/workflows/*`, `.gitlab-ci.yml`, ...): what the team actually runs.
2. Task runners: `Makefile`, `justfile`, `Taskfile.yml`, `package.json` scripts.
3. Manifests: `go.mod`, `Cargo.toml`, `pyproject.toml`, `composer.json`, `pom.xml`, `build.gradle`, ...
4. Services and environment: `docker-compose.yml`, `.env.example`, `README`, `DEV.md`, `CONTRIBUTING`.

Write two files in the feature folder.

`verify.proposed.sh`: **the project's verify script**. It checks the whole app the way the
team does (lint, types, every test suite, build), runs from the repository root, and exits 0
only when all of it passes. Use the same commands as CI. Start it with a shebang
(`#!/usr/bin/env bash` or `#!/bin/sh`) and `set -e`. It runs in the **Verify** stage of every
feature, after the feature's own checks pass. `setup apply` installs it at the path in
`script`; it is committed with the project and protected during runs.

```sh
#!/usr/bin/env bash
set -euo pipefail
make lint
go vet ./...
go test ./...
```

`setup.proposed.json`:

```json
{
  "script": "scripts/planvil-verify.sh",
  "env": { "APP_ENV": "test" },
  "preflight": [{ "name": "postgres", "run": ["pg_isready", "-h", "127.0.0.1"] }],
  "app": {
    "run": ["go", "run", "./cmd/server"],
    "cwd": "backend",
    "ready": { "http": "http://127.0.0.1:8080/health" },
    "startup_timeout_secs": 90
  },
  "checks": [],
  "known_failures": []
}
```

- **script**: where the verify script will live (repository-relative). `script_timeout_secs`
  defaults to 3600.

- **preflight**: tools and services the checks need (database, cache, browser driver).
- **app**: how to start the app for runtime scenarios, plus a readiness probe: an `http` URL,
  or a `run` command that exits 0 when ready. Omit it for a library with no runnable app.
- **checks**: optional extra whole-app checks that run next to the script. Prefer putting
  everything in the script; use a check only to isolate a pre-existing failure (below).
- Use secrets by variable name (`env`), never by value.

Run `planvil setup check`. It runs everything once and reports CANNOT RUN, TIMEOUT, pass or
fails for each step.

- **CANNOT RUN** or a failing preflight: fix the command, or ask the user to start the missing
  service.
- **The verify script fails** on the current code: first confirm the commands are right. If a
  failure is real and pre-existing, take only that part out of the script (for example, the one
  failing package), put it in a separate check, and list that check in `known_failures`. The
  script itself can never be a known failure. Never park a whole test suite there.

Repeat until it says USABLE. Then summarize for the user in a few lines: what the verify
script runs, how the app starts, and any known failures. Run `planvil ask --approve-setup "<summary>"` and ask the user
with AskUserQuestion. After they approve, run `planvil answer --approved "<reply>"`
and then `planvil setup apply`.

**Feature.** If a spec was copied to `SOURCE-SPEC.md`, rewrite it into FEATURE.md under the
template headings without changing its meaning. Otherwise interview the user: goal, users,
scope, out of scope, behavior and edge cases, then acceptance criteria as a table with stable
IDs `AC-01`, `AC-02`, … Then run `planvil ask --confirm-feature "<short summary>. Is this right?"`,
ask the user with AskUserQuestion, and record the reply:

- `planvil answer --approved "<reply>"` if they agree
- `planvil answer --rejected "<reply>"` if not; revise and ask again

Then run `planvil advance`.

**Audit.** Read the code. In AUDIT.md, list the affected files and symbols as backticked
repository paths (`internal/auth/login.go`), plus current behavior, reuse opportunities,
constraints and gaps against FEATURE.md. Record observations, not guesses. Then run
`planvil advance`.

**Plan.** Write PLAN.md (Status, Steps, Verification, Risks). Map every AC under
`## Verification`. Write `validate.json` in the feature folder (an older folder may have
`verify.json` instead; keep using that one) so every AC has at least one check or runtime
scenario. These are **this feature's** checks; the whole app is checked later by the
project's verify script:

```json
{
  "checks": [
    { "name": "reset-unit", "run": ["go", "test", "./internal/auth/..."], "criteria": ["AC-01"] }
  ],
  "scenarios": [
    { "name": "reset-journey", "run": ["npx", "playwright", "test", "e2e/reset.spec.ts"], "criteria": ["AC-01", "AC-02"] }
  ]
}
```

- Use `run` with an argument list. `shell` is for deliberate cases only.
- A check must be able to fail. No-ops (`true`, `echo ok`, `exit 0`) are rejected, as is
  anything that touches planvil's own files. Scenarios always run against the app.
- `cwd` is relative to the repository root. `timeout_secs` defaults to 600.
- Scenarios run against the app that `.planvil.json` starts, and must exercise the real user
  journey: browser automation for web, real HTTP calls for APIs, the real binary for CLIs.
- Scenario scripts you write belong in the repository's test folders. Save screenshots or
  responses into `$PLANVIL_EVIDENCE_DIR`.

Then run `planvil advance`.

**Decide.** Record decisions in DECISIONS.md: decision, why, alternatives, and who decided.
For a material question, run `planvil ask "<question>"`, ask the user with AskUserQuestion
(the alternatives as options), then run `planvil answer "<reply>"`. Ask one question at a time.
Update PLAN.md if an answer changes it. List questions under `## Open questions` as `- [x]`
once answered. If planvil asks for plan approval, use `planvil ask --approve-plan "..."`. Approval covers
PLAN.md and validate.json together.
Then run `planvil advance`.

**Implement.** Implement PLAN.md step by step, following the codebase's conventions. Keep
PLAN.md `## Status` current. You may add tests. Then run `planvil advance`.

**Validate.** Run `planvil validate` and wait. It runs preflight, then this feature's checks
and scenarios from validate.json (against the app when one is configured), and records
evidence. It answers one question: does this feature do what FEATURE.md says? Do not edit
files while it runs. When it passes, the run moves to Verify.

**Verify.** Run `planvil verify` and wait. It runs the project's verify script (and any
project checks) on the whole app and records evidence. It answers: did this feature break
anything else? If the code changed after validation passed, the run goes back to Validate
first. If the verify script itself changed during the run, the run is blocked: tell the
user, who types `planvil resume` if the change was theirs. When it passes, the run is done.

**When validation or verification fails**, the run loops back to **Feature** by itself, and
you go through the stages again without asking the user. Every `next:` line starts with
`Repair cycle N/M`, says which stage failed, and names the failing checks and the evidence
folder. Both stages share one repair budget.

- **Feature:** re-read FEATURE.md against the failure. If it still holds, run `planvil advance`.
  If the failure shows a requirement must change, or that it contradicts what the user
  confirmed, edit FEATURE.md with the proposed change first, then ask **once**:
  `planvil ask --confirm-feature "<what changed and why>"`. Don't ask a separate question
  first.
- **Audit:** read the failing logs and fill `## Failure analysis` in AUDIT.md with the root
  cause of each failing check, found in the logs and the code. The gate requires every failing
  check to be named.
- **Plan:** update PLAN.md for the fix. Fix the code, not the checks: changing or removing a
  check that just failed needs the user's approval (`planvil ask --approve-plan`).
- **Decide:** record new decisions. Ask the user only if the fix needs a business or scope
  decision.
- **Implement:** make the fix. The gate refuses if nothing changed since the failed run.
- **Validate:** run `planvil validate` again, then **Verify:** `planvil verify`.

planvil stops the loop after its limits (default: 5 cycles, or 3 identical failures in a row)
and blocks the run. If validate or verify reports files changed while it ran, a check is
generating files: add them to .gitignore.

**Blocked.** Report the reason and diagnostics to the user honestly, as unverified. They fix
the cause and type `planvil resume`.

**Done.** Run `planvil report` and give the user one completion report: what changed, the
decisions, and the actual evidence. Point to REPORT.md. Then stop.

## When unsure

Run `planvil status`. It prints the stage, any unmet gate items, and the next action.
