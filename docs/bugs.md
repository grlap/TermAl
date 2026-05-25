# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Restart delegated-child pruning can leave a blank workspace pane

**Severity:** Medium - backend restarts can restore a blank workspace even when
nondelegated sessions are available.

When restart recovery prunes delegated child session tabs,
`reconcileWorkspaceState` can remove every tab from an existing pane while still
keeping that pane in `panes` and `root`. Because the root remains present, the
fallback branch that creates an initial tab from `availableSessions` does not
run.

**Current behavior:**
- A restored pane containing only a delegated child session tab becomes an empty
  pane when restart pruning runs.
- If a parent or other nondelegated session exists, it is not opened as the
  fallback session because the empty pane keeps the workspace root alive.

**Proposal:**
- Drop panes that become empty during delegated-child pruning, or treat an
  all-empty workspace as needing fallback reconstruction.
- Add regression coverage for a single restored pane whose only tab is a
  delegated child while a nondelegated parent session is available.

## Wildcard agent dispatch in `delegation_child_missing_runtime_summary`

**Severity:** Low - incorrect per-agent label for stale Cursor/Gemini
delegations, and hidden handling for future agent variants.

`src/delegations.rs` matches on `child.session.agent` and uses a wildcard arm
for everything other than Claude and Codex. `Agent::Cursor` and `Agent::Gemini`
are first-class variants today and surface the generic label, while the existing
`Agent::name()` helper already produces the right per-agent string for all
variants.

**Current behavior:**
- A stale Cursor or Gemini delegation reports "Agent session exited before the
  active turn completed" in `DelegationResult.summary` and the queued fan-in
  prompt sent to the parent.
- Future `Agent` variants would silently inherit the generic label.

**Proposal:**
- Replace the match with `child.session.agent.name()` when building the missing
  runtime summary.
- Add a Rust test covering at least one non-Codex agent label.

## Implementation Tasks

- [ ] P2: Cover restart pruning when a delegated child was the only restored workspace tab:
  add workspace/live-state tests proving empty panes are removed or rebuilt from
  nondelegated sessions when pruning removes all tabs from a pane.
- [ ] P2: Cover result-bearing branches of the stale-runtime fallback:
  add Rust tests for `Active`/`Approval` children with `SessionRuntime::None`
  that also have a completed result message or failed result message, proving
  `delegation_child_outcome` uses the result packet instead of the
  missing-runtime sentinel.
- [ ] P2: Document the `reconcileSessions` identity coupling in the restart-prune test:
  add a short comment in `app-live-state.test.ts` explaining why
  `expect(sessionsRef.current).toBe(previousSessions)` is load-bearing for the
  unchanged-session pruning path.
- [ ] P2: Split delegation lifecycle wait/runtime tests out of the monolithic delegation test module:
  move stale-runtime fan-in and related terminal-refresh coverage into a focused
  delegation lifecycle test module.
- [ ] P2: Extract oversized frontend hot-path helpers:
  move JSON-first `/api/state` parsing into a focused API helper and virtualized transcript measurement/cache logic into focused helper or hook modules so the reviewed hot paths stop growing oversized frontend files.
- [ ] P2: Audit SessionPaneView scroll/signature derivations during store-backed updates:
  `AgentSessionPanel` now derives visible command/diff lists from the store-backed session snapshot, but `SessionPaneView` still computes scroll/signature bookkeeping from React-state `activeSession`; prove this cannot drift during eager store publication, or move the bookkeeping to the same store boundary.
- [ ] P2: Split Telegram settings persistence tests out of the monolithic Telegram test module:
  move the state-backed Telegram config persistence/status/delete-session/delete-project and post-validation resanitize coverage into a focused test module so new coverage does not keep growing `src/tests/telegram.rs`.
- [ ] P2: Split Telegram assistant-forwarding and digest regression tests out of the monolithic Telegram test module:
  move disabled-forwarding, active-baseline, chunk retry, and digest-forwarding coverage into focused test modules or helper-level tests so new relay coverage does not keep growing `src/tests/telegram.rs`.
- [ ] P2: Split Telegram relay runtime lifecycle tests out of the monolithic Telegram test module:
  move startup-from-saved-settings, fallback project selection, missing-config stop, config-save restart, and graceful-shutdown relay coverage into a focused runtime lifecycle test module.
