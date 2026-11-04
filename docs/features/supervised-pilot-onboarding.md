# Supervised pilot: onboarding one colleague (Engram + TermAl)

**Status: first supervised pilot, one colleague, Windows. As of 2026-10-18; the
TermAl facts below were verified against master `13063e3b` and re-checked by
review at `982e043a`. The commit to install is the one the supervisor names,
not either of these.** This document is the
operating guide for that pilot and nothing wider.

**What TermAl records.** The pilot runs with the project's Engram integration
enabled and its Execution Observability on (the default). The agent gets the
Engram MCP server and work context from TermAl; no turn waits for an Engram decision. The
agent names its task's worktree, and TermAl reports each turn to Engram after
it ran, as a host observation: how it ended, whether it changed source, and
the declared tests it ran, each judged from its TRX file as passed, failed or
indeterminate ([Execution Observability](execution-observability.md)). Those
reported checks are not test credit. In TermAl, test credit comes only from a
TermAl test-launcher run registered with committed input, which a dotnet
repository does not have, so in this pilot the agent's note of the TRX
counters on the item is the test evidence. Acceptance evaluation is
temporarily switched off as a blocker until Execution Observability is
implemented: completion is self-asserted unless the supervisor set evaluated
acceptance (section 3).

Related: [Engram host adapter](engram-host-adapter.md) (settings,
verification), [Execution Observability](execution-observability.md), [Declared test commands](declared-test-commands.md) (the
`termal-tests.toml` route), [Test runs](test-runs.md) (the runs TermAl shows),
[Root coding sessions](root-coding-sessions.md).

## The envelope

The pilot is:

- one colleague, supervised by Greg; the supervisor is reachable while the
  colleague works;
- one Windows machine;
- one repository with no Git submodules and no Git LFS;
- plain agent sessions with Engram tracking: a root session per task, in a
  worktree used for that task only and named for it (section 5);
- planned restarts only; the colleague never restarts TermAl or Engram on their
  own;
- no capacity pauses expected.

Outside the pilot, and not to be tried during it: repositories with submodules
or LFS; the Merger/landing train used by TermAl's own development; delegated
child sessions (reviewers, workers — the acceptance evaluator child of
section 6 is the one exception); cross-machine task sync; any Engram policy
or store change; any TermAl or Engram update that is not a planned stop,
replace, start by the supervisor.

## 1. Prerequisites on the colleague's machine

- Git for Windows.
- Rust via [rustup](https://rustup.rs/), stable toolchain (the crate uses
  edition 2024; `cargo` on `PATH`).
- Node.js at the version in `.nvmrc` of the TermAl repository; `npm` on `PATH`.
- PowerShell 7.3 or later (`pwsh`; check with `$PSVersionTable.PSVersion`):
  the declared test route assumes 7.3's argument passing. Windows PowerShell
  5.1 (`powershell`) and `cmd` are not accepted by it.
- The .NET SDK the repository builds with (`dotnet --version` works).
- One supported agent CLI signed in and working in a plain terminal: Codex or
  Claude Code. Verify it answers a prompt before involving TermAl.
- The colleague's repository cloned locally (no submodules, no LFS), and a
  clone of TermAl checked out at the commit the supervisor names (section 2).

Everything an agent writes stays inside the repository folder it works in — a
rule the agents follow, not one TermAl enforces (section 7). TermAl keeps its
own data under `~/.termal/` (that is `C:\Users\<name>\.termal\`), Engram under
its home (section 3); the agent CLIs keep their own state under the user
profile (`~/.claude`, `~/.codex`), and dotnet, npm and cargo write their
caches there (`%USERPROFILE%\.nuget\packages`, `~/.cargo`). Nothing else on
the machine is written by TermAl or by the agent.

## 2. Install and run TermAl from master

From the TermAl clone, in `pwsh`, with `<commit>` the master commit the
supervisor names (the supervising host runs the same one):

```powershell
git fetch origin
git checkout <commit>
cargo build --release
cd ui
npm ci
npm run build
cd ..
.\target\release\termal.exe
```

`cargo build --release` produces `target\release\termal.exe`. The host serves
the built UI from `ui\dist`, so `npm run build` must have run once from the
same checkout. Start the host from the repository root and open
`http://localhost:8787` in a browser. Sessions, projects and preferences are
stored in `~/.termal/termal.sqlite`.

Record the build: the commit, and the SHA-256 of `target\release\termal.exe`
(`Get-FileHash .\target\release\termal.exe`), on the pilot's tracker item.

Already installed (a machine that already runs a TermAl host): skip the build
unless the supervisor names a new commit; check `git rev-parse HEAD` against
that commit and continue with the supervisor steps below.

To update TermAl later: stop the host; keep the previous build for rollback by
renaming `target\release\termal.exe` to `termal.exe.previous` and copying
`ui\dist` to `ui\dist.previous`; `git fetch origin` and
`git checkout <new commit the supervisor names>`; build the executable AND the
UI as above; start the host. Never replace `termal.exe` while it is running
(the host spawns helpers from that path); always stop, replace, start. The
backend serves whatever `ui\dist` holds, so the executable and the UI must
always come from the same commit.

Rollback: stop the host; restore both `termal.exe.previous` and
`ui\dist.previous` (or check out the recorded previous commit and rebuild
both); start the host. The data files under `~/.termal/` are not touched.

**Supervisor steps, once per machine** (the colleague does not do these):

1. Add the colleague's repository as a project in the control panel
   (Projects), pointing at its root folder.
2. Connect Engram for that project (section 3): **Settings > Engram** for the
   binary and home; then the project's Engram settings — Enabled on, and the
   **Execution Observability** checkbox left on; **Verify**, then
   **Save & enable**.
3. Create the colleague's root session IN that project (the new-session
   dialog: agent, project). The settings belong to the session's project: a
   session created elsewhere has the other project's binding, or none.

The task's worktree (section 5): in this pilot the clone's main checkout is
the worktree, for one task at a time, and the new-session dialog creates every
session in it (the dialog takes no working directory; with a project selected
the session opens in the project's root). A second task in parallel is outside
this pilot: it needs a linked worktree INSIDE the project folder
(`git worktree add .worktrees\<task> -b <branch>` from the repository root,
with `.worktrees/` in `.gitignore`) and a session created by the supervisor
through `POST /api/sessions` with the project's `projectId` and that folder's
absolute `workdir`, checking `session.workdir` in the response
([Root coding sessions](root-coding-sessions.md)). A worktree outside the
project folder (a sibling folder, for example) cannot be named as a source
root and a session there is not in the project: it gets no Engram tools and no
host observations.

## 3. Connect Engram

What the repository and the host each provide
([Engram host adapter: configuration authority](engram-host-adapter.md#configuration-authority)):

- The repository declares the project with a non-empty `.engram-project` file
  at its root. The team commits it, so every clone is declared the same way.
- The host stores, machine-wide, a developer name, the Engram binary path and
  the Engram home: **Settings > Engram**. The developer name is the
  colleague's short handle (lowercase ASCII letters, digits, `.`, `_`, `-`).
- Each project is enabled by **Verify** followed by **Save & enable**, and
  turned off by **Disable Engram**; its other setting is the
  **Execution Observability** checkbox, on by default, which the pilot leaves
  on. **Run Full Audit** there is not a setting but a whole-store `doctor`
  run: it opens the store writable and may perform SQLite recovery, so only
  the supervisor runs it, and never during a session's work. TermAl asks the
  store for no assurance level.
- **Verify** runs Engram's readiness check without changing anything;
  **Save & enable** repeats it and persists. A readiness refusal names its
  cause; fix the cause rather than retrying. The host refuses a change to
  **Settings > Engram** (developer name, binary path, home) while any project
  is enabled: disable the project first, change the setting, then Verify and
  Save & enable again.

TermAl injects the Engram MCP server into each agent session of an enabled
project and adds `engram work next --peek` context at session start.
The colleague configures nothing in the agent CLI for this.

Engram-side steps, provided by Engram's coordinator (Engram::Fable, 2026-10-14,
cited to Engram master `a524699`; quoted here in substance, with the two
statements marked INFERENCE kept as such):

**Binary.** Build Engram from a checkout at the commit the supervisor names
(today `a524699`; the supervisor says if master moves before the pilot):

```powershell
cargo +stable install --path . --locked
```

It installs to `%USERPROFILE%\.cargo\bin\engram.exe`; there are no prebuilt
binaries. Verify with `engram --version`: it must show the pinned revision and
no `+dirty`. Point TermAl's **Settings > Engram** binary path at that absolute
`engram.exe`. An installed upgrade does not reach running MCP processes until
the session is recreated.

**Project and store.** In the repository root, commit a `.engram-project` file
containing one stable id, for example `example.com/team/repo`. If the team's
Engram project already exists, use its id: a reused id shares that project's
store and policy. It is looked up from the working directory upward; do not
add a second marker in a subfolder, and do not place a clone under another
declared checkout (a clone without its own marker would silently use the
outer one).
The Engram home is an absolute, local, non-synced folder, for example
`C:\Users\<colleague>\.engram`; there is no default — without `--home` or
`ENGRAM_HOME` the CLI refuses, and a relative home silently splits the store.
The store is created at `<home>\projects\<sha256 of the project id>\engram.db`.
Run `init` before any other Engram command touches the store — but skip it
if the project's store already exists under that home (an `init` there would
be a store change); the supervisor decides. Run every Engram command in this
document from the colleague's repository root (`cd <repo>` first): without
`--project-file`, Engram selects the nearest `.engram-project` above the
working directory, so a command run from another checkout acts on that
checkout's project, or refuses on a fresh home.

```powershell
cd <repo>
engram --home <H> --project-file <repo>\.engram-project init --required-assurance advisory --authorized-by <colleague>
engram --home <H> --project-file <repo>\.engram-project doctor
engram --home <H> --project-file <repo>\.engram-project readiness --json
```

These are the commands for the Engram commit named above. An Engram build
that has removed turn gating no longer takes `--required-assurance` or
`--authorized-by` on `init`; with such a build, run `init` without them. TermAl
does not read the store's assurance level either way. INFERENCE: a writing
command run before `init` may create the store with a default, so `init`
comes first. Acceptance evaluation defaults to self-asserted completion, and
stays so unless the supervisor sets evaluated acceptance; if set, no criterion
may be bound to a test, since nothing in this pilot can earn host test credit,
so a test-bound criterion could never pass evaluation; the evidence is the TRX
counters noted on the item.

**MCP wiring.** With the project's Engram integration enabled, TermAl injects
`engram mcp --actor-id A --session-id S [--actor-context C]` per session with
`ENGRAM_HOME`, `ENGRAM_ACTOR_ID`, `ENGRAM_SESSION_ID` and
`ENGRAM_ACTOR_CONTEXT`; actor and session are arguments, not read from the
environment. Naming: `<colleague>` for the human, `<colleague>/claude` or
`<colleague>/codex` for agents. One MCP process per session, with distinct
session ids. By hand, the colleague configures nothing beyond enabling the
integration and the binary and home in TermAl's settings.

Readiness in TermAl (**Verify**) must pass on the colleague's machine before
the first task; a refusal names its cause.

## 4. Declare the repository's tests

TermAl recognises a test only when it is declared in `termal-tests.toml` at the
repository root and run exactly as declared
([Declared test commands](declared-test-commands.md)). One entry per test
project and target framework; the result is a TRX file in a git-ignored
directory.

**What is recorded:** TermAl recognises a declared run and judges it from its
TRX file as PASS, FAIL or UNKNOWN (below), and reports that judgement in the
turn's host observation as a check that is `passed`, `failed` or
`indeterminate`. It is never test credit: the agent notes the TRX counters on
the item itself.

`termal-tests.toml`:

```toml
[[test]]
command  = "dotnet test tests/Orders.Tests --logger \"trx;LogFileName=results.trx\" --results-directory tests/Orders.Tests/.termal-results"
cwd      = "."
artifact = "tests/Orders.Tests/.termal-results/results.trx"
```

`.gitignore`:

```
.termal-results/
```

Commit both. Use forward slashes in the declared paths. Do not add `--no-build`
or a `.dll` path. The declared route reads only letters, digits and
`. _ / : = @ + -` in a word, plus space and `;` inside quotes
([declared test commands](declared-test-commands.md)); it refuses `^`, `|`,
`~`, `!`, `,`, `&`, `$` and `%`, so a filter such as `FullyQualifiedName~X`
cannot be declared, and one unreadable entry disables the whole
`termal-tests.toml` until it is fixed. Use a positive filter written without
quotes, `--filter TestCategory=Fast` (`=` is allowed in an unquoted word), or
a runsettings file. A declared command must contain no single quote when
Codex will run it: Codex reports the command inside a `pwsh` wrapper whose
script must be one single-quoted word with no `'` inside, so an entry with a
single-quoted argument is not recognised for Codex (no check, no card line,
no judgement); Claude's `pushd` form is unaffected.

The agent runs one entry exactly as declared, in the foreground, with nothing
piped, redirected or chained, in the form its runtime records. The form
differs by agent:

- **Claude Code**: through its Bash tool, as one call with the declaration's
  directory first, written as a Windows path:

  ```
  pushd "C:\path\to\repo" && dotnet test tests/Orders.Tests --logger "trx;LogFileName=results.trx" --results-directory tests/Orders.Tests/.termal-results
  ```

  A run through Claude's PowerShell tool is not recorded.
- **Codex**: the declared command alone, as a command of its own, with the
  tool's working directory set to the declaration's `cwd` (the repository
  root, absolute). No `pushd`, no `&&`: Codex reports a shell-wrapped command,
  and a `pushd … && …` line inside that wrapper is refused by the declared
  route.

Never wrap the command in a script or another shell, and never retype it with
other flags to make it match. TermAl judges the run after the command returns,
and reports it with the turn's host observation when the turn ends; a run made
in the turn that named the worktree is reported in the workdir the turn began
with (section 5). The judgement:

- **PASS**: exit 0, the TRX file written by this run, at least one test
  executed and passed, none failed.
- **FAIL**: a failed, errored or aborted run, or a non-zero exit.
- **UNKNOWN**: everything else, with the observed reason (artifact missing,
  not ignored, unchanged, zero executed, declaration changed). UNKNOWN is not a
  weak PASS. Fix the declaration or the ignore rule and run again.

Verify the declaration once, before the first real task, by asking the agent
in a session to run the declared command (not by typing it in a terminal: only
the agent's run is observed), and confirm the TRX file appears at the declared
path with the counters the agent then notes.

## 5. The daily workflow

Each task is one Engram work item, one root session, one worktree. The agent
uses Engram through its MCP tools; the same words exist on the command line
(`engram work next --peek`, `claim REF`, `note "…"`, `gate NAME`, `done "…"`,
`add "title" --accept "criterion"`, `ls`, `show REF`, `remember`, `memories`).

1. `engram work next` — see what is ready, then read the project memories (the
   agent does this at session start; the colleague reads the result on the
   card).
2. Claim the item and, in the same turn, name the worktree for it once
   (`termal_name_source_root`); one item per worktree, never two. Then end the
   turn: naming takes effect at the next turn. Edit nothing and run no test in
   the naming turn.
3. In a fresh turn, work in the named worktree. TermAl measures it and
   reports the turn's source and checks there.
4. Run the declared test in the agent's run form (section 4), in the
   foreground, and wait for it to return. An edit or another command in that
   worktree while the test runs makes the reported check `indeterminate`.
5. Note findings and the test evidence on the item: the TRX path, the
   executed/passed/failed counters and the exit status, copied from the result
   file.
6. `done` completes the item. Acceptance is self-asserted unless the
   supervisor set evaluated acceptance (section 3), in which case request the
   evaluation first and keep that turn open until it submits; the item's
   criteria must then not be bound to a test (section 3).

The rules the agent keeps so the reported check says what happened:

- Run the declared test in the foreground and wait for it; nothing in this
  pilot runs in the background or detached (TermAl's own background and
  detached full-gate forms belong to its development repository, not to a
  dotnet repository).
- Never wrap the test command in a script or another shell, and run it
  exactly as declared: a wrapped or otherwise non-matching command is
  silently unrecognised and reported as no check, because the only
  non-recognition notice TermAl writes is for its own node test launcher, not
  for declared commands.
- Lint and format commands are not tests; run them as ordinary commands.
- Name each worktree for one item only.
- No local helper agents (for example a Claude Explore or Task subagent) in a
  session while its tests run; their work marks the turn's observation mixed.

Land code through the repository's own rules; TermAl's Merger train is not part
of the pilot.

## 6. When something looks stuck

Read the session card first: TermAl writes a line for a refusal or an unknown
result it can name, with its cause; a command it did not recognise as the
declared test leaves no line at all (section 5). Then:

- A test reads UNKNOWN: check the reason (section 4); do not rerun blindly.
- A prompt stays queued or a card says the queue is paused: do nothing in that
  session; tell the supervisor and quote the card line.
- A session is idle for longer than expected: check the **Test Runs** tab for
  a run still in progress; what it lists for the pilot's build is stated in
  [Test runs](test-runs.md), and the supervisor says which build the pilot
  runs.
- An Engram refusal: every refusal names its code and the next commands to
  run; follow those. A lapsed claim is recovered by the agent with
  `claim REF --recover "why"`. Anything else goes to the supervisor before any
  restart, with the command, the exact message and the time.
- Nothing responds: tell the supervisor.

**Rule: ask your supervisor before restarting TermAl or Engram.** A restart is
planned, done by the supervisor as stop, replace or rebuild, start. A restart
lets claims lapse (the agent recovers its claim with `claim REF --recover`, as
in the Engram refusal line above); the supervisor re-names the worktree after
the claim is recovered.

Do not cancel a delegated child session mid-run. The one delegated session
the pilot uses is the acceptance evaluator child that
`termal_evaluate_acceptance` spawns, only when an evaluation is requested;
the rule covers it — wait for its fan-in. Any other
child: leave it to the supervisor. Do not edit `~/.termal/`, the
Engram home or the Engram store by hand, and never kill an `engram.exe`
process. Off-limits Engram commands in the pilot: `migration export/import`,
`restore`, `doctor --repair-projections`, `--recover-policy`, any
`control-policy set-*`, `task-sync enable`, `backup target set`.

## 7. Risk boundaries, plainly

- Data: `~/.termal/termal.sqlite` holds sessions and transcripts; the Engram
  home holds the project store (SQLite in WAL mode; INFERENCE from Engram's
  coordinator: an app or agent crash keeps committed work, a power loss or OS
  crash can lose the most recent commits). Run
  `engram --home <H> --project-file <repo>\.engram-project backup` from the
  repository root daily and before any planned restart (the project selection
  rule of section 3 applies: from another checkout it would back up that
  checkout's project); it writes under `<home>\backups\` on
  the same machine and does not protect against losing the machine — off-host
  backup is outside the pilot. Restoring is the coordinator's act. Never
  delete or replace the data files. A TermAl restart loses no Engram data,
  and a turn running across it is not reported. A restart alone does not
  prove that every Engram process of the old build has exited: before a
  session's new work-session connection opens, that session's earlier
  Engram child and its descendants must be confirmed exited, with no overlap
  between the old and the new. The running build is the one measured after
  the restart (its hash taken then), since a start script may rebuild or
  replace the installed binary.
- Machine: agents create, change and delete files only inside the repository
  folder they work in. Anything outside it — installing software, changing
  user or global configuration — is the supervisor's act, never the agent's.
- Evidence: the test evidence is the agent's note of the TRX counters. TermAl
  also reports each declared run in the turn's host observation, never as
  test credit; a missing result is UNKNOWN, never a pass.
- Excluded, repeated: submodules/LFS repos, the landing train, delegated
  children (except the acceptance evaluator child, section 6), cross-machine
  sync, policy or store changes, unplanned restarts.

## 8. Evidence before inviting the colleague

The supervisor ticks each box with its locator on the pilot's tracker item.
The decision to invite is Greg's; this list is what the decision rests on.

- [ ] Greg's own run on the colleague's machine, following only this document,
      completed one task end to end: a named worktree, a declared test run with
      its TRX counters noted on the item, and the turn's host observation in
      Engram (this run is a checklist box, not a criterion of the plan item).
- [ ] Visible test-run registration is completed and is the running host build
      on the supervising machine.
- [ ] The pilot validation Greg directed on 2026-10-16, in place of any dry
      run on a fixture clone: once the relevant work is on master, one safe,
      independent P3 item of the Phoenix project is run through the normal
      runner, test and evaluation path by an agent session that never held
      the plan item. Before anything starts, Greg receives the proposed item,
      why it is isolated, the exact session and project route, and the one UI
      action (if any) he must take; nothing starts before his reply. The run
      is noted on the item: the declared test ran and the TRX counters were
      noted. Phoenix's index holds gitlinks (submodules), and a repository with
      submodules has no source basis ([Engram host adapter](engram-host-adapter.md),
      "When there is none"), so its turns report no source change there. A
      fixture clone may still serve the supervisor to qualify the
      declaration's form; that qualification does not tick this box.
- [ ] The declared dotnet/TRX live proof after install and restart is
      completed: it shows the declaration is well formed and its judgement
      reaches the turn's host observation.
- [ ] The Engram-side steps in section 3 are the ones Engram's coordinator
      provided, quoted with attribution and unchanged in substance, and the
      readiness check passes on the colleague's machine.
- [ ] The supervisor can name the two open operational workarounds (no
      cancelling delegated children mid-run; binary updates only by stop,
      replace, start) and the restart effects (claims lapse, a turn running
      across the restart is not reported).
- [ ] The Engram-alone pilot plan and this plan were reported to Greg together.
- [ ] A rollback for the colleague's machine is written down and the previous
      build is kept: stop the host, restore `termal.exe.previous` and
      `ui\dist.previous` together (or rebuild both from the recorded previous
      commit), start; the data files are not touched (section 2).

## 9. Pilot evidence

What the pilot itself must produce, agreed with Engram's coordinator; the
Engram-side lines (Engram tools listed, store and policy facts, and E4, the
backup drill) are in Engram's colleague sheet, which this list cross-references
rather than repeats.

- E1 — Installation: TermAl built from the named commit with its build hash
  recorded on the tracker item; a root session in the project lists the Engram
  tools; Execution Observability on and one worktree named.
- E2 — One real task end to end: add, claim, name the worktree, work, the
  declared test in the foreground with its TRX counts noted on the item and
  its host observation in Engram, note, done; plus one acceptance evaluation
  if the policy enables it.
- E3 — A planned-restart drill with one claimed item: the host is stopped and
  started on the same build ("host back"); the claim is recovered and the
  worktree re-named, as section 6 documents.
- E4 — Backup: per Engram's sheet.
- E5 — An incident log, including every TermAl stuck state the colleague met,
  quoted from the UI card.
- E6 — After the agreed sessions, the colleague's two-line verdict; Greg
  decides the rollout.
