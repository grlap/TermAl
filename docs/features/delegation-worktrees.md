# Delegation worktrees

An `isolatedWorktree` delegation runs its child in a Git worktree of its own.
This brief says where that worktree lives and what happens to it after the
delegation ends. The rest of the delegation contract (modes, write policies,
results, waits) is in [agent delegation sessions](./agent-delegation-sessions.md).

## Where it lives

The worktree lives inside the source repository, in a directory Git ignores,
together with its build output and test data. Nothing is created under the
TermAl data directory (`~/.termal`).

- With no `worktreePath`, the worktree is
  `<source repository root>/.worktrees/d-<first twelve hex digits of the
  delegation id>`. The path is relative to the repository root, so it is the
  same on Windows, Linux and macOS. The name is short because Git names the
  worktree's administrative directory (`.git/worktrees/<name>`) after it too.
- On Windows, Git refuses a `$GIT_DIR` longer than 220 characters (its
  PATH_MAX of 260 less 40), failing with "'$GIT_DIR' too big". Before it runs
  `git worktree add`, TermAl checks the longer of the two paths Git uses: the
  worktree's `.git` file and the administrative directory Git will create
  (`<common dir>/worktrees/<name>`, or `<name>1`, `<name>2`, ... when that name
  is taken). Over the limit, the spawn is refused with a reason that says the
  path is too deep for Git on Windows and names the path and its length.
  Nothing is created. The fix is a shorter repository path or a shorter
  explicit `worktreePath`.
- An explicit `worktreePath` is accepted only strictly inside the source
  repository, outside its `.git`, in a directory Git ignores (`.worktrees/` or
  any other). A path anywhere else, the repository root itself, or a path that
  resolves outside the repository through a link is refused with a reason
  saying so. There is no exception directory outside the repository.
- Either way the location must be ignored by Git (`git check-ignore`). If it is
  not, for example because the repository's `.gitignore` has no `.worktrees/`
  entry, the spawn is refused with a reason saying so. TermAl never edits the
  repository's ignore rules and never falls back to a location outside it.
  TermAl's own `.gitignore` ignores `/.worktrees/`.

Because the location is ignored, the parent's `git status`, its dirty-state
capture for the next spawn and its Engram source identity exclude the nested
worktree. The file watcher drops its events for the parent's
root and keeps them for the child's own root. Instruction search does not
descend into `.worktrees/`, so a delegation's copy of `AGENTS.md` or
`CLAUDE.md` is never reported as the workdir's own.

A nested linked worktree is a Git tree of its own, not part of the repository
that contains it. (A submodule or a nested repository is not a linked
worktree and stays part of the repository's tree.) While a read-only delegation reviews the repository, writes
through TermAl and manual approvals in a nested worktree are not blocked by
that review, and a write there is not attributed to the containing project.
Writes in the repository's own tree stay blocked as before.

## After the delegation ends

TermAl does not yet remove a delegation's worktree when the delegation ends.
The worktree, with its build output and any work in it, stays under the
source repository's `.worktrees/`, and a follow-up runs in it as before. The
spawning parent removes it with `git worktree remove` once it has integrated
or discarded the work; the worker handoff says so. A worktree lives inside the
parent's checkout, so removing that checkout, or `git clean -X` in it, also
deletes the worktree: integrate its work first. Automatic removal of a
worktree with nothing to lose is a separate, later change; until then a
worktree is removed by hand, by an agent under the "Cleanup deletions" rule in
AGENTS.md.

TermAl records, in the worktree's Git administrative directory, which
delegation created it. A follow-up runs only in that worktree: once it has
been removed, or its path holds anything else (a worktree another delegation
or a person made, a plain directory, a partly removed worktree whose Git link
is broken, or a worktree from before owners were recorded), the follow-up is
refused and the worktree is not recreated; start a new delegation instead.
The check reads Git's metadata only as regular files and never judges what a
HEAD names, so any branch name Git accepts passes. A repository whose HEAD is
stored as a symbolic link (`core.preferSymlinkRefs`) is refused as well: a link
could lead the check outside the worktree's own metadata.

## Tests

Under test, TermAl's data directory never resolves to the user's real
`~/.termal`; it resolves inside the test's own temporary tree. The default
location is covered by a test run in a child process whose home is a sentinel
directory the test owns, and which must stay empty. On Windows, cases build
the fixture repository at an exact path length: at 189 characters the default
worktree's `.git` file is exactly 220 characters and the worktree is created,
at 190 the spawn is refused with the reason, and a second worktree whose
administrative name is taken (`x1` instead of `x`) is refused at 221.
