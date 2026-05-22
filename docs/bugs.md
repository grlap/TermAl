# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Focused live-session responsiveness remains pending re-profile after state-adoption cuts

**Severity:** Medium - previous focused-session profiles showed user-visible main-thread churn, and the current mitigation batch has not yet been re-profiled against the same live active-session path.

Recent changes cut several suspected hot paths: unchanged session adoption can early-return, `/api/state` now uses a JSON-first response path, and transcript virtualization caches more estimate work. Those changes are useful, but they do not prove the original profile is resolved. The active acceptance gate is still a fresh profile of a visible active Codex session.

**Current behavior:**
- The last reproduced focused active-session profile showed long main-thread tasks while Codex was active.
- The current diff reduces state-adoption, JSON parsing, and transcript-estimation work, but no new profile has verified `TaskDuration`, next-frame latency, or `[TermAl perf] slow state event ...` output.
- Closing this bug before the re-profile would make the new P2 task the only record of a still-unverified user-visible performance issue.

**Proposal:**
- Keep this issue active until `scripts/perf/prompt-responsiveness-smoke.js` is rerun against a visible active Codex session.
- Close it only if the focused profile shows long-task bursts have dropped below user-visible jank thresholds.
- If the profile still fails, use the slow-state-event phase timings to split the next concrete cut.

## Implementation Tasks

- [ ] P2: Re-profile focused live-session responsiveness after state-adoption cuts:
  rerun `scripts/perf/prompt-responsiveness-smoke.js` against a visible active Codex session; close this bug if it passes, or refine the active bug if `TaskDuration`, next-frame latency, or `[TermAl perf] slow state event ...` output still points at state adoption or transcript measurement.
- [ ] P2: Extract oversized frontend hot-path helpers:
  move JSON-first `/api/state` parsing into a focused API helper and virtualized transcript measurement/cache logic into focused helper or hook modules so the reviewed hot paths stop growing oversized frontend files.
- [ ] P2: Add Telegram migration-marker write-path coverage:
  assert the `configMigratedToAppState` marker is persisted after legacy import, and cover `update_telegram_config()` plus prune/helper paths so stale mirrored file config cannot become the source of truth again.
- [ ] P2: Add app-state adoption no-op coverage:
  cover the `adoptSessions` early-return path when reconciled sessions are unchanged and no pending-open recovery exists, proving broad workspace/session updates are skipped.
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
- [ ] P2: Add Diffs-view session-store alignment coverage:
  mirror the command-update eager session-store regression for a diff-bearing update path, proving Diffs view props stay aligned with store-backed session slices before the broad React session render flush.
- [ ] P2: Tighten session-store eager-render RAF coverage:
  extend the new eager session-store delta regression to flush the pending animation frame and assert the command/diff view remains consistent after the broad React session render catches up.
- [ ] P2: Audit SessionPaneView scroll/signature derivations during store-backed updates:
  `AgentSessionPanel` now derives visible command/diff lists from the store-backed session snapshot, but `SessionPaneView` still computes scroll/signature bookkeeping from React-state `activeSession`; prove this cannot drift during eager store publication, or move the bookkeeping to the same store boundary.
- [ ] P2: Split Telegram settings persistence tests out of the monolithic Telegram test module:
  move the state-backed Telegram config persistence/status/delete-session/delete-project coverage into a focused test module so new coverage does not keep growing `src/tests/telegram.rs`.
- [ ] P2: Add assistant-reply forwarding disabled-path regressions:
  cover `sync_telegram_digest` and `select_telegram_project_session` with `forward_assistant_replies=false` so digest and selection paths cannot accidentally forward assistant replies.
- [ ] P2: Clarify pending queued-prompt cancel tooltip behavior:
  either restore/replace the removed `PendingPromptCard` `title` affordance or document the intentional aria-label-only behavior in the component/test coverage.
- [ ] P2: Add reconnect-specific gapped session-delta recovery coverage:
  arm reconnect fallback polling, reopen SSE, dispatch an advancing stamped `textDelta`/`textReplace` across a revision gap, and assert live text renders before snapshot repair while recovery remains pending until authoritative repair succeeds.
- [ ] P2: Add reconnect transport branch coverage for non-rearming adoption:
  cover the `adopted`-without-rearm path in the live-state transport and make intentionally discarded reconnect-confirmation boolean returns explicit at call sites.
- [ ] P2: Add remaining production SQLite persistence coverage:
  with the SQLite runtime path now compiled under `cargo test`, cover targeted delta upsert, metadata-only update, hidden/deleted row removal, malformed SQLite row/load errors, and startup load assertions that exercise the split `app_state` / `sessions` / `delegations` tables directly.
- [ ] P2: Restore Windows AppState bootstrap path-normalization coverage:
  reintroduce a Windows-gated `AppState::new_with_paths` test using a temp `termal.sqlite` store and a `\\?\` workdir, asserting `default_workdir`, the default project root, and bootstrapped Codex/Claude live session workdirs all normalize to the canonical root.
- [ ] P2: Add Windows state-path redirection coverage:
  cover SQLite main-file symlinks, sidecar symlinks, and `.termal` directory junction/symlink cases behind Windows-gated tests.
- [ ] P2: Add post-shutdown persistence ordering coverage:
  race a late background commit against `shutdown_persist_blocking()` and prove the final persisted state reflects the latest `StateInner`, not an older worker-drained delta.
- [ ] P2: Add concurrent shutdown idempotency race coverage:
  call `shutdown_persist_blocking()` concurrently from two `AppState` clones and assert `persist_worker_alive` cannot flip false until the join owner has returned.
- [ ] P2: Add shutdown persist failure retry coverage:
  force the final shutdown persist attempt to fail once and then succeed, and assert the worker does not exit before the successful write.
- [ ] P2: Add non-send action restart live-stream delta-on-recreated-stream coverage:
  the round-13 fix proves `forceSseReconnect()` is called on cross-instance `adoptActionState` recovery, but does not dispatch live deltas through the recreated EventSource. Submit an approval/input-style action after backend restart, then dispatch assistant deltas on the new `EventSourceMock` and assert they render in the active transcript bubble.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P2: Cover Telegram relay active-project reconciliation:
  start an in-process relay with subscribed projects but no default and assert startup fails or status exposes the effective `activeProjectId`; delete a project used by a running relay and assert the relay is stopped or restarted without the deleted id.
- [ ] P2: Cover remaining Telegram relay runtime lifecycle paths:
  use the AppState-owned test relay runtime to cover startup from saved settings, implicit first subscribed-project fallback, invalid/missing config stop, config-save restart, and graceful-shutdown stop.
