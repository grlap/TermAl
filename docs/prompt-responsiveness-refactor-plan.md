# Prompt Responsiveness Refactor Plan

## Goal

Make prompt typing feel immediate even while an active Codex session is
streaming, waiting for output, or resyncing state.

Success means:
- typing in the active composer does not visibly stutter during normal live
  Codex activity
- switching to a live tab and typing the first character does not trigger a
  noticeable hitch
- hidden sessions do not keep full transcript/render work alive just to keep
  tabs "warm"
- `/api/state` snapshots stop acting as the normal render driver for the whole
  session tree

This plan is the proper fix. It is intentionally aimed at removing the
structural causes of prompt lag rather than adding more local guards.

## Problem Summary

The current UI still mixes three different responsibilities in the same urgent
render path:

- live transport adoption in `ui/src/app-live-state.ts`
- transcript/virtualization work in
  `ui/src/panels/VirtualizedConversationMessageList.tsx`
- composer/footer state in `ui/src/panels/AgentSessionPanel.tsx`

That coupling is why prompt typing feels much worse when a Codex session is
active than when the UI is otherwise idle. The prompt is not slow because the
textarea itself is inherently expensive. It is slow because background session
adoption and transcript work keep landing on the same main-thread path that the
composer needs to feel responsive.

## Design Principles

1. Urgent input must depend on the smallest possible state surface.
2. Full snapshots are for cold start, recovery, and repair, not for steady-state
   rendering.
3. Active and hidden sessions must not pay the same rendering cost.
4. Transcript rendering must be isolated from composer input.
5. Derived comparisons must not walk transcript history during routine prompt
   edits or live updates.
6. Each migrated slice must have one authoritative write path at every step of
   the transition.
7. Selectors must preserve identity for unchanged data.
8. Remove obsolete mitigation code once the architectural boundary exists.

## Current Structural Issues

### 1. Full-state adoption is still a broad UI driver

`useAppLiveState` still funnels large updates through `adoptState(...)` and
`adoptSessions(...)` in `ui/src/app-live-state.ts`. Even after smaller
optimizations, this path still does too much work relative to what the active
composer actually needs.

### 2. Session state is too coarse for the UI slices that consume it

Large `session` objects are still pushed through the view tree even when most of
that data is irrelevant to the active prompt. This keeps unrelated updates
eligible to rerender the composer/footer path.

### 3. Transcript work is too easy to wake up

Transcript virtualization is still close enough to tab activation and session
updates that active typing can compete with measurement, page-range management,
and search-pinning work.

### 4. Composer logic still depends on expensive session-derived comparisons

The footer/composer path in `ui/src/panels/AgentSessionPanel.tsx` still derives
some stability from whole-session comparisons, including prompt-history-derived
checks. That is the wrong dependency shape for a latency-sensitive input.

## Target Architecture

### A. Normalized client session store

Introduce a small client-side store owned by the frontend and consumed through
`useSyncExternalStore` selectors.

The store should separate:

- `sessionSummariesById`
- `sessionMessagesById`
- `sessionMessageOrderById`
- `sessionUiById`
  - busy/stopping/updating flags
  - waiting indicator state
  - model/approval/sandbox settings that affect the composer
- `paneSelections`
- `composerDraftsBySessionId`
- `draftAttachmentsBySessionId`

The key rule is that the active composer must subscribe only to the tiny subset
of state it actually needs.

The store must also preserve structural sharing:

- unchanged slices keep object/array identity
- selector helpers must not allocate fresh containers for unchanged reads
- identity preservation is a correctness requirement for this refactor, not an
  optional optimization

The composer-facing selector contract should be explicit. At minimum it must
carry the fields needed to keep the footer/composer correct without subscribing
to transcript state:

- active session identity and agent label
- busy/stopping/updating/waiting state
- model / approval / sandbox / effort settings that affect slash availability
- pending prompt / send-stop availability state
- local draft text and attachments

### B. Delta-first live transport

SSE deltas should become the normal source of truth for incremental updates only
for slices that have complete typed delta coverage.

`/api/state` should be treated as:

- initial hydration
- reconnect recovery
- watchdog repair
- explicit resync after detected inconsistency

It should not continue to act like a broad, frequent replacement signal for the
whole live session array during normal healthy operation.

However, full snapshot adoption remains authoritative for:

- any slice in `StateResponse` that does not yet have complete delta coverage
- restart/revision recovery using `serverInstanceId` and the current revision
- authoritative rollback after a detected gap, restart, or inconsistency

Delta-first does not weaken the current SSE contract:

- `delta` events must still be exact-next-revision gated
- stale or out-of-order deltas must be rejected
- gaps still force snapshot recovery
- snapshot adoption must update revision and `serverInstanceId` atomically with
  the visible store state

### C. Active-only transcript ownership

The active transcript should own heavy virtualization/render work. Hidden tabs
should keep only the light summary state needed for tab titles, badges, and
resume behavior.

If hidden-tab warming remains desirable, it should happen via cheap metadata or
idle work, not by keeping full conversation rendering live.

The hidden-session contract must be explicit. Hidden sessions may keep:

- tab title / status / unread badge metadata
- latest known revision and summary state
- activation restore metadata such as scroll anchor, search anchor, or selected
  item identity when needed for visible UX

Hidden sessions must not keep:

- live transcript DOM
- active measurement observers
- page-height recalculation work
- other continuously running transcript-render machinery

### D. Composer-local urgency boundary

The composer should own its local draft state and respond immediately to input
without waiting on:

- transcript updates
- hidden session changes
- full session object replacement
- prompt-history rescans

Settings changes that affect slash commands or composer availability should flow
through narrow selectors, not whole-session prop churn.

## Refactor Nodes

### Node 0: Establish a Stable Perf Harness

**Purpose:** Lock in the scenario we are trying to fix before moving state
ownership.

**Work:**
- keep the existing active-Codex typing and first-key-after-tab-switch
  profiling scripts as reusable local tooling
- document the exact scenarios used for comparison
- record baseline metrics before each major node lands
- add a deterministic perf-smoke harness with explicit thresholds for the main
  latency scenarios, even if it remains a local checked-in script rather than a
  CI gate

**Completion criteria:**
- there is a repeatable local measurement path for:
  - active Codex waiting while typing
  - active Codex streaming while typing
  - tab switch followed by first character
- the repo has a deterministic perf-smoke path for those scenarios with
  baseline thresholds recorded alongside the plan or the harness

### Node 1: Introduce a Frontend Session Store

**Purpose:** Stop routing most live UI through broad React state replacement.

**Work:**
- add a small store module, likely `ui/src/session-store.ts`
- expose selector hooks with `useSyncExternalStore`
- keep the first version thin and mechanical; do not redesign behavior yet
- define slice ownership explicitly; once a slice moves into the store, the
  store becomes the only write target for that slice
- keep legacy props only as read-only adapters derived from store-owned slices
  during the transition
- require structural sharing so unchanged selector reads preserve identity

**Completion criteria:**
- the store exists and can serve selected session slices without requiring
  whole-tree prop threading
- at least one non-trivial UI surface reads from selectors instead of the old
  broad props path
- unchanged slices preserve identity across no-op patches and compatible
  snapshots

### Node 2: Move Composer Ownership to Narrow Selectors

**Purpose:** Make prompt typing independent from transcript and unrelated
session updates.

**Work:**
- move active composer reads onto selector-based session UI state
- keep draft text and attachments in a composer-oriented state boundary
- remove prompt-history-based equality checks from the footer/composer memo path
- replace whole-session comparator logic with a cheap, explicit composer input
  model
- define a single composer selector contract so model / approval / sandbox /
  effort / busy / waiting state update independently from transcript changes

**Likely affected files:**
- `ui/src/panels/AgentSessionPanel.tsx`
- `ui/src/SessionPaneView.tsx`
- new composer/store helpers

**Completion criteria:**
- active typing no longer depends on whole-session object identity
- prompt-history scans are not part of routine composer rerender control
- busy/stop/send/settings state still stays correct
- composer-critical fields update correctly without subscribing to transcript
  state

### Node 3: Split Session Summary State from Transcript State

**Purpose:** Ensure hidden sessions do not drag transcript work into prompt
input.

**Work:**
- make tab strips and pane/session selection depend on summary state only
- isolate message arrays and transcript-derived state behind active-session
  selectors
- ensure hidden tabs do not mount or preserve full conversation trees unless
  explicitly required for a visible feature
- define exactly which hidden-session metadata survives off-screen and what is
  rebuilt on activation

**Completion criteria:**
- hidden tabs can stay functional without mounted transcript rendering
- active prompt responsiveness does not degrade simply because another live
  session exists in the same workspace
- scroll/search/resume behavior restore correctly when a hidden session becomes
  active again

### Node 4: Convert Live Transport to Delta-First Patching

**Purpose:** Remove broad snapshot adoption from the healthy steady-state path.

**Work:**
- define store patch operations for:
  - append/update message
  - pending prompt lifecycle
  - session busy/waiting state changes
  - session setting changes
  - session create/remove/rename/move
- apply SSE deltas directly to the store
- keep snapshot adoption as the authoritative path for slices that do not yet
  have full delta coverage
- make recovery merge targeted by id/revision instead of broad session-array
  reconciliation when practical
- exact-next-revision gate all delta application
- keep revision and `serverInstanceId` updates atomic with snapshot adoption
- keep one reducer/patch source feeding both the store and any temporary
  read-only legacy adapters until the old path is removed

**Likely affected files:**
- `ui/src/app-live-state.ts`
- `ui/src/api.ts`
- new session store patch helpers

**Completion criteria:**
- healthy SSE flow updates only the changed sessions/messages
- `/api/state` resync does not routinely force broad visible work when nothing
  relevant changed for the active prompt
- restart recovery and revision-gap recovery remain correct
- slices without typed delta coverage still stay correct through authoritative
  snapshot adoption

### Node 5: Rebuild Transcript Virtualization Around Stable Metadata

**Purpose:** Prevent transcript measurement work from competing with prompt
typing and tab activation.

**Work:**
- cache message/page layout metadata by keys or invalidation rules that account
  for all layout-affecting inputs, not only message identity
- keep page-range/search-pinning logic isolated from the live viewport range
- defer non-urgent measurement after activation/paint where possible
- ensure hidden sessions do not run active transcript measurement logic

**Likely affected files:**
- `ui/src/panels/VirtualizedConversationMessageList.tsx`
- `ui/src/panels/conversation-virtualization.ts`

**Completion criteria:**
- first-key-after-tab-switch no longer competes with large synchronous
  transcript estimation work
- search pinning, scroll restoration, and long-message rendering remain correct
- cache invalidation remains correct across resize, width changes, and other
  layout-affecting UI state changes

### Node 6: Remove Transitional Guards and Dead Mitigations

**Purpose:** Finish with a simpler system instead of stacking permanent
workarounds.

**Work:**
- delete comparator hacks that existed only to hide broad prop churn
- remove obsolete hidden-tab rendering shortcuts that are no longer needed
- simplify snapshot-adoption branches that the new store/render ownership makes
  unnecessary

**Completion criteria:**
- the final system is easier to reason about than the current one
- latency wins come from cleaner ownership boundaries, not from accumulated
  conditionals

## Verification Standard

Every node should be accepted only if all three are true:

1. **Correctness**
   - no lost messages
   - no missed session status transitions
   - no broken recovery/open flows
   - no stale settings in the composer
   - no split-brain state between the store and temporary legacy adapters during
     migration
   - the newest assistant message becomes visible immediately after reconnect or
     snapshot repair without requiring another user action

2. **Responsiveness**
   - active typing remains smooth with a live Codex session visible
   - first-key-after-tab-switch hitch is materially reduced
   - hidden live sessions do not noticeably degrade the active prompt

3. **Complexity**
   - ownership boundaries are clearer after the node than before
   - the node removes coupling instead of adding more special cases

## Test Plan

The refactor needs both behavioral and performance-oriented coverage.

### Behavioral tests

- selector/store unit tests for patch application and snapshot merge behavior
- selector/store identity tests proving unchanged slices preserve object/array
  identity across no-op patches and compatible snapshots
- `useAppLiveState` integration coverage for delta-first updates and targeted
  recovery
- exact-next-revision delta gating tests and fallback snapshot recovery tests
- restart recovery tests that prove `serverInstanceId` and revision updates stay
  atomic with snapshot adoption
- composer tests that prove prompt typing does not depend on transcript updates
- tab-switch tests covering active hidden/live session combinations
- hidden live session tests proving summary state can update while hidden
  transcript DOM and active measurement work remain absent
- transcript tests for search pinning, scroll restoration, and height caching
- reconnect visibility tests proving the latest assistant message appears
  immediately after recovery without a forced rerender or another prompt

### Performance checks

- run the deterministic perf-smoke harness and the existing local typing
  profiles after each major node
- compare:
  - task duration while typing during active Codex waiting
  - task duration while typing during active Codex streaming
  - worst frame after tab switch + first key
- reject nodes that improve one path while regressing another materially
- reject nodes that restore hidden-session churn while the active composer is in
  use

## Migration Strategy

Do this incrementally, not as a flag day rewrite.

Migration invariant:

- for any slice already owned by the store, every write goes through one
  reducer/patch source
- legacy props remain temporary read-only adapters for migrated slices
- snapshot and delta adoption must update the store and any temporary adapters
  atomically until the old path is removed

Recommended order:

1. Add the store and selectors.
2. Move the composer to narrow selectors and local draft ownership.
3. Move pane/tab surfaces to summary-only selectors.
4. Route healthy SSE updates through store patches.
5. Narrow snapshot recovery to targeted merges.
6. Rebuild transcript virtualization around stable metadata.
7. Delete obsolete guard code.

Each node should leave the tree working and measurable before the next node
starts.

## Non-Goals

This plan does not require:

- introducing a third-party state-management library
- changing backend wire formats unless a targeted transport improvement is
  clearly justified
- redesigning the visual UI
- solving every virtualization edge case before the composer path is isolated

## Expected Payoff

If this plan lands cleanly, the prompt should stop feeling materially different
between:

- no active Codex session
- a Codex session that is waiting for output
- a Codex session that is actively streaming

That is the bar for calling this fixed.
