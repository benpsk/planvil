# planvil

Enforced feature workflow for Claude Code. You give it a feature; the agent works through
**Feature → Audit → Plan → Decide → Implement → Validate → Verify → Done** in your normal
Claude Code terminal. Validate checks that the feature works; Verify checks that the whole app
still works. When either fails, it loops back through Feature → Audit → Plan → Decide →
Implement → Validate → Verify by itself, with a written failure analysis, until it passes or
hits its limits. Hooks enforce each stage, and verification evidence is recorded rather
than claimed.

- The agent cannot change application code until FEATURE, AUDIT, PLAN and DECISIONS pass their
  content gates.
- It cannot finish while the run is incomplete; the stop hook sends it back with the next step.
- "Done" requires `planvil validate` to pass every acceptance criterion and then
  `planvil verify` to pass the project's verify script, both against the same, current code.
  A model saying "tests pass" counts for nothing.
- Failure loops are bounded (default: 5 cycles, or 3 identical failures in a row), after which
  the run is reported as **blocked**, not done.

planvil is a single static binary with no runtime dependencies.

## Install

Inside Claude Code:

```
/plugin install planvil --marketplace benpsk/planvil
```

That is all. The first time you run `/planvil:planvil`, it checks for the `planvil` program
and, with your OK, installs it from this repository's GitHub releases. Until the program is
installed, the plugin stays out of the way: nothing is blocked.

Requirements: Claude Code 2.1.295 or later, and git.

<details>
<summary>Other ways to install the program</summary>

```sh
# macOS / Linux
curl -LsSf https://github.com/benpsk/planvil/releases/latest/download/planvil-installer.sh | sh
# Windows (PowerShell)
irm https://github.com/benpsk/planvil/releases/latest/download/planvil-installer.ps1 | iex
# From source (development), then register the plugin with this binary's absolute path
cargo install --path . && planvil install
```

</details>

## Codex CLI

The same repository is also a Codex plugin (`.codex-plugin/plugin.json`, with its own hooks
file).

```sh
codex plugin marketplace add benpsk/planvil
codex plugin add planvil@planvil
```

Then start `codex`. When it asks to review hooks, choose **Trust all and continue** (or use
`/hooks`). Codex runs plugin hooks only after you trust them, and again **after every plugin
update**. Until then, `planvil start` refuses to begin a run (`HOOKS_NOT_ACTIVE`), so nothing
runs unenforced. Invoke the skill as `$planvil:planvil sp20/mcp`.

Differences from Claude Code:

- **Branch changes.** Codex's default sandbox keeps `.git` read-only, so creating, switching
  or renaming branches asks you to approve the command once (`GIT_SANDBOXED`).
- **Questions.** They arrive as numbered options in chat; Codex's question tool is only
  available in some modes.
- **Hook failures.** A failing Codex hook does not block on its own (there is no
  `onFailure: block`); planvil's own errors still deny.

## Use

Everything happens in the Claude Code chat:

| Type | What happens |
|---|---|
| `/planvil:planvil sp20/mcp` | Start a feature on branch `sp20/mcp`, or resume it |
| `/planvil:planvil sp20/mcp docs/spec.md` | Start from an existing spec file |
| `/planvil:planvil` | Resume this branch's run |
| `/planvil:planvil init` | Discover how this project runs and is checked, without a feature (run again to re-discover) |
| `/planvil:planvil rename refactor/new-name` | Rename the run's branch; the folder, docs, evidence and progress move with it |
| `/planvil:planvil status` | Stage, what is missing, and the next step |
| `/planvil:planvil report` | The completion or blocked report |
| `/planvil:planvil doctor` | Check the installation and the project config |
| `/planvil:planvil help` | List these commands |

Plugin skills are always named `plugin:skill`, so the command is `/planvil:planvil`.

The first run in a repository starts with **Setup**, unless you already ran `init`. The agent
discovers how the project runs and you approve it once. After that, every feature run:

1. Creates or checks out the feature branch from the base branch. It stops if you have
   uncommitted work; it never stashes or resets.
2. Creates `planvil/<branch-with-dashes>/` (`sp20/mcp` → `planvil/sp20-mcp/`) and adds
   `/planvil/` to `.gitignore`.
3. Writes the document templates: FEATURE, AUDIT, PLAN, DECISIONS, HANDOFF and `validate.json`.

Then the agent works through the stages. You are asked only for things that are yours to
decide: confirming FEATURE.md, approving the setup, business rules, and destructive or
materially different outcomes. You answer by replying normally.

### Your controls

Type these as a message in Claude Code, or run them in a terminal outside Claude Code. The
agent cannot run them: the CLI form refuses to run from Claude Code's shell, and the hook
denies it.

| Message | Effect |
|---|---|
| `planvil pause` | Pause the run; the agent is allowed to stop |
| `planvil resume` | Resume a paused or blocked run (a blocked run gets a fresh repair budget) |
| `planvil cancel` | Cancel the run; enforcement ends |

Interrupting with Esc always works; planvil never overrides it.

## Feature folder

```
planvil/sp20-mcp/
├── FEATURE.md     Goal · Scope · Out of scope · Behavior · Acceptance criteria (AC-01…)
├── AUDIT.md       Affected code · Current behavior · Reuse · Constraints · Gaps
├── PLAN.md        Status · Steps · Verification (AC → check) · Risks
├── DECISIONS.md   Decided · Open questions
├── HANDOFF.md     Short resume packet (not gated)
├── validate.json  This feature's checks and runtime scenarios (Validate stage)
├── REPORT.md      Completion or blocked report
└── .planvil/      state.json, lock, evidence/NNN/ (written only by planvil)
```

Each gate checks substance: every required section must have real content (template
comments, TODO and empty tables do not count). AUDIT must name at least one existing
repository path, PLAN must map every AC, and DECISIONS must have no unchecked open question.

## Configuration: `.planvil.json`

Every key is optional; `planvil init` writes a minimal file. `verify` (including the verify script) is filled in by the Setup stage.

| Key | Default | Meaning |
|---|---|---|
| `base_branch` | ask once | Branch new feature branches are cut from |
| `docs_root` | `planvil` | Root for feature folders (may be a symlink, e.g. `project-docs`) |
| `docs_gitignored` | `true` | Add `docs_root` to `.gitignore` |
| `require_plan_approval` | `false` | Require your explicit approval of PLAN.md before Implement |
| `max_repair_cycles` | `5` | Repair limit before Blocked (1–20) |
| `max_equivalent_failures` | `3` | Identical consecutive failures before Blocked (1–20) |
| `max_stop_continuations` | `3` | Stop attempts without progress before Blocked (1–20) |
| `max_run_minutes` | none | Wall-clock limit for a run |
| `readonly_commands` | `[]` | Extra shell prefixes allowed before Implement, e.g. `"go test"` |
| `allowed_tools` | `[]` | Extra tools allowed before Implement, e.g. a read-only MCP tool |
| `verify` | none | Project verification (below) |

Limits are copied into the run when it starts, so editing the file mid-run cannot loosen them.

### Validate and Verify: two checks, two questions

| Stage | Command | Question | Runs | Written |
|---|---|---|---|---|
| **Validate** | `planvil validate` | Does **this feature** work? | The feature's checks and scenarios from `validate.json`, each tied to an acceptance criterion | By the agent, for each feature |
| **Verify** | `planvil verify` | Is the **whole app** still fine? | The project's verify script (e.g. `scripts/planvil-verify.sh`) | Once, during Setup; you approve it |

Validate runs first. When it passes, Verify runs against the same code. If either fails, the
run loops back to Feature with a failure analysis that says which one failed, and goes
through Validate and Verify again. Both share one repair budget. If the code changes after
Validate passed, Verify sends the run back to Validate.

### `verify` and the verify script (discovered for you)

You don't write these by hand. The first `/planvil` run in a repository without `verify`
starts with a **Setup** stage:

1. The agent works out how the project is checked from its CI workflows, task runners,
   manifests, docker-compose and docs, for any language or framework.
2. It writes a **verify script** that checks the whole app (lint, types, every test suite)
   and exits 0 only when everything passes, plus a proposal for `.planvil.json`.
3. `planvil setup check` proves it: tools exist, services respond, the app starts and becomes
   ready, and the script passes on the current code. Pre-existing failures are taken out of
   the script into a separate check listed in `known_failures`, isolated as narrowly as
   possible, so later features aren't blamed for them.
4. You approve a short summary once (`planvil ask --approve-setup`). Changing the script or
   the proposal afterwards voids the approval.
5. `planvil setup apply` installs the script (for example `scripts/planvil-verify.sh`) and
   saves the config to `.planvil.json`. Commit both. From then on they are protected during
   runs: the agent can run the script but not change it. planvil also records the script's
   hash when a run starts, so if it changes anyway, Verify blocks the run until you type
   `planvil resume` to accept the change.

The script is an ordinary shell script. You can run it yourself or from CI at any time, and
edit it outside a run (or re-run `/planvil:planvil init`).

The resulting section looks like this:

```json
{
  "verify": {
    "script": "scripts/planvil-verify.sh",
    "env": { "APP_ENV": "test" },
    "preflight": [{ "name": "db", "run": ["pg_isready"] }],
    "app": {
      "run": ["go", "run", "./cmd/server"],
      "cwd": "backend",
      "ready": { "http": "http://127.0.0.1:8080/health" },
      "startup_timeout_secs": 90
    }
  }
}
```

- **script**: the verify script, run in Verify. Time limit `script_timeout_secs` (default 3600).
- **preflight**: environment checks, run before both Validate and Verify. A failure, or a
  missing tool anywhere, makes the run **blocked / unverified**, never a pass.
- **app**: launched for the feature's runtime scenarios, polled until ready (`http` URL or a
  `run` command), and stopped afterwards with its whole process group.
- **checks** (optional): extra whole-app checks run in Verify next to the script, mainly to
  isolate `known_failures`. Projects set up before the verify script existed keep working
  with checks only.
- Steps use an argument list (`run`). `shell` (runs `sh -c`) is allowed as a deliberate choice.
- Values of variables whose names contain TOKEN, SECRET, PASSWORD or KEY are redacted from
  logs.

The agent writes the feature's own `validate.json`, and every acceptance criterion must be
covered:

```json
{
  "checks":    [{ "name": "reset-unit", "run": ["go", "test", "./internal/auth/..."], "criteria": ["AC-01"] }],
  "scenarios": [{ "name": "reset-journey", "run": ["npx", "playwright", "test", "e2e/reset.spec.ts"], "criteria": ["AC-01", "AC-02"] }]
}
```

Scenarios run against the live app. When an app is configured, at least one scenario is
required. A health check returning 200 does not prove a feature. Feature folders created
before this change keep their `verify.json`, which is still read.

### Evidence and staleness

`planvil validate` and `planvil verify` record the command, working directory, start and end
times, exit code, and stdout/stderr for every step in `.planvil/evidence/NNN/`. They
fingerprint the code first: tracked files including uncommitted edits, untracked non-ignored
files, FEATURE.md, `validate.json` and `.planvil.json`. If anything changes during the run,
the results are discarded. A failed run cannot be revalidated until the code actually
changes. Editing FEATURE.md after it was confirmed sends the run back to the Feature stage and
invalidates the plan and the evidence. The report shows both stages' evidence.

## What is enforced

`planvil doctor` prints this matrix.

| Area | How |
|---|---|
| Edit / Write / MultiEdit / NotebookEdit before Implement | **Enforced**: denied outside the feature folder |
| Writes outside the repository during a run | Denied, except the temp folder and Claude memory files |
| Shell commands before Implement | **Best effort**: one-line, recognized read-only commands only, with per-command option rules (`sed -n 'Np'` only, no `sort -o`, `rg --pre`, `git -c`/`--output`, …). Loops, substitution, heredocs, comments, redirection into files and variable prefixes are denied |
| Unknown and MCP tools before Implement | Denied unless listed in `allowed_tools` |
| Scheduling tools and subagents in their own worktree | Denied during a run |
| Run state, `.planvil.json`, the verify script, `.claude/settings*`, `.claude/hooks/`, `.mcp.json`, `.git/`, the planvil binary and plugin | Protected in every active stage (the verify script may still be run on its own) |
| `git push`, `pull`, `merge`, `rebase`, `reset --hard`, `clean`, `checkout`, `switch`, `stash`, branch rename/delete | Denied during a run |
| HEAD moved off the run's branch, or a write into the run's repo from elsewhere | Still enforced: a live run on another branch of the checkout blocks mutations |
| Finishing early | Stop hook continues the same session; bounded |
| Answers and confirmations | Accepted only after a real user message or AskUserQuestion answer; your verbatim reply is recorded |
| Weakening checks | No-op checks rejected; changing or removing a check that just failed needs your plan approval (which covers PLAN.md and validate.json) |
| pause / resume / cancel | Applied only when you type them |
| Hook crash, bad payload, git failure or timeout | Mutation-capable calls denied (`onFailure: block` on PreToolUse) |
| Interrupted verification | Records nothing; leftover processes from it block the next verification until stopped |

**Limits.** Hooks are not a sandbox. From Implement on, the agent's shell is open by design,
and code running as your OS user (a script it writes, `sh -c '…'`, a renamed copy of a binary)
can get past text-based checks, write run state, or call planvil's hook entry point. Run
state is not signed. Whether a reply counts as `--approved` is the agent's reading of what
you said; your verbatim words are recorded next to it, and the transcript is the source of
truth. planvil makes skipping steps hard and visible, not impossible.

Claude Code and Codex CLI are supported, through one shared core and a thin adapter per host.

## Uninstall

```sh
planvil uninstall   # removes the plugin and marketplace; project files are left untouched
```

## Development

```sh
cargo test                                   # unit, integration and hook-fixture tests
cargo clippy --all-targets -- -D warnings
```

Releases: bump `version` in `Cargo.toml` and `plugin/.claude-plugin/plugin.json` (a test checks
they match), commit, then push a matching `vX.Y.Z` tag. cargo-dist (`.github/workflows/release.yml`) builds Linux
(musl), macOS and Windows binaries and publishes the installers.

The design, requirements, compatibility matrix and test coverage are in [docs/DESIGN.md](docs/DESIGN.md).

## License

MIT. See [LICENSE](LICENSE).
