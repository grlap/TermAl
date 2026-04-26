# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

Also fixed in the current tree: `SessionCreated` and
`OrchestratorsUpdated.sessions` deltas now publish metadata-first session
summaries instead of full transcript-bearing `Session` objects. The backend
uses summary wire projections for local, forked, remote-proxy, and
orchestrator-referenced session payloads, and the frontend preserves already
hydrated messages when a summary `sessionCreated` delta updates an existing
session.

Also fixed in the current tree: inbound remote summary `SessionCreated` and
`OrchestratorsUpdated.sessions` deltas now preserve nonzero remote
`messageCount` values and keep proxy sessions unhydrated when the payload has
`messagesLoaded: false` with an empty `messages` array. Rust regressions cover
both remote summary delta shapes and their republished localized summaries.

Also fixed in the current tree: remote proxy transcript recovery no longer
treats metadata-only `/api/state` or summary deltas as a full transcript repair.
Unloaded remote proxies keep the remote `messageCount`, cached transcripts are
marked stale when summary counts diverge, `/api/sessions/{id}` performs a
targeted remote full-session fetch before returning a proxy session, and
session-scoped remote deltas hydrate unloaded proxies before local index checks.
This also closes the old `wire_session_from_record`/`messagesLoaded` ambiguity
for remote proxy GET hydration because the route hydrates unloaded remote
proxies before returning `SessionResponse`. Rust regressions cover GET
hydration, delta-before-gap hydration, and count-decrease summary handling.

Also fixed in the current tree: targeted remote transcript repair now rejects
full-session responses from revisions newer than the delta being repaired, so a
single-session fetch cannot silently advance one proxy beyond the global remote
watermark. Stale remote deltas also skip before attempting targeted hydration.
Rust regressions cover newer targeted-session responses and stale unloaded
delta skips.

Also fixed in the current tree: metadata-only remote summaries no longer treat
`messageCount` equality as proof that a cached loaded transcript is fresh. The
merge now preserves cached messages only when both the count and known remote
`sessionMutationStamp` match; same-count summaries with a newer stamp demote the
proxy to `messagesLoaded: false` so targeted hydration can repair the transcript.
Rust regressions cover both same-stamp preservation and new-stamp demotion.

Also fixed in the current tree: remote hydration now keeps the single-session
repair path aligned with the global remote revision cursor. Direct
`/api/sessions/{id}` hydration performs a full remote `/api/state` resync before
applying a newer per-session response, targeted delta repair re-checks the
remote cursor under lock after the fetch returns, and session-scoped remote
deltas retain the remote `sessionMutationStamp` on cached proxy sessions so
later metadata-only summaries can make a real freshness decision.

Also fixed in the current tree: `wire_session_summary_from_record` now builds
metadata-first summaries directly from `SessionRecord` fields instead of
cloning the full transcript-bearing `Session` and then clearing `messages`.
This removes the hidden O(total transcript messages) allocation/drop cost from
summary snapshot construction under the state mutex.

Also fixed in the current tree: the remaining `wire_session_summary_from_session`
clone-and-clear call sites have been removed. Codex fork, local create-session,
remote `SessionCreated` republish, and remote proxy create/fork announcements
now build `SessionCreated` delta summaries from `SessionRecord` metadata via
`wire_session_summary_from_record`, and the transitional helper was deleted.
The Codex fork regression now asserts the route response remains
transcript-bearing while the published delta is metadata-only with the correct
`messageCount`.

Also fixed in the current tree: Monaco diff worker cancellations are now
filtered before they reach the browser as uncaught promise rejections. The
startup path installs a narrow `unhandledrejection` filter that only prevents
Monaco `Canceled: Canceled` errors with both a Monaco worker/service frame and
`computeDiff`, so normal app promise failures still surface in the console.
`monaco-cancellation-filter` coverage pins the positive Monaco stack, unrelated
canceled-error cases, and a negative `computeDiff`-without-Monaco stack.

Also fixed in the current tree: single-session hydration now rejects stale
full-session responses instead of marking old transcripts loaded over newer
metadata. The hydration request captures the session's `messageCount`,
`sessionMutationStamp`, revision, and server instance at dispatch; lower
same-instance responses are accepted only when the current summary still
matches that captured metadata and the response matches the current summary.
In-flight old-server responses are also rejected after a server-instance change.
`ui/src/App.live-state.deltas.test.tsx` covers both a newer metadata delta
overtaking hydration and an old-instance hydration resolving after a restart.

Also fixed in the current tree: visible metadata-only session hydration now
schedules a bounded retry after a stale full-session response is rejected or a
transient non-404 fetch failure is reported. The stale-hydration regressions
now require the fresh retry response to hydrate the pane and assert the newer
delta/restarted-server data remains visible, so the tests no longer pass on an
empty pane that merely lacks stale text.

Also fixed in the current tree: the Create Session dialog now initializes its
assistant picker from the active session only when the active session id
changes. User changes inside the open dialog no longer retrigger the sync effect
and snap the picker back to the active session's agent. The session-lifecycle
test suite covers changing the Assistant combobox away from the active Codex
session and verifying the Claude selection sticks.

Also fixed in the current tree: `AdoptStateOptions` now explicitly carries
`disableMutationStampFastPath`, and `adoptState` preserves a caller-requested
deep session reconcile by OR-ing that flag with the server-instance-change
guard. A focused `app-live-state` unit test pins the explicit-true,
server-restart, and default-false branches.

Also fixed in the current tree: `sessionCreated` delta reconciliation now
disables the mutation-stamp fast path when merging an authoritative summary
into an existing session, so same-stamp re-emits can still update metadata while
preserving hydrated messages. `live-updates.test.ts` covers matching-stamp
metadata changes and now pins `messageCount` plus `sessionMutationStamp` on the
summary-preservation path.

Also fixed in the current tree: the stale search-band range mutation entry no
longer applies to the current virtualized transcript implementation. The
rendered mounted range is derived from either the pinned search range or the
current mounted range, and the mutating `mergeRanges(...)` helper described by
the bug is no longer present.

Also fixed in the current tree: remote `MessageUpdated` deltas now behave as
true in-place replacements. Existing local proxy messages are replaced and
republished as localized `MessageUpdated` events, while missing targets are
treated as remote sync gaps instead of being synthesized as `MessageCreated`;
the handler returns a recoverable error before advancing the remote applied
revision so the event bridge can resync. Focused remote tests cover
existing-target replacement, missing-target gap handling, stale revision
skips, payload-id mismatch, and stale remote `messageIndex` hints.

Also fixed in the current tree: local Codex interaction submissions now have
route-level regression coverage for `MessageUpdated`. The user-input, MCP
elicitation, and generic app-request POST routes each assert the runtime
JSON-RPC response is delivered, no full `StateResponse` SSE snapshot is
published, exactly one `MessageUpdated` delta is emitted with the same
`sessionMutationStamp` as the route response, and the pending map is cleared.
The shared route-test helper now also asserts the delta `revision`,
`message_index`, `message_count`, `preview`, and `status`, and the route tests
cover the user-input, MCP-elicitation, and Codex app-request payload shapes
directly.

Also fixed in the current tree: `commit_interaction_message_update` now uses
the visible-session lookup expected for user-facing routes, and its impossible
"updated index is out of bounds / points at another message" branches now
panic-fast with explicit invariant messages instead of surfacing as recoverable
API errors. The dead approval-status check after the public pending-decision
guard was removed.

Also fixed in the current tree: the frontend `messageUpdated` reducer now has
edge coverage for applying by message id when the supplied index is stale,
rejecting payload/event id mismatches, and rejecting unsafe indexes. The
frontend `messageCreated` branch now has matching negative and non-integer index
coverage, plus a forward-reorder regression that pins the supplied insertion
index semantics. The reducer also documents that `messageUpdated` is an
in-place replacement and must not copy the `messageCreated` reordering behavior.
Follow-up coverage now also pins the missing-`sessionMutationStamp` fallback.

Also fixed in the current tree: `messageUpdated` is documented in the SSE
contract in `docs/architecture.md`, including the whole-message replacement
semantics and the rule that `messageIndex` is only a fast-path hint.

Also fixed in the current tree: session-scoped delta events now carry
`messageCount` on the wire. `MessageCreated`, `MessageUpdated`, `TextDelta`,
`TextReplace`, `CommandUpdate`, and `ParallelAgentsUpdate` source the value
from the mutated session record after each change, and the frontend keeps it
on the session projection for metadata-first state adoption.

Also fixed in the current tree: full `Session` snapshots and metadata-first
state summaries now carry `messageCount`. `SessionResponse` and
`CreateSessionResponse` serialize the count computed by
`wire_session_from_record` from the current transcript, while `StateResponse`
serializes the same count on transcript-free session summaries.
`snapshot_bearing_routes_include_message_count` pins the JSON shape for both
`/api/state` and `/api/sessions/{id}`.

Also fixed in the current tree: `/api/state` responses and SSE `state`
snapshots no longer include transcript payloads. The snapshot builder now emits
metadata-first session shells with `messages: []`, `messagesLoaded: false`, and
the transcript-derived `messageCount`, while `GET /api/sessions/{id}` remains
the full hydration route. The frontend reconciler preserves already-hydrated
messages when summary snapshots arrive, and unhydrated session deltas update
metadata without forcing `/api/state` resync loops.

Also fixed in the current tree: `docs/architecture.md` now documents
`messageCount` as a property of every full `Session` or state-session summary
serialized on the wire, including snapshot-bearing responses plus
delta-carried `SessionCreated` and `OrchestratorsUpdated.sessions` payloads.

Also fixed in the current tree: raw create-session route coverage now pins
`CreateSessionResponse.session.messageCount` before typed decoding, so the test
would fail if the field disappeared from the POST `/api/sessions` wire JSON
despite `Session.message_count` having a serde default.

Also fixed in the current tree: the `messageCount` compatibility story is now
explicitly documented as a coordinated current-tree wire bump for local-only
Phase 1. Current session-scoped deltas require the field, full session
snapshots serialize it, and older persisted local JSON can still load because
the `Session` field has a serde default before outbound projection recomputes
the value.

Also fixed in the current tree: `MessageCreated` / `MessageUpdated` contract
docs now list `message_count` and `session_mutation_stamp`, and the
`MessageCreated` entry explicitly documents that it may reorder the transcript
to the supplied insertion index while `MessageUpdated` must stay in-place.

Also fixed in the current tree: the frontend `messageUpdated` and
`messageCreated` guards now reject unsafe, non-integer, or negative
`messageIndex` hints before falling back to id lookup. `live-updates.test.ts`
pins the unsafe-integer and non-integer `messageUpdated` and `messageCreated`
resync paths.

Also fixed in the current tree: remote `MessageCreated` replay now normalizes
existing-message creates server-side before rebroadcasting. Existing proxy
messages are replaced and moved to the applied local index, impossible insertion
gaps fail before the remote applied revision advances, and the localized delta
publishes the actual applied `message_index` plus `message_count`. Focused
remote tests cover existing-id replacement/reorder, payload-id mismatch,
existing-message bounds rejection, and gap rejection.

Also fixed in the current tree: missing-target remote `CommandUpdate` and
`ParallelAgentsUpdate` deltas now use the same remote sync-gap guard as
`MessageCreated`. Impossible insertion gaps fail before the local proxy
transcript mutates or the remote applied revision advances, and synthesized
`MessageCreated` deltas publish the actual applied local index. Focused remote
tests cover both command and parallel-agent gap rejection.

Also fixed in the current tree: the remote `MessageUpdated` missing-target path
no longer writes remote-supplied ids directly to stderr before returning its
recoverable sync-gap error. It now matches the sibling `CommandUpdate` and
`ParallelAgentsUpdate` branches by relying on the central remote-event failure
logging path, closing both duplicate-log noise and unsanitized id interpolation.

Also fixed in the current tree: stale remote `MessageUpdated` frames now hit
the applied-revision guard before payload-id validation. A stale frame with a
mismatched embedded message id is skipped without requesting recovery, and the
stale-delta tests now assert the applied-revision tracker state.

Also fixed in the current tree: `live-updates.test.ts` now asserts
`messageCount` propagation on the `textDelta`, `textReplace`, `commandUpdate`,
and `parallelAgentsUpdate` reducer branches, so all six session-scoped delta
branches lock the metadata-first count update.

Also fixed in the current tree: the local-route delta tests
(`src/tests/review.rs`) now pin `revision`, `message_index`, `message_count`,
`preview`, and `status` on every published `MessageUpdated` delta through the
extended `assert_no_state_and_one_message_updated_delta` helper. The four
interaction-submission tests (approval, user-input, MCP elicitation, Codex
app-request) each assert the full wire shape, including equality with the
route response's revision, so a regression that leaked a `preview` or drifted
a `revision` would fail loudly.

Also fixed in the current tree:
`remote_message_updated_delta_uses_message_id_when_remote_index_is_stale` is
now load-bearing. It asserts that the untouched message at index 0 retains
its original text (not just its id), pins the published delta's `revision`,
`message_id`, `message_index`, `message_count`, `preview`, and `status`, and
checks that `should_skip_remote_applied_delta_revision` advanced for the
applied remote revision. A regression overwriting message-1 while moving
message-2 to the right slot, or skipping the revision advance, would fail
the test.

Also fixed in the current tree: `interaction_message_update_parts` no longer
pretends to have a recoverable `ApiError` path. It returns the replacement
message metadata directly, and `commit_interaction_message_update` now has a
contract comment documenting that invalid returned indexes are internal
invariant violations.

Also fixed in the current tree: the dialog backdrop mouse-button contract now
runs through `isDialogBackdropDismissMouseDown` for Settings, create-session,
and create-project dialogs. Middle-click, physical right-click, and macOS
Ctrl-click no longer dismiss those backdrops before the browser can handle the
native paste/context-menu gesture.

Also fixed in the current tree: the Settings dialog shell tests now locate the
backdrop through `screen.getByRole("dialog").parentElement` instead of repeated
raw `.dialog-backdrop` selectors, and they cover macOS Ctrl-click, plain macOS
primary click, and non-Apple Ctrl+primary behavior.

Also fixed in the current tree: deferred heavy-content activation no longer
depends on `.virtualized-message-list` DOM ancestry. `DeferredHeavyContent`
accepts an explicit immediate-render flag, the message-card callers now thread
`preferImmediateHeavyRender` through Markdown/code-heavy subcards, and Mermaid
coverage pins that a heavy thinking card renders immediately without waiting
for `IntersectionObserver`.

Also fixed in the current tree: virtualized transcript mounted-range
reconciliation no longer calls `flushSync` from native/custom scroll listeners,
so programmatic scroll notifications fired during layout/effect work no longer
trip React's "flushSync was called from inside a lifecycle method" warning. The
same patch gates heavy Markdown/code activation synchronously on the first
active render instead of waiting for a passive post-activation effect.

Also fixed in the current tree: composer draft publication no longer emits the
external `session-store` from inside React state updater callbacks. Send-failure
draft restore, draft-attachment removal, and queued-prompt cancellation now
compute ref/store updates before scheduling React state, avoiding the
"Cannot update SessionComposer2 while rendering App" warning.

Also fixed in the current tree: active Codex/Claude streaming deltas no longer
publish full-session React/store updates for every output chunk. Session refs
and revisions still advance immediately, but the active transcript's
`session-store` record and broad `sessions` render update now flush together at
most once per animation frame; `ui/src/App.live-state.deltas.test.tsx` pins
that a burst of live deltas schedules a single frame render.

Also fixed in the current tree: `session-store` subscribers no longer allocate
fresh `useSyncExternalStore` snapshot closures on every render, and pane tab
rails now subscribe only to the session summaries referenced by their own tabs.
Unrelated session-summary changes can no longer invalidate every `PaneTabs`
instance through the whole `sessionSummariesById` dictionary.

Also fixed in the current tree: live Codex global-state deltas and repeated
transport recovery no-ops no longer force immediate React work on every event.
`codexUpdated` updates its ref immediately but batches the visible
`CodexState` render to one animation frame, and the App-level backend
connection-state setter now ignores same-value writes before entering React.

Also fixed in the current tree: slow SSE `state` handling now reports a
development-only phase breakdown (`parse`, `adoptState`, `postAdoption`,
`clearErrors`) when total handling exceeds 50 ms, and rejected stale snapshots
no longer walk every session just to cancel active-prompt recovery polls.

Also fixed in the current tree: stale same-instance SSE `state` snapshots now
use a raw-payload revision/server-instance peek before `JSON.parse`. When the
peek proves the snapshot is stale and not an `_sseFallback`, the handler
rejects it without parsing the full transcript-bearing payload. The metadata
peek is capped to the first 4 KB so rejected snapshots do not trade parse
latency for a full-payload regex scan.

Also fixed in the current tree: create-session and create-project backdrop
handlers now have direct integration coverage in `ui/src/AppDialogs.test.tsx`.
Primary-button backdrop mousedown dismisses each dialog only when idle;
middle-click, right-click, and macOS Ctrl-click do not; and pending create
operations keep the dialogs open.

Also fixed in the current tree: rendered-diff complete-document coverage now
asserts that the "Patch-only rendering" banner is absent, source-renderer math
counter coverage now includes consecutive multiline `$$` blocks, `$$` inside
fenced code, and same-line `$$...$$` pairs, and Mermaid diagram tests now cover
normal-size scrollbar slack plus negative/zero viewBox lower-bound clamping.

Also fixed in the current tree: virtualized conversation scrolling now gates
both sticky-bottom and non-bottom height-measurement scroll writes during direct
user-scroll cooldowns, routes the explicit "Load N earlier messages" button
through the same anchor-preserving prepend helper as scroll-triggered auto-load,
and keeps the auto-load scroll listener stable by reading volatile viewport
state through refs instead of re-subscribing on every scroll tick.

Also fixed in the current tree: the near-bottom cooldown regression test is
now load-bearing. `lastUserScrollInputTimeRef` initialises to
`Number.NEGATIVE_INFINITY` so mount-time measurements never falsely register
inside the cooldown, and the test itself now runs a two-phase negative /
positive control â€” a no-input measurement must write the pin target, a
post-wheel measurement must not â€” so an unbound or broken wheel listener can
no longer leave the test passing.

Also fixed in the current tree: compact command-heavy virtualized transcripts
no longer expose blank pages inside the viewport when measured command pages
shrink during an active user-scroll cooldown. The virtualizer now clamps range
selection to its current virtual layout instead of a stale larger DOM
`scrollHeight`, and it grows the mounted page band from real DOM bounds when
the visible viewport would otherwise fall into the bottom spacer. A focused
`AgentSessionPanel.test.tsx` regression covers short command pages shrinking
under cooldown and asserts the list mounts enough pages below to keep content
visible.

Also fixed in the current tree: session-switch transcript scroll state no
longer leaks between sessions in the same pane. `SessionConversationPage` is
keyed by active session id, so the virtualized list remounts on session switch
and resets its scroll-intent refs, deferred-layout timers, idle-compaction
timers, and page-height caches. The scroll regression coverage now includes
programmatic bottom-boundary writes that immediately mount the bottom window.

Also fixed in the current tree: dangerous-Markdown-link neutralization is
now directly covered. A new `ui/src/markdown-links.test.ts` pins the
`transformMarkdownLinkUri` contract with table-driven cases for `javascript:`
(including mixed-case variants), `vbscript:`, non-image `data:`, and
`data:image/*` all â†’ `""`; safe `http`/`https`/`mailto`/`tel` round-trip
unchanged; `#anchor` and relative paths bypass uriTransformer via the
`isExternalMarkdownHref` early return. A new `MarkdownContent` render test
pairs feeds Markdown like `[click me](javascript:alert(1))` through the full
render pipeline and asserts no `<a>` role, no `href="javascript:void(0)"`,
and the link text still renders as plain content.

Also fixed in the current tree: `adoptCreatedSessionResponse`
no longer leaves a phantom workspace pane when the create
response violates the wire contract. The function now returns
a discriminated outcome â€” `"adopted" | "stale" | "recovering"`
â€” instead of a boolean. The three call sites in `App.tsx`
(create-session, fork-Codex-thread, remote-project create)
each fall through to the workspace-pane fallback ONLY on
`"stale"` (an earlier SSE delta already raised the revision
past the POST response, so the session IS in `sessionsRef`
and the pane points at a real session). On `"recovering"`
(the `created.session.id !== created.sessionId` mismatch
branch) the fallback is skipped â€” the mismatched id was never
inserted into `sessionsRef`, so opening a pane would leave a
phantom that persists until the scheduled
`requestActionRecoveryResyncRef.current()` reconciles. On
`"adopted"` the function already opened the pane internally.
The TypeScript compiler enforces the three-way distinction at
every call site. A dedicated Vitest test for the recovering
branch is tracked as a P2 task â€” the full-App integration
scaffolding for a mocked `api.createSession` mismatch response
is heavier than the fix itself.

Also fixed in the current tree:
`failed_remote_snapshot_sync_restores_session_tombstones` now
compares the full content of every restored session plus full
orchestrator-instance contents, not just ID membership and
counts. Previously a partial rollback that restored session IDs
while leaving mutated `Session` fields, remote metadata, or
orchestrator payloads behind would have slipped past. The test
now (a) serializes each `SessionRecord` into a canonical shape
(full `Session` JSON + `remote_id` + `remote_session_id`) and
asserts pre/post equality, and (b) directly compares
`inner.orchestrator_instances` (which already derives
`PartialEq`) for full-payload equality â€” orchestrator id,
template id, project id, status, sessions, prompts, settings.
The helper sidesteps needing `PartialEq` on `SessionRecord`
(which contains runtime handles like `SessionRuntime`) by
comparing via `serde_json::Value`. 73 remote tests stay green.

Also fixed in the current tree: the read-only rendered-Markdown
flash regression test is now load-bearing. The previous test
waited for the `readOnlyResetVersion` remount via `waitFor` and
asserted the restored DOM â€” which would still pass if
`event.currentTarget.textContent = segment.markdown` were
reintroduced in `markdown-diff-change-section.tsx::handleInput`
and subsequently overwritten by the remount. A new immediate
post-input assertion runs BEFORE the `waitFor` yields:
`expect(addedSections[1].querySelector("p")).not.toBeNull()`
plus `toContain("Ready to save.")` â€” the raw-source regression
collapses the `<p>` wrapper to a plain-text node on the same
tick, so reintroducing the assignment now fails the test on
the first assertion before the remount can paper it over.
Verified load-bearing by reintroducing the raw-source write in
the production code and observing the test fail, then
restoring. The matching P2 task in the Implementation Tasks
list is removed.

Also fixed in the current tree: the Settings dialog tab bar
(`ui/src/preferences/SettingsTabBar.tsx`) now implements the
WAI-ARIA tablist keyboard pattern. Roving `tabIndex` â€” only
the active tab carries `tabIndex={0}`, every other tab is
`tabIndex={-1}` â€” so `Tab` leaves the tablist after one stop
instead of visiting all seven tab buttons. `ArrowLeft` /
`ArrowRight` wrap around the tablist; `Home` / `End` jump to
the first and last tab. The handler lives on the tablist
wrapper and moves DOM focus imperatively via
`document.getElementById(\`settings-tab-${id}\`).focus()` so
selection and keyboard position stay in lockstep (a WAI-ARIA
protocol requirement). Click / Enter / Space still work via
native `<button>` semantics â€” only the keyboard navigation
surface changed. A new test file
`SettingsTabBar.test.tsx` (8 tests) pins roving tabindex,
Arrow wrapping at both ends, Home/End jumps, unrelated-key
pass-through, and click preservation.

Also fixed in the current tree: the duplicated
`if changed { publish_delta(DeltaEvent::SessionCreated { ... }) }`
block in `remote_create_proxies.rs` and `remote_codex_proxies.rs`
now routes through a single shared helper,
`AppState::announce_remote_session_created_if_changed` in
`src/sse_broadcast.rs`. Both proxy call sites collapsed from a
5-line block with a 5-line cross-reference comment to a single
method call. The helper takes `local_session: &Session` so the
caller can still move the owned `Session` into its
`CreateSessionResponse` without an extra clone outside the
`if changed` branch. The `remote_routes.rs` site that emits a
`SessionCreated` delta unconditionally was deliberately left
unchanged â€” it forwards an incoming SSE delta from a remote
and must announce even when the local record did not change,
so it has a different semantic from the two proxy sites. The
full remote test suite (73 tests) stays green.

Also fixed in the current tree: a 404 on `fetchSession` (the
lazy session-hydration path in `App.tsx`) no longer surfaces as
a user-visible request error toast. The hydration effect's
catch branch now special-cases `ApiRequestError` with
`status === 404` and calls
`requestActionRecoveryResyncRef.current()` without
`reportRequestError(error)` â€” matching the shape
`fetchWorkspaceLayout` already uses for 404. 404 is the benign
race where a session is deleted, hidden, or renumbered between
a delta event referencing it and the hydration fetch; the
action-recovery resync repairs the local view on the next SSE
tick without dropping a toast. The hydration effect only runs
when the backend emits `session.messagesLoaded === false`,
which is forward-compat scaffolding today (backend still
emits full transcripts), so the fix is a pre-landing correctness
improvement rather than an active-bug repair. A direct Vitest
test for the new 404 branch is tracked as a P2 task â€” it needs
a `messagesLoaded: false` fixture that the rest of the test
suite does not exercise today.

Also fixed in the current tree: the rendered-Markdown diff view
no longer smears a pre-fence line edit into the same added
block as a changed fence below it. Previously the
`pushChangedRange` heuristic in
`ui/src/panels/markdown-diff-segments.ts` only split into
pre-fence + fence-onwards pairs when the REMOVED side started
EXACTLY at a fence opener (`removedStartsWithFence`) â€” so a
change like `docs/mermaid-demo.md` (a blank line above a
Mermaid fence became "333", AND the fence's interior also
changed) rendered as one red block with the old diagram and
one green block with "333" + the new diagram smashed
together. The fix generalizes the heuristic to "both sides
have a fence in the changed range": emit pre-fence
removed/added as their own pair, then fence-onwards
removed/added as a second pair. The renderer's change-block
grouping breaks on the `added â†’ removed` transition between
the two pairs, so the view lands as two distinct visual
blocks â€” the user's pre-fence edit (e.g. green "333") on top,
the atomic fence swap (red old diagram â†’ green new diagram)
below. Five new Vitest cases in
`markdown-diff-segments.test.ts` pin the full fence-split
surface: (a) the primary regression â€” exactly one
`added â†’ removed` transition, no "333" bleed into the fence
segments, fence opener and interior all in the fence-onwards
segments; (b) multi-line pre-fence edits stay together and
land before the fence pair (via an explicit
`preFenceAddedIndex < fenceRemovedIndex` assertion so
reversing the emission order fails the test); (c) the
heuristic applies to any fence language â€” a TypeScript
fence with an intro paragraph added before it gets the same
treatment as Mermaid, proving the logic keys on `fenceBlock`
generally and not on a Mermaid-specific language tag;
(d) negative control: a pure fence-content change with no
pre-fence edit produces exactly `[removed, added]` non-normal
kinds â€” no spurious empty pre-fence segments from the new
code path; (e) line numbers (`oldStart` / `newStart`) on the
emitted segments stay correct after the split â€” the
`preFenceAdded.newStart`, `fenceRemoved.oldStart`, and
`fenceAdded.newStart` all match their 1-based document
positions so a future line-number-wiring regression would
show up here instead of as shifted labels in the diff
gutter. Four of five are verified load-bearing by reverting
the heuristic and observing the assertions fail; the fifth
is a stability invariant that passes both with and without
the fix and guards against future over-eager emission.

Also fixed in the current tree: the session-find index now
covers BOTH text variants of a `ConnectionRetryCard`. The
live-card detail (the stored `message.text`) and the resolved
card's synthesized past-tense copy ("Connection recovered",
"Connection dropped briefly; the turn continued after â€¦")
previously drifted apart â€” the index only carried the stored
text, so a search for terms visible only in the resolved card
missed the message, and a search for live-card terms could
land on a resolved card whose rendered copy didn't visibly
contain the query. A new `collectConnectionRetrySearchText`
helper in `ui/src/session-find.ts` mirrors the literal
strings `ConnectionRetryCard` renders and contributes them to
the text-message branch's searchable parts, so the index
answers "does this message match?" correctly regardless of
which card variant is rendered. The per-card highlight
positions are still computed at render time against each
variant's rendered text, so the card that is visible
highlights correctly without double-render. Comment in the
session-find helper flags the coupling to the rendered copy
â€” keep the two in sync. New Vitest case pins all three
matches (live detail, resolved heading, resolved synthesized
detail). Verified load-bearing by dropping the helper call
and observing the resolved-card assertions fail.

Also fixed in the current tree: the session-find active-hit
scroll no longer fights a user who scrolls away from the
current match. The `useLayoutEffect` in
`VirtualizedConversationMessageList.tsx` that pins
`scrollTop` to `activeConversationSearchScrollTop` now gates
the write on the active hit id changing
(`lastPinnedConversationSearchIdRef`): re-fires caused by
measurement churn (message-height recompute, a new card
streaming in, layout refinement) leave the user's scroll
position alone, while deliberate navigation to a different
match (via "next match" / "previous match" / a new query)
still pins. The ref resets to `null` on the early-return
branches (no active hit, search closed, query cleared) so a
later search landing on the same id still fires the pin.
Separately, `shouldKeepBottomAfterLayoutRef.current = false`
moved inside the `Math.abs(...) >= 1` guard so it only
clears the bottom-pin intent when this effect actually
overrides the viewport position â€” previously a no-op invocation
(scrollTop already at the target) still cleared the ref, so
closing search left sticky-bottom broken until the user
nudged the viewport. All 55 AgentSessionPanel tests still
green.

Also fixed in the current tree: the three read-first
check-then-maybe-mutate sites named in the
"`session_mut` helpers stamp eagerly" bug no longer stamp on
the no-op path. A new `StateInner::session_by_index(index)`
helper returns an immutable `&SessionRecord` without bumping
`last_mutation_stamp`, and the three callers now inspect via
the read-only helper before deciding whether to re-borrow
through `session_mut_by_index` for the mutation.
(1) `sync_session_cursor_mode` skips the stamp when the agent
doesn't support cursor mode or the mode already matches.
(2) `sync_session_agent_commands` skips when the incoming
command list is identical to the current one.
(3) `orchestrator_lifecycle.rs`'s stopped-session cleanup
checks for any orchestrator-sourced queued prompt first and
skips the whole `clear_stopped_orchestrator_queued_prompts`
call when there is nothing to clear; this also simplifies the
surrounding `changed` tracking (the len-diff check is no
longer needed since the pre-check establishes dirtiness by
construction). The two other
`clear_stopped_orchestrator_queued_prompts` sites
(`orchestrator_lifecycle.rs:431` inside the aborted-stop
cleanup loop and `orchestrator_transitions.rs:401` inside a
batched per-instance-session loop) were left unchanged â€” both
run inside flows where neighboring mutations on the same
sessions already bump the stamp, so the extra stamp on a
no-clear session is amortized rather than wasted. 450-test
backend suite green.

Also fixed in the current tree: the message-stack native-wheel
handler in `ui/src/SessionPaneView.tsx` now has direct
regression coverage. `App.test.tsx` has a new test
(`"registers the message-stack wheel listener as non-passive so
preventDefault takes effect"`) that wraps
`Element.prototype.addEventListener` globally during render,
captures every `"wheel"` registration with its options, locates
the one installed on `.workspace-pane.active .message-stack`
after `renderAppWithProjectAndSession` settles, and asserts
`{ passive: false }`. The `toBeDefined` on the registration
itself also catches the coarser regression â€” React's delegated
`onWheel` prop would install through a document-level handler
rather than on the message-stack node, so the filtered lookup
would return `undefined`. Verified load-bearing by omitting the
options argument (`node.addEventListener("wheel", listener)`)
and observing the `{ passive: false }` assertion fail.

Also fixed in the current tree: `SqlitePersistConnectionCache`
now invalidates the cached connection on any persist error.
`persist_delta_via_cache` wraps the transaction path in an
inner helper and drops the cached connection via
`SqlitePersistConnectionCache::invalidate` when that helper
returns `Err`. The next persist tick reopens fresh and re-runs
`ensure_sqlite_state_schema`. Without invalidation, a cached
connection poisoned by `SQLITE_BUSY` / `SQLITE_CORRUPT` / an
unlinked backing file / a Windows-side handle glitch would be
reused forever â€” every subsequent tick would log the same
error with no way to recover short of a backend restart. The
happy path still reuses one connection per process lifetime;
only the error path pays the cost of one reopen. A direct
regression test is tracked as a P2 â€” the entire SQLite cache
is `#[cfg(not(test))]`-gated (test builds use JSON persistence
instead), and exposing the cache to the test tree would be a
larger refactor than the Medium-severity fix itself.

Also fixed in the current tree: `saveError` visibility is no
longer over-gated by an informational `externalFileNotice`.
Previously the "Save failed: <reason>" diagnostic in
`DiffPanel.tsx` was gated on `!externalFileNotice &&
!diffEditConflictOnDisk`, so any notice â€” including purely
informational ones like "Rendered Markdown edits will save this
document to the worktree file." or "File reloaded from disk."
â€” suppressed the diagnostic. A save failure while such a
notice was visible produced the "Save failed" pill with no
explanation, the exact regression the diagnostic was added to
prevent. The gate is now narrowed to `!diffEditConflictOnDisk`
â€” the conflict path still renders its own recovery UI ("Apply
my edits to disk version" / "Save anyway" / "Reload from disk")
in place of the raw diagnostic, and the informational notice
now coexists with the diagnostic (the two stack). A new Vitest
case (`DiffPanel.test.tsx::"surfaces the save-error diagnostic
when an informational externalFileNotice is visible"`) pins the
contract: rendered-Markdown edit â†’ informational notice set â†’
save rejects with non-stale error â†’ assert "Save failed: ..."
diagnostic AND the notice are both present, and the stale-save
recovery UI is NOT (negative control). Verified load-bearing by
reverting the gate to the old form and observing the test fail.

Also fixed in the current tree: the only High-severity backend
bug â€” `commit_session_created_locked` performing synchronous
SQLite I/O under the state mutex â€” now routes through the
existing background persist channel. `src/sse_broadcast.rs`'s
`commit_session_created_locked` sends `PersistRequest::Delta`
instead of calling `persist_created_session` synchronously,
matching the pattern `persist_internal_locked` already used.
The state mutex is no longer held across a full SQLite
transaction (connection open, schema-ensure, metadata + session
upsert, commit with fsync), so other requests that need
`inner.lock()` no longer stall behind session-create disk I/O.
The crash-before-persist window loses at most a just-created
empty session shell â€” metadata + config, zero user content
(messages are always `[]` at commit time) â€” which is the same
durability posture `persist_internal_locked` already has for
every subsequent mutation. Test + shutdown fallback preserved:
when `persist_tx.send` fails (tests construct `AppState` with a
dropped-receiver channel; shutdown happens when the persist
thread exited early) the original synchronous path still runs.
A new co-located test in `src/tests/persist.rs` builds an
`AppState` with a LIVE persist-channel receiver, calls
`commit_session_created_locked` directly, and asserts both that
a `PersistRequest::Delta` was sent (primary assertion) and that
no synchronous persist-file write happened (negative control).
Verified load-bearing by reverting the channel-send and
observing the test fail.

Also fixed in the current tree: the synchronous persist path
(`persist_persisted_state_to_sqlite` in `src/persist.rs`) no
longer deep-clones every session transcript just to discard the
clones. The previous `persisted.clone(); metadata.sessions.clear();`
pattern paid a full `PersistedSessionRecord::clone` â€” and every
message inside it â€” per call; a new
`PersistedState::metadata_only()` helper clones only the
metadata fields and leaves `sessions: Vec::new()`, so the
caller avoids touching session transcripts at all and feeds the
original `&persisted.sessions` slice to
`persist_state_parts_to_sqlite` unchanged. This path is a
fallback (tests with a disconnected persist channel, plus
shutdown after the background persist thread has exited), so
the perf hit was silent but still wasteful on large
transcripts. All 32 persist tests pass.

Also fixed in the current tree: `load_state_from_sqlite` no
longer eagerly evaluates the legacy app-state lookup on the
happy path. `.or(sqlite_app_state_value(...)?)` forced both
`SELECT ... FROM app_state WHERE key = ?` round-trips to run on
every startup even though the fallback value is only needed
when the primary `SQLITE_METADATA_KEY` row is missing. The
lookup now uses an `if let Some(...) else if let Some(...) else`
chain so the legacy query only runs when the primary returns
`None` â€” silent cost on every startup was a second query
against a connection that's about to be dropped. The optional
follow-up from the bug entry (sharing the cached connection
across load/persist) is not done; it was explicitly marked
optional.

Also fixed in the current tree: the `/api/terminal` Unix spawn
path now uses `sh -lc` instead of `sh -c`, restoring login-shell
semantics. `build_terminal_shell_command` in `src/terminal.rs`
passes `-l` so `sh` sources `/etc/profile` and `~/.profile`
before executing the command â€” users who extend `PATH` from
those files (`nvm`, `uv`, `poetry`, pyenv, rbenv, Homebrew on
Apple Silicon, `cargo env`, gcloud shims, etc.) again see their
tooling resolve from the terminal panel the same way it
resolves from their desktop terminal. The change is a revert of
the incidental flag drop in commit `e208dde` ("Add terminal
command execution support"), which did not document a reason
for dropping `-l`. The Windows branch continues to use
`powershell.exe -NoProfile` â€” the tradeoff tilts differently on
Windows (PS profiles commonly do heavy per-invocation work;
`PATH` there comes from the registry, not the profile), and the
comment in `build_terminal_shell_command` explains the
asymmetry for future readers.

Also fixed in the current tree: a third small-items pass landed
two more. (1) The dialog-backdrop platform fallback is now
directly exercised â€” two new Vitest cases in
`ui/src/dialog-backdrop-dismiss.test.ts` delete
`navigator.userAgentData` and stub only `navigator.platform`,
verifying macOS and non-Apple detection both work through the
`navigator.platform` fallback branch (previously every test
wrote both values, leaving the Safari/Firefox-style branch
unpinned). (2) `markdown-diff-change-section.tsx::handleInput`
no longer assigns `event.currentTarget.textContent = segment
.markdown` in the read-only `allowReadOnlyCaret` branch â€” the
subsequent `onReadOnlyMutation()` remount already restores the
rendered DOM under React's control, and the raw-source-text
assignment was producing a one-frame "plain source flash" on
every disallowed read-only edit.

Also fixed in the current tree: a second small-items pass landed
four more. (1) `.mermaid-diagram-frame` in `ui/src/styles.css`
now sets `min-width: 0` (was `min-width: 100%`) so the iframe
can shrink below its intrinsic 4096-px width inside a flex
ancestor â€” fixes the "Mermaid iframe max-width: 100% defeated
by a flex ancestor" entry at its root. (2) The dialog platform
stub `afterEach` in `ui/src/dialog-backdrop-dismiss.test.ts`
and `ui/src/preferences/SettingsDialogShell.test.tsx` now deletes
the stubbed own `navigator.platform` property when there was no
original own descriptor â€” previously the stub could leak into
later tests on the same worker since jsdom inherits `platform`
from the prototype. (3) The "Flagship deadline-guard test
doesn't isolate the post-await check" entry is retired as
already-fixed: the main test was renamed to `"hard-cap
\`stopped\` flag bails an in-flight await when the cap
setTimeout fires"` and the post-await deadline check is
covered independently by `"post-await \`now() >= deadlineMs\`
check bails when the injected clock passes the deadline"` â€”
both with clear docstrings.

Also fixed in the current tree: four small ledger-polish items
landed together. (1) `MonacoCodeEditor.tsx`'s
`computeInlineZoneStructureKey` doc comment and
(2) `DiffPanel.tsx`'s `commitRenderedMarkdownDrafts` doc comment
no longer reference-by-title the active `docs/bugs.md` headings
they replaced â€” both comments now describe the invariant in
situ so they don't rot when the matching ledger entry moves to
preamble. (3) The scratch `docs/math-demo.md` edit left over
from mid-session debugging has been reverted (it was never an
intentional fixture update); the remaining `docs/mermaid-demo.md`
fixture edit is tracked below until it is either reverted or
documented as intentional. (4) The
`MarkdownDocumentEolStyle` / `detectMarkdownDocumentEolStyle` /
`applyMarkdownDocumentEolStyle` docs in
`markdown-diff-segments.ts` now describe the contract accurately
â€” "detected dominant EOL style preserved across the round-trip",
not "original CRLF/LF mix preserved". Homogeneous inputs still
round-trip byte-exact; heterogeneous inputs are normalised to
the dominant style (documented trade-off, not a bug).

Also fixed in the current tree: React dep-array hygiene across
three hot effects + timers. (1) `App.tsx`'s session-hydration
`useEffect` dropped `activeSession?.messages.length` from its
deps â€” the body reads only `activeSession?.id` and
`activeSession?.messagesLoaded`, so streaming tokens no longer
re-trigger the effect just to hit the "already hydrated /
already hydrating" early-returns. (2) `message-cards.tsx`'s
Markdown line-marker `useEffect` dropped `documentPath`,
`hasOpenSourceLink`, `workspaceRoot` from its deps â€” those
affect the `<a>` renderer (href resolution, click handlers), not
the `[data-markdown-line-start]` attributes the ResizeObserver
re-queries. Tearing down + rebuilding the observer on unrelated
context changes is now avoided. (3) `active-prompt-poll.ts`'s
belt-and-suspenders hard-cap `setTimeout` is now cleared when
the chain self-stops via `shouldStop` / deadline-lapsed (not
just via external `cancel()`). Extracted a shared `stopPoll()`
helper that clears BOTH the chained and hard-cap timers and is
called from every self-stop site; previously a completed prompt
left a pending timer slot for up to 5 minutes. New co-located
test in `active-prompt-poll.test.ts` uses `vi.getTimerCount()`
to verify zero pending timers after a `shouldStop` exit â€”
verified load-bearing by temporarily dropping the shared
`stopPoll()` call from the `shouldStop` branch and watching the
test fail.

Also fixed in the current tree: the narrower startup hard-cap
timer edge in `startActivePromptPoll` is now guarded too. The
first synchronous `schedule()` call can self-stop before
`hardCapTimerId` has been assigned, so the post-`schedule()`
hard-cap setup now checks `!stopped` before arming the timer.
`active-prompt-poll.test.ts` covers the `isMounted: () => false`
startup path and asserts `vi.getTimerCount()` stays at zero.

Investigated and closed (not the claimed bug): the "`MessageCard`
default-prop inline arrows defeat memoization" entry was based
on a misdiagnosis. React's `memo` comparator receives the RAW
props object as passed by the parent, NOT the destructured
values â€” so an optional prop the parent omits reads as
`undefined` on both `prev` and `next` and passes the `===`
identity check cleanly without any help from stable defaults.
Verified empirically by temporarily reverting the "fix" and
watching the regression test still pass.

Also improved (on code-quality grounds, not a correctness fix):
the two no-op defaults on `MessageCard` (`onMcpElicitationSubmit`,
`onCodexAppRequestSubmit`) are now hoisted to module-scope
constants (`NOOP_MCP_ELICITATION_SUBMIT`,
`NOOP_CODEX_APP_REQUEST_SUBMIT`) in `ui/src/message-cards.tsx`
â€” named defaults, reusable across future call sites, one fewer
arrow allocation per render. Paired with a co-located test
(`MarkdownContent.test.tsx::"skips re-rendering when a parent
re-renders with identical props and no optional callbacks"`)
that counts `parseConnectionRetryNotice` invocations across a
rerender to prove the memo DOES hit for the omitted-optional
case â€” a forward-looking regression guard for the memo
comparator itself (e.g., a future change that forgets to
compare a new prop) rather than for the cosmetic cleanup
itself.

Also fixed in the current tree: inline-zone id stability is now
directly exercised. Four new Vitest cases in
`ui/src/panels/SourcePanel.test.tsx::"inline-zone id stability"`
pin the current contract across Markdown-fence and `.mmd`
whole-file ids â€” same-span-and-body â†’ stable, body-edit â†’
flipped, insert-above â†’ flipped today (the latent gap is
tracked separately as an active bug entry with fix proposals),
`.mmd` any-edit â†’ flipped (intentional whole-file hashing).

Also fixed in the current tree: MonacoCodeEditor's inline-zone
`ResizeObserver` no longer disconnects and rebuilds on every
keystroke. Previously the observer's `useEffect` depended on
`inlineZoneHostState`, which the `[inlineZones]` effect rewrites
with a fresh array on every prop change (and `inlineZones` itself
is rebuilt on every keystroke via `SourcePanel`'s
`renderableRegions` memo, whose deps include `editorValue`). The
observer was then torn down and re-built on every keystroke â€”
correctness-preserving (diagram DOM survived via stable ids +
portal key), but wasteful at O(zones) per keystroke. The fix
extracts `computeInlineZoneStructureKey(hosts): string` â€” a pure
function that joins the hosts' ids with `\n` â€” and depends the
observer effect on that string instead. Same ids in the same
order â†’ same string â†’ `Object.is` passes â†’ effect body is
skipped. Any zone added, removed, or re-hashed (the
`mermaid:start:end:hash` / `mermaid-file:hash` id format in
`source-renderers.ts` bakes a content hash into the id) flips the
key and correctly rebuilds the observer. The portal-render path
still writes `setInlineZoneHostState` unconditionally so
appearance / workspaceRoot changes flow through to fresh
`zone.render()` closures â€” only the observer is de-churned.
Unit coverage in `ui/src/MonacoCodeEditor.test.ts` pins the
key-derivation contract: structural equality in â†’ equal strings
out (verified via `Object.is`), empty handled, add / remove /
reorder / id-change all flip the key.

Also fixed in the current tree: the Monaco inline-zone helper
coverage file is now part of the change set instead of an
untracked local file. The `ui/src/MonacoCodeEditor.test.ts`
coverage claim above now matches the files that will ship with
the Monaco observer-key change, rather than depending on a
forgotten working-tree-only test.

Also fixed in the current tree: the generated Vitest cache file
is no longer dirty in the working tree. `git status --short`
does not show the prior
`node_modules/.vite/vitest/.../results.json` metadata change, so
the active tracker entry was stale and has been removed.

Also fixed in the current tree: MonacoCodeEditor inline-zone
portal children are now wrapped in an `InlineZoneErrorBoundary`
class component. A throw inside the zone's `render()` callback â€”
the common failure modes are a malformed Mermaid fence, a KaTeX
parse error that escapes `throwOnError: false`, or any other
synchronous render exception from the MarkdownContent subtree â€”
no longer unmounts the whole Monaco editor. The boundary catches
the error, logs it (with the zone id) to the dev console for
diagnosability, and renders a compact fallback notice
("Diagram failed to render â€” view the source below for details."
via `role="status"` + `aria-live="polite"`). The source text and
the rest of the editor stay mounted; the user's unsaved buffer
is preserved. The boundary resets its error state when the
portal's `zoneId` changes so a new zone gets a clean render
attempt. Co-located tests in `ui/src/InlineZoneErrorBoundary.test.tsx`
cover: happy-path pass-through, catch-then-fallback, error-log
contract (zoneId + Error instance), sibling-isolation (a bad
zone does not take down a sibling), zoneId-reset, and "same
zoneId stays in error state".

Also fixed in the current tree: the Rendered-diff fallback for
staged diffs no longer leaks worktree content when the backend
didn't supply `documentContent`. `renderedDiffAfterContent` in
`DiffPanel.tsx` now derives its fallback from a patch-only
`buildDiffPreviewModel(diff, changeType)` call (no `latestFileContent`
argument, so the `expandDiffPreviewToWholeFile` expansion is
skipped and `modifiedText` is built from the hunk rows alone). The
Rendered preview's "Patch-only rendering" banner still warns that
the view is a best-effort approximation, but the content is now
faithful to the patch's after-side regardless of whether the
worktree carries unrelated unstaged edits. A new Vitest case pins
this: a staged diff whose worktree (via `fetchFileMock`) contains
an unrelated `X --> Y` mermaid node, where the patch's after-side
is `flowchart LR`, asserts the mermaid renderer saw only
`flowchart LR` and never `X --> Y` or the worktree's `flowchart TD`.

Also fixed in the current tree: `handleApplyDiffEditsToDiskVersion`
no longer silently continues past a failed rendered-Markdown draft
commit. `commitRenderedMarkdownDrafts` now returns a boolean
(`true` when the flushed drafts were applied cleanly or when there
was nothing to flush; `false` when `handleRenderedMarkdownSectionCommits`
rejected the batch for unresolvable or overlapping commits), and the
apply-to-disk handler captures that via
`flushSync(() => commitRenderedMarkdownDrafts())` and short-circuits
with a dedicated `externalFileNotice("Resolve rendered Markdown
conflicts before applying edits to the disk version.")` before
touching `fetchFile` or rebase. Callers that ignore the new boolean
(Save / reload / navigation flows) continue to treat the function
as a best-effort flush and surface errors through the existing
`saveError` banner â€” backward compatible. A new Vitest case pins
the empty-commits `return true` path (verified load-bearing by
flipping it to `false` and watching the test fail): a
rendered-Markdown save-then-apply-to-disk-version scenario where
`handleSave`'s own internal commit already drained the draft so
the apply-time flushSync has no work. The
success-with-commits and conflict-short-circuit branches are
tracked as a P2 task below â€” engineering those states reliably
through a React integration test is non-trivial.

Also fixed in the current tree: the `isSafePastedMarkdownHref`
Windows drive-letter exception is removed. Paste-sanitize no longer
short-circuits `/^[a-zA-Z]:[\\/]/` to `true`; drive-letter hrefs now
fall through the normal protocol-allowlist check and are rejected
because `c:`, `d:`, etc. are not in the `http`/`https`/`mailto`
allowlist. The `<a>` element itself stays (it's in ALLOWED), so its
link text survives as plain content, but the `href` attribute is
stripped â€” no latent path-handler-invocation risk if TermAl ever
wraps in Tauri/Electron. Local-path file links that the user types
or authors continue to work through
`markdown-links.ts::resolveMarkdownFileLinkTarget`, which has its own
allowlist for drive-letter paths; only the paste-sanitize entry point
is tightened. Test file flipped the six drive-letter rows from
`toBe(true)` to `toBe(false)`, consolidated the bare-drive-letter
and two-letter-lookalike cases into the same `it.each` table, and
added a direct sanitizer test proving `<a href="C:\Windows\System32\cmd.exe">`
loses its href while the `<a>` element and link text survive.

Also fixed in the current tree: the `markdown-diff-edit-pipeline` paste
sanitizer now has direct Vitest coverage. A new
`ui/src/panels/markdown-diff-edit-pipeline.test.ts` pins the
`isSafePastedMarkdownHref` contract (empty/whitespace â†’ reject;
`javascript:` / `vbscript:` / `data:` / `file:` / `ftp:` / `blob:` /
other non-allowlisted protocols â†’ reject; mixed-case + control-byte
obfuscation like `java\u0000script:` â†’ reject after normalisation;
`http` / `https` / `mailto` â†’ accept; drive-letter paths accept per
current contract, with the tighter-allowlist follow-up tracked
separately), and it covers `sanitizePastedMarkdownFragment`'s three
gates end-to-end: every element in the 24-entry drop set is verified
removed (scripts, iframes, forms, buttons, svg, math, option,
audio/video, etc.), every element in the 31-entry allow set survives,
unknown tags (`<section>`, `<mark>`, `<article>`, `<details>`,
`<font>`, etc.) are unwrapped with their children preserved, and
attribute scrubbing keeps only safe `href` on `<a>` plus normalised
`language-*` class on `<code>` â€” `onclick` / `onmouseover` / `style` /
`data-*` / `id` are all stripped everywhere. Four smoke tests exercise
`insertSanitizedMarkdownPaste` end-to-end, confirming the
sanitize-before-insert ordering means no dropped tags ever reach the
section DOM.

Also fixed in the current tree: rendered-Markdown commits on CRLF-on-disk
documents no longer silently convert the whole buffer to LF on save. Two
new helpers (`detectMarkdownDocumentEolStyle` and
`applyMarkdownDocumentEolStyle`) in `ui/src/panels/markdown-diff-segments.ts`
capture the original EOL style at the source-content boundary and re-apply
it after the segment math runs on LF, so `handleRenderedMarkdownSectionCommits`
now round-trips CRLF â†’ LF â†’ CRLF transparently. A new Vitest integration
case loads a CRLF README, edits a rendered section, saves, and asserts the
`onSaveFile` payload still has CRLF everywhere; unit tests pin the
detection (pure LF, pure CRLF, mixed CRLF/LF dominant, ties â†’ LF, bare
`\r` legacy Mac ignored) and application (empty strings, LF identity,
CRLF expansion, round-trip invariant) contracts.

Also fixed in the current tree: the focused `AgentSessionPanel` suite is
green again. The current transcript/page-band virtualization contract now
passes `ui/src/panels/AgentSessionPanel.test.tsx` end to end, so mounted-range
reconcile, startup resync, seek, idle compaction, and footer coverage are back
to being a real release gate instead of a red known bug.

Also fixed in the current tree: stale fork recovery now opens on the later
authoritative state instead of stalling after the first empty recovery
snapshot. `ui/src/App.session-lifecycle.test.tsx` now keeps both the stale
create and stale fork deferred-open cases green, including the later-SSE
adoption path.

Also fixed in the current tree: the session composer no longer rerenders on
assistant-only live-session churn. `SessionComposer` now compares only
composer-relevant session fields plus user prompt history instead of raw
`Session` identity, and `ui/src/panels/AgentSessionPanel.test.tsx` pins that
assistant preview/output updates do not recompute the slash palette while a
draft is in progress.

Also fixed in the current tree: `adoptSessions(...)` no longer fans every
authoritative snapshot through the full cleanup cascade. The current
`ui/src/app-live-state.ts` change now gates `setSessions(...)`, per-session
flag pruning, agent-command invalidation, and unknown-model confirmation
cleanup behind explicit membership/workdir/key-set changes, so ordinary live
session churn stops revalidating unrelated per-session stores on every state
adoption turn. The focused live-state/lifecycle suites stayed green
(`App.session-lifecycle`, `App.live-state.reconnect`,
`App.live-state.visibility`, `App.live-state.watchdog`,
`session-reconcile`), and a follow-up active-session typing profile dropped
the sampled `TaskDuration` from about `1.45 s` to `0.29 s`, `ScriptDuration`
from about `0.335 s` to `0.007 s`, and the worst keystroke frame from about
`34.7 ms` to `13.3 ms`. The broader whole-tab adoption/render churn bug
remains open below.

Also fixed in the current tree: `/api/state` success responses now take a
JSON-first fast path. `ui/src/api.ts::request(...)` sniffs the `Content-Type`
via a new `isJsonResponseContentType(...)` helper (which accepts
`application/json`, `application/json; charset=...`, and RFC 6838 `+json`
structured suffixes like `application/problem+json`) and hands the healthy
path directly to `response.json()`. The old `response.text()` +
`looksLikeHtmlResponse(...)` + `JSON.parse(...)` chain now runs only when
the content type is missing or suspicious, or when `response.json()` throws
— `looksLikeHtmlResponse(...)` itself was also narrowed to a bounded
256-character prefix probe. `ui/src/api.test.ts` pins the fast path: the
text clone stays unconsumed on healthy `application/json` and
`application/json; charset=utf-8` responses, and malformed JSON still
routes through the HTML-fallback detection. Two residual follow-ups (the
256-char slice dropping leading-whitespace tolerance in
`looksLikeHtmlResponse`, and the unconditional `response.clone()` that
buffers the body a second time on every successful response) are tracked
as their own bug entries below.

Also fixed in the current tree: targeted remote hydration no longer accepts
strictly-newer per-session responses on the delta path. `hydrate_remote_session_target`
now hard-rejects `remote_response.revision != min_revision` for the
delta-fast-path and routes the GET-hydration path through a follow-up
`/api/state` synchronization that advances the per-remote watermark. Same-stamp
metadata-only summaries are also gated by `session_mutation_stamp` so cached
transcripts only survive when both count and freshness marker agree. New
`src/tests/remote.rs` regressions cover the strict-equality rejection and the
stamp-aware preservation/demotion split.

Also fixed in the current tree: GET-path remote session hydration no longer
marks an older transcript loaded after a newer `/api/state` summary has advanced
the remote watermark. If the synchronized state revision is newer than the
single-session response and the current summary metadata does not match, the
route rejects the stale transcript after committing the broad state mutation.
The missing-target exit also commits the already-applied broad state before
returning `404`. A focused Rust regression covers revision N session hydration
racing revision N+1 state for the same session and asserts the newer summary
remains metadata-only.

Also fixed in the current tree: targeted remote hydration now treats a replayed
same-revision session-scoped delta as already reflected when the loaded proxy
transcript has the same `messageCount` and `sessionMutationStamp`. This keeps
valid same-revision delta sequences possible while preventing duplicate
non-idempotent replays after a targeted full-transcript repair. The remote delta
handlers now also preserve an existing cached `sessionMutationStamp` when an
incoming payload omits the optional stamp. A Rust regression covers a replayed
`TextDelta` after targeted hydration and asserts the text is not appended twice.

Also fixed in the current tree: `get_session()` no longer relies on
`unreachable!()` in the request-handler path. The lock section now returns an
explicit `Ready(SessionResponse)` or `HydrateRemoteProxy` plan and the outer
code exhaustively matches it, so a future control-flow refactor returns an
`ApiError` path instead of introducing a panic.

Also fixed in the current tree: SourcePanel rendered-Markdown commits now keep
the source snapshot they were based on, propagate commit failures through the
shared `onCommitDrafts` / `onCommitSectionDraft` boolean contract, and clear
child draft refs only after the parent accepts the commit. Split-mode rendered
drafts can no longer overwrite concurrent code-pane edits, Ctrl-S no longer
saves after a failed rendered-draft commit, and blur-time failures keep the
user's draft visible with an action error. SourcePanel instances are now keyed
by active source tab and clear rendered-Markdown committers on document-path
change, so stale preview drafts cannot leak across source-file switches.
Focused SourcePanel regressions cover stale split drafts and failed Ctrl-S.

Also fixed in the current tree: editable rendered-Markdown drops now prevent
the browser default in edit mode and route `text/html`, `text/plain`, and
`text/uri-list` payloads through the same sanitizer used for paste. The
Markdown serializer also re-checks anchor hrefs with
`isSafePastedMarkdownHref` before emitting `[text](href)`, so an unsafe anchor
that reaches the DOM serializes as plain text instead of
`javascript:`-bearing Markdown. Pipeline tests cover serializer
defense-in-depth, and SourcePanel coverage drops active markup plus an image
payload and asserts the saved source contains only safe Markdown.

## Markdown href serialization can break out of link syntax

**Severity:** High - safe-scheme hrefs can still persist dangerous Markdown if
the destination is not escaped before serialization.

`serializeMarkdownInlineNode` now gates anchor hrefs through
`isSafePastedMarkdownHref`, but it still emits `[text](href)` with the raw
destination. A DOM anchor whose href starts with an allowed scheme can contain
Markdown syntax such as `)` followed by a second link, for example
`https://example.com/) [x](javascript:alert(1)`. The scheme allowlist accepts
the destination because it starts with `https:`, then serialization writes a
second dangerous Markdown link to disk.

**Current behavior:**
- The serializer accepts `http`, `https`, `mailto`, and no-colon hrefs.
- Accepted hrefs are interpolated into Markdown link syntax without escaping
  link-destination metacharacters.
- A safe-scheme href can break out of the destination and introduce a second
  Markdown link.

**Proposal:**
- Escape Markdown link labels and destinations before emitting `[text](href)`,
  or conservatively reject destinations containing whitespace/control chars,
  brackets, parentheses, or angle brackets.
- Add a regression that builds a DOM anchor with a safe-scheme breakout href
  and asserts serialization emits safe text or a single safe link only.

## DiffPanel save ignores failed rendered-Markdown draft commits

**Severity:** Medium - toolbar or parent saves can proceed after a rendered
Markdown draft failed to apply.

`DiffPanel.tsx:959` calls `commitRenderedMarkdownDrafts()` from `handleSave`
but ignores the boolean return. The SourcePanel save path now aborts on a
failed rendered-draft commit; DiffPanel still continues into the file save
flow. If range resolution fails, the user can see an unresolved rendered draft
and an error while the underlying buffer save still proceeds.

**Current behavior:**
- `commitRenderedMarkdownDrafts()` returns `false` when a rendered draft cannot
  be applied.
- `DiffPanel.handleSave()` ignores that return value.
- Save can continue with the current text buffer while a rendered draft remains
  unresolved.

**Proposal:**
- Gate `handleSave` with `if (!commitRenderedMarkdownDrafts()) return;`,
  matching SourcePanel and the Ctrl-S rendered editor contract.
- Add a DiffPanel regression where rendered-draft range resolution fails and
  assert `onSaveFile` is not called.

## Rendered Markdown commit lifecycle fields lack contract documentation

**Severity:** Low - exported commit fields carry ordering-sensitive behavior
without a nearby contract comment.

`RenderedMarkdownSectionCommit` now includes `allowCurrentSegmentFallback` and
`onApplied`. `allowCurrentSegmentFallback=false` disables a range-resolution
fallback that is unsafe for SourcePanel full-document preview drafts, while
`onApplied` must run only after the parent has accepted the commit. The names
are not enough to make that lifecycle obvious to future callers.

**Current behavior:**
- `allowCurrentSegmentFallback` silently changes commit range-resolution
  semantics.
- `onApplied` performs child draft cleanup and must not run on rejected
  commits.
- The exported type has no field-level contract comment.

**Proposal:**
- Add a short comment on `RenderedMarkdownSectionCommit` documenting when the
  fallback may be disabled and that `onApplied` is a post-acceptance callback.
- Consider splitting the pure range payload from UI lifecycle callbacks if the
  type grows further.

## `SourcePanel` has two parallel dirty-state notification paths that can double-fire

**Severity:** Medium - `ui/src/panels/SourcePanel.tsx:155-156` and `:273-275`. The component derives `isDirty` from `isEditorDirty || hasRenderedMarkdownDraftActive` and propagates it via `useEffect(() => onDirtyChange?.(isDirty), [isDirty, onDirtyChange])`. The same `onDirtyChange?.()` is also called manually from `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionDraftChange`, `handleEditorChange`, and `handleRenderedMarkdownSectionCommits`. Manual calls inside a setState batch can produce flicker between dirty/clean states or hand the parent a stale value before the post-commit effect resolves.

**Current behavior:**
- Two notification mechanisms own the same contract: a `useEffect` keyed on `isDirty`, and ad-hoc imperative calls scattered through ~four handlers.
- Both fire on a typical edit/commit/save cycle.
- The `isDirty` derivation reads React state but the imperative calls compute dirtiness from `editorValueRef.current`/`fileStateRef.current`; the two sources of truth can drift inside the same component.

**Proposal:**
- Pick one mechanism: rely on the `[isDirty]` effect (drop the manual calls) OR drop the effect and call `onDirtyChange` only at known mutation sites with computed-from-refs values.
- Add a regression that asserts `onDirtyChange` is called exactly once per dirty/clean flip, not twice.

## `isProgrammaticBottomFollowScroll` discriminator may misclassify real user scrolls

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1470-1473`. The new branch in `syncViewport` distinguishes programmatic-bottom-follow scroll ticks from real user scrolls purely on (a) the cooldown window being live and (b) `node.scrollTop >= lastNativeScrollTopRef.current - 1`. There is no marker on the event itself. `markUserScroll` defends against this by clearing the cooldown on `wheel`, `touchmove`, and `keydown`, but trackpad inertial momentum events without a wheel event, or programmatic scrolls inside a ResizeObserver tick, can slip past — the user's downward inertial scroll would then be silently classified as a continuation of the programmatic follow and the user-scroll bookkeeping cleared.

**Current behavior:**
- The discriminator is purely positional + temporal; it cannot distinguish a real user scroll that happens to arrive without a wheel/touch/keydown predecessor from a continuation of the programmatic smooth-scroll.
- `markUserScroll` is wired to `wheel`, `touchmove`, and `keydown` only.

**Proposal:**
- Tighten the discriminator to also require `lastUserScrollInputTimeRef.current === Number.NEGATIVE_INFINITY` (the value `bottom_follow` event handler sets). A real user scroll between dispatch and completion sets `lastUserScrollInputTimeRef`, which then disqualifies subsequent ticks from the programmatic branch.
- Add coverage for the inertial-scroll-without-wheel-event case (e.g., synthetic `scroll` events without preceding `wheel`).

## Wheel `deltaMode` conversion lacks line/page branch coverage

**Severity:** Low - upward-wheel prewarm behavior depends on browser-specific
wheel delta units, but only pixel deltas are currently pinned.

`ui/src/panels/VirtualizedConversationMessageList.tsx` adds
`resolveWheelDeltaYPx` so upward-wheel prewarm can normalize pixel, line, and
page wheel deltas before projecting the next scroll position. The new coverage
exercises the pixel-delta path, but the `DOM_DELTA_LINE` and `DOM_DELTA_PAGE`
branches are untested even though those modes vary by browser and input device.

**Current behavior:**
- Pixel-delta upward-wheel prewarm is covered.
- Line-mode and page-mode wheel delta conversion can regress without a focused
  test failure.

**Proposal:**
- Extend the upward-wheel prewarm regression with `DOM_DELTA_LINE` and
  `DOM_DELTA_PAGE` cases.
- Assert the converted projected scroll position mounts earlier pages without
  stealing the user's scroll progress.

## Per-keystroke remount of editable Markdown preview's contentEditable subtree in Split mode

**Severity:** High - in Split mode, every Code-pane keystroke unmounts and remounts the rendered preview's contentEditable subtree. `ui/src/panels/SourcePanel.tsx:183-199` recomputes `renderedMarkdownSegment` on every `editorValue` change, producing a new `segment.markdown` string. The downstream `useEffect([segment.markdown])` at `markdown-diff-change-section.tsx:213-246` (when `hasUncommittedUserEditRef.current === false`) bumps `setRenderResetVersion(c => c + 1)`. The `<div key={renderResetVersion}>` at line 675 re-keys, fully unmounting and remounting `MarkdownContent` — including Mermaid sandbox iframes and KaTeX nodes — on every Code-pane keystroke.

The visible effects are: any text-selection in the rendered pane is destroyed on every keystroke, Mermaid iframes are re-initialised (the file's earlier comments call out inline-zone stability for diff-side editing but not for the Split-mode preview path), and KaTeX is regenerated. This also amplifies the cost of the documented callback-identity churn — every keystroke triggers the unstable-prop unregister/register cycle for committers.

**Current behavior:**
- `renderedMarkdownSegment` is keyed on `[editorValue, ...]`, so it recomputes per keystroke.
- The reset effect bumps `renderResetVersion` whenever `segment.markdown` differs from the previously-stored baseline, even when the only change is in Monaco.
- `<div key={renderResetVersion}>` causes a full subtree remount on every bump.

**Proposal:**
- Derive `renderedMarkdownSegment` from `fileState.content` (the on-disk baseline) and only update `segment.markdown` when the user crosses a save/blur/mode-switch boundary. Monaco edits still apply to the source buffer; the rendered preview shows the last "settled" Markdown until the next commit.
- Or compare incoming `segment.markdown` to the rendered DOM (or against a value-derived guard) before bumping `renderResetVersion`.
- Or make `MarkdownContent`'s rendering tolerant of in-place markdown updates without a key bump.
- Add a Split-mode regression that types into the Code pane and asserts the rendered preview's Mermaid iframe `src` (or a representative DOM-identity probe) is unchanged across keystrokes.

## `handleDrop` ignores drop coordinates and inserts at stale selection

**Severity:** Low - `ui/src/panels/markdown-diff-change-section.tsx:401-430`. The new `handleDrop` correctly prevents the browser default and routes payloads through `insertSanitizedMarkdownPaste`, but it ignores the drop coordinates (`event.clientX`/`clientY`). After `event.preventDefault()`, the browser does not move the caret to the drop point, and `insertSanitizedMarkdownPaste` operates on the current `window.getSelection()` (or appends to the end of the section when no selection lives inside it). The user perceives "I dropped here, content inserted somewhere else." Functionally safe — the sanitizer still runs — but UX-asymmetric.

The new SourcePanel drop test pre-calls `selectRenderedMarkdownSectionContents()` to seed a selection inside the section, so the test path always inserts where expected. Production drops without a prior selection in the section silently fall back to the end-of-section path.

**Current behavior:**
- `handleDrop` calls `insertSanitizedMarkdownPaste` without setting the selection to the drop coordinates.
- Insertion lands at the user's previous selection or appends to the end of the section.

**Proposal:**
- Use `document.caretPositionFromPoint(event.clientX, event.clientY)` (Firefox) or `document.caretRangeFromPoint(event.clientX, event.clientY)` (Chromium) to derive the drop point, set the selection to that range before calling `insertSanitizedMarkdownPaste`.
- Add a regression that drops without a pre-existing selection inside the section and asserts the content lands at the drop point, not at the end.

## `onApplied` clears uncommitted-but-equivalent drafts, causing contentEditable remount on round-trip

**Severity:** Low - `ui/src/panels/SourcePanel.tsx:386-391`. When `nextDocumentContentLf === sourceContent` (the user typed something that round-trips to the original after normalization), `onApplied` runs and calls `clearCommittedDraft`, which bumps `renderResetVersion`. The user's typed-but-equivalent text is replaced by the normalized base segment — DOM identity churns and any text selection is destroyed. Subtle UX echo of the previously-tracked "Per-keystroke remount in Split mode" finding, but on a narrower trigger.

**Current behavior:**
- A round-trip-equivalent edit triggers `onApplied`, which bumps `renderResetVersion` and remounts the contentEditable subtree.
- Visible text is unchanged, but DOM identity churns and selection state is lost.

**Proposal:**
- When `nextDocumentContentLf === sourceContent`, skip the `onApplied` calls (don't bump `renderResetVersion`) — just clear `hasRenderedMarkdownDraftActive`. The draft refs were already in a clean state in this branch; nothing else needs to reset.
- Add a regression that types `foo` then types backspaces back to the original, asserts the contentEditable's first child node identity is preserved across the round-trip.

## `renderedMarkdownDocumentPathRef` clear runs in `useEffect` rather than `useLayoutEffect`

**Severity:** Low - `ui/src/panels/SourcePanel.tsx:250-256`. The path-change clear runs post-paint via `useEffect`. With `<SourcePanel/>` now keyed by `tab.id ?? path` (the structural fix that landed alongside this), full remount is the primary defense and this clear-on-path-change effect is mostly redundant. But for the path-change-within-same-tab case (file rename keeping a stable tab id), the committers `Set` survives one paint after `fileState.path` changes. A `commitRenderedMarkdownDrafts()` triggered between path-change and the post-paint clear (e.g., via `flushSync(() => onCommitDrafts())` from a future `flushDraftsBeforeNavigation` handler) would still see the old committers, re-resolved against the new file's `editorValueRef.current`. Today the keying makes the window almost-always zero, so this is defense-in-depth.

**Current behavior:**
- The clear runs in `useEffect`, not `useLayoutEffect`.
- Same-tab path-change opens a single-paint window where stale committers can fire against the new file.

**Proposal:**
- Promote the clear to `useLayoutEffect` so it runs synchronously after the path change settles in render but before paint.
- Or rely entirely on the `key={...}` remount and drop the effect, since same-tab path renames are not a common code path today.

## `onCut` is a no-op when `canEdit=true`, symmetric to the `onDrop` finding

**Severity:** Medium - `markdown-diff-change-section.tsx:666` wires `onCut={handleReadOnlyMutationEvent}`, which only `event.preventDefault()`s when `allowReadOnlyCaret && !canEdit`. In editable mode the browser default cut path runs. Less severe than drop (no untrusted content enters the document), but the resulting clipboard payload is rich HTML — including any sandboxed iframe markup or rendered SVG nodes — and a subsequent paste in another app or another editable surface within TermAl runs through `insertSanitizedMarkdownPaste` only on paste, not on the source. Cut/copy of a rendered Mermaid diagram carries rendered HTML rather than the source `mermaid`-fenced Markdown the user likely expects.

**Current behavior:**
- `onCut` and `onCopy` (the latter not bound) follow the browser default in editable mode.
- The clipboard payload is rich HTML, not source Markdown.

**Proposal:**
- Implement explicit `onCopy`/`onCut` handlers that write the segment's source Markdown (or a serialized substring for partial selections) instead of relying on the browser default. Set `text/plain`, `preventDefault()`, and handle the DOM removal manually.
- Group with the `onDrop` fix so cut/copy/drop are all handled symmetrically.

## Pane-level smooth-scroll intermediate ticks cancel `settledScrollToBottom` RAF

**Severity:** Medium - the same root as "Pane-level bottom-follow state can detach during smooth scroll" with an additional symptom: `SessionPaneView.tsx:2518-2531` cancels `settledScrollToBottom` whenever `shouldStick` flips false. While the scrollbar smooth-animates toward the bottom, an intermediate tick at, say, `scrollTop = bottom − 200` trips `shouldStick = false`, which calls `cancelSettledScrollToBottom()` and clears `settledScrollToBottomCancelRef.current`. Any settled-scroll attempts queued for layout settling (e.g., when a streaming chunk's height was still measuring) are aborted mid-flight even though the user did nothing.

**Current behavior:**
- Pane-level `onScroll` cancels the settled-scroll RAF on every `shouldStick` flip.
- Smooth-scroll intermediate ticks satisfy the flip without the user scrolling.
- Settled-scroll attempts queued during layout settling are aborted.

**Proposal:**
- Gate `cancelSettledScrollToBottom()` on a real user-driven scroll signal (e.g., a recent wheel/keyboard/touch interaction within the last N ms), or on the same `pendingProgrammaticBottomFollowUntilRef` window the virtualizer uses.
- Cover the symptom in the regression for the parent "Pane-level bottom-follow" entry.

## `bottom_follow` cooldown can be implicitly cancelled by a concurrent `bottom_boundary` reveal

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:1470-1473`. If a user clicks "scroll to bottom" while a `bottom_follow` smooth-scroll is mid-animation, both cooldowns are pending. The bottom-boundary reveal mounts new pages, which can momentarily decrease `scrollTop` (because the bottom spacer changes when pages mount). Subsequent native scroll ticks then fail the `node.scrollTop >= lastNativeScrollTopRef.current - 1` test, the bottom_follow path defers, and the user-scroll bookkeeping path runs — flipping `hasUserScrollInteractionRef.current = true` and clearing `shouldKeepBottomAfterLayoutRef`. The `bottom_follow` cooldown is implicitly cancelled despite the user not scrolling.

The two cooldowns (`pendingBottomBoundaryRevealNodeRef` and `pendingProgrammaticBottomFollowUntilRef`) are independent ref clusters with no documented precedence rule. The `isBottomBoundaryRevealScroll` check at line 1468 takes priority over `isProgrammaticBottomFollowScroll` at line 1470 (the order in `if/else if`), but the bottom_follow handler at line 1610 doesn't reset the boundary's `pendingBottomBoundarySeekRef`/`pendingMountedPrependRestoreRef`, and the boundary handler at line 1590 doesn't reset `pendingProgrammaticBottomFollowUntilRef`. New scroll-kind variants will need explicit interleaving rules to avoid this drift.

**Current behavior:**
- Two independent cooldown ref clusters with no documented precedence.
- A `bottom_boundary` reveal can implicitly cancel an in-flight `bottom_follow` cooldown via spacer-driven `scrollTop` decrease.

**Proposal:**
- Document the precedence between `bottom_follow` and `bottom_boundary` cooldowns in the `message-stack-scroll-sync.ts` comment.
- When entering `bottom_boundary`, also reset `pendingProgrammaticBottomFollowUntilRef = NEGATIVE_INFINITY` so the priority is unambiguous.
- Add a regression that fires `bottom_follow` followed quickly by `bottom_boundary` and asserts neither cooldown's invariants are violated.

## `bottom_follow` programmatic-write doesn't own a mount strategy (smooth-scroll into spacer)

**Severity:** High - `ui/src/panels/VirtualizedConversationMessageList.tsx:1731-1746`. The `bottom_follow` branch arms the cooldown and resets bottom-stick state but never grows the mounted range. By contrast, `bottom_boundary` (lines 1711-1729) explicitly calls `mountBottomBoundary(node)` + `scheduleBottomBoundaryReveal(node)`, and the unspecified-scroll-kind branch calls `reconcileMountedRangeForNativeScroll`. `bottom_follow` only calls `scheduleProgrammaticViewportSync(node)`, which reads viewport state but does not grow the mounted band.

If the smooth scroll's target (`scrollHeight - clientHeight`) lies below the currently mounted bottom page, the smooth animation scrolls into the bottom spacer for the first ~1200ms until the cooldown ends and a non-`bottom_follow` reconcile path mounts the rest. `followLatestMessageForPromptSend` only takes the smooth path when `isMessageStackNearBottom()` is already true (so bottom is usually mounted), but `SessionPaneView.tsx:1847`'s `requestAnimationFrame` jump-to-bottom path takes `bottom_follow` unconditionally — the contract gap is silent. As more scroll kinds get added, the contract drifts further from "every kind owns its mount strategy."

**Current behavior:**
- The `bottom_follow` branch does not call any mount-growth helper.
- Smooth scrolls whose target lies in the bottom spacer animate into empty space until the cooldown ends.
- The contract that "every scroll kind owns its mount strategy" is undocumented.

**Proposal:**
- Either (a) document explicitly that callers must only dispatch `bottom_follow` when the bottom messages are already mounted, AND add a callsite-level guard in `followLatestMessageForPromptSend` / the bottom-follow effect; or (b) call `reconcileMountedRangeForNativeScroll(node, scrollDelta, "seek")` after setting the cooldown so the bottom band is mounted when the smooth animation lands.
- Add a regression where the bottom band is unmounted, `bottom_follow` is dispatched, and either the producer rejects the dispatch (variant a) or the bottom band is mounted before the animation completes (variant b).

## Wheel-prewarm bypasses `skipNextMountedPrependRestoreRef` first-upward-escape semantics

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1001-1039` (prewarm) × `:984-991` (incremental reconcile). `prewarmMountedRangeForUpwardWheel` unconditionally writes `pendingMountedPrependRestoreRef.current` and ignores `skipNextMountedPrependRestoreRef.current`. The native-scroll reconcile path runs after the prewarm (the wheel handler is synchronous; the `scroll` event fires next), sees the upward `nextRange`, consults `skipNextMountedPrependRestoreRef`, and consumes the flag — leaving the prewarm's prepend-restore intact. The "skip restore on first upward escape" intent (added precisely to avoid the layout-tick snap-back) is silently re-introduced for any wheel-driven upward gesture from the bottom.

**Current behavior:**
- Prewarm writes `pendingMountedPrependRestoreRef.current` regardless of the skip flag.
- Native reconcile then consumes `skipNextMountedPrependRestoreRef` without affecting the prewarm's already-set restore.
- First-escape upward wheel still queues a scrollHeight-delta restore.

**Proposal:**
- Have `prewarmMountedRangeForUpwardWheel` consult and consume `skipNextMountedPrependRestoreRef.current` symmetrically: if the flag is set, do not write `pendingMountedPrependRestoreRef`, and clear the skip flag.
- Add a regression that establishes "at bottom, then first upward wheel" and asserts no scrollHeight-delta restore is queued.

## `bottom_follow` programmatic-write branch doesn't clear `pendingMountedPrependRestoreRef`

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1731-1746`. Unlike the `bottom_boundary` branch directly above it (lines 1721-1722), the `bottom_follow` branch leaves `pendingMountedPrependRestoreRef.current` and `skipNextMountedPrependRestoreRef.current` intact. If an upward wheel just queued a prepend-scroll-restore (via `prewarmMountedRangeForUpwardWheel:1031-1034`) and the user immediately triggers a prompt-send that emits `bottom_follow`, the queued restore-ref fires later and snaps `scrollTop` backward to the pre-wheel position mid-smooth-scroll. The smooth-scroll-to-bottom then visually stutters or partially reverses.

**Current behavior:**
- `bottom_follow` write entry sets up the cooldown but does not reset prepend-restore refs.
- A queued upward-wheel restore can fire mid-bottom_follow and contradict the smooth-scroll target.

**Proposal:**
- In the `bottom_follow` branch, also set `pendingMountedPrependRestoreRef.current = null` and `skipNextMountedPrependRestoreRef.current = false`, mirroring `bottom_boundary`.
- Add a regression: queue an upward-wheel prepend-restore, dispatch `bottom_follow` immediately, and assert `scrollTop` does not regress backward.

## New top-spacer guard layout effect doesn't respect `pendingProgrammaticBottomFollowUntilRef`

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1171-1238`. The new "wheel-prewarm DOM-bounds coverage" `useLayoutEffect` checks `hasUserScrollInteractionRef.current` for cooldown gating but does not check `pendingProgrammaticBottomFollowUntilRef`. If a layout shift fires during an in-flight `bottom_follow` cooldown and `hasUserScrollInteractionRef.current` is set from a prior gesture, the top-spacer guard could prepend pages and call `setMountedPageRange` against a programmatic smooth-scroll, with `pendingMountedPrependRestoreRef` set to `node.scrollTop`. The next layout phase would then attempt a scroll-restore that contradicts the smooth-scroll target — cross-cooldown interference between two new this-round subsystems.

**Current behavior:**
- Top-spacer guard only consults `hasUserScrollInteractionRef`.
- Programmatic `bottom_follow` cooldown is invisible to the layout effect.

**Proposal:**
- Gate the layout effect's user-scroll cooldown check additionally on `pendingProgrammaticBottomFollowUntilRef.current < performance.now()`.
- Add a regression that runs a layout shift during an in-flight `bottom_follow` and asserts no prepend-restore is queued.

## Near-bottom early termination of `bottom_follow` cooldown can mid-animation hand control to user-scroll branch

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1611-1613`. The "near bottom shortcut" inside the `isProgrammaticBottomFollowScroll` branch unconditionally cancels the cooldown when any single intermediate native scroll tick lands near the bottom. But the smooth-scroll animation typically spans many ticks — an intermediate tick at, say, `scrollTop = scrollHeight - clientHeight - 60` (within the 72px near-bottom threshold) would terminate the cooldown midway. A subsequent tick at `bottom - 100` (still mid-animation) would then fall through to the "user scroll" branch (because the cooldown is now expired) and flip `hasUserScrollInteractionRef.current = true`. This re-introduces the same class of bug the cooldown was added to prevent.

**Current behavior:**
- A single near-bottom intermediate tick cancels the cooldown.
- Subsequent ticks (still mid-animation) are reclassified as user scrolls.

**Proposal:**
- Replace the early termination with a check that the node is BOTH near-bottom AND the scroll has stopped (delta < 1 vs `lastNativeScrollTopRef.current`).
- Or remove the branch entirely and let the cooldown's natural 1.2 s timeout drain.
- Add a regression that fires intermediate ticks crossing the near-bottom boundary and asserts the cooldown survives until either timeout or stopped-scroll.

## `resolveWheelDeltaYPx` constants undocumented; touch input is silently not pre-warmed

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:173-181` (`resolveWheelDeltaYPx`) hardcodes `40` for `DOM_DELTA_LINE` and `DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT` (720) for `DOM_DELTA_PAGE` fallback with no inline comment explaining the values. Real browser/OS line-mode multipliers vary considerably (Chromium ~40, Firefox 16-20 on Linux). The constant is load-bearing for the prewarm projection — too low and trackpad inertial scrolls in LINE mode will under-grow the band; too high and a small line-mode scroll over-grows the mounted range and forces unnecessary work.

Separately, the wheel handler at `:1700-1702` is the only input modality that drives `prewarmMountedRangeForUpwardWheel`. `markUserScroll` is bound to `wheel`, `touchmove`, and `keydown`, but only the wheel handler does prewarm. Touch users (mobile/iPad) get the spacer-blank that wheel users no longer see, and touch typically produces LARGER per-gesture deltas than wheel — so the spacer-exposure window is potentially worse on touch.

**Current behavior:**
- `resolveWheelDeltaYPx` constants are bare with no doc comment.
- Only wheel events trigger prewarm; touch and keyboard do not.
- The layout-effect DOM-bounds guard at lines 1171-1230 is the implicit fallback for non-wheel inputs but its asymmetric role is undocumented.

**Proposal:**
- Add a `///`-style comment documenting `resolveWheelDeltaYPx`'s constants as deliberate over-estimates that match Chromium's typical wheel-line scroll amount; over-projection only widens the prewarmed band.
- Either (a) extend the prewarm to read `touchmove` deltas (track previous touchY, compute deltaY) and pre-warm symmetrically, or (b) document explicitly that the layout-effect DOM-bounds guard at lines 1171-1230 handles touch as a fallback.

## New `AgentSessionPanel.test.tsx` test scaffolding has hardening gaps

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx` (NEW this round, +211 lines). Two small hardening gaps:

1. `:2018-2024` — `ResizeObserverMock.observe` synchronously invokes the callback during `observe(target)`. Other tests in this file use a deferred-callback shape via `resizeCallbacks` map (see line 2224 onward). A synchronous mock that schedules a `setState` mid-effect can produce a "Cannot update a component while rendering a different component" warning under certain React 18 reconciliation orderings, especially if tests are later parallelized.
2. `:2214-2215` — The `finally` block restores `Element.prototype.getBoundingClientRect = originalGetBoundingClientRect` directly. If `originalGetBoundingClientRect` is `undefined` (the type signature allows it), this would set the prototype property to `undefined`, breaking subsequent tests' `getBoundingClientRect` calls. The staged `App.scroll-behavior.test.tsx` uses `Object.defineProperty(...)` with original-descriptor checks; the same pattern would harden this test.

**Current behavior:**
- `ResizeObserverMock` invokes its callback synchronously inside `observe()`.
- `finally` restores `getBoundingClientRect` via plain assignment without descriptor check.

**Proposal:**
- Use the existing `resizeCallbacks` deferred-callback shape from the sibling tests instead of synchronously invoking the callback.
- Capture the descriptor with `Object.getOwnPropertyDescriptor` and conditionally restore (or use `Object.defineProperty` when the original was missing).

## `registerRenderedMarkdownCommitter` lacks idempotency guard

**Severity:** Low - `SourcePanel.tsx:300-305`. Adds to the live ref Set with no idempotency guard. A section that re-runs its register effect (which it does on every parent render — see "Inconsistent `useCallback` discipline" entry) will momentarily have two committer closures registered in the brief window between adding the new closure and the cleanup running. `collectRenderedMarkdownCommits` invokes BOTH; each closure inside the section calls `collectSectionEdit(section)` against the same DOM section. The first one mutates `hasUncommittedUserEditRef.current = false` and clears `draftSegmentRef.current`; the second sees the cleared state and returns `null`. Today benign, but order-sensitive — a regression here could re-introduce the previously-flagged "double-fire" parent dirty notification.

**Current behavior:**
- Set-based registration with no dedup.
- Transient duplicate registrations during effect cycles.

**Proposal:**
- Key by a stable id (e.g., the section's `useId()` value) so duplicate registrations dedupe.
- Or track a registration count and warn in dev when more than one committer is alive for the same section.

## `SourcePanel.tsx` is growing along a separable axis

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` grew from ~803 to 1119 lines in this round (+316). It is approaching but has not crossed the ~2,000-line scrutiny threshold. The new responsibility (rendered-Markdown commit pipeline orchestration: collect → resolve ranges → check overlap → reduce edits → re-emit with EOL style) is meaningfully separable from the existing source-buffer/save/rebase/compare orchestration. It has its own state (`hasRenderedMarkdownDraftActive`, `renderedMarkdownCommittersRef`), pure helpers already split into `markdown-commit-ranges`/`markdown-diff-segments`, and a clean parent-callback interface.

**Current behavior:**
- SourcePanel owns two distinct orchestration responsibilities in one component.

**Proposal:**
- No action this commit. Consider extracting a `useRenderedMarkdownDrafts(fileStateRef, editorValueRef, setEditorValueState, ...)` hook in a follow-up, owning `renderedMarkdownCommittersRef`, `hasRenderedMarkdownDraftActive`, `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleRenderedMarkdownSectionDraftChange`.
- The hook would expose a small surface for SourcePanel to consume and keep the file under the scrutiny threshold.

## Inconsistent `useCallback` discipline on `SourcePanel` handlers crossing the prop boundary

**Severity:** Low - `ui/src/panels/SourcePanel.tsx`. `commitRenderedMarkdownDrafts`, `commitRenderedMarkdownSectionDraft`, `handleRenderedMarkdownSectionCommits`, `handleRenderedMarkdownSectionDraftChange`, `handleRenderedMarkdownReadOnlyMutation`, `handleSelectDocumentMode`, and `handleEditorChange` are plain function declarations recreated on every render. The sibling `registerRenderedMarkdownCommitter` is correctly wrapped in `useCallback` (line 305). All cross the prop boundary into `EditableMarkdownPreviewPane` / `EditableRenderedMarkdownSection`. Combined with `normalizeMarkdownDocumentLineEndings(editorValue)` being recomputed twice per render in JSX (lines 843-870, 891-906), the editable contentEditable subtree receives shifting prop identities on every parent render.

This is the exact regression the React review checklist warns about — complex component trees with inline `components` props don't survive re-renders. `EditableRenderedMarkdownSection` does have its own internal `previousSegmentMarkdownRef`/`renderResetVersion` machinery to absorb this, but stable inputs help.

**Current behavior:**
- Inconsistent stabilization: one handler `useCallback`-wrapped, six others not.
- `normalizedEditorValue` recomputed twice per render at JSX call sites.
- Editable preview pane sees fresh prop identities on every parent render.

**Proposal:**
- Wrap the prop-crossing handlers in `useCallback` with the right deps.
- Compute `normalizedEditorValue` once at the top via `useMemo([editorValue])` and reuse in both call sites.
- Or document why identity stability is unnecessary if `EditableRenderedMarkdownSection` is robust to inline-handler thrash.

## Editable rendered Markdown surface lacks textbox semantics

**Severity:** Low - keyboard and screen-reader users can focus an editable
rendered Markdown region without an accessible control role or name.

`ui/src/panels/SourcePanel.tsx:988` labels the preview wrapper as
`aria-label="Rendered preview"`, but the actual focusable
`contentEditable` section created by `EditableRenderedMarkdownSection` does not
receive a textbox role or accessible name. Once the rendered preview becomes an
editing surface, the focus target should announce itself as editable content
rather than as an unnamed generic element.

**Current behavior:**
- The wrapper has an accessible label.
- The contentEditable child is the focusable/editable element.
- The child lacks `role="textbox"`, `aria-multiline`, and a specific label.

**Proposal:**
- Extend `EditableRenderedMarkdownSection` with an editable-only
  `aria-label` prop.
- When `canEdit` is true, set `role="textbox"` and
  `aria-multiline="true"` on the contentEditable target.
- Add a focused React Testing Library assertion for the editable rendered
  preview role/name.

## Rendered Markdown editing reuses diff-owned modules without updating ownership docs

**Severity:** Low - SourcePanel now treats `markdown-diff-*` modules as a
general rendered-Markdown editing layer, but their ownership still reads as
diff-only.

`ui/src/panels/SourcePanel.tsx` imports `EditableRenderedMarkdownSection`,
`markdown-commit-ranges`, and `markdown-diff-segments` for full-document source
editing. That reuse is practical, but the module names and comments still
present the code as diff-pane infrastructure. Future maintainers can therefore
change a "diff-only" helper without realizing SourcePanel's editable preview
depends on the same commit/range contract.

**Current behavior:**
- Diff-named modules are now consumed by SourcePanel's normal source-editing
  path.
- Ownership comments do not name SourcePanel as a supported consumer.
- The shared commit contract spans both rendered diffs and full-document
  rendered source preview.

**Proposal:**
- Either extract the shared rendered-Markdown edit contract into neutral
  modules, or update the existing module headers to document SourcePanel as a
  supported consumer.
- Add a brief SourcePanel comment near the imports naming the shared contract
  so future refactors do not accidentally break the full-document editor path.

## `bottom_follow` "enter state" reset block duplicated across two branches

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:1482-1495` and `:1610-1625`. Two branches duplicate the same ~six-line ref-reset (`pendingProgrammaticScrollTopRef`, `lastNativeScrollTopRef`, `shouldKeepBottomAfterLayoutRef`, `isDetachedFromBottomRef`, `hasUserScrollInteractionRef`, `lastUserScrollKindRef`, `lastUserScrollInputTimeRef`). Future ref additions risk getting added to one but not the other.

**Current behavior:**
- The `syncViewport` re-arm path and the `syncProgrammaticScrollWrite` event handler each duplicate the ref-reset cluster.

**Proposal:**
- Extract a small helper inside the effect (`function enterBottomFollowMode(node) { ... }`) and call it from both sites.

## New scroll-intent refs and scroll-write kinds lack contract documentation

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:336-338` adds `pendingProgrammaticBottomFollowUntilRef` to the already-large cluster of ~30 scroll-intent / mounted-range / cooldown refs without a doc comment. `ui/src/message-stack-scroll-sync.ts:5-19` adds the `bottom_follow` variant to the scroll-write enum but the file's existing comment block describes only the `bottom_boundary` exception — the `bottom_follow` variant has its own subtle contract (extends a 1.2 s cooldown that re-classifies subsequent native scroll ticks) and deserves a peer-level mention.

The session-virtualized-transcript subsystem is already cited in `bugs.md` history as a maintenance hazard. Each new ref or scroll-kind expands the surface that future refactors must reset / clear correctly.

**Current behavior:**
- New ref has no inline comment explaining its role or reset semantics.
- The shared scroll-sync seam describes only one variant in its prose despite three existing.

**Proposal:**
- Add a one-line comment near `pendingProgrammaticBottomFollowUntilRef` explaining "Latest deadline at which a programmatic `bottom_follow` smooth-scroll can still claim native scroll ticks. Reset to `Number.NEGATIVE_INFINITY` from `markUserScroll` so user gestures cancel the cooldown."
- Extend the `message-stack-scroll-sync.ts` comment block to describe `bottom_follow`'s 1.2 s cooldown contract symmetrically with `bottom_boundary`.

## `bottom_follow` virtualizer state machine has no synthetic-native-scroll test coverage

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:1610-1624` (production), no test. The new `bottom_follow` scroll-kind sets a 1.2s programmatic-bottom-follow window and re-classifies subsequent native scroll ticks as programmatic at lines 1467-1495. The new `App.scroll-behavior.test.tsx` asserts only that `scrollTo` is called with `top: 900, behavior: "smooth"` (the SessionPaneView side). The actual regression-prevention contract — that intermediate native scroll ticks during the smooth-scroll do NOT flip `hasUserScrollInteractionRef`, that `shouldKeepBottomAfterLayoutRef` survives, and that the cooldown re-arms each forward-progress tick — has zero direct coverage.

**Current behavior:**
- Production has the cooldown + re-classification logic in two cooperating branches (event handler + syncViewport).
- Tests only check the dispatcher side.
- A regression dropping the `pendingProgrammaticBottomFollowUntilRef` re-arm would still pass the new test.

**Proposal:**
- Add a test that fires synthetic native `scroll` events with `scrollTop` advancing toward the bottom after a `bottom_follow` write and asserts:
  - `hasUserScrollInteractionRef` is not set (e.g., no "New response" indicator emerges on the next assistant delta).
  - `shouldKeepBottomAfterLayoutRef` survives across the smooth-scroll ticks.
  - A user-initiated wheel/keyboard event during the window cancels the programmatic-bottom-follow marker.
  - The early-exit `if (isScrollContainerNearBottom(node))` branch at 1492-1495 is exercised separately.

## Pane-level bottom-follow state can detach during smooth scroll

**Severity:** Medium - `SessionPaneView` can treat programmatic smooth-scroll
intermediate positions as user detachment.

`ui/src/SessionPaneView.tsx` now marks smooth bottom-follow writes for the
virtualizer, but the pane-level `onScroll` bookkeeping still sees the native
smooth-scroll ticks. While the browser animates toward bottom, intermediate
positions can be more than the near-bottom threshold from the final target.
That can flip pane-level sticky-bottom state off even though the user did not
scroll away.

**Current behavior:**
- `bottom_follow` intent is consumed by the virtualized list.
- Pane-level scroll state does not know that the smooth-scroll is
  programmatic.
- Intermediate smooth-scroll ticks can clear bottom-stick and make later
  streaming chunks show "New response" or stop following.

**Proposal:**
- Add pane-level programmatic bottom-follow tracking in `SessionPaneView`.
- Clear it on real user scroll intent and when the final bottom target is
  reached.
- Add a regression where a smooth bottom-follow emits intermediate scroll
  events far from bottom and the pane remains sticky until the target is
  reached.

## `App.scroll-behavior.test.tsx` "scroll away from bottom" geometry never moves away from bottom

**Severity:** Medium - `ui/src/App.scroll-behavior.test.tsx:944-948`. The test writes `scrollTop = 800` with `scrollHeight = 1000` and `clientHeight = 200` — i.e., `scrollHeight - scrollTop - clientHeight = 0`. That's exactly the bottom. The "still smooth-follow even when not yet pinned" intent of the test name is undermined: the panel was already pinned, so the assertions about smooth-follow and the absence of the "New response" indicator are trivially satisfied even if `bottom_follow` did nothing.

The geometry helper duplication at `:879-899` (re-implementing `stubElementScrollGeometry` inline) compounds the issue — future definition-shape changes would need to be updated in two places.

**Current behavior:**
- Test passes regardless of whether `bottom_follow` is wired through to the virtualizer.
- Test name doesn't match the geometry it sets up.

**Proposal:**
- Change `scrollTop` to a non-bottom value (e.g., 200) and dispatch a `wheel` event with `deltaY: -300` to actually establish the "scrolled up" state, OR rename the test to match what it does.
- Extract a `stubMutableElementScrollGeometry` helper returning `{ setScrollHeight, restore }` and use it instead of the inline `Object.defineProperty` block.

## Strict-revision check on targeted hydration triggers a state resync per same-session delta on busy upstreams

**Severity:** High - `src/remote_routes.rs:457-462` enforces `remote_response.revision == min_revision` on the delta-fast-path. `remote_response.revision` is the upstream's *global* revision at request time, not the targeted session's revision, so on any non-quiescent upstream `M > N` is the common case. Every targeted hydration on the delta path then fails with `bad_gateway`, propagates as `anyhow::Error` through `hydrate_unloaded_remote_session_for_delta`, and triggers `resync_remote_state_snapshot` in `dispatch_remote_event`. Net effect: every same-session inbound delta against an unloaded proxy on a busy chained-remote produces a full state resync.

The `remote_delta_repair_rejects_newer_targeted_session_revision` test codifies the rejection as intended behavior, but the design trade-off (data safety vs. extra round-trip per delta) is not documented and may not be the intended end state. `docs/architecture.md:183` still describes `GET /api/sessions/{id}` as a cheap "Fetch one session" route.

**Current behavior:**
- Strict equality on `remote_response.revision` rejects the realistic common case where the upstream has advanced.
- The fallback path (`anyhow::Error` → `resync_remote_state_snapshot`) is the documented recovery, so the cost is a full state fetch per same-session delta.

**Proposal:**
- Either accept `remote_response.revision >= min_revision` with per-session applied-revision tracking (so the per-session response can land safely without polluting the global watermark), OR document the latency cost in `docs/architecture.md` so future readers know targeted hydration is expected to fall back to resync per delta.
- If the safe-but-expensive behavior is the intended end state, capture that decision in `docs/metadata-first-state-plan.md` so it isn't accidentally relaxed in a future round.

## Same-revision replay suppression can drop valid sibling deltas

**Severity:** High - session-level metadata is not a safe event identity for
same-revision remote delta suppression.

`remote_session_delta_already_reflected()` currently treats
`(remoteRevision, sessionId, messageCount, sessionMutationStamp)` as proof that
a session-scoped delta was already applied. Same-revision deltas are valid in
the current remote protocol, and different sibling events can share the same
session-level count and mutation stamp. The second event can then be skipped
with `Ok(())`, leaving the local proxy missing remote data while never
triggering `/api/state` recovery.

**Current behavior:**
- The replay guard uses session-level metadata rather than event identity.
- A valid sibling same-revision delta with the same count/stamp can be treated
  as already reflected.
- The skip path returns success, so downstream recovery does not run.

**Proposal:**
- Deduplicate only exact targeted-hydration trigger events.
- Track or compute an event key that includes at least variant, `sessionId`,
  `messageId`, revision, and a payload hash or offset-equivalent for text
  operations.
- Add a same-revision sibling-delta regression that proves one replay is
  skipped while a distinct sibling event still applies or triggers recovery.

## Remote `SessionCreated` can publish a non-advancing delta

**Severity:** Medium - an unchanged inbound remote `SessionCreated` redelivery
can publish a local delta with the current revision instead of a newly committed
revision.

When `ensure_remote_proxy_session_record()` returns `changed == false`,
`apply_remote_delta_event::SessionCreated` keeps `revision = inner.revision` but
still publishes `DeltaEvent::SessionCreated`. Delta events are documented as
carrying the exact next revision. Publishing a duplicate/current revision means
clients can ignore the event as stale, and chained remote replay semantics
become ambiguous.

**Current behavior:**
- The unchanged branch does not call `commit_session_created_locked()`.
- The code still publishes `DeltaEvent::SessionCreated` with the current
  revision.
- Clients that enforce monotonic delta revisions may drop the event.

**Proposal:**
- Skip publishing when `changed == false`.
- If chained remotes require a rebroadcast, route it through an explicit
  non-delta protocol path or force a real committed revision bump with a clear
  comment explaining why.
- Add a regression for duplicate remote `SessionCreated` redelivery that asserts
  no stale/non-advancing delta is published.

## Stamp-preservation contract not honored for `SessionCreated` / `OrchestratorsUpdated.sessions[]` payloads

**Severity:** Medium - the new `docs/architecture.md` note documents that "session-scoped deltas preserve cached `session_mutation_stamp` when the wire payload omits it." The fix landed for the six message-mutating delta arms (gated with `if remote_session_mutation_stamp.is_some()`), but `SessionCreated` and `OrchestratorsUpdated.sessions[]` payloads still flow through `apply_remote_session_to_record` → `localize_remote_session(&remote_session.clone())` (`src/remote_sync.rs:312` and `:368-411`), which clobbers a cached `Some(stamp)` to `None` if the inbound payload omits it.

After such a delta lands, the next metadata-only summary's freshness gate (`remote_mutation_stamp_matches`) sees `previous == None && next == Some(_)` and demotes the proxy to `messages_loaded: false` — exactly the symptom the message-arm fix was meant to prevent. Per-delta hydration HTTP fan-out then re-issues `/api/sessions/{id}` until a stamp arrives.

**Current behavior:**
- `apply_remote_session_to_record` writes the inbound `session_mutation_stamp` directly without checking presence.
- A `SessionCreated` (or `OrchestratorsUpdated.sessions[]`) payload omitting the stamp wipes a cached `Some(_)` value.
- The `docs/architecture.md` doc note implies this path also preserves cached stamps; the implementation diverges.

**Proposal:**
- Gate the stamp assignment in `apply_remote_session_to_record` on `remote_session.session_mutation_stamp.is_some()` (with the existing `previous_remote_mutation_stamp` already captured at line 311).
- Or tighten the doc note to say "narrow message-mutating deltas" and explicitly call out that `SessionCreated`/`OrchestratorsUpdated.sessions[]` payloads are authoritative session shapes whose absent stamps replace the cached value.
- Add a regression where a `SessionCreated` summary with `session_mutation_stamp: None` lands against a cached `Some(_)`; assert the cached stamp survives.

## `remote_session_delta_already_reflected` triples lock acquisitions per inbound delta

**Severity:** Medium - `src/remote_routes.rs:432-461`. The helper opens a fresh `inner.lock()` independently of the subsequent `hydrate_unloaded_remote_session_for_delta` and the apply-step lock. The sequence per inbound delta is: lock (dedup check) → unlock → lock (hydration check) → unlock → fetch → lock (apply). Benign for safety because the apply step re-checks `should_skip_remote_applied_delta_revision` under its own lock, but each cycle re-acquires the global mutex. Under high delta throughput on busy upstreams, this is observable contention.

**Current behavior:**
- Each inbound delta acquires the state mutex up to three times (dedup + hydration + apply).
- `remote_session_delta_already_reflected` and the hydration eligibility check inspect the same record back-to-back through separate locks.

**Proposal:**
- Fold the dedup check into the existing `hydrate_unloaded_remote_session_for_delta` lock acquisition (it already inspects the same record).
- Or factor a single `inspect_inbound_delta_eligibility(...)` helper that returns an enum of `{AlreadyReflected, NeedsHydration(target), Apply}` from one lock cycle.

## Repeated `commit_locked` boilerplate at four early-exit paths in `hydrate_remote_session_target`

**Severity:** Low - `src/remote_routes.rs:561-606`. The new `remote_state_applied` flag must be checked and `commit_locked` called before each of four early-exit paths. The same `if remote_state_applied { commit_locked(...).map_err(...)?; } return Err(...)` boilerplate is duplicated. A future maintainer adding a fifth early-exit could easily forget the conditional commit, reintroducing the watermark-race bug under a new condition.

**Current behavior:**
- The conditional commit-before-error is replicated four times in close proximity.
- No helper or invariant comment near the function body documents that every error-return must commit broad state when applied.

**Proposal:**
- Extract a small helper closure (`bail`) that takes an error and conditionally commits before returning.
- Or invert the flow so the broad-state apply runs after the can-this-target-survive checks, eliminating the need to commit-before-error.

## `remote_session_metadata_matches_record` is permissive on both-None stamps

**Severity:** Low - `src/remote_routes.rs:428-431`. Returns `true` when both stamps are `None` — only `message_count` distinguishes a count-equal stale transcript from a current one. Consistent with sibling helpers but a remote that never emits stamps has no positive freshness evidence; a count-equal stale transcript can still be applied through the matching-metadata pass-through branch.

**Current behavior:**
- Both-None stamps yield a `true` match by virtue of `message_count` alone.
- The function has no doc comment explaining the both-None acceptance.

**Proposal:**
- If positive-stamp evidence is required for the older-revision branch, change to `record.session.session_mutation_stamp.zip(session.session_mutation_stamp).is_some_and(|(a, b)| a == b) && session_message_count(record) == session.message_count`.
- Or document the both-None acceptance in the helper's doc comment so the policy is explicit.

## GET-path 502 error message after broad-state commit is misleading

**Severity:** Low - `src/remote_routes.rs:557-572`. When `current < response.revision` triggers the rejection, the broad state has already been committed (line 561-565 `if remote_state_applied { commit_locked(...) }`) and `current_remote_revision` may now match or exceed the response revision. The error message reads "newer than synchronized remote state" — diagnostically confusing post-commit because the synchronized state was just advanced.

**Current behavior:**
- Error message refers to "synchronized remote state" without acknowledging that the commit-before-error path has already advanced it.

**Proposal:**
- Refine the message to "remote session response revision {} cannot be safely applied; broad state advanced to revision {} but transcript may have changed".
- Or split the check so the broad-state apply happens only after rejection-eligibility is confirmed.

## `remote_mutation_stamp_matches` demotes on either-side `None`

**Severity:** Medium - `src/remote_sync.rs:316-319` uses `Option::zip(...).is_some_and(...)`, which evaluates to `false` whenever either side is `None`. Combined with the just-listed stamp-clobber behavior, any remote that does not emit `session_mutation_stamp` will demote loaded proxy transcripts to `messages_loaded: false` on every metadata-only summary. The per-delta hydration HTTP fan-out then re-issues `/api/sessions/{id}` for every subsequent delta on that proxy until a stamp arrives.

The contract isn't documented at the call site — a future reader cannot tell whether `None` means "unknown freshness, be safe" or "no mutation observed."

**Current behavior:**
- `Option::zip(None, _) == None`, so any missing stamp on either side fails the match.
- A Phase-1-compatible upstream (older builds without stamps) demotes loaded transcripts on every metadata refresh.

**Proposal:**
- Add a `///` doc comment near `apply_remote_session_to_record` describing the four `(prev, next)` stamp-presence cases and the chosen demotion policy.
- If a Phase-1-compat mode is desired (preserve when both are `None`), make it explicit via `previous_stamp == next_stamp || (previous_stamp.is_none() && next_stamp.is_none())` and document the rationale.
- Add a regression for the both-None case.

## GET-path resync broadens the route's side-effect surface from "one session" to "all proxy records for the remote"

**Severity:** Medium - `src/remote_routes.rs:470-525` calls `apply_remote_state_if_newer_locked(..., None)` with `focus_remote_session_id: None`. The broad sync runs `retain_sessions`/upsert across every proxy record for that remote, so a single `GET /api/sessions/{id}` for one proxy can mutate all proxy records on the remote. `docs/architecture.md:183` still describes the route as "Fetch one session (full transcript hydration)" with no mention of the global-resync side-effect or the additional `/api/state` outbound call.

**Current behavior:**
- The route's documented contract is "fetch one session"; the actual implementation also performs a per-remote state sync as a side-effect.
- Frontend authors auditing latency expectations have no signal in the doc.

**Proposal:**
- Pass `focus_remote_session_id: Some(&target.remote_session_id)` to scope the sweep to the targeted session, OR explicitly document the broad sweep with a comment near line 506.
- Update `docs/architecture.md` to reflect the new latency contract: targeted hydration may issue an additional `/api/state` fetch and a global SSE broadcast.

## GET-path skipped-side-fetch branch does not advance the per-session watermark

**Severity:** Medium - `src/remote_routes.rs:559-564` advances `note_remote_applied_revision` only when the side-fetch ran and `apply_remote_state_if_newer_locked` accepted it. If `latest_remote_revision >= remote_response.revision` at the pre-lock check, the side-fetch is skipped and the watermark is never explicitly bumped for `remote_response.revision`. This works today because the watermark already covers the response revision, but the implicit invariant ("if we skipped because we're caught up, we don't need to advance") is undocumented and a future refactor could break it.

**Current behavior:**
- Three branches diverge in whether the watermark is advanced: side-fetch ran and applied / side-fetch ran and was rejected / side-fetch skipped.
- The skipped branch relies on `latest_remote_revision >= remote_response.revision` already holding.

**Proposal:**
- Unconditionally call `inner.note_remote_applied_revision(&target.remote.id, remote_response.revision)` after applying the targeted response. The op is idempotent (`max`-based) so it is safe under all three branches and removes the implicit invariant.
- Move the `note_remote_applied_revision` call above the `commit_locked` invocation so the published SSE state event reflects an already-advanced watermark (consistent with `apply_remote_delta_event`'s ordering).

## New stale-skip / equality-rejection tests use unrealistic fixtures

**Severity:** Low - `src/tests/remote.rs:2475,2580` construct `MessageCreated` deltas with `session_mutation_stamp: None`. Real backend emitters always emit `Some(record.mutation_stamp)`. The unrealistic fixture masks the stamp-clobber issue (the delta arms unconditionally write `None` over cached `Some(_)`).

**Current behavior:**
- New tests use `session_mutation_stamp: None` instead of the production wire shape.
- The stamp-clobber bug is not surfaced because the cache is also `None` in the fixture.

**Proposal:**
- Change the fixtures to `session_mutation_stamp: Some(...)` matching the production wire shape.
- Add a separate dedicated regression for the `None`-clobber case once that bug is fixed.

## `adoptFetchedSession` clobbers in-flight deltas read from a stale `sessionsRef` snapshot

**Severity:** High - `ui/src/app-live-state.ts:1099-1172` reads `previousSessions = sessionsRef.current` at the top of the function, computes `existingIndex` and `currentSession = previousSessions[existingIndex]`, runs the hydration-still-matches predicates against that stale snapshot, then does `sessionsRef.current = nextSessions; setSessions(nextSessions)` where `nextSessions = previousSessions.map(...)`. Between the snapshot read and the eventual write, same-instance SSE delta paths (e.g., `messageCreated`, `textDelta`) may have already mutated `sessionsRef.current` in place. The captured `messageCount` / `sessionMutationStamp` guard catches *count*-changing deltas but not in-place text mutations on the latest assistant message.

The Phase 2 hydration test scenarios pass because they add a delta that bumps `messageCount`. A same-`messageCount` shape mismatch — e.g., a `textDelta` that only changes content of the same message — is uncovered. The result on a live system would be a hydration response landing after a streaming chunk and silently rewinding the visible text to the snapshot.

**Current behavior:**
- `adoptFetchedSession` snapshots `sessionsRef.current` once at the top of an async-resolved path.
- After the awaited `fetchSession()` resolves, the function writes back via `previousSessions.map(...)`, discarding any same-instance deltas that landed during the in-flight fetch.
- The `hydrationRequestStillMatchesSession` predicate runs against the same stale snapshot, so the gate cannot detect in-place mutations.

**Proposal:**
- Re-read `sessionsRef.current` immediately before constructing `nextSessions` and re-derive `existingIndex` / `currentSession` against that fresh snapshot.
- Re-run `hydrationResponseMatchesSession` on the fresh snapshot before writing.
- Add a regression that lands a `textDelta` that does not change `messageCount` while a hydration is in flight, and asserts the streamed text survives.

## `get_session()` for unloaded remote proxies silently changed its latency contract

**Severity:** Medium - `src/state_accessors.rs:151-181` quietly turned `GET /api/sessions/{id}` from a constant-time local read into a synchronous outbound HTTP fetch + global SSE state broadcast (via `commit_locked`) when the proxy is unloaded. A slow or wedged remote stalls every visible-pane hydration request and every reconnect resync; a remote that returns `messages_loaded: false` returns `bad_gateway` to the local client instead of degrading to the unloaded summary. The new contract is not documented in `docs/architecture.md`, which still describes `GET /api/sessions/{id}` as the cheap full-hydration route.

(See the sibling entry above, "GET-path resync broadens the route's side-effect surface", for the additional global-sweep side-effect.)

**Current behavior:**
- `get_session()` performs synchronous remote HTTP I/O in the unloaded-proxy branch.
- The hydration call publishes a global SSE state event via `commit_locked`, fanning out to every connected client.
- `bad_gateway` propagates to the caller when the remote returns metadata-only or refuses; no fallback to the local unloaded summary.

**Proposal:**
- Document the new latency dependency in `docs/architecture.md` next to the "GET /api/sessions/{id} stays as the full hydration route" note, including the remote round-trip and SSE broadcast.
- Add a remote-fetch timeout that falls back to returning the local unloaded summary (`messagesLoaded: false`, `messageCount` from cache) instead of bubbling 502 to the browser.

## `hydrate_remote_session_target` rejects upstream `messages_loaded: false`, breaking chained-remote topology

**Severity:** Medium - `src/remote_routes.rs:444-462,558-561` returns `ApiError::bad_gateway("remote session response did not include a full transcript")` when an upstream remote responds with `messages_loaded: false`. On the delta path (`min_remote_revision.is_some()`), this propagates back through `hydrate_unloaded_remote_session_for_delta` as a fatal `anyhow::Error`, converting every same-session inbound delta into a hard error. In a chained-remote topology where the immediate upstream is itself proxying a third remote with an unloaded record, every session-scoped remote delta now becomes a hard error and triggers a fallback resync loop until the inner chain repairs itself.

`hydrate_unloaded_remote_session_for_delta` also collapses the structured `ApiError` (with status code) into a flat `anyhow!("failed to hydrate remote session ...: {err.message}")`, discarding the `bad_gateway` vs `not_found` distinction that downstream recovery branches might need. This is a soft observability/extensibility cost on top of the chained-remote correctness issue.

**Current behavior:**
- `hydrate_remote_session_target` enforces strict `messages_loaded: true` regardless of caller context.
- The wrapped `anyhow::Error` flows back through `apply_remote_delta_event` and triggers state resync.
- `ApiError` status codes are lost before downstream consumers see them.

**Proposal:**
- Gate the strict `messages_loaded` rejection on `min_remote_revision.is_none()` so the GET path keeps strict-mode but the delta-fast-path tolerates the chained-summary case.
- Return `Result<bool, ApiError>` from the helper (or wrap with `anyhow::Error::context(...)`) so the original error category is preserved.
- Add a regression with a fake remote that returns `messages_loaded: false` to confirm the delta path falls through to normal apply rather than hard-erroring.

## Per-delta hydration HTTP fan-out has no in-flight deduplication

**Severity:** Medium - `src/remote_routes.rs:505-534` adds `hydrate_unloaded_remote_session_for_delta` calls at the top of eight delta handlers (`MessageCreated`, `MessageUpdated`, `TextDelta`, `ThinkingDelta`, `CommandUpdate`, `ParallelAgentsUpdate`, plus two more). For a burst of N inbound deltas on a still-unloaded proxy, each call drops the lock, performs a synchronous HTTP fetch, and reacquires the lock — without any in-flight tracking. The first fetch flips `messages_loaded: true` and subsequent fetches short-circuit, but the in-flight ones still serialize on the remote registry and on the local async runtime.

A 100-delta burst on an unloaded proxy issues up to 100 HTTP fetches in sequence before the per-delta short-circuit kicks in. On chained-remote topologies where many proxies are unloaded after a summary `state` arrives, a small flurry of inbound activity can wedge the remote registry queue.

**Current behavior:**
- Eight delta handlers call `hydrate_unloaded_remote_session_for_delta` without coordination.
- Each call independently sees `messages_loaded: false`, drops the lock, fetches, and reacquires.
- The first fetch wins; subsequent fetches still serialize.

**Proposal:**
- Track in-flight hydrations per `(remote_id, remote_session_id)` (e.g., `HashMap<_, Arc<Notify>>` or a per-session `AtomicBool` + waiter pattern).
- Have parallel callers `await` the same future, falling through to the existing skip path on the first success.
- Add a regression with concurrent same-session `MessageCreated` deltas that asserts only one HTTP fetch is issued.

## `adoptFetchedSession` allow-downgrade flag duplicates the server-restart guard at two layers

**Severity:** Medium - `ui/src/app-live-state.ts:1146-1156` ORs `isServerRestartResponse` with `canAdoptLowerRevisionHydration` to compute `allowRevisionDowngrade`, but `shouldAdoptSnapshotRevision` already short-circuits restart cases via the `isServerInstanceMismatch` branch. The dead-flag interaction means restart vs. downgrade semantics are encoded across two layers of branching that must stay aligned — a regression that narrows `shouldAdoptSnapshotRevision`'s instance-mismatch branch would silently start using the downgrade path for restart cases, with subtle semantic differences.

This gate is the only frontend defense against the prior High-severity stale-hydration data loss. Two near-redundant guards encoding the same intent are fragile.

**Current behavior:**
- Restart-vs-downgrade decisions are computed at two layers (`adoptFetchedSession` and `shouldAdoptSnapshotRevision`).
- A regression in either layer can silently shift behavior without surfacing in tests.

**Proposal:**
- Split into three explicit branches: `if (isServerRestartResponse) { adopt unconditionally } else if (canAdoptLowerRevisionHydration) { adopt with downgrade } else { gate via shouldAdoptSnapshotRevision }`.
- Same observable behavior, single layer of branching, easier to audit.

## `scheduleHydrationRetry` re-runs the entire hydration effect on every tick

**Severity:** Medium - `ui/src/app-live-state.ts:647-670` arms a `setTimeout` whose callback fires `setHydrationRetryTick(...)`, a `useState` counter included in the visible-session hydration effect's deps. Each tick re-runs the whole effect and walks every visible session in `sessionIdsToHydrate`, even if only one session was retrying. With multiple sessions in pending-retry state, the cascading effect re-runs multiply network requests and CPU work under load.

The early-out at the top of the effect (`hydratingSessionIdsRef.current.has(sessionId)`) only guards against parallel in-flight requests, not against re-evaluation of unrelated sessions.

**Current behavior:**
- `setHydrationRetryTick` is a global counter dep.
- A retry for session A bumps the tick and re-evaluates session B's hydration too.
- The retry timer cleanup lives in a separate `useEffect(() => () => cancelHydrationRetries(), [])` rather than the same effect.

**Proposal:**
- Replace the tick counter with a per-session `Set<string>` ref of pending-retry ids; the timer adds the id and triggers a targeted re-fetch through a stable `useCallback` rather than re-running the effect.
- Or have the retry timer call into the fetch loop directly, bypassing the effect entirely.
- Skip work in the effect for sessions not in the retry set.

## `apply_remote_delta_event::SessionCreated` clones the full transcript only to read `.id`

**Severity:** Medium - `src/remote_routes.rs:681-690` builds `local_session = wire_session_from_record(local_record)` (a full transcript-bearing `Session`) just to consume `.id`. Both `delta_session.id` and the in-scope `local_session_id` already hold the same value. For a long-lived remote session whose proxy mirror already has a non-trivial transcript, every inbound `SessionCreated` redelivery rebuilds and drops the full message vec under the state mutex.

This is the same clone-and-discard anti-pattern the rest of this changeset migrated away from for `wire_session_summary_from_session`. The publish path here only reads the id.

**Current behavior:**
- `wire_session_from_record(local_record)` runs once per `SessionCreated` redelivery, just to access `.id`.
- The cloned `Session` is then dropped without further use.

**Proposal:**
- Drop the `local_session` binding entirely; publish using `delta_session.id.clone()` or the already-in-scope `local_session_id`.

## `commit_session_created_locked` summary fallback diverges between branches

**Severity:** Medium - `src/codex_thread_actions.rs:165-180` and `src/session_crud.rs:296-312` use `inner.find_session_index(...).map(|index| ...).unwrap_or_else(|| ...)` to build the published delta. The success arm reads from the index lookup; the fallback arm builds off the caller's `&record` parameter. The two paths can disagree on `messageCount` if the field cache (`record.session.message_count`) and the just-pushed `messages.len()` ever drift. The freshly pushed record cannot legitimately be missing from the index, but a silent divergence between the success and fallback arms is a contract wedge that can hide future regressions.

**Current behavior:**
- The fallback arm's existence implies the index lookup might fail.
- The two arms compute summaries from different sources without explicit equivalence.

**Proposal:**
- Drop the `unwrap_or_else` fallback and replace with `.expect("just-created session must be present in the index")`.
- Or route both arms through a single `wire_session_summary_from_record(record)` helper sourced from the actual `&SessionRecord` passed to `commit_session_created_locked`.

## `installMonacoCancellationRejectionFilter()` default arg crashes in non-DOM contexts

**Severity:** Low - `ui/src/monaco-cancellation-filter.ts:8` defaults `target: RejectionTarget = window`. In a non-DOM context (e.g., SSR rendering, a Node-side test harness, or a future Vitest entry that doesn't load `jsdom`), the default-arg evaluation `ReferenceError`s before any guard inside the function body runs.

The internal `typeof target.addEventListener !== "function"` guard catches *passed-in* non-DOM-like targets, but cannot help when the *default* expression itself fails.

**Current behavior:**
- `target: RejectionTarget = window` is unguarded.
- A future SSR or non-jsdom test entry calling `installMonacoCancellationRejectionFilter()` (no args) crashes at module-load time.

**Proposal:**
- `target: RejectionTarget = typeof window !== "undefined" ? window : ({} as RejectionTarget)`.
- The internal guard then no-ops cleanly when no real `window` is available.

## `captureHydrationRequestContext` null fallback defeats the gate

**Severity:** Low - `ui/src/app-live-state.ts:1019-1033` falls back to `null` for both `messageCount` and `sessionMutationStamp` when the captured summary lacks them. The match predicates (`hydrationRequestStillMatchesSession`, `hydrationResponseMatchesSession`) accept any value when the captured field is `null`, so a captured-context with both fields `null` matches any response — exactly the mixed-instance restart window when the gate matters most.

The wire contract per `Session.messageCount?: number | null` says the field is optional in payloads. After Phase 2 the backend should always emit it, but during the cross-version window the gate becomes a no-op.

**Current behavior:**
- `null` means "accept any response value" rather than "must match exactly null."
- The capture-at-send / reject-at-receive gate is silently bypassed when the captured context lacks metadata.

**Proposal:**
- Treat `null` as a "must match exactly null" marker — a numeric response value should reject when the capture was null.
- Or short-circuit to a recovery resync (`requestActionRecoveryResyncRef.current()`) when the captured context has `null` for both `messageCount` and `sessionMutationStamp`.

## No-change branch builds an unused summary in remote codex/create proxies

**Severity:** Low - `src/remote_codex_proxies.rs:94-95` and `src/remote_create_proxies.rs:183-184` build `wire_session_from_record(&local_record)` and `wire_session_summary_from_record(&local_record)` even when the surrounding `announce_remote_session_created_if_changed` short-circuits because nothing changed. The summary is then dropped on the no-change branch, wasting a metadata-shape clone under the state mutex.

The cost is small (no transcript clone — the helper now sources fields from the record directly), but the no-change branch is hot for clients re-creating an already-mirrored remote session.

**Current behavior:**
- Both helpers run unconditionally.
- The `delta_session` summary is dropped on the no-change path.

**Proposal:**
- Build the summary lazily inside the `if changed { ... }` arm, or pass an `Option<Session>` to the announce helper that defers construction until the announce path actually publishes.

## `docs/architecture.md` missing `OrchestratorsUpdated.sessions[]` skip-when-empty contract

**Severity:** Low - `docs/architecture.md:258-259` documents `OrchestratorsUpdated { revision, orchestrators[], sessions[] }` as a metadata-first delta but does not mention that `sessions[]` carries `#[serde(default, skip_serializing_if = "Vec::is_empty")]` (`src/wire.rs:1346-1347`). A reader auditing the wire contract cannot tell from the doc whether the field is required, optional, or omitted-when-empty.

**Current behavior:**
- The doc's `OrchestratorsUpdated` entry omits the empty-elision behavior.
- Readers must consult `src/wire.rs` to confirm the contract.

**Proposal:**
- Append "`sessions[]` is omitted on the wire when empty (`#[serde(skip_serializing_if = "Vec::is_empty")]`)" to the doc note.
- Confirm `ui/src/types.ts:638` (`sessions?: Session[]`) is consistent with the elision contract.

## Interaction request routes can hide same-revision `MessageUpdated` deltas

**Severity:** Medium - route responses can advance the client past the same-revision SSE delta that contains the actual card replacement.

The interaction submission routes publish `MessageUpdated` deltas, but the
initiating client also adopts the POST response. If that response is
metadata-only and carries the same revision, the client can ignore the matching
SSE delta as already seen, leaving approval, user-input, MCP elicitation, or
Codex app-request cards stale until another hydration path repairs them.

**Current behavior:**
- `src/codex_submissions.rs` interaction routes publish a `MessageUpdated`
  delta for the resolved interaction card.
- The route response can be adopted by the frontend before the same-revision
  SSE delta is processed.
- The response does not necessarily include the fully mutated session content
  needed to make skipping the delta safe.

**Proposal:**
- Return a snapshot with the mutated session fully hydrated, matching the
  `send_message` response shape used for the active session.
- Or have the frontend apply the returned message update directly without
  advancing past the same-revision SSE delta.
- Add a regression where the POST response arrives before the SSE delta and the
  resolved card is still visible immediately.

## Queued-turn dispatch publishes stale `sessionMutationStamp`

**Severity:** Medium - the `MessageCreated` delta for a queued turn can carry a mutation stamp from before pending-prompt cleanup.

Queued-turn dispatch builds the started-turn delta before all queued-prompt
state has been popped and synced. Those later mutations restamp the session
record, so the published delta can advertise a stale `sessionMutationStamp`.
The next metadata snapshot may then look like a new unseen mutation and trigger
unnecessary transcript invalidation or hydration.

**Current behavior:**
- `src/turn_dispatch.rs` captures the delta stamp before the queued prompt list
  reaches its final state for the dispatch.
- The session record can be restamped by pending-prompt cleanup after the delta
  payload was built.
- Metadata-first reconciliation relies on those stamps to decide whether a
  cached transcript is still complete.

**Proposal:**
- Build or refresh the `StartedTurnMessageDelta` after all queued-prompt
  mutations are complete.
- Assert in tests that the published delta stamp equals the final session
  record's `mutation_stamp`.

## Live-delta recovery test no longer proves the recovered message is visible

**Severity:** Medium - a watchdog regression test can pass even if the latest recovered assistant message is hidden.

One App-level live-delta recovery test no longer asserts that the recovered text
is actually rendered after the delta flush and after the stale fetch is
rejected. That weakens coverage for the same class of bug where the newest
assistant message only appears after another prompt, focus change, or unrelated
rerender.

**Current behavior:**
- `ui/src/App.live-state.watchdog.test.tsx` still exercises the recovery flow.
- The test does not prove `"Recovered from live delta."` is visible at the
  critical points.

**Proposal:**
- Restore non-brittle visibility assertions after the delta is applied and
  after the stale fetch resolves.
- Prefer `screen.getAllByText(...).length > 0` when duplicate text can appear in
  both message content and previews.

## Backend streaming delta wire tests do not pin `messageCount`

**Severity:** Medium - four backend delta emitters can drop `message_count` without failing Rust wire-contract tests.

`messageCount` now drives metadata-first reconciliation and hydration decisions,
but backend tests currently pin the field strongly for `MessageCreated` /
`MessageUpdated` paths and not for every streaming delta variant. Frontend
reducer tests help, but they cannot catch a backend serialization omission on a
specific emitter path.

**Current behavior:**
- Local backend coverage does not assert `message_count` for representative
  `TextDelta`, `TextReplace`, `CommandUpdate`, and `ParallelAgentsUpdate`
  broadcasts.
- A future Rust-side omission could still produce current-looking frontend unit
  test fixtures.

**Proposal:**
- Add backend tests that subscribe to delta events and drive one representative
  local mutation for each missing delta type.
- Assert the emitted `message_count` matches the transcript length or expected
  metadata count for that mutation.

## Metadata-first summaries make transcript search incomplete

**Severity:** Medium - search can silently miss transcript matches for sessions that have only metadata summaries loaded.

`/api/state` now returns session summaries with `messages: []` and
`messagesLoaded: false`. The session search index still walks
`session.messages` directly, so non-visible sessions can be treated as having
no searchable transcript even though the transcript simply has not been
hydrated in this browser view.

**Current behavior:**
- `ui/src/session-find.ts` builds transcript search items from
  `session.messages`.
- Metadata-first session summaries clear `messages` before reaching the
  frontend.
- Search has no "transcript not loaded" state and no on-demand hydration path
  before concluding that there are no message matches.

**Proposal:**
- Gate transcript search to hydrated sessions and surface incomplete results
  when a session summary is not loaded.
- Or hydrate/index target sessions on demand when search needs transcript
  content.
- Add coverage proving metadata-only summaries do not silently produce false
  "no transcript match" results.

## Metadata-first state summaries still broadcast full pending prompts

**Severity:** Low - transcript payloads were removed from global state, but queued prompt text can still ride along with every session summary.

Metadata-first state summaries clear `messages`, but the session summary still
includes full pending-prompt data. Queued prompts can contain user-authored
instructions or expanded prompt content, so this remains a smaller but real
data-minimization leak in `/api/state` and SSE `state` broadcasts.

**Current behavior:**
- `src/state_accessors.rs` builds transcript-free summaries but keeps the full
  `pending_prompts` projection.
- Every listening tab can receive pending prompt content for sessions it is not
  actively hydrating.

**Proposal:**
- Project pending prompts to a bounded metadata-only summary in `StateResponse`.
- Keep full queued-prompt content on targeted full-session responses where the
  active pane actually needs it.

## App-level delta fixtures omit required `messageCount`

**Severity:** Low - some tests still dispatch impossible delta payloads after the protocol made `messageCount` required.

Several App-level delta tests construct `delta` events by hand and omit
`messageCount`. Those fixtures no longer match the current `DeltaEvent` wire
contract, so they can pass through behavior that production SSE cannot produce
and miss metadata-first regressions.

**Current behavior:**
- Some `ui/src/App.live-state.deltas.test.tsx` fixtures dispatch current
  protocol delta types without `messageCount`.
- The tests are not forced through a typed helper that requires the full
  current event shape.

**Proposal:**
- Introduce a typed test helper for `DeltaEvent` fixtures and require
  `messageCount` on all session-scoped deltas.
- Update hand-written fixtures to match the current SSE contract.

## Hydration retry loop can spam persistent failures

**Severity:** Low - visible-session hydration retries clamp to the last retry delay and can continue indefinitely for persistent non-404 failures.

The new retry loop correctly recovers from stale hydration rejection and transient `fetchSession` failures, but it has no ceiling. A visible metadata-only session whose targeted hydration keeps failing will retry every 3 seconds and repeatedly call the normal request-error reporting path.

**Current behavior:**
- `ui/src/app-live-state.ts` schedules retry delays of 50 ms, 250 ms, 1000 ms, then 3000 ms, and clamps all later retries to 3000 ms.
- Non-404 `fetchSession` failures report the request error and schedule another retry.
- The transient non-404 failure branch is not covered by a regression test.

**Proposal:**
- Cap repeated user-facing error reporting or retry attempts for the same visible session while keeping event-driven or manual recovery possible.
- Add a test where the first `/api/sessions/{id}` request fails with a non-404 error, the retry succeeds, and the transcript appears without a tab switch or unrelated state event.

## Remote test module size slows review and triage

**Severity:** Note - `src/tests/remote.rs` is large enough that focused remote
review now has to scan many unrelated scenarios.

The file contains hydration, delta, orchestrator, proxy, and sync-gap coverage
in one module. New hydration/replay tests are coherent, but keeping every remote
scenario in the same file makes future review targeting and regression triage
harder, especially as the metadata-first remote work continues adding focused
cases.

**Current behavior:**
- Remote tests for several boundaries live in one oversized module.
- New review findings repeatedly point into the same large file, making
  ownership and intended fixture reuse harder to see.

**Proposal:**
- Split remote tests by boundary, for example `remote_hydration.rs`,
  `remote_deltas.rs`, and `remote_orchestrators.rs`.
- Move shared fake-server and remote-session helpers into a small support
  module used by those test files.

## New orchestrator summary-preservation test missing `.all()` shape assertion

**Severity:** Medium - `src/tests/remote.rs:1505-1521` adds a test covering `OrchestratorsUpdated.sessions` summary preservation after a full `sessions` snapshot, but the assertion only `.find()`s one session by its `message_count == 2` and checks its individual fields. It does not assert that the `.all()` of the projected sessions match the summary-shape invariant (`messages == []`, `messages_loaded == false` when expected, all `message_count` values preserved from the incoming snapshot).

A regression that silently left one session with a full transcript, or that swapped `messages_loaded` on an unrelated session in the batch, would pass the current `.find(|s| s.message_count == 2)` assertion. The test is meant to pin the "the whole republish is metadata-first" contract but only inspects one session.

**Current behavior:**
- Test finds one session by `message_count == 2` and asserts its shape.
- No `.all()` assertion over the full snapshot's `sessions` vec.
- No fixture coverage for multi-session snapshots with a mix of hydrated/unhydrated incoming records.

**Proposal:**
- Replace the `.find()` probe with `assert!(republished.iter().all(|s| s.messages.is_empty() && !s.messages_loaded))`, or add it alongside the existing assertion.
- Expand the fixture to include at least two sessions with distinct `message_count` values so the `.all()` assertion covers more than one session shape.
- Optional: parameterize over a hydrated-input + unhydrated-input mix to also pin the "republish projects metadata regardless of source hydration state" contract.

## Reconnect fallback test no longer proves applied delta is visible

**Severity:** Low - reconnect coverage can pass even if the session delta in the scenario is ignored.

One backend connection test still describes an applied session delta, but the
assertion that proved the delta changed the visible session state was removed.
That leaves the reconnect/fallback path with weaker coverage for the same class
of bug where transport recovery appears healthy while the transcript remains
stale.

**Current behavior:**
- `ui/src/backend-connection.test.tsx` exercises the reconnect fallback flow.
- The test no longer proves that the session delta updates the visible preview,
  transcript, or store state while reconnect remains active.

**Proposal:**
- Restore an assertion against the visible preview, transcript text, or
  session-store state after the delta is dispatched.
- Keep the reconnect-state assertions so the test proves both facts: the UI is
  still recovering and the live delta was applied.

## Cross-window tab drag channel restarts on ordinary renders

**Severity:** High - `useAppDragResize` recreates its `BroadcastChannel` subscription on ordinary renders, which can drop cross-window tab drag coordination messages.

The extracted drag/resize hook now owns the `BroadcastChannel` that carries `drag-start`, `drop-commit`, and `drag-end` across windows. That effect depends on `applyControlPanelLayout`, but `App.tsx` still defines `applyControlPanelLayout(...)` inline, so its identity changes on every render. Any render while a cross-window tab drag is in flight tears down the active channel and creates a fresh one. Because the tab-drag protocol is transient and message-based, the source or target window can miss `drop-commit` / `drag-end`, leaving stale external drag state or failing to close the source tab after a successful cross-window drop.

**Current behavior:**
- `ui/src/app-drag-resize.ts` registers the tab-drag `BroadcastChannel` in a `useEffect` keyed by `applyControlPanelLayout`.
- `ui/src/App.tsx` recreates `applyControlPanelLayout(...)` on ordinary renders.
- A render during cross-window drag/drop can close and reopen the channel mid-protocol, dropping in-flight drag messages.

**Proposal:**
- Keep the channel effect stable per `windowId` instead of per render.
- Read the latest layout helper through a ref inside `channel.onmessage` rather than making it an effect dependency.
- Add a regression test that forces a render between `drag-start` and `drop-commit` and asserts the source tab still closes cleanly.



## Session store subscribers can dispatch stale action callbacks

**Severity:** High - `SessionBody` and `SessionComposer` now rerender from `session-store`, but their custom `memo` comparators still let mutating action callbacks stay frozen on older closures.

The new store-backed session slices in `ui/src/panels/AgentSessionPanel.tsx`
move transcript and composer data onto `useSessionRecordSnapshot(...)` /
`useComposerSessionSnapshot(...)`, while the panel and footer still receive
actions such as `onSend`, `onStopSession`, `onApprovalDecision`,
`onCancelQueuedPrompt`, `onRefreshSessionModelOptions`,
`onRefreshAgentCommands`, and `onSessionSettingsChange` from the parent. Unlike
the render callbacks, those actions were not wrapped in ref-backed adapters, yet
the `memo(...)` comparators still exclude them. That lets the store-driven UI
show fresh session data while user actions continue to execute through stale
closures captured before the latest parent render.

**Current behavior:**
- `SessionBody` and `SessionComposer` rerender from store subscriptions even
  when their parent props comparator short-circuits.
- The comparators at `ui/src/panels/AgentSessionPanel.tsx` omit mutating action
  props such as `onSend`, `onStopSession`, `onApprovalDecision`, and related
  refresh/settings callbacks.
- Those actions come from `useAppSessionActions(...)` closures that still read
  live lookups such as `sessionLookup`, `workspace`, and active session state,
  so invoking them through stale closures can target older app state than the UI
  currently renders.

**Proposal:**
- Treat mutating action props the same way the render callbacks are treated:
  either include them in the memo comparators or route them through stable
  ref-backed adapters inside `AgentSessionPanel`.
- Add focused regression coverage that updates the parent action closures while
  the store-backed composer/body rerender, then asserts send/approval/settings
  actions hit the latest session/workspace state.

## Session switches can briefly show the previous transcript and route actions to the new session

**Severity:** High - the refactor reuses one `SessionConversationPage` across session switches while still deferring `messages` and `pendingPrompts`, so the UI can briefly render stale cards from the previous session under the new session id.

`ui/src/panels/AgentSessionPanel.tsx` now keeps a single conversation page
mounted for the active session instead of keying or remounting that subtree per
session. At the same time, `SessionConversationPage` still runs
`useDeferredValue(session.messages)` and `useDeferredValue(pendingPrompts)`.
During a session switch, React can therefore keep the previous transcript data
alive for a deferred frame while `session.id` and the action bindings already
point at the newly selected session. That is no longer just a visual lag: cards
from session A can momentarily render while approval, queued-prompt, or
Codex-app actions are already bound to session B.

**Current behavior:**
- `SessionConversationPage` reuses the same component instance across session
  switches.
- `messages` and `pendingPrompts` are still deferred with `useDeferredValue(...)`.
- A session switch can render stale cards from the previous session for a
  deferred frame while action handlers are already bound to the newly selected
  `session.id`.

**Proposal:**
- Cut over immediately on `session.id` changes instead of deferring transcript
  arrays across session boundaries.
- Key the conversation subtree by `session.id`, or only use deferred
  message/prompt arrays when the session id is unchanged.
- Add a regression that switches sessions while the previous one has visible
  approval/pending cards and asserts no stale card is actionable under the new
  session id.

## Partial responses can consume restart detection before full snapshot adoption

**Severity:** High - the mutation-stamp fast path disables deep session
reconciliation only when `adoptState(...)` sees a server-instance change, but
partial create/fetch responses can update `lastSeenServerInstanceIdRef` first.

Backend `SessionRecord::mutation_stamp` values reset every process lifetime.
If a restarted backend returns a partial `CreateSessionResponse` or
`SessionResponse` before the next full `/api/state` snapshot, the partial
adoption updates `lastSeenServerInstanceIdRef`. The following full snapshot
then appears to come from the same instance, so `adoptState(...)` may leave the
mutation-stamp fast path enabled. Existing sessions whose stamps collide with
the previous process, especially stamp `0`, can keep stale client objects after
restart.

**Current behavior:**
- `adoptCreatedSessionResponse(...)` and `adoptFetchedSession(...)` can update
  `lastSeenServerInstanceIdRef` before a full state snapshot adopts.
- `adoptState(...)` computes `serverInstanceChanged` from that same ref.
- A full restart snapshot can therefore skip the deep reconcile that should
  run when process-local mutation stamps reset.

**Proposal:**
- Track "server instance changed since last full snapshot" separately from the
  latest instance id observed by any response.
- Or keep a full-state-specific server instance marker and compare full
  snapshots against that marker when deciding whether to disable the
  mutation-stamp fast path.
- Add App/live-state coverage where a partial response from instance B arrives
  before a full instance-B snapshot with colliding stamps, and assert the full
  snapshot still replaces stale session content.

## Session store publication can race ahead of React session state

**Severity:** Medium - the new `session-store` publishes some session slices before the corresponding React `sessions` state commits, so the UI can mix newer store-backed session data with older prop-derived session state in one render.

The staged refactor publishes `session-store` updates directly from
`ui/src/app-live-state.ts` and `ui/src/app-session-actions.ts`, while other
parts of the active pane still derive session data from React state in
`ui/src/SessionPaneView.tsx`. That leaves two live sources of truth on slightly
different timelines: `AgentSessionPanel` / `PaneTabs` can read the new store
snapshot immediately, while sibling props such as `commandMessages`,
`diffMessages`, waiting-indicator state, and other session-derived metadata are
still coming from the previous React `sessions` commit.

**Current behavior:**
- `session-store` is synced directly from live-state/action paths before some
  `setSessions(...)` commits land.
- `AgentSessionPanel` and `PaneTabs` read session data from the store.
- `SessionPaneView` still derives other active-session slices from React state,
  so the same active pane can render mixed-version session data within one
  update.

**Proposal:**
- Keep store publication aligned with committed React state, or finish moving
  the remaining active-session derivations in `SessionPaneView` onto the same
  store boundary.
- Document which layer is authoritative during the transition so later changes
  do not deepen the split-brain state model.
- Add an integration test that forces a store-backed session update plus a
  lagging React-state-derived sibling prop and asserts the active pane never
  renders a torn combination.

## `codexUpdated` SSE delta is missing contract documentation

**Severity:** Note - the backend and frontend now implement a `codexUpdated`
delta for Codex global state, but `docs/architecture.md` and the wire-level
comments do not document the new SSE payload.

`DeltaEvent::CodexUpdated { revision, codex }` is part of the current client
contract. Without documentation beside the other SSE delta variants, future
remote implementers and frontend maintainers have to infer the payload shape
from scattered Rust and TypeScript code.

**Current behavior:**
- `codexUpdated` is emitted and consumed as a valid SSE delta.
- The architecture docs still describe only the older delta variants.
- The wire comments do not call out the payload shape or intended usage.

**Proposal:**
- Add `codexUpdated` to the SSE delta contract in `docs/architecture.md`.
- Update the nearby `DeltaEvent`/wire comments to state that the payload is the
  latest `CodexState` plus the monotonic `revision`.

## Deferred heavy-content activation is coupled into the message-card renderer

**Severity:** Low - `ui/src/message-cards.tsx` now owns deferred heavy-content
activation policy in addition to Markdown, code, Mermaid, KaTeX, diff, and
message-card composition concerns.

The new provider/hook is useful, but keeping the virtualization activation
contract embedded in the same large renderer increases coupling between scroll
policy and message rendering. Future performance fixes will have to reason
through a broad module instead of a small boundary with a clear contract.

**Current behavior:**
- Deferred activation context, heavy Markdown/code rendering, and message-card
  composition live in one large module.
- Virtualization policy reaches into message rendering through exported
  activation context.
- The ownership boundary is not documented near the exported provider.

**Proposal:**
- Extract the deferred activation provider/hook into a focused module with a
  short contract comment.
- Consider extracting the heavy Markdown/code rendering path separately so
  virtualization policy and content rendering can evolve independently.

## `useState` initializer in `SessionComposer` writes to a shared ref

**Severity:** Medium - `ui/src/panels/AgentSessionPanel.tsx:788-806` includes `committedDraftsRef.current[initialSessionId] = initialCommittedDraft;` inside a `useState(() => { ... })` initializer. React's documentation explicitly warns against side effects in state initializers because the initializer can run more than once (StrictMode double-invoke, discarded concurrent renders). The write is idempotent today, but the pattern is a known footgun and any future code making the initialization non-idempotent would silently double-apply.

**Current behavior:**
- The `useState` initializer writes the initial committed draft into `committedDraftsRef.current[initialSessionId]`.
- Under React 18 StrictMode the initializer runs twice; the write is idempotent so no observable difference today.
- Any future non-idempotent logic (e.g. appending to an array, incrementing a counter, allocating a derived id) added to the initializer would silently double-apply.

**Proposal:**
- Move the `committedDraftsRef.current[initialSessionId] = initialCommittedDraft` write into the first `useLayoutEffect` keyed on `[activeSessionId]`.
- The state initializer can still compute the initial draft from `session?.committedDraft ?? ""` without writing the ref.

## `preferImmediateHeavyRender` is computed from a non-reactive ref during render

**Severity:** Medium - `ui/src/panels/VirtualizedConversationMessageList.tsx:666-667` computes the `preferImmediateHeavyRender` prop for `MeasuredPageBand` by reading `hasUserScrollInteractionRef.current` during render. Refs are not reactive, so the computed value only propagates when something else forces a re-render. Today that works because every scroll-event path that flips the ref to `true` also triggers `setViewport(...)` via `syncViewportFromScrollNode` within the same handler, which causes a re-render and re-reads the ref. But the coupling is implicit, undocumented, and brittle.

Any future scroll path that flips `hasUserScrollInteractionRef.current = true` without triggering a React state update will leave memoized pages with the stale `preferImmediateHeavyRender={true}` value until a different render trigger arrives — at which point heavy cards that should have stayed deferred will activate, defeating the purpose of the cooldown gate.

**Current behavior:**
- `preferImmediateHeavyRender` is computed each render from `hasUserScrollInteractionRef.current`.
- The ref is mutated in two handlers that also call `syncViewportFromScrollNode`, which updates `viewport` state and forces a re-render.
- If a future contributor adds a third setter without a matching state update, memoized pages will stay on a stale value.

**Proposal:**
- Promote `hasUserScrollInteraction` to component state (or state+ref pair), so every mutation triggers a re-render automatically.
- Alternatively, expose a helper like `setHasUserScrollInteraction(true)` that both writes the ref and calls a dedicated state-setter, and use that everywhere. Add a comment at the two existing setter sites naming the invariant.

## Composer switched to uncontrolled `<textarea>` without documenting the narrow-state contract

**Severity:** Medium - `ui/src/panels/AgentSessionPanel.tsx:1132-1172` migrated the composer from a controlled input (`value={composerDraft}`) to an uncontrolled input (`defaultValue={initialComposerDraft}`) with imperative `composerInputRef.current.value = ...` writes for everything but slash-prefixed drafts. The React state `composerDraft` is populated only when the current input starts with `/`; plain-text drafts live exclusively in the DOM and `composerDraft` reads `""`.

Today this is intentional — only the slash palette cares about the draft, and it only cares about slash drafts. But any future consumer of `composerDraft` through props, memo deps, or a derived state calculation will see empty strings for plain text and not understand why. The invariant "current draft text must be read via `getComposerDraftValue()`, not `composerDraft`" is implicit.

**Current behavior:**
- `<textarea>` is uncontrolled; `defaultValue={initialComposerDraft}` sets the first paint, imperative `ref.current.value = ...` handles later writes.
- `composerDraft` (React state) tracks only slash-prefixed drafts for palette rendering.
- Any non-slash reader of `composerDraft` observes empty strings for typed plain text.
- Nothing in code documents that current draft text MUST be read via `getComposerDraftValue()` / `composerInputRef.current?.value`.

**Proposal:**
- Add a block comment at the `currentLocalDraftState` declaration explaining the narrow slash-only meaning and that readers of current draft text MUST use `getComposerDraftValue()`.
- Rename `composerDraft` → `composerSlashDraft` (or `trackedSlashDraft`) to make the narrow purpose visible at every call site.

## `disableMutationStampFastPath` is not threaded through `sameSessionSummary`

**Severity:** Medium - `ui/src/session-reconcile.ts:76-110` passes `disableMutationStampFastPath` down to `reconcileSession(...)` but not to `sameSessionSummary(...)`. The summary comparator already checks `previous.sessionMutationStamp === next.sessionMutationStamp` directly (line 79). When the caller requests a deep reconcile (`disableMutationStampFastPath: true`) but the pre/post-restart summaries happen to be equal AND the stamps happen to collide (e.g. both `0` on fresh runtimes, or an unrelated u64 match), `sameSessionSummary` returns `true` and `reconcileSession` early-returns `previous` before `reconcileMessages` runs.

The existing test (`can disable the mutation-stamp fast path after a server restart` in `session-reconcile.test.ts`) forces a summary difference (preview text or messages length), so it passes even though the failure mode — summary-equal with colliding stamps and divergent messages — is the scenario the flag is supposed to address.

**Current behavior:**
- `sameSessionSummary(prev, next)` checks the stamp at line 79, outside any `options` gate.
- When `disableMutationStampFastPath` is `true` but summaries and stamps happen to match, the fast path is re-entered through a different door.
- The restart-divergent-transcript scenario is unprotected when summaries don't change.

**Proposal:**
- Pass `options` through to `sameSessionSummary` and, when `disableMutationStampFastPath` is `true`, treat the stamps as non-equal there too. Alternatively, factor the stamp check out of `sameSessionSummary` entirely and gate it in `reconcileSession` only.
- Add a regression test: two sessions with identical summaries, identical stamps, but diverging message arrays; assert that `disableMutationStampFastPath: true` causes `reconcileMessages` to produce the new messages.

## `CodexUpdated` delta carries a full subsystem snapshot despite the "delta" name

**Severity:** Medium - `src/wire.rs::DeltaEvent::CodexUpdated { revision, codex: CodexState }` publishes the entire `CodexState` on every rate-limit tick and every notice addition. The architectural contract the codebase otherwise respects is "state events for full snapshots, delta events for scoped changes". `CodexUpdated` is small today (rate_limits + notices capped at 5), but the naming invites future bulky additions to `CodexState` (login state, model-availability maps, per-provider metadata) to be broadcast in full on every tiny change.

**Current behavior:**
- The variant ships a full `CodexState` payload.
- Two publish sites in `src/session_sync.rs` send the complete snapshot even when only the rate limits changed.
- Wire name and shape set a precedent for "delta = tiny changes" that this variant violates.

**Proposal:**
- Split into narrower variants: `CodexRateLimitsUpdated { revision, rate_limits }` and `CodexNoticesUpdated { revision, notices }`. The two call sites in `session_sync.rs` already pick their publish trigger, so split dispatch is straightforward.
- Alternatively, add a source-level comment on the `CodexUpdated` variant stating that `codex` is intentionally the full subsystem snapshot and any future field addition to `CodexState` must reconsider whether a narrower event is needed.

## `CodexState.notices` 5-item cap is enforced in the mutator, not the type

**Severity:** Medium - `src/session_sync.rs::note_codex_notice` calls `notices.truncate(5)` to bound the notices vector, but the cap lives only at that call site. `CodexState.notices: Vec<CodexNotice>` in `src/wire.rs` declares no bound. A future caller that assembles a `CodexState` differently (constructing it directly, deserializing from a remote, or adding a second mutator) will bypass the cap and broadcast an unbounded vector over SSE.

**Current behavior:**
- `notices.truncate(5)` runs only inside `note_codex_notice`.
- Any other path that produces a `CodexState` is unconstrained.

**Proposal:**
- Extract a `const CODEX_NOTICE_CAP: usize = 5;` at module scope and use it at both the mutator and any future assembler. Document it in a doc comment on `CodexState.notices`.
- Alternatively, wrap the field in a newtype (`NoticeRingBuffer`) that enforces the cap on insertion.

## `DeferredHeavyContent` near-viewport activation now deferred by one paint

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder → content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ≥1 paint even when the card is already near viewport on mount.
- The deferral was added as part of the `allowDeferredActivation` cooldown gate (to avoid layout thrash during active scrolls).
- Near-viewport mount activation now produces a one-frame placeholder flicker in place of the previous zero-frame activation.

**Proposal:**
- Use `useLayoutEffect` when `allowDeferredActivation === true` (or for the near-viewport branch generally). Keep the `requestAnimationFrame` in the IntersectionObserver entry path for rapid-entry de-dupe.
- Alternatively, add a targeted comment explaining the deliberate trade-off if the new behavior is intended.

## Remote `DeltaEvent::CodexUpdated { .. }` arm uses a silent wildcard destructure

**Severity:** Low - `src/remote_routes.rs:1004-1013` matches `DeltaEvent::CodexUpdated { .. } => { ... }` in the remote dispatch loop. The `..` wildcard silently hides the `codex` field. If a future field is added to the variant, a reviewer walking the remote arm will not notice the new field is being dropped.

The intent ("process-global, not localized") is clearly documented in the comment and is correct — remote Codex state should not be absorbed locally. The hazard is purely in future-proofing.

**Current behavior:**
- `DeltaEvent::CodexUpdated { .. } => { /* no-op except revision bookkeeping */ }`.
- Adding a new field to the variant would not force a compiler or reviewer nudge at this call site.

**Proposal:**
- Use explicit-field destructure: `DeltaEvent::CodexUpdated { revision: _, codex: _ } => { ... }`. Adding a field becomes a compile error.
- Optionally add a doc comment on the variant in `wire.rs` clarifying the localization asymmetry.

## `"sessionId" in delta` poll-cancel branches are not extensible

**Severity:** Low - `ui/src/app-live-state.ts:1613, 1633` handle delta-event poll cancellations by structurally checking `"sessionId" in delta`. The two `revisionAction === "ignore"` / `"resync"` branches each hard-code the knowledge that only `SessionDeltaEvent` variants carry `sessionId`. Adding a third non-session delta type requires remembering to update both branches, and a new session-scoped delta that uses a different key (e.g. `sessionIds: string[]`) would silently miss both gates.

**Current behavior:**
- Two branches each run `"sessionId" in delta && typeof delta.sessionId === "string"`.
- The `SessionDeltaEvent` exclude type in `ui/src/live-updates.ts:76` exists but is not used here.

**Proposal:**
- Extract a `cancelPollsForDelta(delta: DeltaEvent)` helper that switches on `delta.type` (or uses the same `SessionDeltaEvent` narrowing). Call it from both branches.
- That also centralizes the "which deltas cancel which polls" contract in one place.

## `prevIsActive`-in-render replaced with post-commit effect delays the first-activation measurement pass

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:426-432` converted the `prevIsActive !== isActive` render-time derived-state update into a post-commit `useEffect`. Under the previous pattern, a session switching from `isActive: false → true` flipped `setIsMeasuringPostActivation(true)` during render, so the first frame rendered the measuring shell with the correct `preferImmediateHeavyRender` value. The new effect defers that flip to after commit — the first paint of the newly-active session briefly shows `isMeasuringPostActivation: false`, flipping to the measurement shell only on the next render.

Usually invisible (the effect runs the same tick). Under slow devices this may cause a one-frame flicker on session activation.

**Current behavior:**
- Post-commit effect fires after the first frame of the reactivated session.
- First paint uses `isMeasuringPostActivation: false` regardless of the actual transition.

**Proposal:**
- Restore the render-time pattern: `if (prevIsActive !== isActive) { setPrevIsActive(isActive); ... }` (the established React "derived state" form).
- Or upgrade the effect to `useLayoutEffect` so it runs before paint.
- The P2 task for `key={sessionId}` on the virtualizer supersedes this if that fix lands first.

## Focused live sessions monopolize the main thread during state adoption

**Severity:** Medium - a visible, focused TermAl tab with an active Codex session can spend multiple seconds of an 8 s sample on main-thread work even when no requests fail and no exceptions fire.

A live Chrome profile against the current dev tab showed no runtime exceptions, no failed network requests, and no framework error overlay, but the page still burned about `6.6 s` of `TaskDuration`, `0.97 s` of `ScriptDuration`, `372` style recalculations, and several long tasks above `2 s` while Codex was active. The hottest app frames were `handleStateEvent(...)` in `ui/src/app-live-state.ts`, `request(...)` / `looksLikeHtmlResponse(...)` in `ui/src/api.ts`, `reconcileSessions(...)` / `reconcileMessages(...)` in `ui/src/session-reconcile.ts`, `estimateConversationMessageHeight(...)` in `ui/src/panels/conversation-virtualization.ts`, and repeated `getBoundingClientRect()` reads in `ui/src/panels/VirtualizedConversationMessageList.tsx`. A second targeted typing profile pointed the same way: 16 simulated keystrokes averaged only about `1.0 ms` of synchronous input work and about `11 ms` to the next frame, while `handleStateEvent(...)` alone still consumed about `199 ms` of self time. Narrower composer-rerender and adoption-fan-out regressions have been fixed separately, but the remaining profile still points at broader whole-tab churn.

**Current behavior:**
- A visible, focused active session still produces repeated long main-thread tasks while Codex is working or waiting for output.
- Per-chunk session deltas now coalesce their full-session store publication and broad `sessions` render update to one animation frame, but full state snapshots and transcript measurement still need separate cuts.
- `codexUpdated` deltas and same-value backend connection-state updates are now coalesced or ignored, but snapshot adoption remains the dominant unresolved path.
- Slow `state` events now log per-phase timings in development, so the next profiling round should use the `[TermAl perf] slow state event ...` line to pick the next cut.
- Stale same-instance snapshots now avoid full JSON parse, so the remaining problematic lines should be adopted snapshots or server-restart/fallback snapshots.
- `handleStateEvent(...)` still drives broad adoption work through `adoptState(...)` / `adoptSessions(...)`, transcript reconciliation, and follow-on measurement/render work even after the narrower cleanup fan-out cut.
- `/api/state` resync currently reads full response bodies as text and runs `looksLikeHtmlResponse(...)` before JSON parsing, adding avoidable CPU on large successful snapshots.
- Transcript virtualization still spends measurable time on regex-heavy height estimation and synchronous layout reads, so live session churn compounds with scroll/measure work instead of staying isolated to the active status surface.

**Proposal:**
- Make the live state path more metadata-first so transcript arrays, workspace layout, and per-session maps are not reconciled or pruned when the incoming snapshot did not materially change those slices.
- Split the `/api/state` response handling into a cheap JSON-first path and keep HTML sniffing on a narrow error/prefix check instead of scanning whole successful payloads.
- Cache height-estimation inputs by message identity/revision and reduce repeated `getBoundingClientRect()` passes in the virtualized transcript.
- Re-profile the focused active-session path after each cut and keep this issue open until long-task bursts drop back below user-visible jank thresholds.

**Plan:**
- Start at the root of the profile: cut `handleStateEvent(...)` / `adoptState(...)` work first, because that is where both the passive and targeted rounds spend the most app CPU.
- Break the work into independently measurable slices: state adoption fan-out, `/api/state` parsing path, and transcript virtualization measurement/estimation.
- After each slice lands, rerun the live active-session profile and the focused typing round so reductions in `handleStateEvent(...)` self time, `TaskDuration`, and next-frame latency are verified instead of assumed.

## Prompt-settings pane can keep a stale render callback behind SessionBody memoization

**Severity:** Medium - `SessionBody` excludes `renderPromptSettings` from its memo comparator even though prompt mode calls it through a ref, so prompt settings can keep an outdated parent closure until some unrelated prop forces a rerender.

`ui/src/panels/AgentSessionPanel.tsx` memoizes `SessionBody` and intentionally
excludes the render callbacks from its comparator. That works for the message
renderers because the memoized subtree rerenders when their data props change,
but prompt mode calls `renderPromptSettingsRef.current(...)` directly. The ref
is only refreshed when `SessionBody` itself renders. If the parent recreates
the `renderPromptSettings` closure while the compared props stay equal, the
prompt-settings pane keeps using the stale callback.

**Current behavior:**
- `SessionBody` stores `renderPromptSettings` in a ref and reads that ref in
  prompt mode.
- The memo comparator explicitly excludes `renderPromptSettings`.
- Parent renders that only change the prompt-settings closure do not update the
  ref, so prompt mode can keep stale callback behavior.

**Proposal:**
- Include `renderPromptSettings` in the memo comparator, or wrap it in a stable
  ref-backed adapter that is refreshed outside the memo boundary.
- Add a focused regression that re-creates the prompt-settings renderer while
  the compared props stay equal and asserts prompt mode picks up the new
  closure immediately.

## Global Alt+PageUp/PageDown pane cycling outranks nested controls

**Severity:** Medium - the new window-level capture handler for `Alt+PageUp` / `Alt+PageDown` switches pane tabs before focused descendants can consume those shortcuts.

`ui/src/SessionPaneView.tsx` now installs a capture-phase `window` keydown
listener for `Alt+PageUp/PageDown` whenever the pane is active. Because it runs
above the focused widget boundary, nested editors, dialogs, or future
pane-local controls cannot opt into those shortcuts even when they own focus.
That makes the shortcut harder to scope and easier to break as more nested UI
surfaces land inside a session pane.

**Current behavior:**
- Active panes register a capture-phase `window` listener for
  `Alt+PageUp/PageDown`.
- The listener prevents default and switches pane tabs before descendants see
  the shortcut.
- Nested controls therefore cannot claim or suppress the combo from inside the
  active pane.

**Proposal:**
- Route the shortcut through the pane-root key handling path instead of a
  window-global capture listener.
- Or gate the capture listener with the same focused-target checks used for the
  other page-key routing so descendants can opt out.
- Add a focused regression with a nested focusable control that handles
  `Alt+PageUp/PageDown` and assert pane cycling does not preempt it.

## Composer drafts have three authoritative stores

**Severity:** Medium - committed composer drafts are tracked in React state (`draftsBySessionId`), a mutable ref (`draftsBySessionIdRef`), and the new `useSyncExternalStore`-backed `session-store`, with a post-commit effect mirroring state → ref and imperative paths writing the ref before React commits. Under concurrent draft updates the deferred effect can overwrite a newer ref value with a stale committed one, which then propagates to the composer snapshot via `syncComposerDraftForSession`.

`ui/src/session-store.ts` added a third source of truth for per-session drafts. Imperative handlers in `ui/src/app-session-actions.ts` (`handleDraftChange`, `sendPromptForSession`, queue-prompt flows) and `ui/src/app-workspace-actions.ts` write `draftsBySessionIdRef.current` synchronously before calling `setDraftsBySessionId`, so the store sync reads the fresh value. A separate effect in `ui/src/App.tsx` copies `draftsBySessionId` back into the ref after each commit. When two draft updates land in the same tick, the later-committed effect can briefly regress the ref to an older snapshot, and the store's composer-snapshot slice (`syncComposerDraftForSession`) can publish that stale draft to subscribers.

**Current behavior:**
- Three stores own the same data: React state, the ref, and the `session-store` slice.
- Imperative paths write ref → store before React commits; the effect writes state → ref after commit.
- Under concurrent updates the effect can stomp a newer imperative write with a stale React-committed value.

**Proposal:**
- Pick one owner for the ref: either drop the post-commit effect and rely entirely on imperative writes, or remove the imperative ref mutations and let the store read through a ref that mirrors state exactly once per commit.
- Document the invariant in the `session-store.ts` header so future changes do not reintroduce a third writer.
- Add a regression test that drives two overlapping `handleDraftChange` calls in the same tick and asserts the store snapshot matches the last-written value.

## `startActivePromptRecoveryPoll` only armed when adoption is stale

**Severity:** Medium - the recovery poll (renamed from `startActivePromptPoll`) previously armed on every successful `sendMessage` POST to cover the "POST acknowledged but SSE never streams" failure mode; it is now armed only when `adoptState(state)` returns false, removing the belt-and-suspenders check against silent SSE stalls following a successful POST.

`ui/src/app-session-actions.ts` narrowed the recovery poll to the stale-adoption branch. The SSE watchdog (`handleLiveSessionResumeWatchdogTick`) may already cover the "POST succeeded + SSE delta never lands" scenario, but no test exercises that path end-to-end after the change. The pre-existing `active-prompt-poll.ts` docblock explicitly mentions covering the post-POST silent-stall case — so either the watchdog provides equivalent coverage (in which case the comment is wrong), or the change removes a defense that wasn't duplicated elsewhere.

**Current behavior:**
- A successful `sendMessage` POST whose response the revision gate adopts never arms the recovery poll.
- Only stale POST responses (revision already exceeded by SSE) arm the poll.
- No regression test distinguishes "POST succeeds + SSE eventually streams" from "POST succeeds + SSE never streams".

**Proposal:**
- Add a test that mocks a successful POST followed by a blocked SSE stream and asserts the watchdog (or the recovery poll) eventually restores progress.
- If the watchdog provides the coverage, add a comment next to the new conditional documenting the reasoning and noting the `active-prompt-poll.ts` docblock that should be updated in step.
- If no other path covers it, restore the unconditional arm.

## `resolvePromptHistory` identity branches uncovered

**Severity:** Medium - `ui/src/session-store.ts::resolvePromptHistory` gates when the composer's `promptHistory` snapshot keeps object identity, but `session-store.test.ts` exercises only the happy-path branches. Three identity-determining branches — message-list shrinkage, boundary-id mismatch (in-place substitution/reorder), and equal-length "last message is a user prompt" — have no direct coverage, so a regression there would silently force the composer to re-render on every streaming delta.

`resolvePromptHistory` returns the previous `promptHistory` array (preserving identity) when the known-last-prompt still matches, and rebuilds a fresh array otherwise. Composer memoization depends on that identity preservation. If the function accidentally rebuilds on every call — or accidentally preserves identity when the history actually changed — the composer either re-renders on every assistant chunk or fails to refresh when the user edits history. Neither failure mode surfaces in the current tests.

**Current behavior:**
- Happy paths (assistant append, user prompt append, empty list) have tests.
- `nextLength < previousLength` (full recollect), `nextLength === previousLength` with mismatched boundary id (in-place edit/reorder), and the same-length-last-is-user-prompt passthrough are uncovered.

**Proposal:**
- Add one test per uncovered branch, asserting on both the returned value and its identity relative to the prior call's result.

## Session removal pruned only on the snapshot-adoption path

**Severity:** Low - `ui/src/session-store.ts` has no `removeSessionFromStore(...)` entry point, and the delta paths (`orchestratorsUpdated`, session-scoped deltas) only `upsertSessionSlice` for ids present in the delta. Today deltas cannot remove sessions, so this is latent — but the store has no defensive pruning and nothing in the file header documents which caller is responsible for eviction.

`syncComposerSessionsStore` handles pruning as a side effect of diffing `sessions[]`, so a full snapshot adoption cleans up orphans; the delta paths never do. If a future delta shape implies a session has been removed (e.g. a dropped slot in `mergeOrchestratorDeltaSessions`), the orphan slice would linger in `sessionRecordsById`, `sessionSummariesById`, and `composerSessionsById` until the next full snapshot.

**Current behavior:**
- Only `syncComposerSessionsStore` prunes the store; delta-scoped upserts never do.
- No documented contract in `session-store.ts` for which caller owns eviction.

**Proposal:**
- Add a `removeSessionFromStore(sessionId)` helper and wire it to the same places `setSessions` drops a session, or document the pruning contract in the `session-store.ts` header so future delta code knows to call `syncComposerSessionsStore` (or equivalent) when a session is removed.

## Runtime-only session mutation stamps can leak into persisted sessions

**Severity:** Low - `session_mutation_stamp` is now represented on the shared
`Session` wire struct, but that same struct is embedded in persisted
`PersistedSessionRecord` values.

The intended ownership is that `SessionRecord::mutation_stamp` is process-local
runtime metadata and `wire_session_from_record(...)` is the only outbound source
for the frontend-facing `sessionMutationStamp`. Remote proxy localization can
clone an inbound remote session payload into local `record.session`; if that
payload includes a remote process stamp, persistence can serialize it as part
of the local session. That does not break current behavior, but it blurs local
vs. remote stamp ownership and makes durable state carry a meaningless
process-local marker.

**Current behavior:**
- `Session` includes optional `session_mutation_stamp`.
- `PersistedSessionRecord` persists a `Session` value directly.
- Remote-localized sessions can arrive with a remote stamp unless every inbound
  path scrubs it.

**Proposal:**
- Clear `session_mutation_stamp` before persistence and after localizing inbound
  remote sessions.
- Keep `AppState::wire_session_from_record(...)` as the only path that sets the
  outbound stamp.
- Add a backend serialization/localization regression that proves persisted
  sessions do not contain `sessionMutationStamp`.

## `looksLikeHtmlResponse` 256-char slice drops leading-whitespace tolerance

**Severity:** Low - `ui/src/api.ts::looksLikeHtmlResponse(...)` now slices the first 256 raw characters before `trimStart().toLowerCase()`. A proxy or dev-server error page that emits more than 256 bytes of leading whitespace before `<html>` is no longer detected as HTML and falls through to `JSON.parse`, so the "Restart TermAl" guidance never surfaces for that response.

The bounded prefix probe is a performance improvement over the old "scan the whole body" behaviour, but the old code trimmed leading whitespace before inspecting the prefix, which this version does not. Realistic proxies are unlikely to emit >256 bytes of whitespace, but when they do the user sees a generic parse error instead of the restart prompt.

**Current behavior:**
- `raw.slice(0, 256).trimStart().toLowerCase()` — whitespace that exceeds 256 bytes pushes the `<html>` marker past the probe window.

**Proposal:**
- Reorder the operations: `raw.trimStart().slice(0, 256).toLowerCase()` — preserves the old semantics while still bounding the slice cost.

## `response.clone()` buffers every successful JSON response a second time

**Severity:** Low - the new JSON fast-path in `ui/src/api.ts::request(...)` calls `response.clone()` eagerly before parsing so the rare JSON-parse-failure branch can read the body as text. For large responses (full state snapshots, file reads, terminal-run transcripts) this doubles the memory footprint of every healthy JSON response just to preserve a fallback that is never consumed.

The existing `api.test.ts` case ("uses the JSON fast path for successful application/json responses") asserts the clone's `text()` call does NOT run on success — confirming the clone's buffered body is dead weight >99% of the time.

**Current behavior:**
- Every `response.ok` + JSON content-type path clones the response body immediately.
- The clone is only read in the catch branch, which the fast path almost never reaches.

**Proposal:**
- Consume `response.text()` once and `JSON.parse` the string, trading one allocation for avoiding the double-buffering. The HTML-sniff fallback becomes a plain branch on the already-materialised text string.

## `useDeferredValue(pendingPrompts)` receives a fresh `[]` each render

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:555-557` reads `session.pendingPrompts ?? []`, allocating a new empty array on every render when `pendingPrompts` is `undefined` (the common case). `useDeferredValue` treats each render as a changed value and schedules a transition even though the content is identical, which mildly defeats the purpose of deferring the value.

**Current behavior:**
- `pendingPrompts` is `session.pendingPrompts ?? []`.
- Every render when `pendingPrompts` is absent allocates a fresh array.
- `useDeferredValue` schedules a transition for identical empty content.

**Proposal:**
- Hoist a module-scope `const EMPTY_PENDING_PROMPTS: readonly PendingPrompt[] = [];` and use it as the fallback, so the deferred-value input has stable identity when the list is empty.

## Composer sizing double-resets on session switch

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:918-931` runs `resizeComposerInput(true)` synchronously inside a `useLayoutEffect` keyed on `[activeSessionId]`, and a following `useEffect` keyed on `[composerDraft]` schedules another resize via `requestAnimationFrame` on the same first render. The rAF resize is redundant because the synchronous one already measured the new metrics.

**Current behavior:**
- Layout effect resets cached sizing state and calls `resizeComposerInput(true)` synchronously.
- Draft effect schedules a second `requestAnimationFrame` resize on the same first render.
- First render of any newly-activated session does two resize passes instead of one.

**Proposal:**
- Track a "just-resized-synchronously" flag set in the layout effect and checked at the top of `scheduleComposerResize`, or gate the draft effect with a prev-draft ref so the "initial draft equals committed" case is a no-op.

## Composer autosize does not shrink on width-only pane resize

**Severity:** Low - the optimized composer resize path no longer forces a
shrink-capable measurement when only the textarea width changes.

`ui/src/panels/AgentSessionPanel.tsx` coalesces autosize work and only forces
the shrink-capable measurement (previously `height = "auto"`, now
`height = "0px"`) for session switches or panel-height changes. Widening a pane
can reduce text wrapping and therefore reduce the required textarea height, but
a width-only `ResizeObserver` update calls `scheduleComposerResize(...)`
without the force flag. The measured height can remain taller than its content
until another draft or session change triggers a full reset.

**Current behavior:**
- Pane width changes can alter wrapping without changing panel height.
- The resize scheduler does not force the textarea through a shrink-capable
  measurement pass for width-only changes.
- The composer can stay over-tall after widening the pane.

**Proposal:**
- Treat `widthChanged || panelHeightChanged` as a shrink-capable resize input,
  or otherwise force a shrink-capable measurement whenever wrapping can change.
- Add a focused test that widens the composer container and asserts the textarea
  height can shrink without requiring another keystroke.

## Duplicated `Session` projection types in `session-store.ts` and `session-slash-palette.ts`

**Severity:** Low - `ComposerSessionSnapshot` (`ui/src/session-store.ts:36-83`) and `SlashPaletteSession` (`ui/src/panels/session-slash-palette.ts:51-65`) each re-pick overlapping-but-non-identical field sets from `Session`. Three `Session`-like shapes now exist (`Session`, `ComposerSessionSnapshot`, `SlashPaletteSession`) with no compile-time check that additions to `Session` reach both projections — a new agent setting added to `Session` could silently default to `undefined` in consumers that read through either projection.

**Current behavior:**
- Both projection types declare field lists by hand.
- No `Pick<Session, ...>` derivation; nothing fails to compile when `Session` grows a new field.

**Proposal:**
- Derive both types via `Pick<Session, ...>`, or express `SlashPaletteSession` as `Omit<ComposerSessionSnapshot, ...>` where their field sets differ.
- Colocate the derivations in `session-store.ts` so the projection contract is visible in one place.

## `activeSessionId` vs. `session` dual identity in `SessionComposer`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:791-797` computes `activeSessionId = session?.id ?? sessionId` so `activeSessionId` is truthy while the store is still catching up and `session` is still null. Other call sites guard differently (some check `!session`, some check `!activeSessionId || !session`). A future caller that guards only on `activeSessionId` will proceed with a null snapshot.

**Current behavior:**
- `activeSessionId` is truthy in a narrow window where `session` is still null.
- The two notions of "active" diverge in that window, and nothing documents the invariant.

**Proposal:**
- Either treat "snapshot null but id truthy" as "no session" (fall back to `null`), or add a comment near the fallback documenting that `activeSessionId` is a best-effort fallback and callers must still check `session` before reading capability fields.

## `session_message_count` silently saturates at `u32::MAX`

**Severity:** Low - `src/messages.rs:23-25` defines `session_message_count(record)` as `u32::try_from(record.session.messages.len()).unwrap_or(u32::MAX)`. A session with more than 4.29 billion messages silently reports `u32::MAX` rather than failing or surfacing the invariant violation. Practically unreachable (the process would OOM long before), but silent saturation defeats the Contract Precisions choice of `u32` over `usize` if the assumption ever breaks.

The frontend would then treat `4294967295` as truth, which would mis-represent session metadata in a way that's hard to diagnose.

**Current behavior:**
- `.unwrap_or(u32::MAX)` silently caps.
- No `debug_assert!` surfaces the assumption in test runs.
- No comment explaining the intentional saturation.

**Proposal:**
- Add `debug_assert!(record.session.messages.len() <= u32::MAX as usize)` above the conversion so tests catch the impossible.
- Alternatively, leave as-is but add a one-line comment explaining the intentional saturation so a future reviewer doesn't reach for checked arithmetic.

## HTTP-route tests leak persistence and orchestrator-template files into `temp_dir`

**Severity:** Medium - three new `tokio::test`s in `src/tests/http_routes.rs` leak `termal-test-*.json` and `termal-orchestrators-test-*.json` files into `std::env::temp_dir()` on every run. The tests move `state` into `app_router(state)` before capturing `state.persistence_path` / `state.orchestrator_templates_path`, so the `fs::remove_file` cleanup at the end of each test never executes.

Affected tests: `codex_thread_action_routes_update_session_state` (`:510`), `codex_thread_rollback_route_falls_back_when_history_is_unavailable` (`:670`), and `codex_thread_fork_route_returns_created_response` (`:757`). Over repeated local test runs this accumulates test artifacts that persist across development sessions.

**Current behavior:**
- Each test constructs `state`, reads fields off it, then moves `state` into `app_router(state)`.
- No file paths are captured before the move, so cleanup can't run.
- `%TEMP%` accumulates `termal-test-*.json` + `termal-orchestrators-test-*.json` files.
- Sibling tests in the same file that do clean up take the opposite approach (capture paths first).

**Proposal:**
- Clone `state` once at the top of each test (or destructure `state.persistence_path` and `state.orchestrator_templates_path` into local `PathBuf`s) before moving `state` into the router.
- In the cleanup block, call `fs::remove_file` on both the persistence path and the orchestrator-templates path.
- Extend the pattern to any other `http_routes.rs` tests that currently miss cleaning the orchestrator-templates file — this is a cross-cutting cleanup.

## SSE delta test doesn't pin `messageCount` on the delta payload

**Severity:** Low - `src/tests/http_routes.rs::state_events_route_streams_initial_state_and_live_deltas` (`:214-271`) asserts the SSE frame ordering (initial `state`, then `delta`) and the `type: "messageCreated"` discriminator, but does not assert `messageCount` on the delta payload itself. The snapshot test (`snapshot_bearing_routes_include_message_count`) pins `messageCount` on `StateResponse` / `SessionResponse`; the SSE delta wire contract is not pinned at the HTTP layer.

**Current behavior:**
- `snapshot_bearing_routes_include_message_count` pins `messageCount` on snapshot shapes.
- `state_events_route_streams_initial_state_and_live_deltas` asserts frame order and discriminator but not `messageCount` on the delta.
- The `tests/remote.rs` layer covers delta `messageCount` end-to-end, but the HTTP-level wire shape is unpinned for deltas.

**Proposal:**
- Add `assert_eq!(delta["messageCount"], 1);` (or an appropriate expected value) to `state_events_route_streams_initial_state_and_live_deltas` after the `type: "messageCreated"` assertion. One-line addition that closes the symmetry with the snapshot test.

## `docs/architecture.md` documents soft-rollout but not the `DeltaEvent` hard-break

**Severity:** Low - `docs/architecture.md:238-243` describes that `Session.messageCount` now rides on the wire with `#[serde(default)]` (soft rollout), but does not document the companion `DeltaEvent.*.messageCount` hard-break stance. A reader looking only at `architecture.md` cannot tell that mixed-version remote SSE bridges will hard-fail on missing `messageCount`. The policy is recorded in `docs/metadata-first-state-plan.md` Contract Precisions → Field semantics, but not cross-referenced from the architecture doc.

**Current behavior:**
- `architecture.md` describes the `Session.messageCount` soft-rollout.
- It does NOT mention that `DeltaEvent.*.messageCount` is required (no `#[serde(default)]`) and that mixed-version remote bridges are out of scope.
- `docs/metadata-first-state-plan.md:170-175` has the policy; `architecture.md` doesn't link to it.

**Proposal:**
- Add one sentence to the delta section: "Note: `DeltaEvent.*.messageCount` is required on the wire (no `#[serde(default)]`) — see `docs/metadata-first-state-plan.md` Contract Precisions → Field semantics for the intentional soft-rollout-on-Session + hard-break-on-Delta asymmetry."

## `#[cfg(test)] snapshot()` creates test-vs-production contract divergence

**Severity:** High - `src/state_accessors.rs:54-87` defines `snapshot()` as `full_snapshot()` in test builds and `summary_snapshot()` in production. Tests read `state.snapshot()` and rely on the full-transcript form (~40 sites in `src/tests/remote.rs` read `session.messages` directly from the snapshot), while production emits the summary shape. This is a silent behavioral split: a regression that flips a production code path from summary → full transcript (e.g. a new snapshot builder that forgets `wire_session_summary_from_record`) does not surface in any unit test that uses `snapshot()`.

Two HTTP-route contract tests (`snapshot_bearing_routes_include_message_count`) pin the route-level shape, but everything else in `src/tests/` continues to exercise a shape production never emits. The split was introduced as a transitional bridge so existing transcript-reading tests don't have to be rewritten, but the intent is invisible at every call site.

**Current behavior:**
- `#[cfg(test)] fn snapshot(&self) -> StateResponse { self.full_snapshot() }`
- `#[cfg(not(test))] fn snapshot(&self) -> StateResponse { self.summary_snapshot() }`
- ~40 test call sites rely on the full-transcript form.
- Two HTTP-route tests pin the production shape; everything else is legacy coverage.

**Proposal:**
- Rename the cfg-split helpers so intent is audible at every call site: tests explicitly call `full_snapshot_for_test()`; production paths explicitly call `summary_snapshot()`. Same body, different names.
- Or migrate the ~40 `state.snapshot().sessions[...].messages` call sites to read `state.inner.lock().sessions[...]` directly so tests assert against record state rather than wire state. Safer — transcript mutation checks don't route through the wire-projection helper at all.
- Either fix closes the latent cliff. The cfg-split without renames is deprecated.

## `resolvedWaitingIndicatorPrompt` duplicates `findLastUserPrompt` derivation across `SessionBody` and `SessionPaneView`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:399-404` computes `resolvedWaitingIndicatorPrompt` by calling `findLastUserPrompt(activeSession)` inside `SessionBody` whenever the live turn indicator is showing, overriding the `waitingIndicatorPrompt` prop that `ui/src/SessionPaneView.tsx:795-805` already computed via the same helper and `useMemo`. The override was added to pick up store-subscriber updates between parent renders (correct intent), but it leaves two parallel code paths that must be kept in sync.

Two smaller concerns ride along:
- The override's condition includes an `"approval"` status arm (`status === "active" || status === "approval"`) that is presently unreachable: `SessionPaneView` only sets `showWaitingIndicator=true` when `status === "active"` or (`!isSessionBusy && isSending`), and `isSessionBusy` is true for `"approval"`, so `showWaitingIndicator && status === "approval"` never holds. Harmless defensive check but misleading for readers inferring the truth table.
- The resolution is not wrapped in `useMemo`, so it re-runs on every `SessionBody` re-render — once per streaming chunk. `findLastUserPrompt` scans from the tail, so it usually stops early, but sessions dominated by trailing tool/assistant output could scan deep.

**Current behavior:**
- `SessionBody` (`AgentSessionPanel.tsx:399-404`) and `SessionPaneView` (`SessionPaneView.tsx:795-805`) both derive the waiting-indicator prompt by calling `findLastUserPrompt(activeSession)` on the same store record.
- The override runs on every `SessionBody` render, uncached.
- The `status === "approval"` arm of the override's condition is unreachable under current upstream gating.

**Proposal:**
- Collapse to one computation at the store-subscriber boundary. Either `SessionBody` becomes the sole resolver (drop the `useMemo` and prop passthrough in `SessionPaneView`), or add a one-line cross-reference comment on both sites so future readers know the two are paired.
- Narrow the override's condition to `status === "active"` to match the upstream truth table.
- Wrap the override in `useMemo(() => findLastUserPrompt(activeSession), [activeSession.messages])` to avoid re-scanning on every streaming chunk.

Also fixed in the current tree: transcript search pinning no longer replaces
the live mounted page band. `VirtualizedConversationMessageList.tsx` now renders
the viewport band and the active search-hit band as separate page segments with
spacers between them, so keeping a search result pinned does not force every
page between the viewport and the hit into the DOM and does not let the live
viewport fall into blank space. `AgentSessionPanel.test.tsx` now covers both the
"typed search stays virtualized" path and the "search result stays pinned while
the user scrolls elsewhere" path.

Also fixed in the current tree: stale create/fork recovery no longer loses
earlier pending opens when multiple recoveries overlap. `useAppLiveState` now
tracks recovery open intent as a collection keyed by session id, partitions that
collection against each adopted snapshot, and only consumes an intent once the
authoritative session list actually contains the recovered session. Focused
coverage in `ui/src/app-live-state.test.ts` now pins the cleanup-plan branches,
intent partitioning, and duplicate-intent replacement semantics.

Also fixed in the current tree: `SessionComposer` memoization now includes
`session.workdir` and `session.agentCommandsRevision`, so slash-command refresh
rerenders happen when the request key changes instead of leaving stale
agent-command state in place. `AgentSessionPanel.test.tsx` now covers both the
negative path (assistant-only churn stays memoized) and positive workdir /
revision changes that must rebuild the slash palette.

Also fixed in the current tree: session `PageUp` / `PageDown` routing now goes
through `resolvePaneScrollCommand(...)` in `SessionPaneView.tsx` instead of the
old session-only early return. Plain page-scroll commands still use the
transcript-specific fixed-delta path, but `Ctrl+PageUp/PageDown` once again stay
on the shared boundary-jump contract, and editable descendants only get the
capture fallback when they would otherwise stop propagation before the pane
shell can resolve the key.

Also fixed in the current tree: programmatic transcript page jumps now keep the
virtualizer's scroll bookkeeping in sync. The synthetic scroll-write path in
`VirtualizedConversationMessageList.tsx` advances `lastNativeScrollTopRef`,
preserves the current scroll delta, and re-arms idle compaction/reconciliation
instead of clearing that state immediately and leaving the next real scroll to
measure from a stale baseline.

Also fixed in the current tree: the latest-user prompt-follow branch in
`SessionPaneView.tsx` now preserves and invokes the cleanup returned by
`followLatestMessageForPromptSend()`. Rerender/unmount no longer leaves the
settled scroll-to-bottom loop running after the effect should have been torn
down.

Also fixed in the current tree: the nested-editable `PageUp` / `PageDown`
fallback in `SessionPaneView.tsx` now uses a ref-backed window capture listener,
so active-session switches no longer leave the global handler writing scroll
state, stickiness, or new-response bookkeeping under the previous session key.
`App.scroll-behavior.test.tsx` now restores a two-tab session pane, switches the
active tab, and proves a nested editable `PageDown` still updates the current
session instead of the stale one.

Also fixed in the current tree: `syncProgrammaticScrollWrite(...)` in
`VirtualizedConversationMessageList.tsx` now reclassifies every synthetic
scroll from its current delta instead of only when `lastUserScrollKindRef` was
already `null`. Programmatic jumps no longer inherit stale `"incremental"` /
`"seek"` intent from the previous gesture, and
`AgentSessionPanel.test.tsx` now drives a wheel gesture followed by a distant
programmatic jump and a small native scroll to pin that reclassification.

Also fixed in the current tree: the nested-editable `PageUp` / `PageDown`
fallback in `SessionPaneView.tsx` is now scoped to the active pane root instead
of acting as a global owner for any editable target in the window. The capture
listener still rescues nested editors that stop propagation, but it now bails
unless `event.target` lives under the active session pane. A focused
`App.scroll-behavior.test.tsx` case proves an external textarea no longer pages
the transcript.

Also fixed in the current tree: programmatic transcript scroll writes now carry
explicit scroll intent when the caller already knows it. `SessionPaneView.tsx`
tags keyboard page jumps and boundary jumps as `"seek"` in the
`MESSAGE_STACK_SCROLL_WRITE_EVENT` detail, and
`VirtualizedConversationMessageList.tsx` consumes that detail before falling
back to raw-delta classification. Smaller fixed-delta keyboard jumps no longer
lose their seek semantics just because the synthetic delta is below the generic
seek threshold.

Also fixed in the current tree: the virtualizer no longer keeps sticky
keyboard seek intent in a fallback ref. `pendingProgrammaticScrollKindRef`
is gone from `VirtualizedConversationMessageList.tsx`, so no-op
`PageUp` / `PageDown` / `Home` / `End` keys cannot arm a stale `"seek"`
classification for a later unrelated programmatic scroll write. Synthetic
scrolls now classify from explicit `scrollKind` metadata when provided and
otherwise from the current delta only.

Also fixed in the current tree: plain session-transcript `PageDown` now has
focused positive-path coverage in `App.scroll-behavior.test.tsx`. The test
drives an unmodified downward page jump against the real session transcript,
pins the fixed-delta jump itself, and asserts the saved transcript bookkeeping
still lands on the non-sticky path instead of falling back to browser-native
paging.

Also fixed in the current tree: the virtualizer's post-jump idle reconcile is
now directly covered. `AgentSessionPanel.test.tsx` drives an incremental
gesture, a distant programmatic jump, a follow-up small native scroll, then
advances the idle timer and asserts the mounted page band changes again after
settle while staying anchored to the jumped region.

Also fixed in the current tree: the latest-user prompt-send follow branch now
has direct coverage in `App.scroll-behavior.test.tsx`. The test drives a send
while the newest visible message is user-authored, asserts the prompt-follow
path scrolls immediately to the newest message, and proves rerender/unmount
does not leave the settled-scroll loop running afterward.

Also fixed in the current tree: the Windows-specific `Ctrl+PageUp` regression
test in `App.scroll-behavior.test.tsx` now deletes the stubbed own
`navigator.platform` property when the original value was inherited from the
prototype, so the test no longer leaks `"Win32"` into later platform-sensitive
cases.

Also fixed in the current tree: the explicit `scrollKind` event-detail bridge
now has direct regression coverage. `AgentSessionPanel.test.tsx` drives a
sub-threshold synthetic transcript jump, dispatches
`notifyMessageStackScrollWrite(scrollNode, { scrollKind: "seek" })`, and pins
that the virtualizer follows the seek reconciliation path instead of falling
back to raw-delta incremental classification.

Also fixed in the current tree: plain transcript `PageUp` now has focused
positive-path coverage in `App.scroll-behavior.test.tsx`. The test starts near
bottom, fires an unmodified `PageUp` against the real session transcript,
asserts the fixed upward delta, and proves the pane stays detached from bottom
by showing the next assistant update behind the `"New response"` affordance.

Also fixed in the current tree: the remaining extracted session-action
handlers in `ui/src/app-session-actions.ts` now honor unmount boundaries.
Create-project, root-picker, approval/user-input submissions, queued-prompt
cancel, stop-session cleanup, refresh-agent-commands, send, and
session-settings flows all bail before post-`await` state adoption, error
reporting, and `finally` cleanup when `isMountedRef.current` is false, so the
hook no longer mutates torn-down UI after unmount.

Also fixed in the current tree: Unix terminal login-shell behavior now has a
dedicated regression test in `src/tests/terminal.rs`.
`build_terminal_shell_command_uses_login_shell_on_unix` pins the Unix argv as
`sh -lc <command>`, so a future regression back to `sh -c` fails fast instead
of silently dropping login-shell PATH/tooling setup.

Also fixed in the current tree: `docs/features/session-virtualized-transcript.md`
now documents the current keyboard page-jump contract. The brief names
`SESSION_PAGE_JUMP_VIEWPORT_FACTOR`, explains that `SessionPaneView.tsx`
performs the `scrollTop` write directly, and records that
`MESSAGE_STACK_SCROLL_WRITE_EVENT` can carry explicit scroll intent for the
virtualizer.

Also fixed in the current tree: `ui/src/message-stack-scroll-sync.ts` now has
an inline contract comment documenting the producer/consumer seam for
programmatic transcript scroll writes. The comment states that producers must
emit the event immediately after direct message-stack scroll writes and that
keyboard-owned seek jumps should provide explicit `detail.scrollKind` when raw
delta size is not authoritative.

Also fixed in the current tree: `SettingsTabBar.tsx` now prevents default for
recognized `Home` / `End` keys before the no-op destination check. The first
tab's `Home` and the last tab's `End` no longer fall through to browser scroll,
and `SettingsTabBar.test.tsx` now pins both no-op edge cases.

Also fixed in the current tree: code-heavy immediate-render coverage now
reaches a real message-card path. `MarkdownContent.test.tsx` renders a
code-heavy approval card with `preferImmediateHeavyRender`, asserts the real
highlighted content appears immediately, and proves the deferred placeholder /
`IntersectionObserver` fallback path never activates.

Also fixed in the current tree: `AppDialogs.test.tsx` no longer weakens its own
type signal with `as never` fixture coercions. The dialog integration fixture
now uses real union literals and typed theme/style/config values, so prop drift
surfaces at compile time instead of being masked inside the test harness.

Also fixed in the current tree: stale create/fork recovery no longer consumes
open-session intent before the session actually exists. `app-live-state.ts`
now keeps recovery open intent pending, gates `openSessionInWorkspaceState(...)`
on the reconciled session list containing the target id, and only consumes that
intent once an adopted snapshot or SSE state really includes the recovered
session. Missing recovery snapshots therefore stay phantom-free instead of
reopening the tab later through `/api/state`.

Also fixed in the current tree: the stale create/fork lifecycle tests now pin
the pre-resync no-open invariant. `App.session-lifecycle.test.tsx` asserts
immediately after the stale response resolves and before the recovery snapshot
lands that the target session tab/composer is still absent, then separately
asserts the recovered session appears only after the authoritative snapshot
that actually includes it.

Also fixed in the current tree: deferred stale create recovery opening is now
covered after a missing recovery snapshot. `App.session-lifecycle.test.tsx`
extends the stale create flow so the first recovery `fetchState` result still
omits the target session, asserts nothing opens yet, then adopts a later SSE
state that includes the session and proves the pending open fires exactly once
at that point.

Also fixed in the current tree: rendered-Markdown commit range resolution now
normalizes `commit.sourceContent` before running the strategy-2
`mapMarkdownRangeAcrossContentChange(...)` fallback. CRLF-on-disk documents no
longer drift offsets in the fallback path just because the current document is
already LF-normalized for segment math, and
`markdown-commit-ranges.test.ts` now pins that prefix-shift mapping on a CRLF
baseline.

## Conversation cards overlap for one frame during scroll through long messages

**Severity:** Medium - `estimateConversationMessageHeight` in `ui/src/panels/conversation-virtualization.ts` produces an initial height for unmeasured cards using a per-line pixel heuristic with line-count caps (`Math.min(outputLineCount, 14)` for `command`, `Math.min(diffLineCount, 20)` for `diff`) and overall ceilings of 1400/1500/1600/1800/900 px. For heavy messages â€” review-tool output, build logs, large patches â€” the estimate is 20Ã—-40Ã— under the rendered height, so `layout.tops[index]` for cards below an under-priced neighbour places them inside the neighbour's rendered area. The user sees the cards painted on top of each other for one frame, until the `ResizeObserver` measurement lands and `setLayoutVersion` rebuilds the layout.

An initial attempt to fix this by raising estimates to a single 40k px cap (and adding `visibility: hidden` per-card until measured) was reverted after it introduced two worse regressions: (1) per-card `visibility: hidden` combined with the wrapper's `is-measuring-post-activation` hide left the whole transcript empty for a frame whenever the virtualization window shifted before measurements landed; (2) raising the cap made the `getAdjustedVirtualizedScrollTopForHeightChange` shrink-adjustment huge (40k estimate â†’ 8k actual = âˆ’32k scrollTop jump), so slow wheel-scrolling through heavy transcripts caused visible scroll jumps of tens of thousands of pixels. The revert restores the one-frame overlap as the known limitation.

**Current behavior:**
- Initial layout uses estimates that badly under-price long commands / diffs.
- First paint places subsequent cards overlapping the under-priced one for one frame.
- Next frame, `ResizeObserver` fires, `setLayoutVersion` rebuilds, positions correct.
- Visible to the user as a brief "jumble" during scroll.

**Proposal:**
- Proper fix likely needs off-screen pre-measurement (render the card in a hidden measure-only tree, read `getBoundingClientRect` height, then place in the layout) rather than a formula-based estimate. This is a bigger change than a single pure-function tweak.
- Alternative: batch-measurement pass when the virtualization window shifts â€” hide the wrapper briefly, mount the newly-entering cards, wait for all their measurements, then reveal.
- Not: raise the estimator cap. Large overshoots trade one visible artifact for a worse one.


## Rendered Markdown diff view cannot jump between changes

**Severity:** Medium - regular file diffs expose previous/next change navigation, but the rendered Markdown diff view does not, so reviewing a long Markdown document requires manual scrolling and visual scanning.

This is especially noticeable because the same diff tab already has change navigation for the Monaco file-diff view. Switching to the rendered Markdown view removes that workflow even though the rendered segments already know which sections are added, deleted, or changed.

**Current behavior:**
- Monaco file diff shows change navigation controls and a `Change X of Y` counter.
- Rendered Markdown diff view shows highlighted added/deleted/changed sections, but does not expose next/previous change controls.
- Keyboard or toolbar navigation cannot jump between rendered Markdown change sections.

**Proposal:**
- Build a rendered-Markdown change index from the same segment model used to paint added/deleted/changed sections.
- Add previous/next controls and a `Change X of Y` counter for rendered Markdown mode, matching the regular diff affordance.
- Keep per-view scroll position stable when jumping or switching between Monaco and rendered Markdown diff views.
- Add coverage that rendered Markdown diff mode focuses/scrolls to the next and previous changed section without leaving the rendered view.

## Inline-zone id is line-number-dependent, reinitialises Mermaid diagrams on every edit above the fence

**Severity:** Medium - `ui/src/source-renderers.ts::detectMarkdownRegions` builds each Mermaid fence region's id as `mermaid:${fence.startLine}:${fence.endLine}:${quickHash(fence.body)}`. `startLine` and `endLine` are 1-based ABSOLUTE line numbers in the source buffer, so inserting any line above the fence shifts both â€” and the id flips. The `MonacoCodeEditor` portal is keyed on the zone id (see `MonacoCodeEditor.tsx:~718-730`); when the id flips, the portal unmounts and remounts, which tears down the Mermaid iframe and reinitialises it from scratch. Every keystroke in the heading / paragraphs above a Mermaid fence triggers this reinitialisation, producing a visible flicker on slow machines and wasting GPU cycles on fast ones.

The intent of the stable id was exactly the opposite â€” keep the diagram DOM alive across keystrokes outside the fence. A new test pinned the contract as it exists today (`SourcePanel.test.tsx::"inline-zone id stability" â†’ "changes the zone id when lines are inserted above the fence (latent stability gap)"`) so a future fix has a clear assertion to flip from `.not.toBe` to `.toBe`.

**Current behavior:**
- Id format: `mermaid:${startLine}:${endLine}:${hash(body)}`.
- Inserting a line above the fence shifts `startLine` â†’ id changes â†’ portal remounts â†’ Mermaid reinitialises.
- Typing inside the fence body changes the hash â†’ id changes â†’ portal remounts (correct â€” the diagram source changed).
- Editing below the fence (or in-place edits above without line-count changes) preserves startLine/endLine/body â†’ id stable (correct).

**Proposal:**
- **Primary**: drop `startLine`/`endLine` from the id and use `mermaid:${hash(body)}` alone. This preserves id stability under line shifts. The id must stay globally unique per file (the portal-key dedupe via `new Set(inlineZones.map((zone) => zone.id))` in `MonacoCodeEditor.tsx::zone-sync effect` collapses collisions into one entry, so non-unique ids would lose zones), which means a tiebreaker is needed ONLY when two fences collide on body hash. Tiebreaker rule: within a file, take the ordinal position of this fence among all fences that share its body hash, in document order (i.e., `mermaid:0:${hash}` for the first fence with this body, `mermaid:1:${hash}` for the second, etc.). Collisions are rare in practice; when they do happen, reordering two identical-body fences remounts both â€” semantically a no-op because identical bodies render identical diagrams.
- **Simpler but coarser alternative**: use `mermaid:${fenceOrdinal}:${hash}` where `fenceOrdinal` is the position among ALL Mermaid fences in the file (not just ones with the same body). This re-introduces a structural-remount problem the primary proposal avoids â€” inserting a new Mermaid fence BEFORE an existing one re-indexes every downstream fence and remounts them all. Listed for completeness; prefer the primary proposal.
- Flip the assertion in the test from `.not.toBe(idsBeforeEdit)` to `.toBe(idsBeforeEdit)` when the fix lands. Update the describe-header comment too â€” drop the "latent stability gap" paragraph once case (c) passes as "id stable".

## Retry notice liveness ignores session lifecycle and retry sequencing

**Severity:** Medium - `ui/src/SessionPaneView.tsx:900-913` derives connection-retry notice liveness only from whether the message is the latest assistant-authored message.

That is too coarse for the transcript and lifecycle model. If a session leaves the active turn without later assistant output, the retry notice still renders as live with a spinner and `aria-live="polite"`. If one retry notice is followed by another retry notice, the older attempt renders as "Connection recovered" while the newer attempt still renders as "Reconnecting", which presents contradictory connection state.

**Current behavior:**
- The latest assistant-authored message is treated as the only live retry notice.
- Session status is not considered when deciding whether a retry notice is still live.
- Later retry notices are treated the same as later non-retry assistant output, so older retry attempts look resolved while the retry is still in progress.

**Proposal:**
- Derive retry display state from both session lifecycle and subsequent assistant message type.
- Keep the latest retry notice live only while the owning session is active or otherwise busy.
- Treat older retry attempts as superseded while a later retry notice is still the newest assistant output, and mark retry notices resolved only after later non-retry assistant output exists.

## Restart detection accepts late responses from old server instances

**Severity:** Medium - `shouldAdoptSnapshotRevision` treats any non-empty `serverInstanceId` mismatch as a fresh restart, even when the incoming id belongs to a previously-seen old instance.

The intended restart path is "client had instance A, server restarts to unseen instance B, lower revision from B should be accepted." A late response from A after the client already adopted B also differs from the current id, so the helper accepts it before applying revision ordering and can roll the UI back to old-process state.

**Current behavior:**
- The client stores only `lastSeenServerInstanceId`.
- `isServerInstanceMismatch(lastSeen, next)` returns true for any two non-empty different ids.
- The mismatch branch returns true before checking `nextRevision`.

**Proposal:**
- Track a set of seen server instance ids in `App`.
- Treat a different non-empty id as a restart only if it has not been seen before.
- Reject known older ids that differ from the current id, or route them through the normal monotonic revision gate.

## Persist-failure tombstone recovery waits for unrelated mutations to retry

**Severity:** Medium - the persist worker restores drained `removed_session_ids` after `persist_delta_via_cache` fails, but it does not schedule another persist attempt.

If no later state mutation sends another `PersistRequest::Delta`, the restored tombstones remain only in memory. A shutdown before the next unrelated mutation can still leave orphan rows in SQLite, which is the failure mode the tombstone restore was intended to prevent.

**Current behavior:**
- On write error, the worker extends `inner.removed_session_ids` with the drained tombstones.
- The worker logs the error and returns to `persist_rx.recv()`.
- No retry signal or backoff loop is armed for the restored delta.

**Proposal:**
- Re-arm persistence on failure, preferably with a bounded/backoff retry path inside the persist worker.
- Keep the watermark unchanged and recollect after restoring tombstones so changed sessions and deletes retry together.

## `shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` test flake

**Severity:** Low - `tests::shared_codex::shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime` was observed failing intermittently during batched `cargo test --bin termal` runs. Passes when re-run in isolation. The two Gemini-auth siblings (`select_acp_auth_method_ignores_workspace_dotenv_credentials` and `gemini_dotenv_env_pairs_ignore_workspace_env_files`) were fixed by acquiring `TEST_HOME_ENV_MUTEX` and isolating HOME + Gemini/Google env vars; verified via 5 consecutive green `cargo test --bin termal` runs. The shared-codex test did not surface in those 5 runs, so either (a) it is much rarer than the Gemini one, (b) it was indirectly fixed by an unrelated change, or (c) it is still broken but the window is too narrow to hit.

**Current behavior:**
- Pass-in-isolation, fail-in-batch pattern when it surfaces.
- Unlike the Gemini flakes, this test does not obviously share HOME-rooted fixtures â€” likely a temp-file path collision or a side effect of persist-thread teardown.
- Has not surfaced in recent multi-run verification, so concrete reproduction is not yet captured.

**Proposal:**
- Reproduce via a regression harness that runs the test 20 times back-to-back under the full batch context; confirm the flake signature (temp-file collision vs env var vs persist-thread handle leak).
- If the flake is temp-file path collision: switch to `tempfile::tempdir()` with unique per-test directories.
- If env: add `TEST_HOME_ENV_MUTEX` acquisition and `ScopedEnvVar::remove` isolation to match the Gemini pattern.
- Document the root cause in the fix commit message so the "why mutex / why tempdir" is visible at review time.

## Server restart without browser refresh can lose the last streamed message

**Severity:** Medium - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window â€” the persist channel carried a full-state clone â€” so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

The message is not hidden; it is genuinely gone from SQLite. No amount of frontend re-rendering will bring it back.

**Current behavior:**
- Active-turn deltas (e.g., streaming assistant text, `MessageCreated` at the end of a turn) commit through `commit_persisted_delta_locked`, which only signals the persist thread.
- The persist thread acquires `inner` briefly, collects the delta, and writes to SQLite.
- Between "signal sent" and "row written" there is a small time window (usually sub-millisecond, but can stretch under contention) during which a hard kill of the backend loses the mutation.
- On backend restart + SSE reconnect, the browser's `allowRevisionDowngrade: true` adoption path applies the persisted state. The persisted state is missing the un-drained mutation, so the in-memory latest message is overwritten and disappears.

**Proposal:**
- **Graceful-shutdown flush**: install a `SIGTERM` / `Ctrl+C` handler that drains the persist channel before the process exits, so user-initiated restarts (the common case) never lose data.
- **Opt-in synchronous persistence** for the last message of a turn: the turn-completion commit (`finish_turn_ok_if_runtime_matches`'s `commit_locked`) could flush synchronously before returning, trading a few ms of latency on turn completion for zero-loss durability of the final message.
- **Accept and document** as a known limitation that hard process kills (SIGKILL, power loss) can lose at most the last un-drained commit. Add a line to `docs/architecture.md` describing the background-persist durability contract.
- A regression test that exercises "restart backend mid-turn, reconnect browser, assert the final message is visible" would pin whichever fix is chosen; without the fix it is expected to fail.

## SSE state broadcaster can reorder state events against deltas

**Severity:** Medium - under a burst of mutations, a delta event can arrive at the client before the state event for the same revision, triggering avoidable `/api/state` resync fetches.

Before the broadcaster thread, `commit_locked` published state synchronously (`state_events.send(payload)` under the state mutex), so state N always hit the SSE stream before any follow-up delta N+1. Now `publish_snapshot` enqueues the owned `StateResponse` to an mpsc channel and returns; the broadcaster thread drains and serializes on its own schedule. `publish_delta` remains synchronous. A caller that does `commit_locked(...)?` + `publish_delta(...)` can therefore race: the delta hits `delta_events` before the broadcaster drains state N. The frontend's `decideDeltaRevisionAction` requires `delta.revision === current + 1`; if state N hasn't advanced `latestStateRevisionRef` yet, the delta is treated as a gap and the client fires a full `fetchState`.

**Current behavior:**
- `publish_snapshot` is async (channel + broadcaster thread).
- `publish_delta` is sync.
- Client can observe delta N+1 before state N.
- Extra `/api/state` resync fetches fire under sustained mutation bursts.
- Correctness preserved (resync fixes the view), but behavior is chatty and pushes load onto `/api/state` â€” which is exactly the path we just made cheaper.

**Proposal:**
- Route deltas through the same broadcaster thread so state and delta events for the same revision stream in order. Coalescing is fine because deltas are idempotent after a state snapshot.
- Or: have `publish_snapshot` synchronously send a revision-only "marker" into `state_events` immediately and let the broadcaster thread serialize and send the full payload; the client's `latestStateRevisionRef` advances on the marker.
- Or: document the tradeoff and rely on the existing `/api/state` resync fallback; track the extra traffic.

## SSE state broadcaster queue can grow before coalescing

**Severity:** Low - bursty commits can enqueue multiple full `StateResponse` snapshots before the broadcaster gets a chance to drop superseded ones.

The broadcaster thread coalesces snapshots only after receiving from its unbounded `mpsc::channel`. During a burst of commits, the sender side can enqueue several large snapshots first, so the "newest only" behavior does not actually bound queued memory or provide backpressure.

**Current behavior:**
- `publish_snapshot` sends owned `StateResponse` values to an unbounded channel.
- The broadcaster drains and coalesces only after snapshots have already queued.
- Full-state snapshots can accumulate during bursts even though older snapshots will be superseded.

**Proposal:**
- Replace the unbounded queue with a single-slot latest mailbox or bounded channel.
- Drop or overwrite superseded snapshots before they can accumulate in memory.
- Add a burst test that publishes multiple large snapshots while the broadcaster is delayed and asserts only the latest snapshot is retained.

## SQLite persistence lacks file permission hardening and indefinite backup retention

**Severity:** Medium - session history including agent output, user prompts, and captured file contents is readable by other local users on default Unix systems, and a second sensitive copy is kept indefinitely at a predictable path.

The new SQLite persistence path opens `~/.termal/termal.sqlite` via `rusqlite::Connection::open` without setting restrictive permissions; on Unix, the default `umask 0022` yields world-readable `0644`. The JSONâ†’SQLite migration renames the legacy file to `sessions.imported-<timestamp>.json` (same permissions) and never deletes or surfaces it, so the full pre-migration history persists at a predictable path with no garbage collection or user notice.

**Current behavior:**
- `rusqlite::Connection::open` creates the DB with the current umask (0644 by default on Unix).
- `imported_json_backup_path` writes to a predictable directory alongside the DB.
- No GC, no UI notification of the backup path, no explicit "delete imported backup" action.

**Proposal:**
- On Unix, call `fs::set_permissions(path, Permissions::from_mode(0o600))` on both the SQLite DB and the imported backup immediately after open/rename.
- On Windows, document the reliance on `%USERPROFILE%\.termal\` ACL inheritance; optionally tighten via `SetNamedSecurityInfo`.
- Either delete the imported backup after a successful cold start confirms the SQLite file is usable, or emit a one-shot UI notice with the backup path and an explicit delete affordance.

## `persist_created_session` skips hidden Claude spare pool changes

**Severity:** Medium - a crash after session creation but before a full snapshot loses changes to the hidden-spare pool that `create_session` may have triggered.

`persist_created_session` in `#[cfg(not(test))]` writes only the created session's record plus metadata, with `replace_sessions=false`. `create_session` can also invoke `try_start_hidden_claude_spare` to replenish the hidden-spare pool, which adds new session records to `inner.sessions` outside the created-session record. Those new hidden records are not part of the `persist_created_session` call and will not reach SQLite until the next `persist_internal_locked` snapshot runs.

**Current behavior:**
- `persist_state_parts_to_sqlite(..., &[record], replace_sessions=false)` upserts only the created record.
- Hidden Claude spares spawned by `try_start_hidden_claude_spare` live only in memory until a later full commit.
- A crash in the window loses the spare pool; the pool can be respawned on demand so impact is bounded.

**Proposal:**
- Include all sessions whose in-memory state changed during the create (the created record plus any newly spawned hidden spares) in the `persist_created_session` call.
- Or follow the delta-style write with a `persist_internal_locked` snapshot once the spare pool is settled.

## Lazy hydration effect: missing retry guard and unreconciled replace

**Severity:** Medium - the metadata-first hydration path still has two edge-case bugs around failed hydration and duplicate session materialization.

Two distinct issues remain in and around the one-shot `fetchSession` hydration path:
1. The async IIFE only guards against unmount. If the user switches away mid-fetch and the response's `session.id !== sessionId`, the code calls `requestActionRecoveryResyncRef.current()`. A transient server race can loop mismatch -> resync -> refetch -> mismatch.
2. `adoptCreatedSessionResponse` (and `live-updates.ts`'s `sessionCreated` reducer) raw-replace an existing session without per-message identity preservation via `reconcileSession`. If SSE `sessionCreated` materializes the session before the API response lands (or vice versa), memoized `MessageCard` children see new identities and remount.

**Current behavior:**
- The hydration effect is correctly keyed only by `activeSession?.id` and `activeSession?.messagesLoaded`, but the mismatch branch still triggers action-recovery resync without a "tried once" marker.
- Raw `[...previousSessions, created.session]` / `replaceSession(..., delta.session)` on the `existingIndex !== -1` branch.

**Proposal:**
- Add a `hydrationMismatchSessionIdsRef` (or count attempts) to avoid re-firing after one mismatch until an authoritative state event arrives.
- Route the existing-session replace branch through `reconcileSession` (or a similar identity-preserving merge) so memoized children keep stable identity.


## Implementation Tasks

- [ ] P2: Strengthen direct remote GET stale-response coverage:
  in `get_session_rejects_stale_remote_transcript_after_newer_state_snapshot`,
  capture the pre-call local revision and assert the revision advances after the
  rejected hydration. Also assert either a state broadcast or a persisted reload
  contains the newer metadata-only summary, so removing the commit-before-error
  path fails the test.
- [ ] P2: Add same-revision sibling-delta replay coverage:
  cover the over-broad `remote_session_delta_already_reflected` guard with two
  distinct same-revision session-scoped events that share the same
  `messageCount` and `sessionMutationStamp`. Assert the exact replay is skipped
  but the sibling event still applies or triggers remote recovery.
- [ ] P2: Add non-text targeted-hydration replay coverage:
  after the TextDelta replay regression, add a replay test for `CommandUpdate`
  or `ParallelAgentsUpdate` because those handlers include create/update
  branching and use the same replay-suppression guard.
- [ ] P2: Add a hydration retry budget/fake-timer test:
  use fake timers to drive repeated visible-session hydration failures through
  the retry schedule, assert duplicate user-facing error reporting is capped,
  and assert the retry budget resets only after the session revision, count, or
  mutation stamp changes.
- [ ] P2: Split `src/tests/remote.rs` by remote behavior boundary:
  extract hydration, delta, and orchestrator/sync tests into separate modules
  with shared fake-server helpers in a support module.
- [ ] P2: Make stale-hydration retry tests prove transcript adoption:
  use fresh retry transcript text that is distinct from the metadata-only
  preview, or assert against the active transcript pane instead of summary text.
  The test should fail if the retry fetch resolves but the full-session response
  is not adopted.
- [ ] P2: Add transient non-404 hydration retry coverage:
  render a visible metadata-only session, have the first
  `/api/sessions/{id}` request fail with a non-404 error, have the scheduled
  retry return a full transcript, and assert the transcript appears without a
  tab switch, prompt send, or unrelated state event. Include an assertion that
  repeated failures do not spam user-facing errors if retry capping is added.
- [ ] P2: Broaden unloaded remote-proxy delta hydration coverage:
  `src/tests/remote.rs` covers `MessageCreated` hydrating an unloaded proxy
  before local index checks. Add at least one non-`MessageCreated` branch such
  as `CommandUpdate` or `ParallelAgentsUpdate` to prove the same targeted
  hydration path protects other session-scoped remote deltas.
- [ ] P2: Simplify `app-live-state.ts` restart-adoption control flow:
  `ui/src/app-live-state.ts:1075-1085` sets `allowRevisionDowngrade: true`
  when `isServerRestartResponse` is true and then calls
  `shouldAdoptSnapshotRevision`, which already returns `true` unconditionally
  via the `isServerInstanceMismatch` short-circuit. The flag is dead for that
  path and the intertwined expression makes restart vs. downgrade semantics
  hard to audit. Split into
  `if (isServerRestartResponse) { /* adopt */ }` then the downgrade-capable
  branch as an else-if. Same behaviour, clearer intent. The adoption gate is
  the only defense against the prior High-severity stale-hydration data
  loss, so readability is correctness-relevant here.
- [ ] P2: Document Monaco cancellation filter install sites and design:
  `ui/src/main.tsx:10` and `ui/src/monaco.ts:42` both call
  `installMonacoCancellationRejectionFilter()`. Idempotent via
  `INSTALL_MARKER`, but not documented — a future reader may remove one of
  the two sites, breaking the non-obvious test-environment path where
  `main.tsx` didn't run first. Add a one-line comment at the `monaco.ts`
  site ("idempotent safety-net for test environments where `main.tsx`
  didn't run first"). Also add a module-header block to
  `ui/src/monaco-cancellation-filter.ts` covering (a) what it owns (global
  `unhandledrejection` filter for Monaco diff-worker cancellations), (b)
  what it deliberately does NOT own (all other `Canceled` error sources),
  and (c) the Monaco-version-pinning risk of the four-frame-name heuristic
  (if Monaco renames `EditorWorkerClient` → `DiffWorkerClient` or inlines
  `computeDiff`, the filter silently stops matching and noisy rejections
  return — failure-open is correct, but worth documenting).
- [ ] P2: Document `SessionHydrationRequestContext` invariant:
  `ui/src/app-live-state.ts:199-204` adds a new cross-function type used
  by `captureHydrationRequestContext`, three matcher predicates, and
  `adoptFetchedSession`. The "capture-at-send / reject-at-receive"
  semantics are implicit across the four fields. Add a 4-line doc block
  above the type naming the invariant: "Snapshot of session metadata at
  fetch-dispatch time. Used by `adoptFetchedSession` to reject hydration
  responses when (a) the captured server instance has since been
  superseded, or (b) newer delta metadata has advanced the session beyond
  what this response can cover." This anchors the intent so future readers
  don't "optimize" away one of the two redundant guards.
- [ ] P2: Cover Monaco cancellation filter default-target branch:
  `ui/src/monaco-cancellation-filter.test.ts:43-48` exercises the
  idempotency guard only via the injected `target` parameter. The default
  `target = window` branch is one line of production code but uncovered.
  Stub `globalThis.window` to a minimal `{ addEventListener }` object,
  call `installMonacoCancellationRejectionFilter()` twice with no args,
  assert `addEventListener` was called exactly once.
- [ ] P2: Add defensive handling for contract-violating `sessionCreated`:
  `ui/src/live-updates.ts:156-165` routes `sessionCreated` through
  `reconcileSessions`, which correctly preserves hydrated transcripts when
  `messagesLoaded === false`. A future server-side bug that publishes
  `sessionCreated` with `messagesLoaded: true` + `messages: []` would hit
  the hydrated-path in `reconcileSession` and silently clobber local
  transcripts. Add either a comment documenting that `messagesLoaded: true
  && messages.length === 0` is a wire-contract violation, or a
  short-circuit to `{ kind: "needsResync" }` for that case.
- [ ] P2: Pin local `orchestratorsUpdated` referenced-session summaries:
  seed a referenced local orchestrator session with messages, publish an
  orchestrator update, and assert every delta session has empty `messages`,
  `messagesLoaded: false`, and preserved `messageCount`.
- [ ] P2: Remove inline `expect(...)` inside `onDraftCommit` mock body:
  `ui/src/panels/AgentSessionPanel.test.tsx:3464-3467` asserts
  `expect(sessionId).toBe(session.id)` inside a jest mock function that
  runs within an `act()` block. A failing assertion inside a mock can be
  swallowed or surface as an uninformative React error rather than the
  original expect-failure. The test already asserts
  `expect(onDraftCommit).toHaveBeenCalledWith(session.id, ...)` externally
  on line 3525, so the inline guard is redundant. Drop the inline `expect`.
- [ ] P2: Loosen exact-pixel composer-height assertions:
  `ui/src/panels/AgentSessionPanel.test.tsx:3473-3482, 3584-3597` assert
  `toBe("96px")` / `toBe("124px")`. Both values depend on jsdom resolving
  the textarea's `borderHeight` to `0` (CSS unloaded). A future
  `computedStyle` polyfill or inline-border styled-jsx change flips the
  assertions silently without signaling what broke. Derive expected from
  constants (`${40 + 2 * 28}px`), use a permissive regex, or comment the
  implicit jsdom assumption so future readers know the coupling.
- [ ] P2: Broaden the composer-shrink mock predicate in `AgentSessionPanel.test.tsx`:
  the "line is deleted" test at
  `ui/src/panels/AgentSessionPanel.test.tsx:3584-3597` branches its
  `scrollHeight` mock on `textarea.style.height !== "0px"`, coupling the
  test to the production shrink-marker string. A refactor that swaps the
  marker to `"auto"` / `"1px"` / sets `rows = 1` silently un-triggers the
  mock branch. Broaden to
  `style.height === "" || style.height === "auto" || style.height === "0px"`,
  or drive the assertion off a production-exposed helper. At minimum, add
  a brief comment explaining the dependency on production's exact shrink
  marker.
- [ ] P2: Extract a shared `createResizeObserverMock()` helper in
  `AgentSessionPanel.test.tsx`:
  this iteration added a 21st inline `ResizeObserverMock` class at
  `ui/src/panels/AgentSessionPanel.test.tsx:1651-1653`. Pre-existing debt
  (20 copies existed before this round), but each new test compounds it —
  none of the 21 implementations handle `unobserve`, so a future production
  change that relies on it would silently pass every test in the file.
  Extract alongside the other harness utilities already factored out of this
  file.
- [ ] P2: Fix UTF-8 mojibake in `docs/architecture.md:230`:
  the `state` vs `delta` contrast line ("**`delta`** ? incremental") has
  a corrupted long-dash character. Pre-existing but re-rendered during
  this round's docs edit — worth a sweep when the file is next touched.
- [ ] P2: Extend `applyMetadataOnlySessionDelta` reducer coverage to the
  five non-`messageCreated` delta branches:
  `ui/src/live-updates.ts:168-177, 226-235, 277-286, 331-340, 385-394, 443-452`
  now routes six delta types through the metadata-only policy when
  `session.messagesLoaded === false`, but `ui/src/live-updates.test.ts:505-551`
  only pins the `messageCreated` branch. A regression in `messageUpdated`,
  `textDelta`, `textReplace`, `commandUpdate`, or `parallelAgentsUpdate`
  would silently drop back to the "apply message content to empty transcript"
  failure mode. Add `it.each` over the five remaining types asserting
  `messagesLoaded` stays `false`, `messages` stays `[]`, and `messageCount`/
  `preview`/`sessionMutationStamp` advance, with no `needsResync`.
- [ ] P2: Harden summary-only delta handlers against malformed payloads:
  `ui/src/live-updates.ts` at the six session-scoped branches forwards to
  `applyMetadataOnlySessionDelta` without running the `delta.message.id ===
  delta.messageId` guard, `isValidMessageIndex(delta.messageIndex)` guard,
  or `messageIndex > messages.length` guard that the hydrated path uses. A
  drifted delta silently advances `messageCount` and `sessionMutationStamp`
  on summary sessions. Keep the minimal invariant guards (id match +
  safe-integer + non-negative index) on the summary path too; on violation,
  return `needsResync` to repair local state.
- [ ] P2: Add `sessionCreated` reducer id-guard coverage:
  `ui/src/live-updates.ts:144-161` has payload-id-mismatch rejection logic
  for `sessionCreated`, but `ui/src/live-updates.test.ts` has no dedicated
  tests. `messageUpdated` has explicit id-mismatch coverage; `messageCreated`
  got coverage this iteration; `sessionCreated` is now the only id-guard
  without unit tests. Add three cases mirroring the existing `messageUpdated`
  id tests: no existing session (append), existing session replaced, and
  `delta.session.id !== delta.sessionId` (needsResync).
- [ ] P2: Close summary-mutation helper pattern duplication:
  `src/session_messages.rs:50-104`, `src/codex_submissions.rs:80-118`,
  `src/turn_dispatch.rs`, and the `remote_routes.rs::apply_remote_delta_event`
  branches all re-derive the tuple `(message, message_index, message_count,
  preview, status, session_mutation_stamp)` by hand. Any future field added
  to a session-scoped `DeltaEvent` means N+ sites to edit. Extract
  `fn commit_message_delta_locked(&mut inner, index, message_id)
  -> DeltaEventFields` and migrate call sites in one commit. Non-urgent;
  tracks maintenance debt.
- [ ] P2: Cache `hasRenderableStreamingMarkdown` per message id:
  `ui/src/message-cards.tsx:453-465` runs up to nine regex scans over the
  full message text on every streaming render. Result is monotonic (once
  true, stays true). Memoize via `useMemo([message.text])` or latch on
  detection so regexes stop firing after the first positive. Hot path:
  every `textDelta` triggers a re-render, which triggers the scan.
- [ ] P2: Rename or restructure `#[cfg(test)] snapshot()`:
  `src/state_accessors.rs:54-87` silently splits the `snapshot()` contract
  between test builds (full transcripts) and production (summaries). Rename
  the test-side helper to `full_snapshot_for_test()` so the intent is
  explicit at every call site; or migrate the ~40 `state.snapshot().sessions
  [...].messages` test call sites to read `state.inner.lock().sessions[...]`
  directly so transcript-mutation assertions bypass the wire projection
  entirely. Either fix closes the latent test-vs-prod divergence.
- [ ] P2: Memoize `visibleSessionHydrationTargets` more aggressively:
  `ui/src/App.tsx:536-559`'s `useMemo([sessionLookup, workspace.panes])`
  loses reference equality because `sessionLookup` is rebuilt upstream each
  render. The hydration effect in `app-live-state.ts` therefore runs its
  body on most renders (the `hydratingSessionIdsRef` guard prevents
  re-fetching, but the for-loop + `Map` + `flatMap` allocation runs each
  time). Either memoize `sessionLookup` upstream or compute a structural
  key from the visible pane ids + `messagesLoaded` flags.
- [ ] P2: Document the `reconcileSummarySession` invariant:
  `ui/src/session-reconcile.ts:125-176` silently overrides `next.messages`
  with `previous.messages` when `messagesLoaded === false`. Correct per
  Phase 2 design but undocumented. Add a 2-line header comment stating
  "summary payloads carry `messages: []`; we preserve the
  previously-hydrated transcript to avoid accidental truncation."
- [ ] P2: Restore live-delta recovery visibility assertions:
  `ui/src/App.live-state.watchdog.test.tsx` should assert that the recovered
  assistant text is rendered after the SSE delta flush and remains rendered
  after the stale fetch path resolves. Use a duplicate-tolerant assertion such as
  `screen.getAllByText(...).length > 0`.
- [ ] P2: Restore reconnect fallback applied-delta assertions:
  `ui/src/backend-connection.test.tsx` should assert that the dispatched
  session delta changes visible transcript, preview, or session-store state
  while reconnect recovery remains active. This prevents fallback coverage from
  passing when delta adoption silently does nothing.
- [ ] P2: Add backend `message_count` wire coverage for streaming deltas:
  subscribe to local delta events and drive representative `TextDelta`,
  `TextReplace`, `CommandUpdate`, and `ParallelAgentsUpdate` mutations, then
  assert the emitted `message_count` matches the expected transcript count.
- [ ] P2: Migrate App-level delta fixtures to a typed `DeltaEvent` helper:
  update `ui/src/App.live-state.deltas.test.tsx` fixtures so every
  session-scoped delta includes required protocol fields such as `messageCount`,
  preventing tests from dispatching impossible current-tree SSE payloads.
- [ ] P2: Lock remote `MessageUpdated` stamp assertion to `record.mutation_stamp`:
  `src/tests/remote.rs:710-797` asserts the delta's `session_mutation_stamp`
  matches the wire session's stamp via the snapshot. Correct today (snapshot
  helper always builds the wire stamp from `record.mutation_stamp`) but
  transitively — a future change to `snapshot_from_inner` could break the
  real contract without failing this test. Look up the mutation stamp
  directly on `inner.sessions[index]` and compare to the delta to pin the
  contract to the record state. This iteration's three new/strengthened
  remote tests also use the transitive form (`src/tests/remote.rs:1753,
  1907, 2023`), so this refactor should cover all five sites.
- [ ] P2: Extend `messageUpdated` reducer coverage to the three non-approval
  `Message` payload shapes:
  `ui/src/live-updates.test.ts` exercises the `applyDeltaToSessions`
  branch for `messageUpdated` only with the `approval` payload. The Rust
  emitter publishes `MessageUpdated` for four shapes (approval, user-input,
  MCP elicitation, Codex app-request) — the other three are covered on the
  Rust route-test side (`src/tests/review.rs:782-1033`) but a serde
  round-trip regression on `userInputRequest`, `mcpElicitationRequest`, or
  `codexAppRequest` deserialization in the TS reducer would not be caught.
  Add `it.each(resolvedInteractionBoundaryCases)` over a `messageUpdated`
  scenario asserting `preview`, `status`, `messageCount`,
  `sessionMutationStamp`, and the resulting message for each shape.
- [ ] P2: Add a frontend test for `messageCreated` payload id-mismatch:
  `ui/src/live-updates.ts:128` rejects `delta.messageId !== delta.message.id`
  with `needsResync`. The symmetric `messageUpdated` case has a test at
  `ui/src/live-updates.test.ts:661` (`"requests a resync when a whole-message
  update payload id mismatches the event id"`), but `messageCreated` now
  lacks the matching rejection test even though the guard exists. Clone the
  `messageUpdated` variant to close the parity.
- [ ] P2: Pin interaction-request preview helper outputs independently:
  the route tests in `src/tests/review.rs` compute expected preview strings
  with the same production helpers used by the route path, so a broken
  helper can make expected and actual values drift together. Either replace
  those expectations with literals at each route-test call site, or add
  direct unit tests for `user_input_request_preview_text`,
  `mcp_elicitation_request_preview_text`, and
  `codex_app_request_preview_text`.
- [ ] P2: Document `apply_remote_created_text_message_at`'s inert
  `message_count` parameter:
  `src/tests/remote.rs:1630-1665`'s helper takes `message_count: u32` but
  the value is effectively informational — the remote router recomputes
  the count via `session_message_count(record)` before publishing the
  localized delta. Callers still pass a plausible value (`:1681` passes
  `2` for the second seed), which reads like an assertion contract but
  isn't. Add a `///` doc comment on the parameter noting that the value
  is ignored on the emitter side and only affects the incoming-delta
  JSON, so future readers don't mistake it for load-bearing.
- [ ] P2: Decode typed `StateResponse` / `SessionResponse` in
  `snapshot_bearing_routes_include_message_count`:
  `src/tests/http_routes.rs:107-181` deserializes responses into
  `serde_json::Value` and looks up `sessions[...]["messageCount"]` via
  string keys, missing the primary regression class this test is supposed
  to pin (field-rename drift on `StateResponse` / `SessionResponse`).
  Deserialize into the typed structs instead so serde renames fail at
  compile time rather than silently at runtime.
- [ ] P2: Raise or split `recv_timeout` in
  `codex_thread_action_routes_update_session_state`:
  `src/tests/http_routes.rs:510` drives three sequential HTTP calls
  through a single spawned thread that calls
  `recv_timeout(Duration::from_secs(1))` between iterations. Under load
  a slow first archive round-trip could blow the timeout and manifest as
  "expected shared Codex JSON-RPC request" rather than a clear timing
  failure. Bump to 5s (matching other SSE helpers) or split the three
  actions into three separate tests.
- [ ] P2: Add `debug_assert_eq!` in `wire_session_from_record` pinning
  that the recomputed `message_count` matches `record.session.messages.len()`:
  `src/state_accessors.rs::wire_session_from_record` recomputes the wire
  count via `session_message_count(record)` on every projection, which
  means the in-memory `record.session.message_count` field is a cache
  that's always overwritten. If a future code path ever reads
  `record.session.message_count` directly without going through this
  projection, the value can silently drift from `messages.len()`.
  Add a `debug_assert_eq!(session.message_count as usize, session.messages.len())`
  after the recompute so tests surface any invariant violation. A larger
  refactor (drop the field from the in-memory `Session`, only set it on a
  dedicated wire DTO) is Phase 2 material.
- [ ] P2: Extract `test_remote_config()` helper in `src/tests/remote.rs`
  (actively regressing — **total now 52 duplicate literals**, +6 this
  iteration). Each new `RemoteConfig` field (e.g. a future transport
  flag) now requires a 52-site sweep. Extract a
  `fn test_remote_config() -> RemoteConfig` next to
  `seed_remote_proxy_session_for_delta_test` and fold at least the new
  iteration's literals in one commit; older sites can follow.
- [ ] P2: Migrate `cancel_pending_interaction_messages` off `commit_locked`
  (Phase 1 outstanding gap):
  `src/turn_lifecycle.rs:581` still calls `commit_locked` for interaction
  cancellation, which publishes a full-state snapshot instead of the narrow
  delta the plan calls for (`DeltaEvent::MessagesCancelled { session_id,
  message_ids, revision, session_mutation_stamp }`, or a narrower
  generalization). Until migrated, cancellation remains the only interaction
  path still producing full-state SSE fan-outs. Leave a
  `// TODO(metadata-first-phase1): replace commit_locked with a MessagesCancelled delta`
  breadcrumb above the call so the gap surfaces in future grep sweeps.
- [ ] P2: Audit publish-vs-lock ordering for the remaining
  `commit_persisted_delta_locked` sites:
  `commit_interaction_message_update` and the remote `MessageUpdated` replay
  path now publish under the state lock, but older persisted-delta paths in
  `session_messages.rs` still commit under the lock and publish afterward. A
  concurrent reader can observe the revision bump via `/api/state` before the
  matching delta arrives on SSE. Either move the remaining `publish_delta`
  calls into the locked blocks or explicitly document the "publish after
  commit" ordering as the project contract.
- [ ] P2: Extend `live-updates.test.ts` stamp-propagation coverage to the four untested delta branches:
  existing test only covers `messageCreated`. Production (`live-updates.ts:157-336`)
  adds `resolveSessionMutationStamp(...)` to `textDelta`, `textReplace`,
  `commandUpdate`, and `parallelAgentsUpdate`. Extend the existing four tests
  with a `sessionMutationStamp` input and matching output assertion. Add one
  case for the `??` fallback where `delta.sessionMutationStamp` is `undefined`
  and the session's prior stamp must be preserved.
- [ ] P2: Add Rust tests asserting `session_mutation_stamp` on every published delta:
  thirteen production sites (`session_messages.rs`, `session_sync.rs`, etc.) now
  publish `DeltaEvent::*` with `session_mutation_stamp: Some(record.mutation_stamp)`.
  Existing tests pattern-match with `{ message_id, .. }` and ignore the stamp
  entirely. Subscribe to the state event broadcast channel, drive one mutation
  per type (`push_message`, `append_text_delta`, `replace_text_message`,
  `update_command_output`, `update_parallel_agents`), and assert the resulting
  delta's `session_mutation_stamp` matches `record.mutation_stamp` on the wire
  session. Prevents silent stamp-drop regressions.
- [ ] P2: Add a remote-proxy test for `DeltaEvent::CodexUpdated`:
  `apply_remote_delta_event` in `remote_routes.rs:1004-1013` advances remote
  revision bookkeeping and early-returns (no state broadcast). Feed the event
  through and assert (a) no state broadcast is emitted, (b)
  `applied_revision_by_remote` advances. Covers the "process-global, do not
  localize" contract against accidental removal.
- [ ] P2: Add `serverInstanceChanged → disableMutationStampFastPath` integration test:
  `session-reconcile.test.ts:178` covers the option in isolation but nothing
  verifies that `adoptState` computes `serverInstanceChanged` correctly and
  threads it down. Mount → adopt with `serverInstanceId: "a"` → reconnect with
  `"b"` and identical stamps but divergent message text → assert the UI shows
  the new text. Locks the restart-rewind contract.
- [ ] P2: Add a `codexUpdated` SSE delta UI test:
  `ui/src/app-live-state.ts:1652-1694` performs five side effects
  (`confirmReconnectRecoveryFromLiveEvent`, revision advance, `codexStateRef`
  write, `startTransition(setCodexState)`, clear connection-issue state) with
  zero test coverage. Dispatch a `codexUpdated` SSE event and assert the
  CodexState ref update took effect via an observable consumer (the rate-limit
  chip or a direct ref read through a test harness).
- [ ] P2: Broaden `cancelStaleSendResponseRecoveryPollForSessions` call-site coverage:
  `App.session-lifecycle.test.tsx` covers one of five call sites (applied
  session delta). Add tests for (a) the revision-ignored live-delta path
  cancelling the poll, and (b) the `orchestratorsUpdated`-sessions fan-out
  calling `cancelStaleSendResponseRecoveryPollForSessions(deltaSessionIds)`.
- [ ] P2: Tighten `MessageCard.test.tsx` streaming-plain-text guards:
  current tests set `preferStreamingPlainTextRender=true` with
  `searchQuery=""` and `author="assistant"`. Add one case with a non-empty
  `searchQuery` (expecting fallback to `MarkdownContent` to preserve
  highlighting) and one with `author="you"` (expecting fallback so user
  prompts keep the `ExpandedPromptPanel`).
- [ ] P2: Strengthen the first stamp-fast-path test in `session-reconcile.test.ts`:
  the current `"reuses the existing session object when the mutation stamp matches"`
  test uses `previous` and `next` with identical content, so `reconcileSessions`
  would return `previous` even without the fast path. Make `next` differ (e.g.,
  change the last message's text) and assert `merged[0]` still has the old
  text — which is only possible if the stamp fast path actually ran.
- [ ] P2: Cover `reconcileMessages` tail-same fallback branches:
  the new optimisation has four branches; only the tail-same one is covered.
  Add tests for (a) length mismatch (message appended) and (b) an interior
  message deleted or reordered so a mid-iteration id mismatch forces the
  fallback to `reconcileMessagesById`.
- [ ] P2: Add `session-store.test.ts` no-op bailout listener-count assertion:
  `syncComposerSessionsStoreIncremental` returns before calling
  `emitStoreChange` when `changedSessions === []` and
  `removedSessionIds === []`. Subscribe a spy, call the incremental function
  with empty changes, assert the spy's call count is `0`. Locks the perf claim
  that drives the new API.
- [ ] P2: Positive-direction test for `DeferredHeavyContent` with
  `allowActivation={true}`:
  `MessageCard.test.tsx:117-145` covers only the negative direction. Stub
  `IntersectionObserver` to fire immediately (JSDOM lacks it), or assert that
  without the provider the placeholder resolves. Otherwise the context wiring
  could be inverted and both branches may still land on the fallback.
- [ ] P2: Add a `note_codex_notice` publish-path test parallel to the
  rate-limits test:
  `shared_codex_rate_limits_publish_codex_delta_without_full_state_snapshot`
  covers the rate-limit path. The notice publish path in `session_sync.rs:211`
  follows the same pattern but has no dedicated test. Mirror the assertions:
  narrow `CodexUpdated` delta emitted, no full-state broadcast.
- [ ] P2: Tighten `shared_codex_rate_limits_publish_codex_delta_without_full_state_snapshot`:
  the test currently asserts a delta arrives, but does not re-check
  `state_events.try_recv()` returns `Empty` after the delta handling. Add a
  second `try_recv` after draining the delta so a regression that accidentally
  emitted a full-state broadcast alongside the delta would fail.
- [ ] P2: Add end-to-end recovery-open intent coverage in `useAppLiveState`:
  queue overlapping `requestActionRecoveryResyncRef` opens, adopt snapshots in
  stages, and assert each session opens only when the authoritative session list
  actually contains it.
- [ ] P2: Add a zero-height measurement regression for transcript virtualization:
  force every mounted message slot to report `0` height on the first pass and
  assert the virtualized list keeps a stable mounted window instead of
  collapsing to gap-only page heights.
- [ ] P2: Cover `session-store` prompt-history rollback and boundary-rewrite recomputation:
  add `session-store.test.ts` cases where a transcript gets shorter or the
  previous boundary message is replaced, and assert `promptHistory` is rebuilt
  instead of being incorrectly preserved.
- [ ] P2: Add post-mount composer store-sync coverage for workspace actions:
  drive `handleInsertReviewIntoPrompt` and the draft-change sync path through a
  mounted App / `SessionPaneView` flow and assert the active textarea updates
  immediately without relying on a parent rerender.
- [ ] P2: Add post-mount `PaneTabs` store subscription coverage:
  mutate session summaries after initial render and assert tab labels, status
  badges/tooltips, and context-menu-derived workdir/project data refresh from
  the external store.
- [ ] P2: Add `useDeferredValue` + `startTransition` adoption-path regression:
  drive a rapid user-prompt → assistant-chunks → assistant-complete stream
  through `AgentSessionPanel` while a second state adoption is pending, and
  assert `findByText` resolves the newest assistant message without requiring
  a follow-up keystroke or focus change. The existing `App.live-state.reconnect`
  test pins reconnect-snapshot recovery, not the deferred-value boundary
  introduced by the `startTransition`-wrapped `setSessions` in
  `ui/src/app-live-state.ts`.
- [ ] P2: Add App/live-state coverage for restart snapshots with colliding
  session mutation stamps:
  seed one `serverInstanceId`, adopt a partial create/fetch response from a
  different instance, then adopt a full snapshot from that new instance with
  the same `sessionMutationStamp` but changed session content. Assert the full
  snapshot still disables the stamp fast path and replaces stale content.
- [ ] P2: Add App/live-state coverage for `codexUpdated` deltas:
  dispatch a `codexUpdated` SSE delta with changed Codex global state and
  assert the UI/store adopts it without fetching `/api/state`.
- [ ] P2: Add active-prompt poll cancellation coverage for session deltas:
  arm the stale-send recovery poll, dispatch a `messageCreated` or `textDelta`
  SSE delta for the same session, advance `ACTIVE_PROMPT_POLL_INTERVAL_MS`,
  and assert `/api/state` is not fetched.
- [ ] P2: Add table-driven session-mutation-stamp propagation tests for
  `applyDeltaToSessions`:
  cover `textDelta`, `textReplace`, `commandUpdate`, and
  `parallelAgentsUpdate`, including the fallback where a missing delta stamp
  preserves the existing session stamp.
- [ ] P2: Add incremental session-store creation coverage:
  call `syncComposerSessionsStoreIncremental({ changedSessions: [newSession] })`
  for a previously unknown session and assert the composer, record, and summary
  slices are all populated.
- [ ] P2: Add pane-level streaming plain-text selection coverage:
  render a session with an active latest assistant message and a settled or
  non-latest assistant message, then assert only the active latest card uses
  the cheap plain-text streaming shell.
- [ ] P2: Add `syncComposerDraftForSession` pruned-session no-op test:
  in `session-store.test.ts`, sync the store with an empty session list, then
  call `syncComposerDraftForSession` for the dropped id and assert the
  composer snapshot is still `null` with no listener fires. Locks in the
  silent-return contract at `session-store.ts:587-590`.
- [ ] P2: Add `+json` structured-suffix coverage for `isJsonResponseContentType`:
  extend `api.test.ts` with an `application/problem+json` case (and ideally an
  `application/vnd.termal+json` case) to pin that the fast path accepts RFC
  6838 structured suffixes, not just `application/json` literal.
- [ ] P2: Add regression for "POST succeeds + SSE never streams" after
  `startActivePromptRecoveryPoll` narrowing:
  mock a successful `sendMessage` POST whose response adopts cleanly, then
  block the SSE stream from advancing, and assert the session eventually
  recovers (either via the watchdog or the recovery poll). Covers the scenario
  the unconditional arm of `startActivePromptPoll` used to defend against.
- [ ] P2: Re-indent the destructure at `ui/src/app-workspace-actions.ts:264-271`:
  eight parameter lines are four-space-indented while the rest of the
  destructure uses two spaces. Run Prettier (or hand-edit) so the block reads
  as a flat destructure.
- [ ] P2: Surface the `scripts/perf/*` smoke scripts from `ui/package.json`:
  add `"perf": "node ../scripts/perf/prompt-responsiveness-smoke.js"` (or
  equivalent) so contributors can discover the orchestrator without having to
  find it by spelunking. This does not imply CI integration — the scripts
  still require a live `127.0.0.1:4173` + Chrome DevTools Protocol at
  `127.0.0.1:9222`. On Windows, start Chrome for MCP/CDP profiling with:
  `"C:\Program Files\Google\Chrome\Application\chrome.exe" --remote-debugging-port=9222 --user-data-dir="%TEMP%\chrome-profile-stable"`.
- [ ] P2: Add a module-header comment to
  `ui/src/panels/VirtualizedConversationMessageList.tsx`:
  the file sits at ~1600 lines and owns non-obvious invariants
  (page-band virtualization, scroll-intent classification, search-hit
  pinning, deferred-layout anchoring, mounted-range reconciliation,
  post-activation measurement) with no header explaining any of them.
  Write a 20-30 line block covering what the file owns, what it
  deliberately does NOT own (the scroll container itself — owned by
  `SessionPaneView`; message rendering — delegated via
  `renderMessageCard`; session-state mutations — pushed up to
  `AgentSessionPanel`), and the load-bearing ref contracts (which
  refs latch in one direction, which require session-switch reset,
  which timers must be cancelled on unmount vs. on `sessionId`
  change). Matches the CLAUDE.md guidance for large subsystems.
- [ ] P2: Add App-level coverage for the extracted Add project flow:
  open the control-panel "Add project" action, exercise both local and
  remote remotes, assert `pickProjectRoot` only wires the local path,
  and verify a successful `createProject` closes the dialog and adopts
  the new project state.
- [ ] P2: Add focused coverage for the extracted session kill/rename popovers:
  open each popover from a session row/tab, cover Save/New/Kill
  branches, and assert Escape/backdrop dismissal plus listener cleanup
  after unmount.
- [ ] P2: Cover the project context-menu "Start new session" path after the control-surface split:
  open a project's context menu, choose "Start new session", and assert
  the create-session dialog opens with the expected project preselected
  and pane context preserved.
- [ ] P2: Tighten the standalone control-panel scoping save assertion:
  replace `expect(clearedWorkspaceSave).toBeTruthy()` with an assertion
  against the matching saved workspace payload so the test proves the
  target tab's `originProjectId` was actually cleared.
- [ ] P2: Extract a shared `navigator.platform` stub helper:
  `ui/src/dialog-backdrop-dismiss.test.ts` and
  `ui/src/preferences/SettingsDialogShell.test.tsx` duplicate
  ~40 lines of identical scaffolding (`originalPlatform` +
  `originalUserAgentData` capture in `beforeEach`, the
  `stubPlatform` helper, and the delete-or-restore `afterEach`
  cleanup with the jsdom-prototype-shadow workaround). A shared
  `ui/src/test-support/stub-navigator-platform.ts` exporting
  `installPlatformStub()` that returns a `restore()` function
  would let each suite collapse to one `beforeEach` + one
  `afterEach` call. Low priority â€” the duplication is small and
  both call sites are close to their consumers â€” but any future
  change to the jsdom workaround (e.g., if `navigator.userAgentData`
  stops being configurable on a jsdom bump) would otherwise need
  to be made twice.
- [ ] P2: Add integration coverage for the inline-zone
  `ResizeObserver` stability fix:
  `MonacoCodeEditor.test.ts` pins the pure
  `computeInlineZoneStructureKey` contract (same ids â†’ same
  string, verified via `Object.is`; add/remove/reorder/id-change
  all flip the key), but those tests would all still pass if
  someone reverted the observer `useEffect`'s dep from
  `[inlineZoneStructureKey]` back to `[inlineZoneHostState]` â€”
  the helper is unchanged, only its caller is wrong. The
  genuine regression this fix prevents is "observer
  disconnects and rebuilds on every keystroke", which needs a
  MonacoCodeEditor-level test that either (a) renders the
  component with a stubbed `ResizeObserver` and verifies
  `disconnect` / `new ResizeObserver` aren't called across a
  burst of keystrokes with the same zone set, or (b) extracts
  the observer body into a testable hook that can be driven
  without mounting Monaco. Option (a) is cheaper â€” the existing
  MonacoCodeEditor mocks in `App.test.tsx` /
  `DiffPanel.test.tsx` / `SourcePanel.test.tsx` take a different
  path (full replace of the component), so a new dedicated
  Monaco test file with a minimal real-Monaco harness + stubbed
  ResizeObserver would close this gap.
- [ ] P2: Broaden `handleApplyDiffEditsToDiskVersion` rendered-
  Markdown commit coverage beyond the empty-commits path:
  `DiffPanel.test.tsx::"keeps apply-to-disk-version flowing when
  \`commitRenderedMarkdownDrafts\` has nothing to flush"`
  currently pins the `commits.length === 0 â†’ return true` path
  (verified load-bearing by temporarily flipping the production
  return to `false` â€” the test fails, confirming the empty-path
  plumbing is protected against that regression). Two branches
  remain uncovered:
  (A) Success-with-commits
      (`handleRenderedMarkdownSectionCommits(commits) â†’ true`):
      `handleSave` synchronously commits drafts BEFORE
      `onSaveFile` rejects, so by the time a rendered-Markdown
      integration test clicks apply-to-disk-version the
      committers return `null` and the flushSync takes the
      empty-path. Re-editing a section after the failed save
      doesn't help â€” the post-first-commit source buffer
      already advanced past the re-edited segment's original
      markdown, so the resolver fails and the commit returns
      `false` (the opposite of what we want to pin).
  (B) Failure
      (`handleRenderedMarkdownSectionCommits(commits) â†’ false`):
      the conflict-short-circuit path where
      `externalFileNotice` is set and `fetchFile` is NOT called.
  Two cheap alternatives to integration testing:
  (a) extract `handleRenderedMarkdownSectionCommits` into a pure
      helper with injectable dependencies and unit-test the
      boolean-return contract directly (cleanest, strongest
      coverage, also makes the helper reusable).
  (b) `vi.mock("./markdown-commit-ranges", ...)` to force
      `hasOverlappingMarkdownCommitRanges` to return `true`,
      which makes `handleRenderedMarkdownSectionCommits` reject
      the batch deterministically without needing to engineer
      real overlap (~30-40 lines of test, low refactor cost).
- [ ] P2: Extend paste-sanitizer test coverage with obfuscation and
  encoding variants:
  a few Note-level gaps remain worth tracking for future
  tightening: Unicode bidi override (`\u202Ejavascript:`),
  fullwidth-lookalike colon (`javascript\uFF1Aalert(1)`),
  percent-encoded protocol colons (`%6Aavascript:`,
  `javascript%3Aalert(1)`), HTML-entity protocol fragments
  (`java&#115;cript:`), and less-common dangerous protocols
  (`wss:`, `livescript:`, `jar:`, `chrome:`). Extend the `on*`
  handler coverage too: the scrubber is tested against `onclick`
  / `onmouseover`, but not `onerror` / `onload` / `onfocus` /
  `srcdoc` / `formaction` / `xlink:href` / `ping` / `download` /
  `target`. All of these are stripped today (allowlist
  scrubber), but direct tests would catch a regression that
  changed to a denylist shape.
- [ ] P2: Pin the `<template>`-inert-content invariant directly:
  `insertSanitizedMarkdownPaste` parses the pasted HTML into a
  `<template>` before sanitizing. Browsers don't execute
  `<script>` or fire `<img onerror>` inside template content,
  which is why the sanitize-before-insert ordering is safe. Add
  a test that pastes
  `<img src="/nonexistent" onerror="window.__xss=true">` and
  asserts `window.__xss` stays undefined â€” it would pass today
  (template inert) and fail if the code is ever refactored to
  assign `innerHTML` on a live DOM node.
- [ ] P2: Tighten the bare-CR detector test:
  `markdown-diff-segments.test.ts::detectMarkdownDocumentEolStyle`
  currently asserts `"line1\r\nline2\rline3\n"` picks `crlf` (one
  CRLF, one LF, one bare CR â†’ CRLF wins via `crlfCount > lfCount`),
  but wouldn't catch a regression that accidentally counted bare
  `\r` as a second CRLF â€” both the correct count (1) and the
  over-count (2) land on `crlf`. Add a case with multiple bare
  `\r` and a single `\n` where the correct answer is `lf` but an
  over-count would flip to `crlf`.
- [ ] P2: Comment the mixed-doc EOL preservation limit:
  update the helper comments and the mixed CRLF/LF round-trip test
  note to explain that rendered Markdown saves preserve the
  detected dominant document EOL style, not per-line EOL markers.
- [ ] P2: Add fenced-code-block EOL-detection coverage:
  `detectMarkdownDocumentEolStyle` treats the document as a flat
  byte stream â€” code fences, inline code, comments, etc. do not
  affect the count. A future refactor might assume CRLF inside a
  fenced block is "semantic" and skip it. Pin the flat-byte-stream
  contract with a test that feeds a document where CRLF appears
  only inside a fenced block and asserts the detector still picks
  the dominant style based on the total count.
- [ ] P2: Add anchor-stabilized load-earlier button-path coverage:
  `AgentSessionPanel.test.tsx` now covers the near-bottom cooldown
  on `handleHeightChange` and the no-scroll-tick-resubscribe
  behaviour of the auto-load effect, but the explicit "Load N
  earlier messages" button click path has no test. Drive
  `scrollTop = 0` with `hasOlderMessages = true` so the button is
  visible, click it, let the consuming `useLayoutEffect` run, and
  assert the viewport offset of the anchor message is preserved
  across the prepend. Include a no-op case where
  `renderWindowSize` is already at `messages.length` so the anchor
  ref cannot strand.
- [ ] P2: Add else-branch cooldown coverage for `handleHeightChange`:
  the shouldKeepBottom branch is now covered by
  `AgentSessionPanel.test.tsx::"skips the bottom pin write while
  the user is actively scrolling"`. The else-branch
  (`getAdjustedVirtualizedScrollTopForHeightChange` call site) is
  not. Needs a harness that places the viewport far from the
  bottom so `shouldKeepBottom` is false, fires a wheel event,
  triggers a measurement that would normally compute a non-zero
  delta, and asserts `scrollWrites` is empty during the cooldown
  window. The existing post-activation measurement path writes
  scrollTop on mount, so the harness also needs to reset
  scrollWrites after initial settlement.
- [ ] P2: Add message-stack non-passive wheel listener coverage:
  dispatch a cancelable `WheelEvent` on the `.message-stack` `<section>`
  in `SessionPaneView.test.tsx`, assert `event.defaultPrevented === true`
  after the handler runs, and assert the handler's computed `scrollTop`
  was written exactly once (no double-scroll from the browser's
  preventDefault-failure fallback). Protects against a future React
  / DOM-abstraction change re-introducing the passive default.
- [ ] P2: Add `dialog-backdrop-dismiss` navigator.platform fallback tests:
  cover the Safari/Firefox-style path where `navigator.userAgentData`
  is absent and only `navigator.platform` is available. Assert macOS
  Ctrl+primary is suppressed and a non-Apple Ctrl+primary still dismisses.
- [ ] P2: Add App-level backdrop integration tests for create dialogs:
  exercise create-session and create-project `.dialog-backdrop`
  `mouseDown` behavior through `App.test.tsx`, including primary close,
  non-primary/macOS-Ctrl non-dismissal, and the pending-create guard if
  the harness can put the dialog into a creating state.
- [ ] P2: Fix dialog test platform-stub cleanup:
  in both `dialog-backdrop-dismiss.test.ts` and
  `SettingsDialogShell.test.tsx`, delete the own `navigator.platform`
  property in `afterEach` when there was no original own descriptor,
  matching the existing `userAgentData` cleanup path.
- [ ] P2: Add focused `diff-latest-file-state.ts` coverage:
  the module has zero dedicated tests after the Tier 0 split. Add
  a short `diff-latest-file-state.test.ts` covering both branches
  of `createInitialLatestFileState` (null vs. non-null `filePath`,
  asserting `contentHash === null` on each), `toLatestFileState`
  against a full `FileResponse` plus one with `contentHash` /
  `language` omitted (asserting the `?? null` defaulting), and
  `isStaleFileSaveError` for case-insensitive substring matches
  on both hit and miss patterns.
- [ ] P2: Add focused control-panel-layout.ts coverage:
  add `ui/src/control-panel-layout.test.ts` covering the four currently
  untested exports. Cases worth pinning:
  `getDockedControlPanelWidthRatioForWorkspace` with the control panel on
  left, on right, and with a non-split workspace root;
  `resolvePreferredControlPanelWidthRatio` with a saved ratio above the
  CSS-derived minimum (both first-pane and second-pane placement);
  `hydrateControlPanelLayout` for left vs right side, empty workspace, and
  a workspace that already has a dock; and `resolveRootCssLengthPx` with
  `calc(40 * 1rem)` and `calc(1rem * 40)`, `rem`-to-px conversion, and a
  `var(--fallback-chain)` resolution case.
- [ ] P2: Add focused `DiffPanel` `!hasScope` branch coverage:
  the error branch at `DiffPanel.tsx:571-582` (which sets
  `status: "error"` and the "no longer associated with a live
  session or project" copy) has no dedicated test. Existing
  `DiffPanel.test.tsx` cases cover the happy-path fetch, the
  fetchFile rejection path, the watcher-deleted path, and the
  save flow â€” but not the scope-missing case. Render the panel
  with `sessionId={null}`, `projectId={null}`, and a non-null
  `filePath`, then assert the specific error copy appears. The
  recently-landed `contentHash: null` tightening of `LatestFileState`
  makes this branch's contract load-bearing at the type level,
  but the behavioral assertion is still missing.
- [ ] P2: Tighten the session-find virtualization test:
  `ui/src/panels/AgentSessionPanel.test.tsx` asserts fewer than 80 cards
  render for 180 messages during search, which is correct but loose. The
  working range is closer to 14-28 cards given viewport=500 / overscan=960
  / default-height=180. Tighten to `< 40` and add an exclusion assertion
  like `expect(screen.queryByText("message-5")).toBeNull()` so a
  regression where the merged range falls back to `{0, messages.length}`
  fails loudly.
- [ ] P2: Add `SessionPaneView` retry-notice state coverage:
  exercise the real session rendering path, not just direct
  `MessageCard` props. Use rerendered session messages for retry notice
  -> user message -> later retry notice -> later non-retry assistant output,
  and assert the notice remains live only when appropriate, superseded retry
  attempts do not claim recovery, resolved notices drop the spinner and use
  `aria-live="off"`, and terminal session states do not leave a retry notice
  spinning forever.
- [ ] P2: Add focused coverage for `control-surface-state.ts`:
  six of eight exports currently have no targeted tests. Add a
  co-located `control-surface-state.test.ts` covering:
  `createControlPanelSectionLauncherTab` for the five `sectionId`
  branches + blank-root null-gating on `files` / `git`;
  `resolveWorkspaceScopedProjectId` three-way precedence
  (explicit origin project â†’ origin session's project â†’ null) plus
  trim semantics; `resolveWorkspaceScopedSessionId` precedence
  (preferred session â†’ active session â†’ first-in-project â†’ null);
  `buildControlSurfaceSessionListState` no-search fast-path vs
  search-result-map branch; `mergeOrchestratorDeltaSessions`
  dedup + append-unknowns + `reconcileSessions` identity.
- [ ] P2: Add focused coverage for `git-diff-refresh.ts`:
  `collectGitDiffPreviewRefreshes` has no targeted test. Add a
  co-located `git-diff-refresh.test.ts` that builds a tiny
  `WorkspaceState` with mixed tab kinds (`diffPreview`, others)
  plus a `WorkspaceFilesChangedEvent` and asserts the returned
  refresh list against the four skip conditions (non-diffPreview
  kind, missing `gitDiffRequestKey`, missing `gitDiffRequest`,
  touch-predicate short-circuit). Mirror fixture style from the
  `collectRestoredGitDiffDocumentContentRefreshes` test at
  `App.test.tsx:1489`.
- [ ] P2: Add focused coverage for `source-file-state.ts`:
  both exports untested. `sourceFileStateFromResponse` â€” assert
  the 13-field state-object mapping against a full `FileResponse`
  and against one with optional fields omitted, including `status`
  and `language`.
  `isSourceFileMissingError` â€” assert `true` for
  `new Error("File Not Found")`, `new Error("thing was not FOUND")`,
  and a plain `"file not found"` string; `false` for other
  messages.
- [ ] P2: Pin the pending-state tooltip copy for
  `OrchestratorRuntimeActionButton`:
  aria-label regex matchers in `App.test.tsx` cover the three
  labels, but the `isPending: true` `title` tooltips
  (`"Pausing orchestration"` / `"Resuming orchestration"` /
  `"Stopping orchestration"`) are not asserted anywhere. Add one
  assertion per action variant while `isPending: true` to pin the
  copy so it cannot silently regress.
- [ ] P2: Cover the `restartRequired` branch of
  `describeBackendConnectionIssueDetail`:
  `BACKEND_UNAVAILABLE_ISSUE_DETAIL` is already asserted
  indirectly in `backend-connection.test.tsx`, but the
  `isBackendUnavailableError && error.restartRequired` branch â€”
  which surfaces the server's restart-required message verbatim
  instead of the generic copy â€” is not confirmed tested. Add a
  small unit test covering all three branches (`restartRequired`
  true, `restartRequired` false, non-backend-unavailable error).
- [ ] P2: Add focused coverage for
  `markdown-diff-segment-stability.ts`:
  the greedy matcher + FNV-1a hash are untested. Distance
  tiebreaks, context-score weights (8 for immediate neighbours, 2
  for second-degree), and collision resolution via
  `getAvailableMarkdownDiffSegmentId` are exactly the invariants
  that regress silently under refactoring. Construct two near-
  identical candidate ranges with deliberately colliding FNV
  hashes and verify tiebreak order by distance / context score,
  plus a collision test that drives the `:stable-N` suffix path.
- [ ] P2: Add focused coverage for `markdown-commit-ranges.ts`:
  4 of 5 exports are untested (`resolveRenderedMarkdownCommitRange`,
  `markdownRangeMatches`, `mapMarkdownRangeAcrossContentChange`,
  `findClosestMarkdownRange`). The existing
  `hasOverlappingMarkdownCommitRanges` tests in
  `DiffPanel.test.tsx:4798` already pull the module in, so sibling
  tests can sit in the same `describe` block. Cases worth pinning:
  4-strategy resolver happy paths + `null` return when nothing
  matches; bounds-check rejection in `markdownRangeMatches`;
  prefix / suffix diff shift cases + straddling-change `null`
  return in `mapMarkdownRangeAcrossContentChange`; and nearest-
  neighbour tiebreak in `findClosestMarkdownRange`.
- [ ] P2: Add focused coverage for `session-slash-palette.ts`:
  25+ exports with no dedicated tests. Heavy integration coverage
  already exists (~14 slash-palette flows in
  `AgentSessionPanel.test.tsx:691-1489`), so this is the lowest
  urgency of the test-coverage tasks, but a pure-function unit
  test of `buildSlashPaletteState` per (agent, commandId) tuple
  would localise failures better than debugging through React
  rendering. Add `panels/session-slash-palette.test.ts` feeding
  canned tuples through `buildSlashPaletteState` and snapshotting
  the item list.
- [ ] P2: Add focused coverage for `markdown-links.ts`:
  the 20+ helpers moved out of `message-cards.tsx` have subtle
  regex/path edges (UNC root restoration, loopback `[::1]`,
  `/C:/` drive-letter-after-slash, `#L42C3` fragment parsing,
  trailing-dot guard for `foo.tex.#L63`). Indirect coverage via
  `MarkdownContent.test.tsx` is thin. Add `markdown-links.test.ts`
  covering: UNC restoration, loopback `http://localhost/Users/...`,
  `file:///C:/...`, `/C:/` drive-letter, `#L42C3` + `#L42`,
  line-suffix stripping `path:42:5`, trailing-dot guard, and
  `isExternalMarkdownHref` for `//cdn.example/x.md`.
- [ ] P2: Add focused coverage for `deferred-render.ts`:
  the seven pure helpers moved out of `message-cards.tsx` include
  `buildMarkdownPreviewText` which strips code fences / link
  syntax / headings / blockquotes / list bullets / backticks via
  regex and produces user-visible preview text. Add
  `deferred-render.test.ts` covering: `buildMarkdownPreviewText`
  (fenced code replaced, links flattened, headings stripped),
  `buildDeferredPreviewText` (line + char limit + ellipsis
  handling), `measureTextBlock` (empty string counts as one
  line), and the min-height clamps in `estimateCodeBlockHeight` /
  `estimateMarkdownBlockHeight`.
- [ ] P2: Add focused coverage for `mermaid-render.ts`:
  the module-level `mermaidRenderQueue` Promise singleton
  serializes Mermaid `initialize`/`render`/reset cycles so
  concurrent renders cannot leak config through Mermaid's
  module-level singleton. This invariant is invisible to
  integration tests (they render one diagram at a time). Add
  `mermaid-render.test.ts` with at minimum a queue-serialization
  test (spy on a fake `mermaid.initialize`/`render`, fire two
  concurrent `renderTermalMermaidDiagram` calls, assert
  initialize-order matches render-completion order and the base
  config is restored between renders), plus pure-helper coverage
  for `clampMermaidDiagramExtent`, `readMermaidSvgDimensions`
  (well-formed / malformed / negative viewBox),
  `getMermaidDiagramFrameStyle` clamp caps, and
  `buildTermalMermaidConfig` palette-vs-match branches.
- [ ] P2: Consolidate Apple-platform detection helpers:
  the tree now has three near-duplicate platform sniffers â€”
  `ui/src/pane-keyboard.ts:122-136` and
  `ui/src/dialog-backdrop-dismiss.ts:62-76` both read
  `navigator.userAgentData?.platform ?? navigator.platform` and regex
  for `mac|iphone|ipad|ipod`, while `ui/src/app-utils.ts:197-203`
  still uses the older `navigator.platform.toLowerCase().includes("mac")`
  form that misses reduced-UA Chromium. Extract a shared
  `ui/src/platform.ts` exposing `detectPlatform()` +
  `isApplePlatform(platform?)` and retire the three local copies in
  one mechanical commit. Fixes the latent `primaryModifierLabel`
  gap as a side-effect.
- [ ] P2: Extract a shared `<DialogBackdrop>` primitive:
  `isDialogBackdropDismissMouseDown` is now consolidated, but the
  wiring (`onMouseDown={(event) => { if (!isDialogBackdropDismissMouseDown(event.nativeEvent)) return; ... }}`
  is replicated verbatim at three sites â€”
  `ui/src/preferences/SettingsDialogShell.tsx:49`,
  `ui/src/App.tsx:~7871`, and `ui/src/App.tsx:~8169` â€” along with
  the sibling `onMouseDown={(event) => event.stopPropagation()}` on
  each dialog body `<section>`. A `<DialogBackdrop onDismiss>`
  component that internalizes both patterns would retire ~30 lines
  of JSX per site and keep the dismiss contract in one place.
  Worth attempting once the App.tsx dialog-markup split continues
  so the extraction boundary is obvious.
- [ ] P2: Second import-prune pass over `SessionPaneView.tsx`:
  the first pass (`7e84fe1`) pruned the bulk of unused imports
  from App.tsx / SessionPaneView / WorkspaceNodeView, but
  SessionPaneView still carries three unused locals
  (`pendingPrompts`, `composerInputDisabled`, `composerSendDisabled`
  at 760/851/852) plus a few unused imports. The extraction's
  provenance header explicitly notes a follow-up is expected;
  schedule the second pass once the component stops moving.
- [ ] P2: Harden the new active-prompt stale-send-recovery test:
  `ui/src/App.test.tsx` "arms the active-prompt poll when a
  successful send response is stale" has two soft assertions worth
  tightening. First, the rev-2 SSE advance assertion
  `screen.getAllByText("Recover this prompt").length > 0` can pass
  even if adoption never rendered the transcript, because the
  composer textarea still holds the typed value â€” scope the check
  to the message-list container (`within(messageList).getByText(...)`)
  or assert `length > 1` so both the composer echo and the transcript
  copy must be present. Second, the test advances timers once and
  asserts a single `/api/state` call, but does not prove the chain
  stops after the session goes idle â€” add a second
  `advanceTimers(ACTIVE_PROMPT_POLL_INTERVAL_MS)` and assert
  `fetchMock` is still at 1 call so a regression that leaves the
  poll running after idle would fail loudly.
- [ ] P2: Add regression coverage for the delta-persist tombstone restore
  path. The production persist thread lives under `#[cfg(not(test))]` so
  the error-injection test needs either a `#[cfg(test)]` seam in
  `persist_delta_via_cache` that can be primed to fail, or a dedicated
  unit test that exercises a helper extracted from the error branch in
  `src/app_boot.rs`. Assert that after a simulated write failure,
  `inner.removed_session_ids` contains the tombstones that were drained
  into the failed `PersistDelta`, and that the worker re-arms a retry
  without waiting for an unrelated later mutation.
- [ ] P2: Add fake-remote coverage for the "POST response older than SSE"
  gate in `create_remote_session_proxy` + `proxy_remote_fork_codex_thread`.
  Pre-seed `inner.remote_applied_revisions` with a newer revision, then
  have the fake remote respond with an older revision and assert the
  local proxy is NOT refreshed from the POST payload. Pair with an
  `update_existing: true` positive case where the POST revision is
  >= the applied remote revision.
- [ ] P2: Add remote create/fork existing-proxy race coverage:
  pre-seed a local proxy for the same `(remote_id, remote_session_id)`, have
  the fake remote return a fresher
  `CreateSessionResponse { sessionId, session, revision }`, and assert the
  returned response plus local record reflect the fresh payload instead of the
  stale proxy mirror.
- [ ] P2: Add fake-remote regression coverage for the `remote session id
  mismatch` bad-gateway branch in `create_remote_session_proxy` and
  `proxy_remote_fork_codex_thread`. Requires a fake-remote HTTP server
  fixture; today no such fixture exists in `src/tests/`, so the defensive
  validation ships without a direct test. Once that fixture lands, assert
  both routes return `ApiError::bad_gateway` with the
  `remote session id mismatch` message when the fake remote returns a
  `CreateSessionResponse` whose `session.id !== session_id`.
- [ ] P2: Add `adoptCreatedSessionResponse` server-instance-id Vitest
  coverage at the App level: seed `lastSeenServerInstanceIdRef` with one
  id, feed a POST response carrying a different id at a lower revision,
  assert the new session appears in the list and the revision counter
  rewound. The primitive itself is covered by `state-revision.test.ts`,
  but the full adoption wiring through `sessionsRef`, `setSessions`, and
  the workspace layout has no positive test for the restart-rewind
  branch yet. Pair with a same-instance stale response that must preserve
  newer SSE state, and cover both create and fork callers.
- [ ] P2: Add stale old-server-instance coverage for snapshot adoption:
  adopt instance A, then a lower-revision snapshot from new instance B,
  then resolve a late response from already-seen instance A. Assert the
  late A response is rejected instead of treated as a fresh restart.
- [ ] P2: Assert `serverInstanceId` on create/fork route responses:
  extend `create_session_route_returns_created_response` and
  `codex_thread_fork_route_returns_created_response` to check
  `response.server_instance_id == state.server_instance_id` and that the
  id is non-empty.
- [ ] P2: Add remote create/fork stale-POST revision coverage:
  pre-seed a local proxy as if the remote event bridge already applied a newer
  revision for the same remote session, then have the fake POST response return
  an older `CreateSessionResponse`. Assert the existing newer proxy state is
  retained and no stale `SessionCreated` payload is published.
- [ ] P2: Add remote create/fork response identity validation coverage:
  have fake remotes return mismatched `sessionId` and `session.id` values for
  session create and Codex fork, then assert both paths fail with bad-gateway
  errors instead of localizing the wrong remote session.
- [ ] P2: Strengthen remote rollback content assertions:
  extend `failed_remote_snapshot_sync_restores_session_tombstones` so rollback
  compares full session records and orchestrator instance contents, not only
  restored session-id membership and orchestrator count.
- [ ] P1: Add a direct unit test for `StateInner::collect_persist_delta`:
  construct a `StateInner` with three sessions at distinct mutation stamps
  and one hidden session; seed `removed_session_ids` with one tombstone.
  Call `collect_persist_delta(watermark)` and assert (a) `changed_sessions`
  contains only the visible sessions with stamp > watermark, (b)
  `removed_session_ids` in the returned delta includes the seeded
  tombstone AND any hidden session whose stamp advanced, (c) the returned
  `watermark` equals `inner.last_mutation_stamp` at collection time, (d)
  `metadata.sessions` is empty (metadata-only clone), (e) a second call
  with the returned watermark produces empty `changed_sessions` +
  `removed_session_ids` (idempotent). This is the core of the
  delta-persist refactor and currently has zero regression protection â€”
  the `#[cfg(test)]` persist path writes full-state JSON so every
  existing persistence test bypasses the production code path.
- [ ] P1: Add an integration-style test for the production persist path:
  open a temp SQLite path, run `AppState::new` with the persist thread,
  issue two `commit_locked` calls touching different sessions, create/fork
  a session that relies on the post-replace re-stamp, and import a
  discovered Codex thread. Wait for the persist thread to drain, read rows
  directly via `rusqlite`, and assert that touched/created/imported rows
  were written while untouched rows were not rewritten. Or, at minimum,
  unit-test `persist_delta_via_cache` directly against a
  `rusqlite::Connection::open_in_memory` using
  `ensure_sqlite_state_schema` and hand-crafted `PersistDelta`s.
- [ ] P2: Add coverage for `StateInner::remove_session_at` and
  `StateInner::retain_sessions`: build a `StateInner`, push sessions via
  `push_session`, run the helper, and assert `removed_session_ids`
  contains the expected ids while kept sessions' stamps are unchanged.
  The raw `record_removed_session` accumulator is tested; the wrappers
  that production deletion paths actually call are not.
- [ ] P2: Document remote-sync revision, rollback, and tombstone invariants:
  add contract comments around `sync_remote_state_inner`,
  `apply_remote_state_if_newer_locked`, and the remote-orchestrator sync
  handoff so future delta-persistence fixes preserve rollback safety.
- [ ] P2: Document orchestrator lifecycle invariants:
  add a lifecycle block to `orchestrators.rs` covering template normalization,
  instance creation, backing sessions, pause/resume/stop, persistence, and
  where transition scheduling is delegated.
- [ ] P2: Document ACP runtime protocol flow:
  add comments for initialize/auth/session load-or-new/config refresh/prompt
  ordering, pending JSON-RPC request ownership, timeout behavior, and
  Gemini/Cursor fallback differences.
- [ ] P2: Document instruction graph traversal semantics:
  describe seed discovery, path normalization, reference sanitization,
  transitive edge policy, skipped directories, and cycle behavior near
  `build_instruction_search_graph`.
- [ ] P2: Add a frontend test proving the self-chained `/api/state`
  safety-net poll does not stack overlapping requests:
  `vi.useFakeTimers()` + a mocked `fetchState` that takes longer than
  the chain boundary, advance time past the next interval, and assert
  `fetchState` was called exactly once per chain hop rather than
  accumulating parallel fires.
- [ ] P2: Add a `publish_snapshot` delivery test: subscribe to
  `state.subscribe_events()` before a mutation and assert the expected
  payload arrives on `state_events` after `commit_locked` through the
  real broadcaster thread. Cover the sync fallback separately by
  disconnecting the broadcaster channel.
- [ ] P2: Add SSE broadcaster latest-only queue coverage:
  publish several large snapshots while the broadcaster is delayed and
  assert superseded snapshots are dropped or overwritten before they can
  accumulate in the queue.
- [ ] P2: Pin `next_mutation_stamp` saturation semantics: the counter
  uses `saturating_add(1)`, but the existing
  `state_inner_next_mutation_stamp_is_strictly_monotonic` only covers
  three increments from zero. Add a one-line case
  (`inner.last_mutation_stamp = u64::MAX; assert_eq!(inner.next_mutation_stamp(), u64::MAX);`)
  so a regression to `wrapping_add` fails the test.
- [ ] P2: Extend the Mermaid dimension-clamp tests with lower-bound cases:
  `ui/src/MarkdownContent.test.tsx` only covers the upper clamp
  (huge viewBox â†’ 4096). Add `viewBox="0 0 -100 -100"` (negative input)
  and `viewBox="0 0 0 0"` (zero input) and assert the rendered widthPx
  and heightPx fall in `[lowerBound, upperBound]`. The regex in
  `clampMermaidDiagramExtent` accepts `[-+]?` signs, so the lower clamp
  is the live contract.
- [ ] P2: Extend `hasOverlappingMarkdownCommitRanges` tests with a
  three-range unsorted case: e.g., `[[0, 5), [10, 20), [3, 12)]`. The
  helper relies on the ascending-by-start sort to detect the overlap;
  a regression that iterated in insertion order would miss it.
- [ ] P2: Add metadata-only state snapshot coverage:
  backend tests should assert `/api/state` omits transcript payloads while
  `GET /api/sessions/{id}` still returns the full transcript. App tests should
  assert a metadata snapshot preserves an already-hydrated active session and
  does not disrupt prompt input or focus.
- [ ] P2: Add session-create persistence contention coverage:
  prove visible session creation does not hold the state mutex while opening
  SQLite, ensuring schema, or committing a transaction.
- [ ] P2: Add rendered Markdown mixed-batch conflict coverage:
  commit two rendered Markdown sections where one range still maps and the
  other conflicts after a document change, then assert no partial apply clears
  the unresolved draft or conflict notice.
- [ ] P2: Add apply-to-disk-version rendered-draft coverage:
  exercise the save-conflict rebase flow with an active contenteditable DOM
  draft only, and again with both committed refs and a newer DOM draft, then
  assert the rebased save includes the latest rendered Markdown edits.
- [ ] P2: Complete `MermaidDiagram` behavioral coverage:
  cover the remaining gaps: `preserveMermaidSource={true}` keeps the fenced
  source while a diagram renders, the rendered diagram exposes its `role="img"`
  container, the `mermaid-diagram-loading` class is removed after
  `mermaid.render` resolves, and `showSourceOnError={false}` suppresses the
  fallback source. Basic render, light appearance, disabled rendering, and the
  default error fallback are covered.
- [ ] P2: Add fenced-block segmentation edge-case coverage for
  `expandChangedRangeToMarkdownFenceBlocks` / `parseOpeningMarkdownFenceLine`:
  (1) a fence opened with 4+ backticks closed only by a matching-length fence,
  (2) tilde fences (`~~~`) alongside backtick fences, (3) a fence with a
  language followed by an info string, (4) a fenced block adjacent to inline
  code and indented code, and (5) an unclosed fence at end-of-file. Each case
  should assert the segmenter treats the fence as atomic (or explicitly rejects
  it as invalid) instead of splitting opener from body.
- [ ] P2: Cover fenced-block rejection paths in `parseOpeningMarkdownFenceLine`:
  assert inline-code spans (single-backtick runs shorter than 3) do not open a
  fence, assert a fence with a non-language info string (e.g., ``` ``` with
  trailing `{title}`) still matches the fence detector, and assert a fence
  whose language token contains whitespace is parsed as language = first-word
  or rejected consistently.
- [ ] P2: Add a direct unit test for `stripDiffPreviewDocumentContentFromWorkspaceState`:
  feed a workspace state containing a `diffPreview` tab with `documentContent`,
  `documentEnrichmentNote`, `diff`, and `gitDiffRequestKey` populated. Assert
  the output tab has `documentContent` removed but retains
  `documentEnrichmentNote`, `diff`, and `gitDiffRequestKey`. Covers the new
  save/parse pipeline in both `App.tsx` persistence and `workspace-storage.ts`.
- [ ] P2: Replace the brittle `toHaveBeenNthCalledWith(2, ...)` assertion in
  `ui/src/MarkdownContent.test.tsx:106-110` with
  `expect(mermaidInitializeMock).toHaveBeenCalledWith(expect.objectContaining({ theme: "dark" }))`.
  The current assertion pins two successive `mermaid.initialize` calls per
  render (dark init then base reset) and couples the test to the exact
  call order. A future refactor of the mermaid init sequence will break the
  test for reasons unrelated to user-visible behavior.
- [ ] P2: Move or restore the `SVGElement.prototype.getBBox` /
  `getComputedTextLength` polyfills out of
  `ui/src/panels/DiffPanel.test.tsx:228-243`. The current `beforeEach`
  installs them conditionally on first run and never restores them in an
  `afterEach`, so the patches leak across the entire test file (and any
  test that runs after a DiffPanel test in the same file). Move the
  installation into the shared vitest setup file, or wrap in a proper
  `try`/`finally` with saved originals.
- [ ] P2: Add overlapping rendered Markdown commit-range coverage:
  edit two sections with identical Markdown text (e.g., two `## TODO` headers),
  commit both, and assert the saved content is not garbled. The test should
  trigger `resolveRenderedMarkdownCommitRange` with two commits whose resolved
  byte ranges overlap or are adjacent-but-shifted.
- [ ] P2: Replace `toBeTruthy()` DOM element guards with `not.toBeNull()` or
  `toBeInTheDocument()` in `ui/src/panels/DiffPanel.test.tsx`. There are 16+
  occurrences where `expect(section).toBeTruthy()` guards a queried element
  that is `HTMLElement | null`. `not.toBeNull()` gives a more specific failure
  message and `toBeInTheDocument()` also verifies the element is connected.
- [ ] P2: Add direct unit tests for `mapMarkdownRangeAcrossContentChange`,
  `findClosestMarkdownRange`, and `resolveRenderedMarkdownCommitRange`:
  these pure functions have nontrivial logic (prefix/suffix diffing, closest-
  match scoring, range splicing) tested only indirectly via integration-level
  component tests. Direct unit tests would cover edge cases like empty strings,
  zero-length ranges, content shrinking to empty, and two identical matches
  at different offsets.
- [ ] P2: Strengthen the rendered Markdown committer-registry churn test
  (`ui/src/panels/DiffPanel.test.tsx:886-999`): the current assertion only
  proves sibling DOM node identity is preserved, which is strictly weaker
  than "committers are not unregistered/re-registered while typing". A
  React component can keep its DOM node while its `useEffect` deps still
  change and re-run. Add a direct register/unregister counter via a test
  seam (e.g., wrap `onRegisterRenderedMarkdownCommitter` with a `vi.fn()`
  at the wrapper level, or export a counter for the registration effect),
  and assert call counts stay at zero during a sibling keystroke edit.
- [ ] P2: Return a discriminated result from
  `handleRenderedMarkdownSectionCommits` in `ui/src/panels/DiffPanel.tsx`:
  the boolean return value currently overloads "applied cleanly", "no-op
  (no content change)", and "conflict; keep drafts dirty" into two states,
  which requires reading the caller to understand the contract. Return
  `"applied" | "no-op" | "conflict"` (or rename to
  `tryApplyRenderedMarkdownSectionCommits` with a doc comment) so callers
  can distinguish successful application from a silent no-op.
- [ ] P2: Reorder `collectSectionEdit` no-op branch in
  `ui/src/panels/DiffPanel.tsx`: on the "edit reverted to original" path
  the function clears `hasUncommittedUserEditRef`, `draftSegmentRef`, and
  `draftSourceContentRef` before calling `onDraftChange`. If the parent
  transitively re-reads committers during the same flushSync batch, it
  could see "no draft" in the refs while the segment is still listed in
  `renderedMarkdownDraftSegmentIdsRef`. Move `onDraftChange` before the
  ref clears for symmetry with the other branches.
- [ ] P2: Rename and split the Rust
  `git_diff_document_enrichment_note_uses_structured_error_kind` test
  in `src/tests.rs`: the test now also asserts the new status-code
  fallback path for untagged `BAD_REQUEST`/`NOT_FOUND`, which is not
  structured-kind driven. Rename to
  `git_diff_document_enrichment_note_fallback_for_bad_request_and_not_found`
  and keep a separate test that proves a BAD_REQUEST with size-suggestive
  message text still returns the generic fallback (not the size-specific
  note), so the "kind over message text" invariant has dedicated coverage.
- [ ] P2: Loosen the exact-string assertion in the rendered Markdown HTML
  paste sanitizer test (`ui/src/panels/DiffPanel.test.tsx:2178-2265`):
  the final `onSaveFile` assertion pins `"# Draft document\n\nNew section\n\nVisible linksafe text\n"`
  verbatim, which ties the security test to serializer whitespace
  behavior. Replace with a property-based assertion that the saved
  payload does not contain `onclick|onload|srcdoc|javascript:|<script|<svg|<iframe`
  (or document the exact-string contract explicitly so future whitespace
  edits update this test).
- [ ] P2: Extract a shared Monaco mock helper for `ui/src/App.test.tsx`,
  `ui/src/panels/DiffPanel.test.tsx`, and `ui/src/panels/SourcePanel.test.tsx`:
  the three files now duplicate `vi.mock("./MonacoDiffEditor", ...)` and
  `vi.mock("./MonacoCodeEditor", ...)` with subtle shape differences
  (status callback payloads, scroll-handle API). Move the baseline mock
  into a shared module (or `test-setup.ts` with overrides) so future
  real-component changes update one place.
- [ ] P2: Replace the ordering-dependent `mockImplementationOnce` chain
  in the "ignores stale manual Git diff responses" test
  (`ui/src/App.test.tsx:2070-2218`): the test relies on the first
  `fetchGitDiff` call returning `staleDiffDeferred.promise` and the
  second returning `currentDiffDeferred.promise`. A future code change
  that adds any intermediate fetch silently swaps the mapping. Replace
  with `mockImplementation((req) => ...)` that keys off a
  request-correlated field (e.g., call counter + filePath) so the
  deferred mapping is explicit. Also add
  `expect(fetchGitDiffSpy).toHaveBeenCalledTimes(2)` after the stale
  resolve so the test pins the "exactly two fetches" guarantee that
  the `attemptedGitDiffDocumentContentRestoreKeysRef.current.add(requestKey)`
  fix at `App.tsx:6020` actually provides â€” today the Monaco-content
  assertion is satisfied by the separate version-counter guard and
  would still pass even if the dedupe fix regressed.
- [ ] P2: Short-circuit the restored-document-content scan in
  `ui/src/App.tsx:3906-4015` when every `diffPreview` tab already has
  `documentContent`. The scan now runs on every `workspace.panes`
  change; in workspaces with many diff tabs it is O(panes Ã— tabs) per
  `setWorkspace`. An early-return when all tabs are fully hydrated
  keeps the fix for late-hydration restore without adding a per-update
  cost in common cases.
- [ ] P2: Move `useStableEvent` from `ui/src/panels/DiffPanel.tsx` into
  a shared hooks module (`ui/src/hooks.ts` or similar). The primitive
  is general (stable callback ref + `useLayoutEffect` publish window),
  and a future panel that recreates it locally will miss the new
  `flushSync` layout-phase comment and regress the same subtle bug.
- [ ] P2: Tighten the Mermaid sandbox test's `onload` substring
  assertion in `ui/src/MarkdownContent.test.tsx:245-261`:
  `expect.stringContaining("onload")` passes on any `onload` substring,
  including typos like `onloadxx`. Replace with
  `expect(frame.getAttribute("srcdoc")).toContain('onload="alert(1)"')`
  (full attribute match) or drop the assertion entirely in favor of the
  already-present `queryByTestId("mermaid-svg")).not.toBeInTheDocument()`
  which is the real isolation invariant.
- [ ] P2: Add live-updates.ts `sessionCreated` unit tests:
  add three tests in the `applyDeltaToSessions` suite â€” (1) session is
  appended when `sessionIndex === -1`, (2) session is replaced in place
  when it already exists, (3) `needsResync` is returned when
  `delta.session.id !== delta.sessionId`. Mirror the coverage pattern of
  other delta arms.
- [ ] P2: Add App-level tests for lazy session hydration:
  stub `api.fetchSession`, render a workspace with an active session
  marked `messagesLoaded: false`, assert (a) one-shot `fetchSession`
  fires, (b) a repeat render does not re-fetch, (c) a mismatched-id
  response triggers `requestActionRecoveryResyncRef`, (d) a stale
  (lower-revision) response is rejected without marking the session as
  hydrated.
- [ ] P2: Add 404 tests for `GET /api/sessions/{id}`:
  `get_session_route_returns_not_found_for_unknown_id` and
  `get_session_route_returns_not_found_for_hidden_session`. The hidden
  case is especially important â€” `find_visible_session_index` is the
  load-bearing invariant that prevents hidden Claude spares from leaking
  through the public route.
- [ ] P2: Add Rust coverage for `apply_remote_delta_event_locked::SessionCreated`:
  `remote_session_created_delta_creates_local_proxy_and_publishes_local_delta`
  â€” feed a remote `SessionCreated` with a fresh remote session id, assert
  a local proxy appears with remapped project id, the outbound local
  `SessionCreated` carries the local id, the revision bumps. Add an
  id-mismatch variant that returns the `anyhow!` error.
- [ ] P2: Add conflict-batch test for `handleRenderedMarkdownSectionCommits`:
  exercise the new boolean-`false` branch â€” make two sibling rendered
  Markdown edits, trigger a document refresh that unmaps one range,
  commit the batch, and assert (a) save-error banner visible,
  (b) both drafts still dirty, (c) `onSaveFile` not called. This pins
  the atomicity invariant the bug entry claims.
- [ ] P2: Gate the oversized-Mermaid assertion with `waitFor`:
  `ui/src/panels/DiffPanel.test.tsx:1265-1332` asserts
  `expect(mermaidRenderMock).not.toHaveBeenCalled()` synchronously after
  `render()`, but Mermaid is async-gated by a `useEffect`. Add an
  `await waitFor(() => expect(screen.queryByTestId("mermaid-frame")).not.toBeInTheDocument())`
  first so the effect has a chance to not run before the mock assertion.
- [ ] P2: Strengthen `create_session_refreshes_agent_readiness_cache`:
  the updated delta assertion checks ids but not session body fields.
  Add at least one identity-confirming assertion (e.g.,
  `assert_eq!(session.name, "Test Codex Session")`) so a regression
  that emits the wrong session body with a matching id fails.
- [ ] P2: Replace brittle `{ overwrite: undefined }` matcher in the
  save-options test (`ui/src/App.test.tsx`):
  `toHaveBeenNthCalledWith(1, ..., { baseHash, overwrite: undefined, ... })`
  treats missing and `undefined` as equal in Vitest deep-equality. A
  future refactor that conditionally spreads `overwrite` still passes
  even if the intent ("first save does not send overwrite") breaks.
  Use `expect.objectContaining({ baseHash: "..." })` plus
  `expect(saveFileSpy.mock.calls[0][2]).not.toHaveProperty("overwrite")`.
- [ ] Add regression tests for the `VirtualizedConversationMessageList` measuring phase:
  (1) assert the wrapper has `is-measuring-post-activation` on initial mount
  with `isActive=true` + messages; (2) assert the class is removed after all
  visible slots have fired their `ResizeObserver` callback, via the completion
  check path; (3) assert the class is removed after 150 ms via the timeout
  fallback (use `vi.useFakeTimers()` + `vi.advanceTimersByTime(150)`);
  (4) render with `isActive={false}`, rerender with `isActive={true}`, and
  assert the class appears on the inactive â†’ active transition.
- [ ] Add a shake-fix regression test for integer rounding and the `>= 1`
  no-op scrollTop guard in `handleHeightChange`: feed a fractional height
  (e.g., `measuredSlotHeight = 260.7`) through the existing bottom-pin test
  harness and assert the pinned `scrollWrites` target is computed against
  the integer cumulative (not the float). Also add a negative test where
  a measurement implies `target === current scrollTop` and assert
  `scrollWrites` is unchanged after the `ResizeObserver` fires.
- [ ] Update the existing "keeps the bottom pin across successive virtualized
  height commits" test to fire `ResizeObserver` for every visible slot
  before its assertions, so the test doesn't end with `isMeasuringPostActivation`
  still active. Current test survives the measuring-phase refactor only
  because the re-pin effect fires independently â€” a fragile coincidence
  that could silently break under future refactors.
- [ ] Extract a `pinScrollTopToBottomIfChanged(node)` helper in
  `ui/src/panels/AgentSessionPanel.tsx` and call it from the three sites
  that currently duplicate the `const target = ...; if (Math.abs(...) >= 1) ...`
  block (re-pin `useLayoutEffect`, measuring-phase completion check,
  measuring-phase 150 ms timeout fallback). Optionally also use it in the
  `handleHeightChange` shouldKeepBottom branch. Keeps the no-op guard
  threshold in a single place so future tuning stays consistent.
- [ ] Migrate existing git tests to `init_git_document_test_repo`:
  ten other tests still inline `run_git_test_command(&repo_root, &["init"])` +
  user config without `core.autocrlf=false`, leaving latent Windows CI
  flakiness for any fixture that writes mixed line endings. Start with the
  Markdown-diff tests around `src/tests.rs:21841`, `21908`, `22159`, `22205`,
  `22292`, `22338`, `22585`, `22631`, `26106`, `26223`.
- [ ] Re-query section after Escape cancel in the rendered Markdown regression test:
  the existing "cancels an uncommitted rendered Markdown section edit with
  Escape" test asserts `section.toHaveTextContent("Ready to commit.")` on the
  originally captured reference, which could false-pass if a regression
  remounted the section. Re-query after Escape via
  `document.querySelectorAll` + `find`, and assert `document.contains(section)`.
- [ ] Install jsdom geometry mocks inside `try` blocks:
  both new mock-heavy tests (MarkdownContent ResizeObserver + AgentSessionPanel
  multi-commit pin) install `window.*` overrides BEFORE the `try` block, so a
  future edit that throws during installation could leak globals. Move the
  mock installation inside the `try` so `finally` always runs against the
  saved originals.
- [ ] P2: Broaden `writeScrollTopAndSyncViewport` regression
  coverage beyond the initial-mount bottom-pin:
  `AgentSessionPanel.test.tsx::"renders the bottom window
  immediately after a virtualized bottom-pin scroll write"`
  pins the initial-mount bottom-pin call site of the helper,
  but the other call sites (session-find hit pin,
  post-activation completion + fallback timeout,
  anchor-preserving scroll after `renderWindowSize` changes,
  and both branches of `handleHeightChange`) inherit
  correctness from the helper without a dedicated window-
  tracking assertion. The load-more anchor path in particular
  has the same failure mode ("click Load N earlier â†’
  `scrollTop` moves to anchor offset â†’ stale window renders")
  but no direct test. Add a Vitest case that mounts with 200+
  messages at the top, scrolls up to `scrollTop = 0`, clicks
  "Load N earlier messages", and asserts both the anchor-
  aligned `scrollTop` write and that cards around the anchor
  are rendered (while newly-prepended earlier messages are
  absent).
- [ ] P2: Pin the `SqlitePersistConnectionCache` error-driven
  invalidation path:
  the SQLite cache now drops its cached connection on any
  persist error so the next tick reopens fresh. The cache
  struct and its sole write path (`persist_delta_via_cache`)
  are both `#[cfg(not(test))]`-gated today â€” test builds use
  JSON persistence via the test variant of
  `persist_state_from_persisted` instead â€” so a direct
  regression test requires either (a) removing the cfg gate
  (and accepting the rusqlite compile cost in test builds) or
  (b) adding a narrow integration-style test that opens a real
  SQLite path, seeds a persist failure (e.g., delete the
  backing file between calls, or issue a deliberately-failing
  transaction), and asserts the next call reopens and
  succeeds. Option (b) is cheaper; pick a single failure mode
  and pin the reopen.
- [ ] P2: Pin the `fetchSession` 404 â†’ silent resync branch
  (`ui/src/App.tsx:1627`):
  the hydration effect's catch block now routes
  `ApiRequestError` with `status === 404` through
  `requestActionRecoveryResyncRef.current()` instead of
  `reportRequestError(error)`, but no test exercises that
  branch. The hydration effect only runs when a session has
  `messagesLoaded === false`, which the backend does not emit
  today (forward-compat scaffolding), so a test needs to
  construct a state response with `messagesLoaded: false`,
  mock `api.fetchSession` to reject with
  `new ApiRequestError("request-failed", "...", { status: 404 })`,
  and assert (a) no toast / `reportRequestError` invocation,
  (b) a subsequent state-resync call, and (c) the session stays
  in the sessions list (the mismatch branch that causes the
  recovery reset is a DIFFERENT code path). Pair with a
  non-404 failure case that DOES call `reportRequestError` to
  negative-control the branch.
- [ ] P2: Pin the lazy legacy-key lookup in `load_state_from_sqlite`:
  the `.or(...)` â†’ `if let` refactor in `src/persist.rs` is a
  pure startup-cost optimization â€” both the eager and lazy
  variants return identical values, so all 32 existing persist
  tests pass against either. A regression that restored `.or(...)`
  would silently reintroduce the redundant
  `SELECT ... FROM app_state WHERE key = ?` round-trip on every
  startup without any test failing. Pin the contract by either
  (a) wrapping the `Connection` in a query-counting shim used
  only by a dedicated test, or (b) adding a `rusqlite`
  trace-hook-based test that asserts exactly one
  `FROM app_state` SELECT fires when the primary metadata row
  is present. Low priority because the tests still correctly
  pin return-value behavior; the only regression the pin would
  catch is the "silent redundant query" failure mode.
- [ ] P2: Add a direct field-survival test for
  `PersistedState::metadata_only()`:
  the helper is currently `#[cfg(not(test))]`-gated so no test
  exercises it directly. Behavioral equivalence with
  `metadata_from_inner` is load-bearing â€” if a future top-level
  field is added to `PersistedState` and only one of the two
  methods is updated, the production persist path silently
  drops the field from the SQLite `app_state` row. Either drop
  the `#[cfg(not(test))]` gate and add a test that builds a
  `PersistedState` with a populated sessions vec, calls
  `metadata_only()`, and asserts (a) sessions is empty and (b)
  every non-sessions field survives, or keep the gate and add a
  macro/test harness that asserts field parity between the two
  constructors. The inline cross-reference doc comments on each
  method already flag the pairing; a test would make the
  coupling load-bearing.
- [ ] P2: Extract a shared `build_test_app_state(persist_tx)` helper:
  `src/tests/mod.rs::test_app_state` and
  `src/tests/persist.rs::test_app_state_with_live_persist_channel` now
  duplicate ~35 lines of `AppState` field construction. Only two
  fields actually differ between them today â€” `persist_tx` (receiver
  dropped vs. kept alive) and the uniqueness of the `persistence_path`
  temp-file name. A shared constructor that takes a `Sender<PersistRequest>`
  and returns an `AppState` would let both callers collapse to
  `build_test_app_state(mpsc::channel().0)` vs. the live-receiver
  variant, and a future third variant (e.g., a test that wants a live
  broadcaster thread too) would not need to duplicate the field list
  again. Low priority â€” the helper is small and only two call sites
  share the shape today.
- [ ] P2: Rename or document `has_complete_previous_transcript`:
  the boolean at `src/remote_sync.rs:311-318` gates whether an incoming remote
  session payload may demote the local `messages_loaded` from `true` to `false`.
  The name reads as a property of the local cache ("we had a complete
  transcript before"), but it actually evaluates the incoming payload's
  completeness against the cached `message_count`. Either rename to something
  like `incoming_payload_is_complete_relative_to_cache` (verbose but unambiguous)
  or add a 2-3 line doc comment above the binding spelling out exactly what
  "complete previous transcript" means in terms of input vs. cached state.
  Current callers would survive either change mechanically.
- [ ] P2: Document `session_message_count` max-over-cached semantics:
  `src/messages.rs::session_message_count(record)` now returns
  `local_count.max(record.session.message_count)` when
  `!record.messages_loaded`, so the reported count can exceed
  `record.session.messages.len()`. This is load-bearing for metadata-first
  summaries (the cached remote count must not be lost when the local
  transcript is empty/partial), but no comment explains the intentional
  divergence. Add a `///` doc comment stating: "For unhydrated records, returns
  max(local message vec length, cached metadata message_count) so remote
  count metadata survives partial local transcripts." A `debug_assert!` on the
  hydrated branch that `local_count == record.session.message_count` would
  also surface silent drift between the field cache and the vec length.
- [ ] P2: Tighten the new orchestrator summary-preservation fixture
  in `src/tests/remote.rs:1505-1521`:
  the test currently probes `.find(|s| s.message_count == 2)` and asserts the
  matched session's summary shape. Add an `.all()` assertion over the full
  returned vec so the contract "every republished session is metadata-first"
  is pinned, and expand the fixture to include at least two sessions with
  distinct `message_count` values so the `.all()` assertion has more than one
  summary to validate. Optionally parameterize over a hydrated vs. unhydrated
  input mix to pin that republish always projects metadata regardless of
  source hydration state.
- [ ] P2: Pin record-state in the stale-hydration tests:
  `ui/src/App.live-state.deltas.test.tsx:783-791,963-973` now check the newer
  text renders, but a regression that briefly flashes the new content while
  leaving the session record in `messagesLoaded: false` or with the wrong
  `messageCount` would still pass. Query the session store after the
  `await waitFor(...)` and assert `messageCount === <expected>` plus
  `messagesLoaded === <expected>` so silent accept-then-overwrite regressions
  are caught.
- [ ] P2: Cover multi-field `options` in
  `ui/src/app-live-state.test.ts` (NEW):
  the new `resolveAdoptStateSessionOptions` test only passes
  single-property option objects, so a regression that reorders the spread
  (`{ disableMutationStampFastPath: ..., ...options }`) — inverting the OR
  semantics — would not be caught. Add a case like
  `expect(resolveAdoptStateSessionOptions({ openSessionId: "s1", paneId: "p1", disableMutationStampFastPath: true }, false)).toMatchObject({ openSessionId: "s1", paneId: "p1", disableMutationStampFastPath: true })`.
- [ ] P2: Add the `monaco-cancellation-filter` default-target branch test:
  `ui/src/monaco-cancellation-filter.test.ts:43-48` exercises only the
  injected-`target` branch. The two production call sites (`main.tsx:10`,
  `monaco.ts:42`) both rely on the default `target = window`. Stub
  `globalThis.window` to a minimal `{ addEventListener: vi.fn() }`,
  call `installMonacoCancellationRejectionFilter()` twice without args,
  and assert `addEventListener` was called exactly once.
- [ ] P2: Cover the bounded hydration retry backoff schedule directly:
  the new `SESSION_HYDRATION_RETRY_DELAYS_MS = [50, 250, 1000, 3000]` is
  exercised only indirectly via the existing stale-hydration tests using
  real timers. A regression that doubled the first delay or always used
  index 0 would still pass within the default `waitFor` timeout. Add a
  `vi.useFakeTimers()` test that exercises retries 2, 3, and 4 and
  asserts the delay sequence advances through the schedule.
- [ ] P2: Cover the over-cache demotion branch of
  `has_complete_previous_transcript`
  (`src/remote_sync.rs:311-318`):
  the unstaged tightening from `<=` to `==` made the gate strictly
  stricter. The equal-count preservation path is now covered by
  `remote_summary_same_count_with_same_stamp_preserves_cached_transcript`,
  but the over-cache demotion case (cached length > remote `message_count`)
  is still uncovered. Add
  `apply_remote_session_demotes_when_cached_transcript_overshoots`:
  cached length 3, remote `message_count: 2`, matching stamp; expect
  demoted to `messages_loaded: false`.
- [ ] P2: Cover the session-switch-induced loop variant in
  `App.session-lifecycle.test.tsx:899-923`:
  the new "keeps the Create Session assistant pick after changing away
  from the active session agent" test only pins the immediate self-loop
  case. Add a follow-up step where a new session is created and becomes
  active while the dialog is open, and assert the user's "Claude" pick
  still sticks. The original failure mode was
  `setNewSessionAgent → effect re-runs → sets back`; the session-switch
  case is the second half of that loop class.
- [ ] P2: Pin `response.revision` in
  `get_session_hydrates_unloaded_remote_proxy_from_remote_owner`
  (`src/tests/remote.rs:2247-2371`):
  the test pins request lines and `session.message_count == 1` but
  not `response.revision`. Capture the pre-hydration revision via
  `state.snapshot().revision`; after `state.get_session(...)`, assert
  `response.revision > pre_revision` and that the value matches the
  locally-advanced revision after `commit_locked`. Closes the
  "verify the actual response revision is propagated" gap.
- [ ] P2: Spawn a remote server in
  `stale_remote_delta_skips_before_targeted_hydration_fetch`
  (`src/tests/remote.rs:2580-2653`):
  the test does NOT spawn a remote, so it cannot prove the production
  code skipped the HTTP fetch — only that the consequence (no state
  changes) holds. A regression that bypassed the skip and fell through
  to a non-network path would still pass. Spawn
  `spawn_remote_session_response_server`, bind `requests`, and after
  the stale delta is applied assert `requests.lock().is_empty()` (no
  `GET /api/sessions/...` line). Converts the test from "behavioral
  consequence" to "no HTTP fetched."
- [ ] P2: Subscribe to delta events in the new same-stamp tests
  (`src/tests/remote.rs:2169-2245` and
  `remote_summary_same_count_with_new_stamp_marks_cached_transcript_unloaded`):
  the existing sibling
  `remote_summary_count_decrease_marks_cached_transcript_unloaded`
  already subscribes and inspects the published `SessionCreated`
  delta's summary shape. The two new same-stamp tests omit this
  inspection, leaving published-delta regressions undetected.
  Subscribe via `state.subscribe_delta_events()` and assert the
  summary's `messages_loaded`/`messages.is_empty()`/`message_count`
  shape on each.
- [ ] P2: Cover the both-`None` mutation-stamp case in
  `apply_remote_session_to_record`
  (`src/remote_sync.rs:316-321`):
  `Option::zip` returns `None` whenever either side is `None`, which
  evaluates the freshness gate to `false` and demotes. Add a
  regression where both `previous_remote_mutation_stamp` and the
  incoming `remote_session.session_mutation_stamp` are `None` and
  assert the chosen demotion policy. Pair with a comment in
  production explaining the four `(prev, next)` stamp-presence cases.
- [ ] P2: Add a regression for the `session_mutation_stamp: None`
  wire-payload clobber on `SessionCreated`/`OrchestratorsUpdated`
  (`src/remote_sync.rs:312`):
  the message-mutating arm fix landed, but `apply_remote_session_to_record`
  still clobbers cached `Some(_)` to `None` for `SessionCreated` and
  `OrchestratorsUpdated.sessions[]` payloads. Send a `SessionCreated`
  summary with `session_mutation_stamp: None` against a cached `Some(_)`;
  assert the cached stamp survives. Pair with the production fix that
  gates the assignment on `is_some()` for the broad session-shape path.
- [ ] P2: Cover replay-after-hydration for non-`TextDelta` arms
  (`src/remote_routes.rs:867-1490`):
  the new `remote_session_delta_already_reflected` dedup branch is wired
  into all six message-mutating arms but the regression
  (`remote_text_delta_replay_after_targeted_hydration_is_skipped_by_metadata`)
  only covers `TextDelta`. Add at least a `MessageCreated` replay test
  (highest-stakes — appends a new entry rather than mutating an existing
  one) and ideally a parameterized regression covering all six variants.
  A future change that breaks the dedup shape would only blow up `TextDelta`.
- [ ] P2: Cover the new `remote_state_applied` commit-then-error branches
  in `hydrate_remote_session_target` (`src/remote_routes.rs:577-589`,
  `:597-610`):
  two new branches lack regressions: (1) the missing-target exit where
  the proxy disappears between state apply and lookup — assert `404`
  is returned AND `inner.remote_applied_revisions` was advanced
  (broad-state mutation preserved); (2) the stale-but-matching-metadata
  pass-through where the older targeted response carries matching
  `messageCount` + `sessionMutationStamp` — assert the call returns
  `Ok` and the cached transcript is left intact.
- [ ] P2: Bind `requests` and pin endpoint + state-event publish in
  the three `_requests`-prefixed remote tests
  (`src/tests/remote.rs:2445`, `:2538`, `:2638`) — third-round repeat:
  `get_session_rejects_stale_remote_transcript_after_newer_state_snapshot`,
  `remote_message_delta_hydrates_unloaded_proxy_before_gap_check`,
  and the new
  `remote_text_delta_replay_after_targeted_hydration_is_skipped_by_metadata`
  all bind the captured request log as `_requests` and never inspect it.
  Bind `requests` and assert the expected `GET /api/sessions/...` and
  `GET /api/state` lines mirror the pattern from
  `get_session_hydrates_unloaded_remote_proxy_from_remote_owner` (lines 2355-2367).
- [ ] P2: Pin `messageCount` and `sessionMutationStamp` in
  `live-updates.test.ts:472-515`:
  the new "applies authoritative sessionCreated metadata even when the
  mutation stamp matches" test asserts `name`, `preview`, `messages`,
  and `messagesLoaded` but does not pin `messageCount` or
  `sessionMutationStamp` (asymmetric with the sibling preservation test
  at `:458-470` that pins both). Add
  `expect(result.sessions[0].messageCount).toBe(1)` and
  `expect(result.sessions[0].sessionMutationStamp).toBe(202)`.
- [ ] P2: Add a `monaco-cancellation-filter` negative test for a Monaco
  worker frame WITHOUT `computeDiff`
  (`ui/src/monaco-cancellation-filter.test.ts`):
  the matcher was tightened from "Monaco frame OR computeDiff" to
  "Monaco frame AND computeDiff". The negative direction (`computeDiff`
  alone) is now covered, the positive direction (Monaco frame +
  `computeDiff`) is covered, but the remaining quadrant — a Monaco
  worker frame (e.g., `EditorWorkerClient`) without `computeDiff` —
  is uncovered. A regression that drops the `computeDiff` requirement
  (reverting to OR) would not be caught. Add a third negative case
  asserting `isBenignMonacoCancellationReason(createCanceledError("Canceled: Canceled\n    at EditorWorkerClient2.idleCheck"))`
  returns `false`.
- [ ] P2: Cover prompt-send-while-pinned `bottom_follow` change
  (`ui/src/SessionPaneView.tsx:1148-1152`):
  the `followLatestMessageForPromptSend` path silently changed scroll
  behavior from `"auto"` to `"smooth"` and added a new scroll-kind, but
  no test confirms the new behavior. The pre-existing prompt-send test
  uses `auto` and pre-dates this change. Add a test that sends a prompt
  while already at the bottom and asserts the scroll-to call is
  `"smooth"` with `bottom_follow` — symmetric to the existing
  "follows the latest user prompt immediately while a send is in flight"
  test that pins the away-from-bottom `auto` path.
- [ ] P2: Cover the `bottom_follow` virtualizer state machine directly
  (`ui/src/panels/VirtualizedConversationMessageList.tsx:1467-1495,1610-1624`):
  fire a synthetic native `scroll` event with `scrollTop` advancing
  toward the bottom after a `bottom_follow` write and assert
  `hasUserScrollInteractionRef` is not set (no "New response" indicator
  on the next assistant delta), `shouldKeepBottomAfterLayoutRef`
  survives, and a user wheel/keyboard event during the window cancels
  the cooldown. Cover the early-exit `if (isScrollContainerNearBottom(node))`
  branch at 1492-1495 separately.
- [ ] P2: Cover pane-level `bottom_follow` sticky-state preservation:
  simulate smooth bottom-follow scroll events that pass through intermediate
  offsets outside the near-bottom threshold, then assert `SessionPaneView`
  does not clear sticky-bottom or show the "New response" affordance until a
  real user scroll cancels the programmatic follow.
- [ ] P2: Fix the `App.scroll-behavior.test.tsx` "scroll away from
  bottom" test geometry (`ui/src/App.scroll-behavior.test.tsx:944-948`):
  the test writes `scrollTop = 800` with `scrollHeight = 1000` and
  `clientHeight = 200` — that's exactly the bottom. The "still
  smooth-follow even when not yet pinned" intent is undermined; the
  panel was already pinned. Change `scrollTop` to a non-bottom value
  (e.g., 200) and dispatch a `wheel` event with `deltaY: -300` to
  actually establish the "scrolled up" state, OR rename the test to
  match what it does. While there, extract a
  `stubMutableElementScrollGeometry` helper to replace the inline
  `Object.defineProperty` block at `:879-899`.
- [ ] P2: Cover SourcePanel rendered-Markdown conflict / mode-switch
  blocked paths (`ui/src/panels/SourcePanel.tsx:357-362,424` —
  no test in `SourcePanel.test.tsx`):
  the `unresolvedCommitCount > 0` / `hasOverlappingMarkdownCommitRanges`
  branch fires `setActionError(...)` and returns `false`; the
  `handleSelectDocumentMode` blocks mode-switch when
  `commitRenderedMarkdownDrafts()` returns `false`. Both are
  user-visible error paths added by the round and have no coverage.
  Add a test that mutates the source content under an active draft
  (rerender with new `content` while the rendered editor still has
  uncommitted text) and assert the action-error banner appears AND
  `onSaveFile` is not called. Pair with a mode-switch-blocked test.
- [ ] P2: Cover SourcePanel rendered-preview drafts across source-file switches:
  start a rendered Markdown draft in file A, rerender SourcePanel with a
  different Markdown file B, and assert the draft/committer from A is gone and
  cannot affect B's save payload.
- [ ] P2: Cover successful SourcePanel rendered-Markdown commits during
  document-mode changes:
  edit rendered Markdown in Preview or Split, click another mode (`Code`,
  `Preview`, or `Split`), and assert the shared editor buffer, dirty state,
  and later save payload include the rendered edit. This pins the
  `handleSelectDocumentMode` success path instead of only the save/blur paths.
- [ ] P2: Cover Markdown href serialization breakout:
  construct an editable Markdown DOM anchor with an allowed scheme plus
  Markdown metacharacters in the destination, such as
  `https://example.com/) [x](javascript:alert(1)`, then assert
  `serializeEditableMarkdownSection` does not persist a second Markdown link.
- [ ] P2: Cover DiffPanel save abort on failed rendered draft commit:
  create a rendered Markdown draft whose commit range no longer resolves, run
  the toolbar/parent save path, and assert the save callback is not invoked and
  the draft remains recoverable.
- [ ] P2: Add a `userEvent.type` end-to-end test for the rendered
  preview contentEditable in `SourcePanel.test.tsx`
  (the existing `editRenderedMarkdownSection` helper at `:68-79`
  bypasses the contentEditable user surface by directly mutating
  `innerHTML`):
  add at least one test using `userEvent.type` against the
  contentEditable to exercise the typing → input → onDraftChange flow
  end-to-end. Real editing-path bugs (caret position, paste handlers,
  typed text not firing the right input event) cannot surface today.
- [ ] P2: Cover URI-list-only and file-only rendered-Markdown drops:
  the SourcePanel drop regression now covers hostile `text/html` plus image
  markup and serializer defense-in-depth. Add follow-up cases for a
  `text/uri-list`-only `javascript:` drop and file-only/image drops, asserting
  default insertion is prevented and the live DOM does not host remote-loading
  elements.
- [ ] P2: Cover pane-level smooth-scroll intermediate-tick detachment in
  `App.scroll-behavior.test.tsx` or `AgentSessionPanel.test.tsx`:
  fire intermediate `fireEvent.scroll(messageStack)` events with
  `scrollTop` mid-animation (e.g., 600, 700, 850) during a `bottom_follow`
  write and assert (a) `shouldStickToBottom` (proxy: "New response"
  indicator absence) survives, (b) any settled-scroll RAF queued for
  layout settling is NOT cancelled, and (c) a real
  `fireEvent.wheel(messageStack, { deltaY: -300 })` cancels the cooldown.
- [ ] P2: Cover cross-source-file rendered-draft contamination in
  `SourcePanel.test.tsx`:
  start a rendered draft on file A (`editRenderedMarkdownSection(...)`),
  rerender with file B's `fileState`, assert the dirty banner is gone
  and Ctrl+S on file B's editable section does not include file A's
  draft text. This should pin the current `key={tab.id ?? path}` remount
  contract instead of documenting an active leak.
- [ ] P2: Cover `handleSelectDocumentMode` mode-switch blocking on commit
  failure in `SourcePanel.test.tsx`:
  mutate parent `content` while a rendered draft is active, click another
  mode button, and assert the action-error banner appears AND
  `documentMode` did not change (active mode button still selected).
  Today only the happy path through the mode-switch is covered.
- [ ] P2: Add negative-path cases for `resolveRenderedMarkdownCommitRange`
  in `markdown-commit-ranges.test.ts`:
  the existing test only covers the success path. Add table-driven
  negative cases where the source content is mutated before, after, and
  inside the draft segment, asserting `null` for each. This pins the
  foundation predicate that the SourcePanel `unresolvedCommitCount > 0`
  branch depends on.
- [ ] P2: Assert `scrollTop` preservation in the new upward-wheel
  prewarm test (`ui/src/panels/AgentSessionPanel.test.tsx:2008-2217`):
  the new test asserts `getFirstMountedMessageIndex < firstMountedBeforeWheel`
  but does NOT assert that `scrollNode.scrollTop` is preserved across the
  prepend. The `docs/bugs_flicker.md` preamble specifically claims
  "prepends still use the existing scroll-height restore so replacing
  estimated spacer space with real page DOM does not steal the user's
  wheel progress." That contract has no test today. Add an assertion that
  reads `scrollNode.scrollTop` before and after the wheel + prepend, and
  asserts the user's wheel progress is preserved (not regressed to a
  lower value than expected).
- [ ] P2: Cover `resolveWheelDeltaYPx` line/page delta conversion:
  extend the upward-wheel prewarm coverage with `DOM_DELTA_LINE` and
  `DOM_DELTA_PAGE` cases, then assert the converted projection mounts earlier
  pages while preserving the user's scroll progress.
- [ ] P2: Add a "not yet pinned" / "scrolled away from bottom" test for
  the second `bottom_follow` write site at `SessionPaneView.tsx:1846`:
  the path that changed `auto`→`smooth`+`bottom_follow` in the
  `requestAnimationFrame` jump-to-bottom branch has no test. Add a test
  where the user is genuinely scrolled away from bottom (e.g.,
  `scrollTop=200` with `scrollHeight=1000`/`clientHeight=200`) before the
  assistant delta arrives, and assert the dispatcher chooses the settled
  jump-to-bottom rather than `bottom_follow`.
- [ ] P2: Pin the `scrollKind === "bottom_follow"` dispatch contract in
  `App.scroll-behavior.test.tsx:877-1009`:
  the existing "smoothly follows new assistant messages while pinned"
  test asserts only that `scrollTo` was called with `top: 900,
  behavior: "smooth"`. A regression that ships the smooth scroll
  without the new scroll-kind (and therefore without the cooldown
  re-classification at `VirtualizedConversationMessageList.tsx:1467-1495,1610-1624`)
  would still pass. Spy on `notifyMessageStackScrollWrite` (or the
  `dispatchEvent` call) and assert the dispatched `scrollKind` is
  `"bottom_follow"`. Pair with a sibling assertion at the not-yet-pinned
  write site (`SessionPaneView.tsx:1846`).

## Known Design Limitations

These are deliberate design tradeoffs, not bugs, but are recorded here so
they stay visible to future contributors and can be revisited if the
tradeoff space changes.

### Untracked Git diff previews have a 10 MB read cap

Untracked file diffs use the same `MAX_FILE_CONTENT_BYTES` ceiling as rendered
Markdown document reads. Files above that cap return a read-limit error instead
of building an unbounded synthetic `+` diff in memory.

**Accepted tradeoff.** This is a deliberate defense against large accidental
untracked files such as logs or generated artifacts. The UI records the backend
error on the pending diff tab instead of crashing, and normal staged/tracked Git
diffs still come from Git itself.

### Terminal commands have no production watchdog

Terminal commands intentionally run without a production timeout. The terminal
panel is used for long-lived foreground workflows such as `flutter run`, dev
servers, watch tasks, and REPL-like tools, so a watchdog would terminate
commands users expect to keep alive.

**Consequence:** a running command holds its terminal concurrency permit until
it exits or the stream is disconnected. The streamed local path now observes
SSE disconnects and kills the local process tree, but ordinary JSON terminal
runs and remote command lifetime are still governed by the command/backend
itself rather than a TermAl watchdog.

**Mitigations already in place:**
- Local and remote terminal commands have separate concurrency caps.
- Captured output and live stream buffers are bounded.
- The no-timeout behavior is documented at the local and remote terminal
  launch sites in `src/api.rs`.

### Remote terminal stream worker thread can stay parked on a stalled remote

`InterruptibleRemoteStreamReader::spawn` in `src/remote.rs` wraps the
blocking remote HTTP body read in a dedicated OS thread that pushes chunks
into an `mpsc::sync_channel(1)`. The main forwarding loop reads from the
channel with `recv_timeout(10ms)` and observes the cancellation flag
between polls, so the user-visible "4-in-flight remote permit" path is
correctly released on client disconnect â€” the forwarder returns
`terminal stream client disconnected`, drops its end of the channel,
releases the semaphore permit, and exits. The spawned reader thread,
however, is still parked inside `source.read(&mut scratch)` until the
reqwest body read finally returns (a byte arrives, the socket closes, or
an error fires). Because the backend intentionally builds its
`BlockingHttpClient` without a body read timeout (so legitimate long
streams can keep producing output), a remote that holds its socket open
without emitting bytes will pin the reader thread and its TCP socket
until the remote finally closes the connection.

**Consequence:** repeated client-disconnect cycles against a stalled
remote accumulate detached reader threads + sockets, one per disconnect,
until the remote eventually closes its side. Each thread holds a
~2-8 MB stack and a single TCP connection. The bound is set by the
remote's own keepalive / TCP timeout behaviour, not by TermAl.

**Accepted tradeoff.** The three real fixes â€” setting a per-read body
timeout on reqwest (not supported by the blocking API), moving the read
onto an async `tokio::select!` with a cancellation future (a large
rewrite of the remote bridge), or manually `dup`ing the raw socket fd
and closing it from outside (unsafe, platform-specific, bypasses reqwest
encapsulation) â€” are all strictly larger than the bounded dormant-thread
cost. This mirrors the existing "Unix terminal clean-exit cleanup is a
no-op" limitation below: both are bounded native-thread leaks that wait
on an external event (grandchild pipe close, remote socket close), and
both are left as-is because the cleanup machinery needed to plug them
would be more invasive than the leak they prevent.

**Mitigations already in place:**
- The forwarder returns promptly on cancellation via the adapter's
  `recv_timeout` poll, so the user-visible semaphore permit is released
  immediately and new commands are not blocked by dormant reader threads.
- `read_remote_stream_response` re-checks the cancellation flag between
  reads, so a reader thread that finally gets a byte from the remote
  exits on the next loop iteration rather than continuing to buffer.
- `InterruptibleRemoteStreamReader::spawn_unblocks_on_cancellation` and
  `interruptible_remote_stream_reader_observes_cancellation_between_recv_timeouts`
  pin the two sides of the contract so a future edit that breaks either
  the adapter poll or the spawn path fails a specific regression test.

### Unix terminal clean-exit cleanup is a no-op

On Unix, `TerminalProcessTree::cleanup_after_shell_exit` is intentionally
a no-op on the success path. Once `wait_for_shared_child_exit_timeout` has
reaped the shell, the kernel is free to recycle the shell's PID (and
therefore its process group id), so calling `libc::killpg(process.id(),
SIGKILL)` would race PID reuse and could SIGKILL an unrelated local
process group. Rust's stdlib `Child::send_signal` guards against this
same hazard by early-returning once the child has been reaped.

**Consequence:** a command like `sleep 999 & echo done` will return
successfully, release its terminal-command permit, and leave the
backgrounded `sleep` running outside TermAl's accounting. The backgrounded
grandchild re-parents to init (PID 1), so it is owned by the OS rather
than leaked in TermAl itself, but it is not bounded by the terminal
command timeout or the 429 semaphore.

**Mitigations already in place:**
- On Windows the path is completely covered: the Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` terminates every process assigned
  to it when the shell exits, so backgrounded grandchildren are killed.
- On Unix, the per-stream reader-join timeout
  (`TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`, 5s) bounds how long the terminal
  command waits for each stdio reader to finish even if a backgrounded
  grandchild still holds the inherited stdout/stderr pipes.
  `run_terminal_shell_command_with_timeout` joins stdout and stderr
  **sequentially**, so the pathological success-path reader-join phase
  is up to ~10s (5s per stream), not 5s. That phase runs *after* the
  child wait returns, so on the local path the total worst-case wall
  clock is `TERMINAL_COMMAND_TIMEOUT` (60s child wait) + ~10s (reader
  join), or approximately 70s. On the remote-proxy path the same ~70s
  inner budget fits inside the 90s `REMOTE_TERMINAL_COMMAND_TIMEOUT`
  envelope with ~20s of slack. Tuning `TERMINAL_OUTPUT_READER_JOIN_TIMEOUT`
  should account for both budgets and the 2x sequential multiplier.
- Reader threads write into an `Arc<Mutex<TerminalOutputBuffer>>` shared
  with the main thread (see `read_capped_terminal_output_into` and
  `join_terminal_output_reader` in `src/api.rs`). Each reader signals
  completion via a `sync_channel(1)` so `join_terminal_output_reader`
  blocks in `recv_timeout` instead of polling -- the happy-path wake is
  event-driven, not tick-driven. On a reader-join timeout, the main
  thread snapshots whatever prefix the reader already accumulated and
  returns it marked `output_truncated = true`. Without the shared
  buffer, the main thread dropped the `JoinHandle` and returned
  `String::new()`, so any `echo done` output that had already been
  buffered was silently discarded even though the foreground shell
  produced it. The detached reader thread still runs until the
  backgrounded grandchild closes its inherited pipe end, but no data
  captured up to the timeout is lost.
- The timeout path still calls `killpg` (reserved for the pre-reap window,
  where `process.id()` is guaranteed to still refer to our process), with
  an additional `try_wait` defense against a narrower race where
  `wait_for_shared_child_exit_timeout`'s detached waiter thread reaps the
  shell between `recv_timeout` returning and the kill running.

**Residual cost:** the detached reader thread stays alive until the
backgrounded grandchild closes its inherited pipe end (i.e. until it
exits). Repeating long-lived background commands can therefore accumulate
native threads for as long as each grandchild survives. This is a bounded
leak -- the thread exits when its target exits, no data captured up to
the timeout is lost, and on Windows the Job Object prevents the scenario
entirely -- and closing it would require platform-specific pipe-
interruption primitives that are not currently worth the added
complexity.

**Possible future strategies** (none currently implemented because the
tradeoff isn't obviously worth the complexity):
- Linux-only: use `pidfd_open` for a stable process handle and
  `pidfd_send_signal` for race-free kills. Doesn't help macOS, and doesn't
  give us a group handle anyway.
- Install `PR_SET_CHILD_SUBREAPER` on the TermAl process so backgrounded
  grandchildren re-parent to TermAl rather than init, then track them
  explicitly. Linux-only and complicates the whole process model.
- Use cgroups v2 on Linux for a process-group handle that isn't tied to a
  recyclable PID. Requires root or unified cgroups and still Linux-only.
- Unix-only: `dup` the pipe fds before moving them into the reader
  threads, keep the duplicates in the main thread, and `close` them on
  reader-join timeout. This would force the blocking `read` to return
  `EBADF` / `EOF` and let the detached thread unwind immediately,
  eliminating the thread-accumulation residual cost. Adds platform-
  specific code and nontrivial fd plumbing.

### Terminal 429 peek/resolve race drifts local-vs-remote counters

`run_terminal_command` calls `state.terminal_request_is_remote(...)`
under the state lock to decide which permit (local or remote) to
acquire, then drops the lock, acquires the chosen permit, and only later
resolves the full scope via `remote_scope_for_request` inside
`run_blocking_api`. If a caller's `projectId` is local at the peek but
its remote binding flips between the two calls, the local permit is
consumed for a request that then fails deep inside the blocking task --
or vice versa. Both sides fail closed (mismatches return safely as
`ApiError`), but the 429 counters can transiently diverge from what is
actually in flight on each budget.

**Accepted tradeoff.** Closing this race would require snapshotting the
full resolution (scope, not just a boolean) before acquiring the permit,
which means running `ensure_remote_project_binding` -- a blocking
`reqwest::send` on the first-time-bind path -- on the async worker
thread. The round-99 refactor moved that call onto the blocking pool for
async-safety, and reintroducing it on the async worker is a strictly
worse tradeoff than the rare 429-counter asymmetry it would fix. The
race is documented in a large inline comment in `run_terminal_command`
right above the `terminal_request_is_remote` peek so future readers do
not chase the asymmetric counters as a bug.

### Windows `resume_terminal_process_threads` snapshots every system thread

`resume_terminal_process_threads` on Windows calls
`CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)` on every terminal
command, which enumerates every thread on the entire system (typically
2-5k on a dev workstation, more on busy servers). The function then
iterates to find the subset belonging to the child process.

**Accepted tradeoff.** The `TH32CS_SNAPTHREAD` snapshot kind does not
accept a process-id filter (the `pid` parameter is only honored by the
module snapshot kinds), so there is no cheap way to narrow the snapshot
at the Win32 API layer. Capturing the primary thread handle directly
from `CreateProcess` via `PROCESS_INFORMATION.hThread` and calling
`ResumeThread` on just that one handle would work, but it requires
bypassing `std::process::Child`'s encapsulation -- either with a
crate-level extension trait or a direct `CreateProcess` call that
mirrors stdlib's stdio plumbing. That is a substantially larger
refactor than the roughly 10 microseconds the snapshot costs in practice,
so the current implementation is left as-is with a prominent comment
documenting the reason.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
