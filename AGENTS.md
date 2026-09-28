# Agent Instructions

## Engram work tracker

TermAl's work is tracked in Engram, project `github.com/grlap/TermAl`, which
`.engram-project` at the repository root binds. Beads was migrated on
2026-09-28 (679 items, verified with zero differences); the local `.beads`
directory, while it still exists, is read-only history — never run a `bd`
command that writes.

- Use the `engram` tools: `next` to see what is ready, `ls` and `show` to
  read, `claim` before any execution, `note` for findings and evidence,
  `done` to complete, `remember` / `memories` for persistent knowledge
  (never a `MEMORY.md` file or a TODO list).
- Migrated items carry their Beads id as `external_ref` and their Beads
  status in a provenance note. An item that entered with no acceptance
  criteria (label `acceptance-needed`) needs them before it can complete:
  the agent that will complete it may propose them, but someone other than
  that agent — the item's owner, the project's coordinator, or a reviewer
  — concurs on the exact wording before the evaluation is requested, in
  their own note or, for a read-only reviewer, in its own review result
  quoting that wording, which the holder cites by delegation id; the
  holder records the criteria with `update … revise`, and no evaluation is
  requested against criteria only the completing agent has worded.
- In a root session, name the git worktree you work in once per claim with
  `termal_name_source_root`, before your first edit for the item: naming
  takes effect at your next turn, and edits made in the naming turn are
  reported by neither turn. TermAl then measures that worktree and credits
  only tests that run there. A delegated session names nothing; it is
  measured in its own workdir.
- Run each test where TermAl can place it. A runtime that reports its
  working directory (Codex) runs the test as a command of its own with its
  working directory set to the directory in the worktree where it runs.
  Claude runs it through its Bash tool as one call,
  `pushd "DIR" && <test>`, with DIR the absolute directory in the worktree
  (on Windows written as a Windows path, `C:\…` or `C:/…`, not `/c/…`) and
  nothing piped, redirected or chained after the test. The form reads a
  plain DIR only (ASCII letters, digits, spaces and `_ . - /`, plus the
  drive and `\` on Windows, with no `.` or `..` step, no doubled or
  trailing backslash and no network path); for any other path the host's
  line names what to do instead. A test Claude runs through its
  PowerShell tool is not recorded. The host records the result as
  verification evidence on the turn's closing checkpoint.
- Do not commit or push without explicit authority from the user or the
  current instructions. Where this file has a commit-and-push section, that
  section says what counts; until it has one, this sentence is the rule.
- Acceptance is evaluated independently: request
  `termal_evaluate_acceptance` in the turn after the check, never in the
  same turn; `done` consumes that evaluation. A criterion bound to a test
  passes only on a host-recorded check with a passed result at the judged
  revision.
- Never put Engram or Beads ids in source comments, identifiers or
  user-facing text; ids belong in the tracker and in commit messages.
- Until they move to Engram, the `bd` steps in
  `.claude/commands/review-changes.md` and `.claude/commands/fix-bug.md`
  mean the matching `engram` act, and a Beads id they take names the
  migrated item whose `external_ref` it is: `bd create` is `add`;
  `bd comment` is `note`; a field change by `bd update` or `bd priority`
  is `update` with `revise`; `bd close` of work fixed here is `done` on a
  claim you hold, after its evaluation where it has criteria, and of work
  not done here (a false positive, a duplicate, or one already fixed
  elsewhere) `update` with `cancel` and a reason naming where it stands,
  or with `supersede` naming the replacement item and a reason. Never run
  them against the retired store. Beads context that a session-start hook
  or the `beads` skill still loads is superseded by this section.
- Problems with Engram itself go to the Engram project's agents by mailbox;
  problems with TermAl's recording go to the TermAl coordinator.

## Work only in the repository folder

Greg's rule (2026-09-26), after an agent's cleanup script deleted most of his
user profile (cause recorded in bead `tm-t1gh`). TermAl does not block these
operations, so the rule holds only as long as every agent follows it:

- Create, change, move and delete files only inside this repository's folder
  and its worktrees, including worktrees TermAl creates for delegated
  sessions. This covers every shell command and script an agent writes.
  Scratch files, throwaway stores, test homes and logs go under `.tmp/`
  (ignored by Git), never in the system temp folder, `C:\tmp`, the user
  profile or another project, even when a harness names a scratchpad there.
- Outside the repository only tools write, as part of their own work: TermAl
  through its tools and test runs, Engram to its store home, Cargo and npm to
  their caches. Anything else there, such as installing, repairing or updating
  software, toolchains or TermAl itself, or changing user or global
  configuration, needs Greg's explicit word first.
- Reading outside the repository is allowed.
- Before a recursive delete, resolve the target to an absolute path and check
  that it lies inside the repository. Never delete through a variable whose
  value you have not checked. In PowerShell never assign `$home` or any other
  automatic variable: variable names ignore case, `$HOME` is read-only, and a
  failed assignment keeps the old value while the script runs on.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

## Shared test launcher

Use `node scripts/test-launcher.mjs full` for the maintained five-stage gate and
`node scripts/test-launcher.mjs focused -- COMMAND ARG...` for an authorized
focused check. The launcher stores complete stdout/stderr and atomic terminal
results under Git metadata, while context receives only a bounded summary.
Detached runs require a different coordinator via `--notify`; after `STARTED`,
end the turn and wait for the mailbox wake—do not poll status or tail logs.
Use `summary RUN_DIRECTORY` or `notify RUN_DIRECTORY` to recover an existing
run without rerunning tests; a foreground run prints `RUN RUN_DIRECTORY` first,
and `recover RUN_DIRECTORY` settles a run whose launcher died without a result
(see `docs/test.md`). Pinned disposable Engram checks use `live` with an
absolute `--engram-binary` and `--engram-sha256`; never substitute an unpinned
binary. The launcher never installs, builds production UI, mutates `ui/dist`,
restarts a host, or changes live-store policy.

### Failed gate investigation

A failed gate is an investigation trigger, not permission to retry until green.
Preserve the original run and logs, state falsifiable hypotheses, use focused
discriminating diagnostics, and classify the cause as product, test/runner, or
environment/resource. Fix confirmed in-scope test or runner defects without
another user approval round trip. A later pass is validation of the fix; it is
not by itself a diagnosis or closure of the original failure. Never automate
retry/repair/reviewer loops, weaken or ignore tests, inflate timeouts, or change
product semantics to obtain green. Escalate when the evidence requires product
behavior changes, destructive or external actions, missing authority, or a
genuine blocker.
