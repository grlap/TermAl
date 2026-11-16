# Delegation worktrees

An `isolatedWorktree` delegation runs its child in a Git worktree of its own.
This brief says where that worktree lives and when it goes. The rest of the
delegation contract (modes, write policies, results, waits) is in
[agent delegation sessions](./agent-delegation-sessions.md).

## Where it lives

The worktree lives inside the source repository, in a directory Git ignores,
together with its build output and test data. Nothing is created under the
TermAl data directory (`~/.termal`).

- With no `worktreePath`, the worktree is
  `<source repository root>/.worktrees/delegation-<delegation id>`. The path is
  relative to the repository root, so it is the same on Windows, Linux and
  macOS.
- An explicit `worktreePath` is accepted only strictly inside the source
  repository and outside its `.git`. A path anywhere else is refused with a
  reason that names `.worktrees/`. There is no exception directory outside the
  repository.
- Either way the location must be ignored by Git (`git check-ignore`). If it is
  not, for example because the repository's `.gitignore` has no `.worktrees/`
  entry, the spawn is refused with a reason saying so. TermAl never edits the
  repository's ignore rules and never falls back to a location outside it.
  TermAl's own `.gitignore` ignores `/.worktrees/`.

Because the location is ignored, the parent's `git status`, its dirty-state
capture for the next spawn, the review freeze and the test fingerprint do not
see the nested worktree. The file watcher drops its events for the parent's
root and keeps them for the child's own root. Instruction search does not
descend into `.worktrees/`, so a delegation's copy of `AGENTS.md` or
`CLAUDE.md` is never reported as the workdir's own.

## When it goes

A sweep reconciles worktrees with the delegation records. It is event-driven
only, run on a background thread when something happens: at boot, which
recovers whatever a restart left behind; after each delegation ends or its
child's runtime is detached; and when a delegation child's closing source
observation settles. Requests that arrive during a pass coalesce into one more
pass, and nothing runs while nothing has happened. Each pass looks at every
ended (completed, failed or canceled) isolated delegation whose worktree is
still there. Nothing is queued in memory, so a pass that has to wait leaves
the worktree for a later one, and a missed request is caught at the next boot.

The sweep takes a worktree only while, in the same lock that admits a
follow-up, the delegation is still ended, no follow-up is being admitted, the
child's runtime is gone and its shutdown has finished, and no closing source
observation of the child still reads the worktree. It then decides, off the
state lock, whether the worktree holds anything to lose. It never relies on
the child's own report.

- The worktree is removed when `git status --porcelain --untracked-files=all`
  in it is empty, its `HEAD` is the delegation's recorded base or is reachable
  from a branch or tag of the repository, and no other worktree is registered
  inside it. TermAl runs `git worktree remove` (without `--force`) against the
  source repository, which also deletes ignored build output such as
  `target/`.
- Otherwise the worktree is retained: uncommitted or untracked changes, a
  commit no ref holds, a nested worktree (a nested delegation's worktree sits
  in this one's ignored `.worktrees/`, invisible to its status), or a Git
  refusal. A child whose closing source observation can never be delivered
  also keeps its worktree, since that observation still reads it. The
  delegation result gets one bounded note naming the worktree and the reason,
  and the sweep does not inspect it again unless a follow-up starts a new
  attempt. Removing a retained worktree is the owner's act, after
  integrating or discarding its work.
- Only a linked worktree at exactly the recorded path, inside the repository
  that contains it, is ever acted on. Worktrees created before this rule,
  outside their repository, are left where they are.

A follow-up needs the worktree. While the sweep holds it, the follow-up is
refused with a reason to retry shortly; once a clean worktree has been
removed, the follow-up is refused for good and the worktree is not recreated:
start a new delegation instead. A retained worktree keeps follow-ups
available.

## Tests

Under test, TermAl's data directory never resolves to the user's real
`~/.termal`; it resolves inside the test's own temporary tree. The default
location is covered by a test run in a child process whose home is a sentinel
directory the test owns, and which must stay empty. Test states only count
sweep requests and run the same sweep pass directly, so no pass outlives its
fixture.
