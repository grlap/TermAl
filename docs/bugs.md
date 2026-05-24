# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Implementation Tasks

- [ ] P2: Extract oversized frontend hot-path helpers:
  move JSON-first `/api/state` parsing into a focused API helper and virtualized transcript measurement/cache logic into focused helper or hook modules so the reviewed hot paths stop growing oversized frontend files.
- [ ] P2: Add Code Navigation MCP reverse feature links:
  link `code-navigation-mcp.md` back from the referenced feature briefs (`agent-delegation-sessions`, `instruction-debugger`, `file-change-awareness`, and `source-renderers`) if the new brief remains in the tree.
- [ ] P2: Add Code Navigation MCP filesystem safety constraints:
  update `docs/features/code-navigation-mcp.md` to define canonical workspace-root confinement, symlink escape handling, ignored secret/build paths, and hard file/snippet size limits before implementing read-backed MCP tools.
- [ ] P2: Add Telegram settings API/security regressions:
  cover plaintext token-at-rest exposure, corrupt-backup permission hardening, and credential-store failure/fallback behavior beyond the native-store smoke test.
- [ ] P2: Cover post-validation Telegram settings sanitization:
  delete a project/session after validation but before the second sanitize path, or extract a deterministic helper seam, and assert the persisted response cannot retain stale references. The current stale-reference test at `src/tests/telegram.rs:1573` seeds invalid state before validation, so removing the post-validation sanitize in `src/telegram_settings.rs:73` would still pass.
- [ ] P2: Add Telegram preferences panel RTL coverage:
  cover API error display, stale default-session clearing, default-project auto-subscription, `inProcess` running/stopped lifecycle labels including stopped-over-linked precedence, AppDialogs Telegram tab path, and StrictMode-mounted save/test/remove flows proving post-await UI updates still land.
- [ ] P2: Audit SessionPaneView scroll/signature derivations during store-backed updates:
  `AgentSessionPanel` now derives visible command/diff lists from the store-backed session snapshot, but `SessionPaneView` still computes scroll/signature bookkeeping from React-state `activeSession`; prove this cannot drift during eager store publication, or move the bookkeeping to the same store boundary.
- [ ] P2: Split Telegram settings persistence tests out of the monolithic Telegram test module:
  move the state-backed Telegram config persistence/status/delete-session/delete-project coverage into a focused test module so new coverage does not keep growing `src/tests/telegram.rs`.
- [ ] P2: Split Telegram assistant-forwarding and digest regression tests out of the monolithic Telegram test module:
  move disabled-forwarding, active-baseline, chunk retry, and digest-forwarding coverage into focused test modules or helper-level tests so new relay coverage does not keep growing `src/tests/telegram.rs`.
- [ ] P2: Cover remaining Telegram relay runtime lifecycle paths:
  use the AppState-owned test relay runtime to cover startup from saved settings, implicit first subscribed-project fallback, invalid/missing config stop, config-save restart, and graceful-shutdown stop.
