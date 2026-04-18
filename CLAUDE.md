# TermAl — Standing Instructions For Claude

Read this before making changes in this repository. These rules override
anything implied by my default behaviour or by the Claude-Code skills
(`review-local`, etc.).

## Never commit or push without explicit permission

- `git add`, `git diff`, `git status`, `git stash`, `git log`, `git show`
  — all fine to run freely.
- `git commit` and `git push` — **ask first, every time**. Do not batch
  changes into a commit on my behalf. Do not amend existing commits
  without asking. Do not rebase or force-push under any circumstances.
- The phrase "commit" or "ship" in a user message is an explicit
  approval. Ambiguity is not. If I'm not sure, I ask.
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
- `docs/bugs.md` is the active-bug ledger + implementation-task
  backlog. Finished work moves to the "also fixed in the current
  tree" preamble; new findings land as new sections.

## Review cadence

- The `review-local` skill can be invoked to run the review lenses
  against staged / unstaged changes. It never commits — reviewers
  read, I present, the user decides. See the skill definition for
  the full loop.
