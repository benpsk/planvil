# planvil: design and requirements

Requirements for an enforced feature workflow for Claude Code (and later Codex), followed by
the design decisions, compatibility matrix and test coverage (§14–16).

> **Current phase: Claude Code only.** Build, test, and ship the Claude Code adapter first. Codex is deferred: the core stays host-independent so a Codex adapter can be added later, but no Codex work, tests, or acceptance (AC-01) are in scope for this phase.

## 1. Goal

Build a reusable workflow extension that runs inside the normal interactive Codex CLI and Claude Code terminal. The user supplies a Markdown requirements file. The agent audits the code, writes a plan, resolves decisions, updates the plan, implements the feature, runs the application, verifies behavior, and repairs failures automatically.

The user must retain the original terminal interface, live messages, tool activity, and ability to interrupt or steer the agent. Do not replace that interface with a separate chat application or an external process that launches headless agent sessions for every stage.

Markdown defines the procedure and feature intent. Executable hooks enforce observable gates. Persistent state records progress. A standalone user-facing CLI, hosted service, MCP server, and agent framework are not required.

## 2. Primary user experience

1. Install the extension and configure a project once.
2. Write a feature requirements Markdown file, including intended behavior and acceptance criteria.
3. Open normal Codex CLI or Claude Code in that repository.
4. Invoke the workflow using the host's supported skill invocation or a natural-language request naming the skill and requirements path.
5. Watch the agent perform the stages in the same terminal session.
6. Answer only unresolved product questions or provide missing external information when necessary.
7. Receive a completion report with changes, decisions, and actual verification evidence, or an honest blocked report.

Routine technical choices must not require approval after every stage. Presenting a plan must not automatically end the run. A project may optionally require explicit approval before implementation; it is disabled by default.

## 3. Scope

### Required

- Shared workflow logic with separate Codex and Claude Code integrations.
- Markdown skill instructions and reusable document templates.
- Hook-based stage gates, continuation checks, and verification enforcement.
- Feature-specific state, audit findings, plans, decisions, and evidence.
- Runtime validation of the application, in addition to relevant static and automated checks.
- Automatic repair loops and interrupted-session recovery.
- Per-project configuration independent of application language or framework.
- Clear capability reporting for supported host versions.

### Outside the initial scope

- Custom chat UI, terminal emulator, or hosted dashboard.
- Autonomous deployment, pushing, merging, or contacting other people.
- Multi-agent orchestration.
- Restarting a closed terminal or bypassing host usage limits and permissions.
- Proving correctness of arbitrary code or treating the agent as an adversary with unrestricted filesystem access.

## 4. Host compatibility discovery

Before implementing either adapter, inspect the installed host version and verify its supported hook events, configuration locations, input/output schemas, continuation semantics, and trust requirements against current official documentation.

Do not assume Codex and Claude Code share identical configuration or tool names. Maintain a tested compatibility matrix. Unsupported or missing enforcement capabilities must be reported explicitly; never silently downgrade an enforced workflow to instructions only.

Shared semantics must include initialization, pre-action checks where supported, observed action results, attempted completion, and interruption/resumption. Exact host event mappings are adapter responsibilities.

## 5. Workflow and persistent state

Use the following logical stages:

| Stage | Output and exit gate |
| --- | --- |
| Setup | Once per repository, when `.planvil.json` has no `verify`: the agent discovers how the project runs and is checked; `planvil setup check` proves it runs; the user approves once; planvil saves it |
| Audit | Code-grounded findings, affected components, existing behavior, constraints, and missing information |
| Plan | Implementation steps, acceptance-to-verification mapping, risks, and proposed decisions |
| Decide | Routine technical decisions recorded; material product ambiguities resolved |
| Implement | Scoped code changes and updated progress |
| Validate | Required checks and runtime acceptance scenarios executed against current code |
| Repair loop | On failure the run returns to Feature and passes through Audit (with a required `## Failure analysis` naming every failing check), Plan, Decide (human only for material questions) and Implement (code must change) back to Validate, without user interaction |
| Done | All required criteria pass with current evidence and no unresolved blocking findings |

Track execution status separately from stage: active, waiting_for_user, paused, blocked, cancelled, or completed. A blocked run is not a successful completion.

Persist at least: schema version, run ID, feature ID, canonical repository/worktree identity, host/session identity where available, requirements path and hash, stage, execution status, timestamps, attempt counts, document hashes, decisions, open questions, criteria and evidence references, verification code fingerprint, and last actionable failure.

Use atomic writes and a per-run lock. Concurrent sessions must not corrupt or advance the same run. Default to one active workflow per worktree. Do not identify a run solely by its directory basename.

State changes must go through validated transition logic. Hand-editing a state file must not be the normal agent procedure. Validate state schema and artifact hashes before accepting stored completion records.

## 6. Requirements, audit, planning, and decisions

- Treat the supplied requirements file as authoritative for intended behavior.
- Assign stable identifiers to acceptance criteria without changing their meaning.
- Identify ambiguity, missing behavior, and contradictions before coding.
- Ground audit findings in actual files and symbols; record observed behavior and reuse opportunities.
- Plan implementation steps, dependencies, affected areas, and verification before application code changes.
- Separate facts, assumptions, decisions, and questions. Record concise rationale and alternatives, not hidden chain-of-thought.
- Decide routine technical choices autonomously, following repository conventions.
- Ask the user when business rules, destructive scope, or materially different user-visible outcomes remain unresolved.
- Persist user answers and update the plan before continuing.
- Do not silently weaken requirements, remove acceptance criteria, or relax checks to obtain a pass.
- Requirement changes invalidate affected planning and evidence. If impact cannot be determined reliably, return to Audit.
- Check document substance using structured required sections and, where useful, a bounded agent review. File existence alone is insufficient. A review result remains judgment-based evidence, not proof of correctness.

## 7. Action enforcement

Enforcement is active only for an explicitly activated feature run. Ordinary unrelated sessions must not become trapped in its completion loop.

Before implementation is permitted, allow reading, inspection commands, workflow document edits, and configured non-mutating observations. Block application source mutations until audit, plan, and decision gates pass.

Cover dedicated editing tools, patch tools, shell commands, and available alternate execution routes. Do not claim comprehensive enforcement based on simple command-string matching. For stages requiring strict read-only operation, use available host permission or sandbox controls. Deny unclassified mutation-capable actions or mark the strict mode unsupported when they cannot be constrained reliably.

Separate stage-specific writable workflow artifacts from application source. Resolve paths canonically, including symlinks and traversal. Do not allow workflow artifact paths to escape the configured project/run boundaries.

Protect hook configuration, verification policy, authoritative state, and existing required checks from casual agent modification. Changes that weaken gates require an explicit user action and an audit record. Document the boundary: hooks running under the same OS account are not a tamper-proof security boundary against unrestricted code execution.

If enforcement logic errors, times out, receives malformed input, or loses state, do not advance or report success. Show an actionable error while preserving cancellation and recovery paths. Do not turn a guard failure into an endless continuation loop.

## 8. Verification and evidence

Project configuration must define:

- Dependency/environment preflight and required development services.
- Application launch command, working directory, readiness probe, startup timeout, and cleanup behavior.
- Relevant lint, type, unit, integration, and end-to-end checks.
- Browser or runtime interaction mechanism and local test account setup.
- Acceptance scenarios, expected results, and evidence collection rules.
- Command and scenario timeouts and permitted environment settings.

Execute checks through a controlled verification component. Capture command arguments, working directory, start/end times, exit codes, stdout/stderr, and evidence paths. Prefer explicit argument arrays; shell execution must be a deliberate project configuration choice.

For a web feature, launch the actual app, wait for readiness, and exercise its relevant user journey using configured browser automation. For APIs, CLIs, workers, or mobile projects, use an appropriate runtime driver. A successful build or HTTP readiness response alone does not prove feature behavior.

Every acceptance criterion must link to a passing automated check or recorded runtime observation. Agent-authored tests are allowed, but existing checks must not be deleted or weakened to manufacture success. Qualitative observations must be labeled as such.

Fingerprint the code and relevant verification inputs, including tracked modifications, relevant untracked source files, dependency manifests/lockfiles, configuration, and requirements. Git HEAD alone is insufficient. Exclude generated evidence, logs, and other irrelevant volatile outputs. Detect mutations during validation and reject stale results.

Any relevant later change invalidates affected evidence; conservatively invalidate all evidence when impact is uncertain. Missing browser access, credentials, services, or required evidence produces blocked/unverified status, never a pass.

Stop hooks should inspect saved results and currentness quickly. Do not run an entire long test suite in a short-lived completion hook; direct the agent to execute the verifier and then reevaluate its recorded results.

## 9. Automatic repair and continuation

On failure, record the failing criteria and checks, diagnostics, and current stage. Direct the agent to investigate the root cause, update the plan where necessary, repair the implementation, and rerun failed and relevant regression checks.

When the agent attempts to finish an active incomplete run, the completion hook must return the host-supported continuation response naming the next required action and evidence location. This must continue the same visible interactive session.

Do not require the user to repeatedly type continue. However, never loop indefinitely. Configure maximum repair attempts, elapsed runtime, repeated identical failures, and no-progress thresholds. Defaults: five repair cycles and three consecutive equivalent failures without meaningful progress; elapsed-time limits are project configurable. Record the reason when a limit is reached and end as blocked.

Permit waiting_for_user, paused, blocked, and cancelled runs to yield control without claiming done. Honor explicit user cancellation or interruption immediately. Handle host continuation flags correctly without blindly allowing premature completion or creating an unbounded stop-hook recursion.

## 10. Recovery and human control

- Persist stage transitions, failures, decisions, and evidence as they occur.
- After restart or context loss, reload the run and recheck actual files and code before continuing.
- Detect requirement, source, environment, or configuration changes since the checkpoint.
- Never assume an interrupted verification passed.
- Preserve existing user changes; record a baseline and do not reset unrelated files.
- Retain open questions and answers across sessions.
- Provide supported ways to inspect status, pause, resume, cancel, and explicitly approve policy changes from the normal host interaction.
- Hooks do not restart a terminated host process; resume requires reopening the host.

## 11. Packaging and implementation constraints

Use one shared state machine and policy implementation with thin host adapters. The implementation language is Rust, shipped as a single static binary with no runtime dependencies (see §15). Do not introduce an agent framework or background service unless a demonstrated requirement needs it.

Install project-local or user-level skills and hook configuration using supported host mechanisms. Preserve unrelated existing settings; make installation idempotent and removable. Do not overwrite a user's whole configuration file. Document required host trust/permission steps rather than bypassing them.

Keep feature documents readable Markdown. Keep authoritative execution records structured. Internal scripts or commands are allowed, but the user should not need to manually advance every stage.

Include a sample project configuration, document templates, adapter capability diagnostics, and instructions for both hosts. Do not hardcode a particular model, application framework, or repository layout. Store credential references rather than secrets in documents and logs; redact sensitive output.

## 12. Acceptance tests for this extension

| ID | Scenario | Required result |
| --- | --- | --- |
| AC-01 | Start a feature in normal Codex CLI | Original interactive UI remains; workflow activates and audits |
| AC-02 | Start the same workflow in Claude Code | Same stage semantics and evidence rules, using its own adapter |
| AC-03 | Attempt a source edit before the plan gate | Supported mutation paths are blocked with an actionable reason |
| AC-04 | Attempt a shell or alternate write before implementation | Blocked in strict mode, or unsupported capability explicitly reported |
| AC-05 | Audit/plan files exist but required content is missing | Gate rejects advancement |
| AC-06 | Material business rule is ambiguous | Question persists, agent yields, answer updates plan, workflow resumes |
| AC-07 | Routine implementation choice arises | Agent records the decision and continues without approval |
| AC-08 | Runtime acceptance scenario fails | Failure is saved; agent repairs and revalidates in the same session |
| AC-09 | Agent tries to finish before verification passes | Stop hook produces continuation with the next required action |
| AC-10 | Agent claims tests passed without recorded execution | Done gate rejects the claim |
| AC-11 | Source or relevant untracked code changes after validation | Verification becomes stale and must be rerun |
| AC-12 | Required browser driver or service is unavailable | Run becomes blocked/unverified with actionable diagnostics |
| AC-13 | Repeated repair attempts make no progress | Configured bound stops the loop as blocked |
| AC-14 | User interrupts or cancels | Work stops; state persists; no hook overrides cancellation |
| AC-15 | Resume after terminal exit during validation | State reloads, stale checks invalidate, incomplete checks rerun |
| AC-16 | Hook fails or state is corrupt | No successful advancement; recoverable error shown |
| AC-17 | Two sessions touch one run | Lock prevents conflicting transitions |
| AC-18 | Requirements change mid-feature | Affected stages and evidence invalidate; plan is revised |
| AC-19 | Existing settings and unrelated files are present | Installation and feature work preserve them |
| AC-20 | All gates and current evidence pass | One completion report is produced and continuation stops |

Test state transitions and adapter event fixtures automatically. Also demonstrate a real successful feature run and a deliberately failing repair run in each supported host. Report tested versions and known coverage gaps; do not substitute a simulated adapter test for real host compatibility.

## 13. Delivery sequence

1. Verify Claude Code's hook capabilities and define supported enforcement boundaries.
2. Implement the shared state machine, schemas, document templates, and verifier.
3. Implement and demonstrate the Claude Code adapter end to end.
4. Complete cancellation, recovery, stale-evidence, bypass-path, and failure-bound tests.
5. Deliver installation/removal instructions and a short example feature.
6. *(Later phase)* Add the Codex adapter against the same core contract.

The finished extension must preserve the normal AI terminal experience while enforcing observable workflow steps and verification gates. It must never present instruction-following, document existence, or a model's assertion as proof that a feature works.

## 14. Start flow and project layout

### Invocation

```
/planvil                        → asks for the feature branch name
/planvil sp20/mcp               → uses that branch
/planvil sp20/mcp path/spec.md  → seeds FEATURE.md from an existing file
```

### Start sequence (before Audit)

1. **Config:** load project config. If `base_branch` is unset, ask once and save it.
2. **Branch:** if the feature branch does not exist, create it from `base_branch`. If it exists, check it out. If the worktree has uncommitted changes, stop and ask; never stash or reset them.
3. **Folder:** create `<docs_root>/<branch-with-dashes>/` (see Layout). If `docs_gitignored` is true, ensure the docs root is in `.gitignore` without touching unrelated entries.
4. **Feature doc:** if `FEATURE.md` is missing, interview the user (goal, users, behavior, scope, out of scope, acceptance criteria) and write it; the user confirms before the run continues. If a spec path was given, copy it into `FEATURE.md`, normalize it to the template, and assign AC IDs without changing meaning.
5. **Activate:** create run state at stage `Audit`. Enforcement starts here.

Stage order: **(Setup →) Feature → Audit → Plan → Decide → Implement → Validate → Done**, with failures looping back to Feature. Setup runs only for a repository with no project verification yet.

### Layout

One flat folder per branch: `<docs_root>/<branch>/`, with every `/` in the branch name written as `-`. No nested folders.

| Branch | Folder |
| --- | --- |
| `sp20/mcp` | `planvil/sp20-mcp/` |
| `feat/hrm-mcp` | `planvil/feat-hrm-mcp/` |
| `hotfix` | `planvil/hotfix/` |

```
planvil/sp20-mcp/
├── FEATURE.md     Goal · Scope · Out of scope · Behavior · Acceptance criteria (AC-01…)
├── AUDIT.md       Affected code (file:symbol) · Current behavior · Reuse · Constraints · Gaps
├── PLAN.md        Status · Phases (P0, P1…) · AC → verification map · Risks
├── DECISIONS.md   Table: # | Decision | Why | By/date — plus Open questions
├── HANDOFF.md     ≤60-line resume packet; the replaced packet goes to handoff-archive/
└── .planvil/      state.json, lock, evidence/ — machine-owned, not hand-edited
```

Every doc has fixed required headings; the stage gate rejects a doc with a missing or empty required section.

### Project config (`.planvil.json` at repo root)

| Key | Default | Meaning |
| --- | --- | --- |
| `base_branch` | unset → ask once | Branch new feature branches are cut from |
| `docs_root` | `planvil` | Root folder for feature folders |
| `docs_gitignored` | `true` | Add `docs_root` to `.gitignore` |
| `require_plan_approval` | `false` | Pause for explicit approval before Implement |
| `max_repair_cycles` | `5` | Repair limit before Blocked |
| `max_equivalent_failures` | `3` | Identical-failure limit before Blocked |
| `verify` | discovered in Setup | Preflight, app launch and readiness, project checks, `known_failures` (§8). Written by `planvil setup apply` after one user approval, never by hand |

## 15. Implementation and distribution

### Decision: Rust

| Reason | Detail |
| --- | --- |
| Hook latency | Hooks run on every tool call; a native binary starts in ~1–3 ms versus ~40–80 ms for Node |
| Correctness | Stages and statuses are enums; illegal transitions are rejected by `match`, not by runtime checks alone |
| Strict schemas | `serde` with `deny_unknown_fields` for state, config, and hook payloads |
| Locks and atomic writes | Advisory file lock per run; write to temp file, `fsync`, then rename |
| Fingerprinting | Fast hashing of tracked, modified, and relevant untracked files |
| Zero runtime | Users install one file; no Node, Python, or Rust toolchain required |

Trade-off accepted: slower to write than TypeScript, and releases need per-platform builds (automated, below).

### Architecture

```
 Claude Code ─┐                       ┌─ .planvil.json (project config)
              ├─ hook JSON ─► planvil ┼─ planvil/<branch>/.planvil/state.json
 Codex CLI ───┘   (stdin)     binary  └─ planvil/<branch>/*.md (feature docs)
                                │
          adapters: claude / codex ─► core: state machine · policy · verifier
```

- **Core** (host-independent): config, branch→folder mapping, state machine, lock, doc gates, action policy, verifier, fingerprint.
- **Adapters** (thin): translate each host's hook payload into core events and core decisions back into that host's response format. No workflow logic.
- **Skill** (`SKILL.md`) and doc templates stay Markdown and tell the agent which `planvil` subcommand to run at each step.

### Subcommands

| Command | Called by | Purpose |
| --- | --- | --- |
| `planvil start [branch] [spec.md]` | skill | Start sequence (§14) and run activation |
| `planvil status` | user / skill | Show stage, status, next action, blockers |
| `planvil advance` | skill | Check the current gate; move to the next stage if it passes |
| `planvil answer <id> <text>` | skill | Record a user answer to an open question |
| `planvil verify` | skill | Run configured checks and scenarios; record evidence |
| `planvil pause` / `resume` / `cancel` | user | Human control (§10) |
| `planvil hook <host> <event>` | host hooks | Pre-tool gate, post-tool record, stop/continuation |
| `planvil install [--host claude\|codex] [--project]` | user, once | Merge skill and hook config into host settings; idempotent |
| `planvil uninstall` | user | Remove only what `install` added |
| `planvil doctor` | user | Host version and capability report (§4) |

### Distribution

- **Targets:** Linux x64/ARM (`musl`, fully static), macOS Intel/Apple Silicon, Windows x64.
- **Releases:** `cargo-dist` on GitHub Actions. Pushing a `vX.Y.Z` tag builds all targets, publishes a GitHub Release, and generates install scripts.
- **User install:** one line (`curl … | sh` on macOS/Linux, `irm … | iex` on Windows), optionally Homebrew. Then `planvil install` once.
- **Host packaging:** a **user-level Claude Code plugin** containing the `/planvil` skill and the hook config that calls the `planvil` binary. Installed once per user; works in every repo. Each repo only adds `.planvil.json`. Hooks are dormant in repos/sessions without an active run.
- **Signing:** not required initially. Browser-downloaded binaries may trigger macOS Gatekeeper or Windows SmartScreen; the script installers avoid most of this. Code signing can be added later.

### Notes

- The binary is the only enforcement authority; the skill only guides. A skill instruction is never treated as a gate.
- `.planvil/state.json` is written only by the binary. Hooks treat a direct agent edit to it as a protected-path violation (§7).
- Hook failures (panic, timeout, bad payload) fail closed for advancement but never trap the session: cancel and status always work (§7, AC-16).
- Shell-write blocking is best-effort command classification; `doctor` reports it as a known gap rather than claiming full coverage (AC-04).
- `pause`, `resume`, `cancel`, and `answer` are user actions. The state machine cannot tell who issued them, so the pre-tool hook must deny them when the agent runs them through Bash. The user runs them with `! planvil …`, which bypasses agent hooks. Resuming a limit-blocked run grants a fresh repair budget; that is an explicit user decision.
- Repair limits are copied into the run state at start and capped at 20, so editing `.planvil.json` mid-run cannot loosen them.
- Known gaps, tracked from the core review:
  - A deleted `state.json` currently looks like "no run yet"; detect leftover run artifacts and report corruption instead.
  - On Windows, `rename` has no retry on a transient `PermissionDenied`, and directory fsync is a no-op.
  - The feature folder must be canonicalized and checked to stay inside the repository (no symlink escape) when it is created; `planvil start` will do this.
- Claude Code only for now: `install` and `hook` accept only `claude` in this phase. Keep host-specific code inside `adapters/claude` so a Codex adapter can be added later without touching the core.

## 16. Implementation status (Claude Code phase)

Tested host: **Claude Code 2.1.309** on Linux, release binary built with Rust 1.98.1. Line coverage 83% (cargo-tarpaulin; subprocess-driven CLI tests are not counted).

### Compatibility matrix

| Shared semantic | Claude Code event | Status |
| --- | --- | --- |
| Initialization / reattach | `SessionStart` (context), `PostToolUse` on Bash (session binding) | Supported |
| Pre-action check | `PreToolUse` (all tools), `onFailure: block` | Supported |
| Observed action result | `PostToolUse` on `Bash|AskUserQuestion` | Supported |
| User input / user-only controls | `UserPromptSubmit` (records time, applies typed `planvil pause|resume|cancel`) | Supported |
| Attempted completion | `Stop` with `decision: block` | Supported; bounded by `max_stop_continuations` and the host cap of 8 |
| Interruption | Esc does not fire `Stop` (host behavior) | Honored |
| Strict OS-level read-only mode | — | Not supported (reported by `planvil doctor`) |

### Acceptance coverage

"Real host" means a headless `claude -p --plugin-dir` session on the tested version, resumed
across turns with `--resume`.

| ID | Evidence |
| --- | --- |
| AC-01 | Deferred: Codex phase |
| AC-02 | Real host: full successful run and full repair run |
| AC-03 | `policy` and hook-fixture tests; real host: Edit on `README.md` denied at Feature, file unchanged |
| AC-04 | `shell` tests (redirection, substitution, `sed -i`, `find -exec`, unknown commands); best-effort status reported by `doctor` |
| AC-05 | `docs` tests (blank templates fail); `ops` gate tests |
| AC-06 | `ops` tests (answer requires a later user message); real host: the agent asked a scope question, the run waited, and the answer updated FEATURE.md |
| AC-07 | Real host: routine choices (`printf` over `echo`, argument handling) recorded in DECISIONS.md without asking |
| AC-08 | `ops::full_run_fails_repairs_and_completes`; real host: verification #1 failed, #2 passed in the same session |
| AC-09 | Hook test; real host: stop blocked three times with the next action |
| AC-10 | Done is reachable only through `planvil verify`; `report` refuses unfinished runs (CLI test) |
| AC-11 | `fingerprint` tests; Repair cannot revalidate unchanged code; results discarded if files change during verify |
| AC-12 | `ops::missing_tool_blocks_instead_of_passing`; `verify` preflight tests |
| AC-13 | `machine` limit tests; `ops::repeated_identical_failures_block_the_run`; real host: no-progress stops end as Blocked |
| AC-14 | Prompt-hook control tests; agent-issued `planvil cancel` denied |
| AC-15 | `ops` resume test; SessionStart context test; interrupted verification records nothing and must rerun. Not yet exercised on the real host |
| AC-16 | Hook tests (corrupt state, malformed payload, panic output); `store` corruption tests |
| AC-17 | `store` lock tests; verify claims a run number under the lock and discards results if the run changed |
| AC-18 | `ops::changing_requirements_mid_run_rewinds_to_feature`; real host: editing FEATURE.md rewound the run and re-confirmation was asked |
| AC-19 | `ops` test preserving `.gitignore` and `.planvil.json` keys; install uses `claude plugin` commands only |
| AC-20 | `ops` full-run test; real host: one REPORT.md, continuation stopped |

### Reviews

Two independent reviews (Rust correctness; security of the enforcement surface) were run
after the build. Every cheaply fixable finding was fixed with a regression test:

- the shell parser desync (ANSI-C quoting, heredocs, comments, substitution inside redirects);
- "read-only" tools that write or execute (`sed -i` clusters and `w`/`e`, `awk`, `sort -o`, `uniq IN OUT`, `rg --pre`, `fd -x`, `git -c`/`--output`/`-O`, variable prefixes);
- branch switching and detached HEAD escaping the run;
- writes into the run's repo from another working directory;
- writes outside the repository during Build;
- scheduling tools and worktree-isolated subagents;
- answers bound to the recorded user reply;
- plan approval covering verify.json;
- no-op and weakened checks;
- stale-evidence loops;
- orphaned processes;
- report honesty about stale evidence;
- `.gitignore` and porcelain-parsing bugs;
- a safe uninstall.

Inherent limits that remain, by design:

- The Build-stage shell is open.
- Text checks on protected paths and planvil controls can be evaded by code the agent runs as the same OS user.
- Run state is unsigned.
- Whether a reply is an approval is the agent's reading, with the verbatim reply recorded beside it.

### Known coverage gaps

- Windows: builds via cargo-dist, but the test suite uses POSIX `sh` and is run only on Linux and macOS.
- A completed run is not re-checked if the code changes after Done; start a new run.
- If `planvil verify` itself is killed, its process groups survive until stopped; the next verification detects them and refuses to run (it never kills a recorded ID itself).
- Qualitative (manual) observations are not supported; every criterion needs an automated check or scenario.
