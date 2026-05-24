# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Implementation Tasks

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
