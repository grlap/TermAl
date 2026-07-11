# TermAl — Standing Instructions For Claude

Read this before making changes in this repository. These rules override
anything implied by my default behaviour or by the Claude-Code skills
(`review-local`, etc.).

## Never commit or push without explicit permission

- `git add`, `git diff`, `git status`, `git stash`, `git log`, `git show`
  — all fine to run freely.
- `git commit`, `git push`, or any other "check in" operation —
  **ask first, every time**. "Check in", "commit", "ship", "land",
  "publish", "push it up" — all of these are the same restricted
  operation and need explicit approval before I run them.
- Do not batch changes into a commit on my behalf. Do not amend
  existing commits without asking. Do not rebase or force-push under
  any circumstances.
- **Explicit approval looks like this**: the user's message contains
  "commit", "push", "ship", "check in", "land it", or a direct "yes"
  to a commit-prompt I sent first. Nothing else counts — not "looks
  good", not "that works", not "thanks", not the fact that I just
  finished a nice clean fix. When in doubt, I do not commit. When
  in doubt, I ask: "Commit now, or keep iterating?"
- Do NOT auto-commit when I wrap up a fix, even if tests are green
  and the diff is trivial. Do NOT auto-commit because "the user will
  probably want this." The cadence is the user's to control.
- After I stage files, I pause and wait. After I run tests green, I
  pause and wait. The final `git commit` is its own explicit step
  that requires its own explicit approval.
- This rule applies even when the work is trivially small, tests are
  green, and the change is "obviously safe". The point is the user
  controls the commit cadence, not me.

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
  run` green before any commit prompt. If a test was already flaky
  before the change, note it explicitly.

## Working on the Rust backend

- `cargo check` clean. `cargo test --bin termal` green or explained.
- Respect the project conventions captured in `.claude/reviewers/rust.md`.

## Documentation

- Feature briefs live in `docs/features/*.md`. Cross-link them both
  ways when a new doc references an existing one.
- Bugs and implementation tasks are tracked in **beads** (`bd`), not a
  markdown file — see the Beads Issue Tracker section below. Use `bd
  ready` / `bd list` to see open work, `bd show <id>` for detail, and
  `bd create` to file new issues. `docs/bugs.md` has been retired.

## Review cadence

- The `review-local` skill can be invoked to run the review lenses
  against staged / unstaged changes. It never commits — reviewers
  read, I present, the user decides. See the skill definition for
  the full loop.


<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->
