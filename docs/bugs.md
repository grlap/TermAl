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
- [ ] P2: Add virtualized transcript estimate-cache coverage:
  cover `estimatedMessageHeightsRef` WeakMap cache hits plus width-bucket or expanded-prompt invalidation so the cache cannot return stale estimates for a changed rendering context.
- [ ] P2: Cover first-chunk Telegram forward failure:
  force the first chunk of a long assistant message to fail and assert bounded retry/escalation behavior instead of an endless replay loop.
- [ ] P2: Cover first-settled active-baseline same-message growth policy:
  pin the current conservative behavior and, if a future turn-boundary signal lands, add the positive forwarding case for same-message reply text already present on first settled poll.
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
- [ ] P2: Add assistant-reply forwarding disabled-path regressions:
  cover `sync_telegram_digest` and `select_telegram_project_session` with `forward_assistant_replies=false` so digest and selection paths cannot accidentally forward assistant replies.
- [ ] P2: Clarify pending queued-prompt cancel tooltip behavior:
  either restore/replace the removed `PendingPromptCard` `title` affordance or document the intentional aria-label-only behavior in the component/test coverage.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P2: Cover Telegram relay active-project reconciliation:
  start an in-process relay with subscribed projects but no default and assert startup fails or status exposes the effective `activeProjectId`; delete a project used by a running relay and assert the relay is stopped or restarted without the deleted id.
- [ ] P2: Cover remaining Telegram relay runtime lifecycle paths:
  use the AppState-owned test relay runtime to cover startup from saved settings, implicit first subscribed-project fallback, invalid/missing config stop, config-save restart, and graceful-shutdown stop.
