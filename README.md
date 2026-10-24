# planvil

Enforced feature workflow for Claude Code. You give it a feature; the agent works through
**Feature → Audit → Plan → Decide → Implement → Validate → Done** in your normal Claude Code
terminal. When validation fails, it loops back through Feature → Audit → Plan → Decide →
Implement by itself, with a written failure analysis, until it passes or hits its limits. Hooks enforce each stage, and verification evidence is recorded rather
than claimed.

- The agent cannot change application code until FEATURE, AUDIT, PLAN and DECISIONS pass their
  content gates.
- It cannot finish while the run is incomplete; the stop hook sends it back with the next step.
- "Done" requires `planvil verify` to pass every acceptance criterion against the current
  code. A model saying "tests pass" counts for nothing.
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
3. Writes the document templates: FEATURE, AUDIT, PLAN, DECISIONS, HANDOFF and `verify.json`.

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
├── verify.json    Feature checks and runtime scenarios
├── REPORT.md      Completion or blocked report
└── .planvil/      state.json, lock, evidence/NNN/ (written only by planvil)
```

Each gate checks substance: every required section must have real content (template
comments, TODO and empty tables do not count). AUDIT must name at least one existing
repository path, PLAN must map every AC, and DECISIONS must have no unchecked open question.

## Configuration: `.planvil.json`

Every key is optional; `planvil init` writes a minimal file. `verify` is filled in by the Setup stage.

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

### `verify` (discovered for you)

You don't write this by hand. The first `/planvil` run in a repository without `verify`
starts with a **Setup** stage:

1. The agent works out how the project runs from its CI workflows, task runners, manifests,
   docker-compose and docs, for any language or framework.
2. It writes a proposal, and `planvil setup check` proves it: tools exist, services respond,
   the app starts and becomes ready, every check runs. Checks that already fail are recorded
   as `known_failures`, isolated as narrowly as possible, so later features aren't blamed for
   them.
3. You approve a short summary once (`planvil ask --approve-setup`).
4. `planvil setup apply` saves it to `.planvil.json`. Commit that file. From then on it's
   protected during runs.

The resulting section looks like this:

```json
{
  "verify": {
    "env": { "APP_ENV": "test" },
    "preflight": [{ "name": "db", "run": ["pg_isready"] }],
    "app": {
      "run": ["go", "run", "./cmd/server"],
      "cwd": "backend",
      "ready": { "http": "http://127.0.0.1:8080/health" },
      "startup_timeout_secs": 90
    },
    "checks": [
      { "name": "vet", "run": ["go", "vet", "./..."] },
      { "name": "unit", "run": ["go", "test", "./..."], "timeout_secs": 900 }
    ]
  }
}
```

- **preflight**: environment checks. A failure, or a missing tool anywhere, makes the run
  **blocked / unverified**, never a pass.
- **app**: launched for runtime scenarios, polled until ready (`http` URL or a `run`
  command), and stopped afterwards with its whole process group.
- **checks**: required for every feature. These are protected; the agent cannot edit them.
- Steps use an argument list (`run`). `shell` (runs `sh -c`) is allowed as a deliberate choice.
- Values of variables whose names contain TOKEN, SECRET, PASSWORD or KEY are redacted from
  logs.

The agent writes the feature's own `verify.json`, and every acceptance criterion must be
covered:

```json
{
  "checks":    [{ "name": "reset-unit", "run": ["go", "test", "./internal/auth/..."], "criteria": ["AC-01"] }],
  "scenarios": [{ "name": "reset-journey", "run": ["npx", "playwright", "test", "e2e/reset.spec.ts"], "criteria": ["AC-01", "AC-02"] }]
}
```

Scenarios run against the live app. When an app is configured, at least one scenario is
required. A health check returning 200 does not prove a feature.

### Evidence and staleness

`planvil verify` records the command, working directory, start and end times, exit code, and
stdout/stderr for every step in `.planvil/evidence/NNN/`. It fingerprints the code first:
tracked files including uncommitted edits, untracked non-ignored files, FEATURE.md,
`verify.json` and `.planvil.json`. If anything changes during the run, the results are
discarded. A failed run cannot be revalidated until the code actually changes. Editing
FEATURE.md after it was confirmed sends the run back to the Feature stage and invalidates the
plan and the evidence.

## What is enforced

`planvil doctor` prints this matrix.

| Area | How |
|---|---|
| Edit / Write / MultiEdit / NotebookEdit before Implement | **Enforced**: denied outside the feature folder |
| Writes outside the repository during a run | Denied, except the temp folder and Claude memory files |
| Shell commands before Implement | **Best effort**: one-line, recognized read-only commands only, with per-command option rules (`sed -n 'Np'` only, no `sort -o`, `rg --pre`, `git -c`/`--output`, …). Loops, substitution, heredocs, comments, redirection into files and variable prefixes are denied |
| Unknown and MCP tools before Implement | Denied unless listed in `allowed_tools` |
| Scheduling tools and subagents in their own worktree | Denied during a run |
| Run state, `.planvil.json`, `.claude/settings*`, `.claude/hooks/`, `.mcp.json`, `.git/`, the planvil binary and plugin | Protected in every active stage |
| `git push`, `pull`, `merge`, `rebase`, `reset --hard`, `clean`, `checkout`, `switch`, `stash`, branch rename/delete | Denied during a run |
| HEAD moved off the run's branch, or a write into the run's repo from elsewhere | Still enforced: a live run on another branch of the checkout blocks mutations |
| Finishing early | Stop hook continues the same session; bounded |
| Answers and confirmations | Accepted only after a real user message or AskUserQuestion answer; your verbatim reply is recorded |
| Weakening checks | No-op checks rejected; changing or removing a check that just failed needs your plan approval (which covers PLAN.md and verify.json) |
| pause / resume / cancel | Applied only when you type them |
| Hook crash, bad payload, git failure or timeout | Mutation-capable calls denied (`onFailure: block` on PreToolUse) |
| Interrupted verification | Records nothing; leftover processes from it block the next verification until stopped |

**Limits.** Hooks are not a sandbox. From Implement on, the agent's shell is open by design,
and code running as your OS user (a script it writes, `sh -c '…'`, a renamed copy of a binary)
can get past text-based checks, write run state, or call planvil's hook entry point. Run
state is not signed. Whether a reply counts as `--approved` is the agent's reading of what
you said; your verbatim words are recorded next to it, and the transcript is the source of
truth. planvil makes skipping steps hard and visible, not impossible.

Only Claude Code is supported. The core is host-independent, so a Codex adapter can be added
later.

## Uninstall

```sh
planvil uninstall   # removes the plugin and marketplace; project files are left untouched
```

## Development

```sh
cargo test                                   # unit, integration and hook-fixture tests
cargo clippy --all-targets -- -D warnings
```

Releases: push a `vX.Y.Z` tag. cargo-dist (`.github/workflows/release.yml`) builds Linux
(musl), macOS and Windows binaries and publishes the installers.

The design, requirements, compatibility matrix and test coverage are in [docs/DESIGN.md](docs/DESIGN.md).

## License

MIT. See [LICENSE](LICENSE).
