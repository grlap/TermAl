# Supervised pilot: onboarding one colleague (Engram + TermAl)

**Status: first supervised pilot, one colleague, Windows. As of 2026-10-18; the
TermAl facts below were verified against master `13063e3b` and re-checked by
review at `982e043a`. The commit to install is the one the supervisor names,
not either of these.** This document is the
operating guide for that pilot and nothing wider.

**The tier decides what TermAl records.** The pilot runs at tier I1 unless Greg
chooses tier I2 in writing:

- **I1 (Base):** the agent gets the Engram MCP server and advisory context from
  TermAl; no turn is gated by Engram. At I1 TermAl **names no worktree and
  records no test check**: a declared test runs as an ordinary command, with no
  card line and no host credit; the agent's own notes on the item are the test
  evidence, and acceptance is self-asserted unless the coordinator set
  evaluation (section 3). This is the tier the plan assumes.
- **I2 (turn-gated control):** Engram authorizes each turn; the agent names its
  worktree and TermAl records declared tests as host checks with credit. This
  is the only tier at which sections 4 and 5's credited-test steps apply. It
  is not in the plan unless Greg chooses it, and its known open defects are
  the supervisor's to weigh (retained prompts, manual Resume after a mid-turn
  cut).

Every section below says which tier it applies to.

Related: [Engram host adapter](engram-host-adapter.md) (tiers, settings,
verification), [Declared test commands](declared-test-commands.md) (the
`termal-tests.toml` route), [Test runs](test-runs.md) (the runs TermAl shows),
[Root coding sessions](root-coding-sessions.md).

## The envelope

The pilot is:

- one colleague, supervised by Greg; the supervisor is reachable while the
  colleague works;
- one Windows machine;
- one repository with no Git submodules and no Git LFS;
- plain agent sessions with Engram tracking: a root session per task, in a
  worktree used for that task only (named for it at I2; at I1 naming is
  refused, section 5);
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
   Turn-gated control checkbox per the tier Greg chose (I1 off, I2 on);
   **Verify**, then **Save & enable**.
3. Create the colleague's root session IN that project (the new-session
   dialog: agent, project). The tier belongs to the session's project: a
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
tier.

## 3. Connect Engram

What the repository and the host each provide
([Engram host adapter: configuration authority](engram-host-adapter.md#configuration-authority)):

- The repository declares the project with a non-empty `.engram-project` file
  at its root. The team commits it, so every clone is declared the same way.
- The host stores, machine-wide, a developer name, the Engram binary path and
  the Engram home: **Settings > Engram**. The developer name is the
  colleague's short handle (lowercase ASCII letters, digits, `.`, `_`, `-`).
- Each project is enabled by **Verify** followed by **Save & enable**, and
  turned off by **Disable Engram**; its only other setting is the
  **Turn-gated control** checkbox. **Run Full Audit** there is not a setting
  but a whole-store `doctor` run: it opens the store writable and may perform
  SQLite recovery, so only the supervisor runs it, and never during a session's
  work. At I1: enabled, Turn-gated control
  **off**. Only at I2, by Greg's written choice: enabled with the checkbox on
  (the checkbox also requires the Engram store to have been initialised with
  `--required-assurance turn_gated`, not `advisory`; the supervisor sets that
  up, never the colleague).
- **Verify** runs Engram's readiness check without changing anything;
  **Save & enable** repeats it and persists. A readiness refusal names its
  cause; fix the cause rather than retrying. The host refuses a change to
  **Settings > Engram** (developer name, binary path, home) while any project
  is enabled: disable the project first, change the setting, then Verify and
  Save & enable again.

TermAl injects the Engram MCP server into each agent session of an enabled
project and adds advisory `engram work next --peek` context at session start.
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

Why `advisory`: a plain `engram init` makes the store `turn_gated`, which only
an honest turn-gated host can claim; for plain agent sessions with Engram
tracking, advisory is the truthful setting and matches tier I1. INFERENCE: a
writing command run before `init` may create the store with the turn-gated
default, so `init` comes first. Acceptance evaluation defaults to self-asserted
completion, and at I1 it stays so unless the supervisor sets evaluated
acceptance; if set at I1, no criterion may be bound to a test — TermAl records
no host check at I1, so a test-bound criterion could never pass evaluation;
the evidence is the TRX counters noted on the item. At I2 evaluated acceptance is a
PREREQUISITE the supervisor sets once before any I2 work (`engram
control-policy set-acceptance-evaluation --modes independent_session …`),
never the colleague: with the default policy the I2 workflow's evaluation
request is refused ("the project's Engram policy admits no
acceptance-evaluation mode") and the item could never complete as section 5
describes.

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

**Tier:** the declaration is committed at both tiers, so a later move to I2
needs no change. At I1 the declared command runs as an ordinary command: no
check is recorded, no card line and no credit appear, and the agent notes the
TRX counters on the item itself. The recognition, the card lines and the
PASS/FAIL/UNKNOWN judgement described below exist only at I2.

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
no credit); Claude's `pushd` form is unaffected.

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
other flags to make it match. At I2 TermAl writes the card line at the turn's
closing checkpoint, not the moment the command returns; a run made in the
turn that named the worktree earns nothing (section 5). What TermAl then
shows on the session card:

- **PASS**: exit 0, the TRX file written by this run, at least one test
  executed and passed, none failed — eligible for verification credit.
- **FAIL**: a failed, errored or aborted run, or a non-zero exit.
- **UNKNOWN**: everything else, with the observed reason (artifact missing,
  not ignored, unchanged, zero executed, declaration changed). UNKNOWN is not a
  weak PASS; it earns nothing. Fix the declaration or the ignore rule and run
  again.

Verify the declaration once, before the first real task, by asking the agent
in a session to run the declared command (not by typing it in a terminal: only
the agent's run is observed): at I2, confirm the card shows the TRX counters
at the turn's end; at I1, confirm the TRX file appears at the declared path
(there is no card line to check).

The three outcomes, exercised at I2 (the dry run and the invitation checklist
use them), one per turn so each card line is its own: PASS — the declared
command on a green test project; FAIL — change one assertion so a test fails,
run the same declared command, restore the assertion afterwards; UNKNOWN
(artifact missing) — before the run, in a turn of its own, change the entry's
`artifact` in `termal-tests.toml` to a file name the command never writes
(for example `results-missing.trx`), then run the declared command, and
restore the declaration afterwards. The declaration is matched and hashed and
the artifact's starting state taken when the command starts, and both are
checked again after it returns, on TermAl's own thread; so the declaration
must be stable throughout the run, the artifact must already be absent when
the command ends (deleting the TRX after the command has returned is too late
and does not produce UNKNOWN), and every restoration — the assertion, the
declaration's `artifact` — waits for the NEXT turn, after the run's turn has
closed, never the same turn as the run (section 5, I2 step 4). Run the PASS
last, after every drill edit is restored exactly, so the acceptance
evaluation requested in the following turn judges the tree the PASS ran on.

## 5. The daily workflow

Each task is one Engram work item, one root session, one worktree. The agent
uses Engram through its MCP tools; the same words exist on the command line
(`engram work next --peek`, `claim REF`, `note "…"`, `gate NAME`, `done "…"`,
`add "title" --accept "criterion"`, `ls`, `show REF`, `remember`, `memories`).

**At I1 (the plan's tier):**

1. `engram work next` — see what is ready, then read the project memories (the
   agent does this at session start; the colleague reads the result on the
   card).
2. Claim the item before any edit. Do not name a worktree: at I1 TermAl refuses
   naming ("Engram control is not enabled for this session or its project").
3. Work in the task's worktree; one item per worktree, never two.
4. Run the declared test in the agent's run form (section 4), in the
   foreground, and wait for it to finish. TermAl records nothing for it at I1.
5. Note findings and the test evidence on the item: the TRX path, the
   executed/passed/failed counters and the exit status, copied from the result
   file.
6. `done` completes the item. Acceptance is self-asserted at I1 unless the
   supervisor set evaluated acceptance (section 3), in which case request the
   evaluation first and keep that turn open until it submits; the item's
   criteria must then not be bound to a test (section 3).

**At I2 (only by Greg's written choice):**

1. `engram work next`, then the project memories, as above.
2. Claim the item and, in the same turn, name the worktree for it once
   (`termal_name_source_root`); one item per worktree, never two. Then END THE
   TURN: naming takes effect at the next turn, and a turn that acquired its
   claim cannot report evidence under it. Edit nothing and run no test in the
   naming turn: a test run there earns nothing.
3. In a fresh turn, work in the named worktree. TermAl measures it and credits
   only tests run there.
4. Run the declared test in the agent's run form (section 4), in the
   foreground, wait for it to return, and make it the LAST command in that
   worktree for the turn. TermAl reads the declaration, the artifact and the
   closing snapshot after the command returns, on its own thread, and the
   check stays open until those reads finish: an edit or another command in
   that worktree while the test runs, or before the turn's closing checkpoint
   has settled, marks the check overlapped and voids its credit. Tracker words
   (`note`) are not worktree commands and are safe. The card line appears at
   the turn's closing checkpoint.
5. Note findings and evidence on the item.
6. Request the acceptance evaluation in the turn after the passing check, and
   keep that turn open until the evaluator submits; the evaluation is recorded
   on the item. This step presumes the supervisor set evaluated acceptance in
   section 3 (an I2 prerequisite); if the request is refused with "admits no
   acceptance-evaluation mode", stop and tell the supervisor — do not fall
   back to a self-asserted `done`. If it is refused with "source root changed
   while it was evaluated", request it once more and do not rename the root.
7. `done` completes the item once the evaluation is recorded.

The credit rules at I2, verified on the pilot's host build (the rules the agent
must keep for a test to count):

- Run the declared test in the foreground and wait for it; nothing in this
  pilot runs in the background or detached (TermAl's own background and
  detached full-gate forms belong to its development repository, not to a
  dotnet repository).
- Never wrap the test command in a script or another shell, and run it
  exactly as declared: a wrapped or otherwise non-matching command is silently
  unrecognised — no check, no card line, no credit — because the only
  non-recognition notice TermAl writes is for its own node test launcher, not
  for declared commands. A missing card line after a test therefore means the
  command did not match the declaration; compare it word for word.
- Lint and format commands are not test records; run them as ordinary
  commands, not as tests.
- Name each worktree for one item only.
- No local helper agents (for example a Claude Explore or Task subagent) in a
  session while its tests run; one restricts that session's credit.
- Ask before any restart: a restart voids a passed but not yet evaluated check
  (section 7).

Land code through the repository's own rules; TermAl's Merger train is not part
of the pilot.

## 6. When something looks stuck

Read the session card first: TermAl writes one line for every hold, refusal or
unknown result of a command it recognised, with its cause; a command it did not
recognise as the declared test leaves no line at all (section 5). Then:

- A test shows UNKNOWN: read the reason on the card (section 4); do not rerun
  blindly.
- A prompt is retained or a card says a request is waiting: do nothing in that
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
in the Engram refusal line above); at I2 it also voids a credited check that
was not yet evaluated, and the supervisor re-names the worktree and reruns the
test.

Do not cancel a delegated child session mid-run. The one delegated session
the pilot uses is the acceptance evaluator child that
`termal_evaluate_acceptance` spawns (at I2 for every item, the supervisor
having set evaluated acceptance in section 3; at I1 only when an evaluation
is requested); the rule covers it — wait for its fan-in. Any other
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
  delete or replace the data files. A TermAl restart loses no Engram data; it
  voids in-flight host test credit and unevaluated checks, and running MCP
  processes switch to the newly installed build.
- Machine: agents create, change and delete files only inside the repository
  folder they work in. Anything outside it — installing software, changing
  user or global configuration — is the supervisor's act, never the agent's.
- Evidence: at I1 the test evidence is the agent's note of the TRX counters;
  TermAl records no check. At I2 a PASS counts only when it ran in the named
  worktree, in the agent's run form, in the foreground, unwrapped, with the
  declaration unchanged, no other command or edit in that worktree and no
  helper agent in the session while it ran; a missing result is UNKNOWN, never
  a pass.
- Excluded, repeated: submodules/LFS repos, the landing train, delegated
  children (except the acceptance evaluator child, section 6), cross-machine
  sync, policy or store changes, unplanned restarts.

## 8. Evidence before inviting the colleague

The supervisor ticks each box with its locator on the pilot's tracker item.
The decision to invite is Greg's; this list is what the decision rests on.

- [ ] The tier is decided and recorded in writing: I1 (the plan's tier) or I2
      by Greg's choice. Every box below is read at that tier.
- [ ] Greg's own run on the colleague's machine, following only this document,
      completed one task end to end: at I1, a declared test run with its TRX
      counters noted on the item; at I2, a credited declared test (this run
      is a checklist box, not a criterion of the plan item).
- [ ] Visible test-run registration is completed and is the running host build
      on the supervising machine.
- [ ] The pilot validation Greg directed on 2026-10-16, in place of any dry
      run on a fixture clone: once the relevant work is on master, one safe,
      independent P3 item of the Phoenix project is run through the normal
      runner, test and evaluation path by an agent session that never held
      the plan item. Before anything starts, Greg receives the proposed item,
      why it is isolated, the exact session and project route, and the one UI
      action (if any) he must take; nothing starts before his reply. The run
      is noted on the item: the declared test ran as an ordinary command and
      the TRX counters were noted (at I1 also: no check record exists, the
      document's statement verified). At I2 the credited part of this box is
      BLOCKED on Phoenix: its index holds gitlinks (submodules), and a
      repository with submodules has no source basis, so no declared run there
      is ever credited ([Engram host adapter](engram-host-adapter.md), "When
      there is none"); I2 credited evidence needs a supported repository or a
      separately approved proof route, recorded on the item as the blocker —
      it is not supplied by a fixture-clone dry run. A fixture clone may still
      serve the supervisor to qualify the declaration's form; that
      qualification does not tick this box.
- [ ] The declared dotnet/TRX live proof after install and restart is
      completed (I2 evidence; at I1 it shows the declaration is well formed).
- [ ] The Engram-side steps in section 3 are the ones Engram's coordinator
      provided, quoted with attribution and unchanged in substance, and the
      readiness check passes on the colleague's machine.
- [ ] The supervisor can name the two open operational workarounds (no
      cancelling delegated children mid-run; binary updates only by stop,
      replace, start) and the restart effects (claims lapse, unevaluated credit
      voids).
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
  tools; at I2, turn-gated control enabled and one worktree named.
- E2 — One real task end to end: add, claim, work, the declared test in the
  foreground (at I2 a host Test check on the card; at I1 the TRX counts noted
  on the item), note, done; plus one acceptance evaluation if the policy
  enables it.
- E3 — A planned-restart drill with one claimed item: the host is stopped and
  started on the same build ("host back"); at I2, the worktree is re-named and
  the voided passed-but-unevaluated check is seen, as section 6 documents.
- E4 — Backup: per Engram's sheet.
- E5 — An incident log, including every TermAl stuck state the colleague met,
  quoted from the UI card.
- E6 — After the agreed sessions, the colleague's two-line verdict; Greg
  decides the rollout.
