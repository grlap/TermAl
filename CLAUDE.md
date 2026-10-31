# TermAl — Standing Instructions For Claude

Read this before making changes in this repository. These rules override
anything implied by my default behaviour or by the Claude-Code skills
(`review-code`, etc.).

## Commit and push

- `git add`, `git diff`, `git status`, `git log`, `git show`, and
  `git stash` in the owned-path form below — all fine to run freely,
  within the session's write policy.
  The stash list is shared by all worktrees of this repository, including
  dedicated worktrees. In every worktree, stash only owned paths with
  `git stash push -u -m <unique-tag> -- <owned-paths>`. Resolve the saved
  object id by its unique tag in `git stash list --format='%H %gs'`, never
  assume `stash@{0}` is yours. Restore with `git stash apply <object-id>`
  after checking those paths. After verifying the restore, drop the entry
  only with coordinated exclusive access to the shared stash list granted
  by the task coordinator (as of 2026-09-27: Termal::Fable2) by mailbox for
  a named entry: match its tag and object id to its current stash reference
  before dropping it.
  Otherwise retain it for coordinated cleanup. Never use bare `git stash`
  or `git stash pop`, or capture, restore or drop another session's work.

Greg's standing rule for both Engram and TermAl (2026-09-27, 19:15Z,
in Engram::Advisor's session, recorded verbatim):

> regula jest taka sama dla Engram i Termal
>
> jezeli review nie ma uwag i testy sa green,
>
> taski maja evidence ze mozna je zamknac
>
> to robimy commit

- A changeset may be landed (committed on master, or master fast-forwarded
  to a candidate commit) without a further permission round trip only when
  all three hold for the exact commit being landed (for the staged-tree
  lane, the exact tree being committed):
  1. The review pair (`/review-changes`: two independent read-only
     `/review-code` children of different vendors, one Codex and one
     Claude, `writePolicy: readOnly`; while Codex or Claude is unavailable —
     a usage limit or outage met in this round, recorded on the item with
     the refusal text — Kimi stands in for the unavailable reviewer, and
     Kimi never stands in for both reviewers of the same round, under
     Greg's word to Termal::Opus2 of
     2026-09-28, 21:20Z, 'let's go with 3. we could try how the new model
     is working' (option 3 being Kimi as the
     second reviewer while Codex is out), and to Engram::Opus of
     2026-09-29, 'if clean you have a go', 'you should just check-in, that
     should be the rule', and, for an unavailable Claude, his word in the
     acting Engram::Advisor session of 2026-10-14/15 (Pacific), relayed
     verbatim by Engram::Advisor in TermAl mailbox b9d13887 #805, line
     breaks shown as ' / ': 'claude will soon run of tokens / we need to
     use Kimi as replacement / and focus on simple cleanup / we have
     enormous backlog') has no outstanding in-scope finding of Medium or
     higher on the reviewed input, and every in-scope finding of Medium or
     higher from earlier rounds was fixed and reviewed again; a Low or a
     Note need not be fixed before the landing, and a Low left unfixed is
     filed as its own item (Greg, 2026-09-30, in Engram::Opus's session:
     'ignore Notes and trivial lows, that can be handled later. if they choose to fix low, fine, but
     that is not must have', 'anything including Medium must be fixed').
     Pre-existing defects outside
     the change's scope, filed as their own items with provenance, do not
     block. Engram's amendment of the same rule is its commit fc3c3aa; the
     two coordinators decided on 2026-09-29 to narrow both projects' rule
     to an unavailable Codex, and Greg's word of 2026-10-15 above widened
     it to an unavailable Codex or Claude.
  2. The full gate is green on exactly that input, compiled from it, before
     every landing whether or not a commit prompt is shown.
  3. The tasks carry the evidence needed to close them. A criterion that
     only the landing itself can satisfy, namely that the change is
     committed, pushed, on master or installed, counts as carried when
     conditions 1 and 2 hold and an independent acceptance evaluation of the
     frozen tree found every other criterion, and every other part of that
     criterion, met; a criterion that needs the build running, a restart, or
     a live or remote observation is not one of these. The landing then
     makes it true, and the item completes only after a post-landing
     evaluation passes it.
- Greg's earlier wording of the same rule (2026-09-12, to Termal::Fable):
  "jak review jest czyste mozna commit push sync"; and on 2026-09-25, to
  Termal::Opus: "git commit means also push and dolt push". A commit under
  this rule is a landing: `git push origin master` follows it, subject to
  the integration and migration restrictions below.
- Candidate commits (Greg, 2026-10-31: 'zatwierdzam opcje B', 'idzmy na
  prostsa opcja B / zatwierdzam'). So that focused checks, reviews and the
  full gate run on a clean commit, a writable session may commit its change
  on its own feature branch or detached worktree, never on master and never
  pushed. A candidate commit is not a landing and grants no push, install or
  restart. The final full gate runs on a clean checkout of a single
  candidate commit whose parent is the current master. The integration
  owner lands it only by fast-forwarding master to exactly that gated
  commit, when the standing rule's three conditions above hold. If master has
  moved, the change is committed afresh as a new candidate whose parent is
  the current master, and gated again. A candidate that fails is superseded
  by a new candidate commit, never amended, rebased or force-pushed. Each
  new candidate is committed on a new branch or a detached HEAD at the
  current master, and the earlier candidate is left as it is. The
  staged-tree lane remains available.
- A commit in a feature branch or detached worktree is not yet on `master`.
  Hand its hash and exact-input gate, review and task evidence to the
  integration owner; do not run `git push origin master` from that worktree.
- Only the integration owner performs pushes under this rule. Every other
  committing session, including one committing directly on master, sends
  the commit hash and exact-input gate, review and task evidence to that
  owner by TermAl mailbox.
- Before every push under this rule, including for a commit made directly
  on master, the integration owner must verify that master's tip is the
  reviewed and gated commit, that `origin/master` still matches the reviewed
  integration base, and that the push is a fast-forward. For a change that
  conditions (a), (b) and (c) below cover, the integration owner also
  verifies that the final-diff audit recorded on the item passed and that
  it names the input fingerprint of the reviewed and gated input. If any
  check fails, do not push: coordinate integration and validate and review
  the resulting input before publishing. Push from the master checkout,
  then verify that the remote contains that commit. Never rebase or
  force-push to bypass these checks.
- The integration owner is the session assigned to integrate the change
  onto master (as of 2026-09-27: Termal::Opus2); send the handoff by TermAl
  mailbox. If no owner has been assigned, request the assignment from the
  task coordinator (as of 2026-09-27: Termal::Fable2) before integration.
- The tracker is Engram; agents run no bd command. Beads was retired on
  2026-09-28 after a verified import and removed from the repository on
  2026-10-03 on Greg's word ('Nie jest potrzebny. Możemy Usunąc beads
  completnie.'); its history is kept in the imported Engram items.
- Changes to this section, or to any repository instruction or command
  file that grants or limits commit, push, tracker or approval authority,
  land under the standing rule only after (a) both projects' coordinators
  (as of 2026-09-28: Engram::Fable and Termal::Fable2) have recorded their
  concurrence on the exact wording in the item's notes, (b) Greg has been
  sent the exact wording and its consequence before the landing, and (c)
  the final-diff audit that `/review-changes` requires is recorded on the
  item as passed before the landing; an objection from Greg, by any route,
  stops it. An edit that widens agent authority — a new act granted, or a
  condition of this rule loosened — also needs Greg's recorded word on that
  widening, verbatim with its source, by any route; recording his words,
  narrowing, or clarifying needs only (a), (b) and (c). No other approval
  is asked.
- Outside the standing rule's three conditions above and the
  candidate-commits bullet, a commit or push needs Greg's own word in the
  acting session. Outside this standing rule and the candidate-commits
  bullet, a word relayed by another session never carries a commit or push.
  This rule grants no restart, deploy or global-configuration authority; those
  acts are Greg's and need his explicit word. The moment of a TermAl
  restart is the agents' decision (Greg, 2026-10-02: 'Wolę aby agenci
  zdecydowali na moment restaru Termal. Wtedy kiedy potrzebuja poprawek. To
  nie jest problem, nie chce przerywać pracy.'): when running sessions need
  a landed fix, the Engram and TermAl coordinators agree on a moment when
  running work can resume, and the TermAl coordinator sends Greg, through
  Engram::Advisor, 'restart now' with the build hash and reason. Greg
  performs the restart; no agent stops or starts the TermAl host. This
  covers the timing of TermAl restarts only. A landing under this rule
  also installs the
  binary built from the exact gated tree (Greg, 2026-09-23, recorded in
  Engram's instructions and extended to TermAl on 2026-09-27: his word
  "commit" for a presented changeset also authorizes pushing it and
  installing its build; and 2026-09-28, on the install/restart split:
  'Fable ma racje to dobra regula. W sumie mamy system kontroli. Agenci
  moga podejnowac takie decyzje.'): the integration owner, outside any
  gate window, puts it at target/release/termal.exe in the master checkout,
  where the host runs it from, renaming the running binary aside as a
  backup, and records its hash. Installing is not deploying: the running
  host keeps its build until Greg restarts TermAl at the moment the
  coordinators choose (as above); the landing report says whether running
  sessions need the new build, with its build hash and reason. Nothing is
  installed outside the repository.
- Test refs for remote runs (Greg, 2026-10-19: "tak, Opus moze wypychac"; a
  test ref is a merge-ready patch, not scratch; Mac and Windows gates carry
  equal weight). As an exception to the previous bullet, to give a remote
  runner an exact input, the integration owner (as of 2026-10-19:
  Termal::Opus2) may commit a merge-ready input tree and push it as
  `refs/heads/test/<item>-<fingerprint8>`, where `<item>` is the item's
  short work ref and `<fingerprint8>` is the first eight hex digits of the
  frozen input's review-freeze fingerprint. Only a merge-ready input is
  pushed: every required review is complete with no outstanding
  Medium-or-higher finding, and a fresh focused compile and tests covering
  its changes and new tests have passed on that exact input. Before
  committing, the integration owner (as of 2026-10-19: Termal::Opus2)
  records on the item, beside the freeze fingerprint and while that
  fingerprint still verifies, the tree id of the complete frozen input,
  including any untracked files it contains. The commit's parent is the
  master commit the input was frozen on, its tree is that recorded tree id,
  and its message names the fingerprint and nothing else. The commit is
  created detached, and no local branch adopts it. The integration owner (as
  of 2026-10-19: Termal::Opus2) pushes only that commit, to origin, with the
  explicit refspec `<commit>:refs/heads/test/<item>-<fingerprint8>`: never
  master or another branch, another remote, a force-push, rebase or merge,
  and never a ref whose tree differs from the recorded tree id. The runner
  fetches the ref into its own clone, checks the commit out in a fresh
  worktree whose index and tracked files equal that commit and which holds
  no untracked files, records that state, runs the standard launcher, and
  requires that the commit's tree id equals the tree id recorded before the
  push and that its own review-freeze fingerprint is the same before and
  after its run. The runner pushes nothing back; it returns its evidence
  through the existing channels, never through Git. A focused run on a test
  ref is evidence only. A full gate run by the standard launcher on the
  exact test ref counts as the landing gate for that tree on any host, with
  the same weight as one run on Windows, when its results record the same
  fingerprint before and after and the integration owner has
  recorded on the item the run id, host, commit, tree id and the
  launcher's before and after fingerprints from that run's results;
  it gates only a landing whose master commit has that
  tree id, and until then it is evidence only. A change that touches
  Windows-only code or tests also needs the relevant gate on Windows. In
  this remote flow, reviews finish before the push and the gate runs on the
  pushed ref, so review and gate never run in parallel on an unpublished
  candidate; a failed remote gate sends the input back to its owner, like
  any failed gate. Landing still follows the standing approval on master,
  including installing a binary built from that exact tree. The integration
  owner (as of 2026-10-19: Termal::Opus2) deletes the ref once the run's
  evidence is recorded, and at the latest when the input's items close, and
  records the push and the deletion (commit id, tree id, fingerprint, times)
  on the item. This grants no other commit, push, install or restart.
- S3 pilot lane (Greg, 2026-10-20, option (c)): a TermAl session
  assigned as a pilot host's lane may, as an exception to the
  work-only-in-the-repository rule, write only under the Engram
  checkout's `target/s3-pilot/` on that host and run Git there exactly
  as Engram's S3 pilot bullet allows; nothing else outside this
  repository. This grants no other commit, push, install or restart.
- Use explicit paths only: never `git add -A` or `git add -u`.
- Do not amend, rebase or force-push. Commit message bodies (below the
  subject line) must not contain tracker ids (Engram ids, or Beads ids
  kept as `external_ref`); subject lines may contain them. Review-only
  sessions never commit.
- This standing rule is Greg's explicit authorization for commit and push
  when its three conditions hold. This entire section takes precedence over
  all conflicting repository instructions, including approval, review
  thresholds, stash ownership and explicit staging paths, and tracker
  writes. This includes other sections of AGENTS.md and CLAUDE.md,
  `.claude/commands/review-changes.md` and
  `.claude/commands/fix-bug.md`. Generic ask-first wording does not require
  another approval when these conditions hold; an Engram item completes
  only by `done` after the acceptance evaluation its project policy
  requires (Engram work tracker section).
  Task-specific restrictions given in the session still apply. Review-only
  sessions remain read-only. The Work only in the repository folder rule
  is not overridden.

## Work only in the repository folder

Greg's rule (2026-09-26), after an agent's cleanup script deleted most of his
user profile (cause recorded in bead `tm-t1gh`). TermAl does not block these
operations, so the rule holds only as long as every agent follows it:

- Create, change, move and delete files only inside this repository's folder
  and its worktrees, including worktrees TermAl creates for delegated
  sessions. This covers every shell command and script an agent writes.
  Scratch files, throwaway stores and test homes go under `.tmp/`; logs an
  agent keeps go under `.logs/`, and raw evidence that must outlive a
  worktree under `.evidence/`, both at the main checkout's root. Git ignores
  all three. Nothing goes in the system temp folder, `C:\tmp`, the user
  profile or another project, even when a harness names a scratchpad there.
- Greg's rule (2026-10-06, relayed by Engram::Advisor): 'use Git-ignored
  .logs/ and .evidence/ at the root of each project repository. Remove
  artifacts after the associated task closes, unless agents record a
  concrete need for a different retention policy.' A file there is removed
  once every task it serves has closed. The agent that closes the last such
  task removes it, or the coordinator's regular sweep does, in either case
  after the path check below, and records what it removed. To keep it
  longer, an agent records on an open task the concrete need and when it
  ends; the file goes when that need ends. Removal touches only `.logs/` and
  `.evidence/`, never a live worktree's run files, which leave only when
  that worktree is removed. Engram's notes, gates and seals stay the lasting
  record: what a decision rests on is quoted there, never left only in a raw
  file.
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

## Working on the UI

- The UI source lives under `ui/src/`. Keep new modules small and
  focused — the project has a few very large files already and we are
  actively splitting them smaller, not larger.
- When splitting a file, add a header comment at the top of each new
  file explaining: (1) what it owns, (2) what it deliberately does
  not own, (3) the file it was split out of. This keeps the
  provenance legible for later readers.
- Preserve public behaviour exactly during refactors. A split commit
  should be a pure code move: no renames, no signature changes, no
  new imports beyond what the move requires. Feature changes land in
  their own commits.
- Keep `cd ui && npx tsc --noEmit` clean. Keep `cd ui && npx vitest
  run` green before any commit prompt. There is no acceptable "flaky
  test" category: nondeterminism means the test is poorly written,
  its assumptions are wrong, the runner violates its resource
  contract, or the product has a defect. Diagnose and fix the cause;
  do not hide it with retries, quarantine, or larger timeouts.

## Working on the Rust backend

- `cargo check` clean. `cargo test --bin termal` green or explained.
- Respect the project conventions captured in `.claude/reviewers/rust.md`.

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

## Documentation

- Feature briefs live in `docs/features/*.md`. Cross-link them both
  ways when a new doc references an existing one.
- Bugs and implementation tasks are tracked in Engram, not a markdown
  file — see the Engram work tracker section below. `docs/bugs.md` has
  been retired.

## Review cadence

- Invoke `/review-changes` directly in the active writable session. It owns
  quality gates and obtains the review pair, at most two reviews that count,
  from read-only `/review-code` children of different vendors — one Codex
  and one Claude, with at most one Kimi stand-in, for an unavailable Codex or
  Claude, as the Commit and push section describes, so a round makes at most three
  spawns — each with `writePolicy: readOnly`. A round that ends with fewer
  than two reviews does not satisfy the commit rule's review pair.
- `/review-code` is the read-only, non-nesting leaf. It inspects staged,
  unstaged, and untracked changes through every reviewer lens and never runs
  quality gates, edits files, or writes to the tracker.

## Engram work tracker

TermAl's work is tracked in Engram, project `github.com/grlap/TermAl`, which
`.engram-project` at the repository root binds. Its history includes 679
items migrated from Beads on 2026-09-28, verified with zero differences;
Beads itself was removed from the repository on 2026-10-03.

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
  A full gate outlasts a Claude tool call's 10-minute cap: in a root session
  measured in a named source root, launch it in the same form,
  `pushd "DIR" && node scripts/test-launcher.mjs full`, in the Bash tool's
  background mode (or with `--detach` from a runtime that can
  notify a coordinator), then end the turn. The host carries it and
  records its result, passed or failed, on a later checkpoint of the same
  claim. While the run is going, any command the session runs in that
  worktree refuses the credit, except, in a Claude session, one that only
  reads (such as `git status`, `git diff`, or
  `node scripts/test-launcher.mjs summary RUN_DIRECTORY` from the repository
  root); so do a file edit the session reports, another writable session's
  turn or command there, a file change the host's watcher sees there, and a
  source change still there at that checkpoint. An evaluation requested
  before the record lands goes stale when it lands (docs/test.md).
- A repository may declare its test commands in `termal-tests.toml` at its
  root (docs/features/declared-test-commands.md). Run a declared command
  exactly as declared, from its `cwd`, in the same one-call form; the host
  judges it from the result artifact the command writes, never from output;
  a passing result is eligible for verification, and UNKNOWN is not PASS: it
  names what was observed. This needs a running TermAl build that implements
  declared commands. Until the running host implements it, the declaration
  adds no recognition and built-in commands behave as today.
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
- Problems with Engram itself go to the Engram project's agents by mailbox;
  problems with TermAl's recording go to the TermAl coordinator.
