# Feature Brief: Session Virtualized Transcript

This document describes how the session transcript virtualizer works today.

Primary implementation:

- `ui/src/panels/VirtualizedConversationMessageList.tsx`

Supporting owners:

- `ui/src/panels/conversation-virtualization.ts`
- `ui/src/panels/AgentSessionPanel.tsx` (deferral + the pending-prompt queue)
- `ui/src/SessionPaneView.tsx`
- `ui/src/message-stack-scroll-sync.ts`
- `ui/src/message-cards.tsx`
- `ui/src/ExpandedPromptPanel.tsx`

## Purpose

The session transcript can contain long conversations, large command output,
heavy Markdown, diffs, and expanded prompts. Rendering the entire transcript is
too expensive, but active reading still needs to feel like normal browser
scrolling through real DOM.

The current model is:

1. **Mounted pages** are real DOM and own the live reading surface.
2. **Unseen pages** are represented by top and bottom spacers.
3. **Measured page heights** refine spacer geometry, but mounted DOM remains
   authoritative while the user is actively reading.

## Network History Paging

Large transcripts are not returned to the browser as one unbounded session
document. Initial hydration fetches the newest 20-message tail. When the reader
approaches the top, the UI requests an ascending page of at most 64 older
messages from:

```text
GET /api/sessions/{id}/history?before={exclusiveMessageId}&limit=64
```

The first retained message id is the stable, exclusive backwards cursor. Each
response supplies `nextBefore` and `hasMore`; `messagesLoaded` becomes true only
when the resident window spans both the true beginning and the live transcript
tail. Reaching only one boundary leaves it false. `GET /api/sessions/{id}` is
also bounded; it defaults to the recent 20-message tail and has no unbounded
response branch.

Boundary navigation is bounded too. Jump-to-start uses exactly one page:

```text
GET /api/sessions/{id}/history?from=start&limit=64
```

That replaces the resident tail with the true first page. Subsequent downward
reading uses an exclusive forward cursor:

```text
GET /api/sessions/{id}/history?after={exclusiveMessageId}&limit=64
```

Position-targeted navigation also uses exactly one centered page:

```text
GET /api/sessions/{id}/history?around={globalMessagePosition}&limit=64
```

The response carries `messageStartIndex`, so the browser can replace its
resident window without reconstructing a global position from message ids or
walking intervening pages.

Tail growth during the start-page request is not a merge conflict. The first
page replaces transcript residency while current session metadata, counts, and
the newer-history state remain authoritative. If the request still cannot be
adopted (for example, replacement server instance or protocol failure), the
keypress falls back to the top of the resident window instead of disappearing
silently.

After jump-to-start, the resident messages are a historical window, not the
live tail. The pane keeps `hasNewerHistory` explicit, does not render live-only
activity or pending-prompt cards below stale history, and shows a persistent
**Jump to latest** affordance. Jump-to-bottom replaces the historical window
with exactly one bounded latest page:

```text
GET /api/sessions/{id}/history?limit=64
```

Only after that page is adopted may bottom-follow resume. Reaching the bottom
of the currently resident historical window is not equivalent to reaching the
live transcript tail.

Prompt submission is an explicit UI event. The pane may reattach a historical
window for that event, but it must never reconstruct a prompt-send event from
resident transcript data such as the last visible author. Replacing residency
can legitimately change that author without any new prompt having been sent.

Boundary scrolling and search never recursively request pages merely because
more history exists. Marker, prompt, and overview-rail jumps use their durable
global message position to request one centered page. They do not walk
intervening pages and must never revive a full-transcript hydration branch.

Older pages are prepended with message-id deduplication. The transcript
virtualizer is the only owner of scroll anchoring while the page is inserted
and measured; the history-loading hook does not write `scrollTop`. This avoids
two independent compensation layers moving the visible messages after a page
arrives. A prepend or trim changes the resident conversation signature, but it
does not arm **New response** or **New activity**: those affordances describe
actual live-tail or pending-prompt advancement, not history loaded because the
reader scrolled away from the bottom. While the resident window still includes
the live tail, live SSE
appends remain visible during an older-page request. After explicit
jump-to-start replaces the tail with a historical window, live state continues
through session metadata, while live-only transcript cards remain hidden until
the reader explicitly jumps back to the bounded latest page. A missing cursor
or replacement `serverInstanceId` discards the page and requests an
authoritative state/tail resync.

Remote-proxy tail and history reads are freshness-sensitive. A proxy never
returns cached summary metadata as a successful transcript response when its
owner is unreachable, because that would make stale or absent transcript bytes
look authoritative. Transport failures surface to the caller. Delayed remote
REST responses are admitted only when their session count/mutation metadata is
still compatible with the proxy state synchronized by SSE; incompatible pages
return a conflict and are retried from current metadata.

Search operates on the resident window and labels its results as loaded-only
whenever older or newer pages are absent, so a zero-result resident search is
never presented as a whole-transcript result. Marker CRUD resolves anchors
against the indexed durable transcript, not just the resident tail. Marker
navigation then requests one centered bounded page as described above. The
virtualizer below still limits mounted DOM after multiple user-requested
network pages have accumulated.

Loaded pages currently remain resident for the active browser session. Network
responses and JSON parsing are bounded, but total JavaScript heap is not yet
bounded after a reader walks the whole transcript. The indexed-message and
bidirectional-window work in the SQLite storage plan must add page eviction and
re-fetch; the current implementation must not be described as complete
bounded-memory transcript storage.

### Transcript residency is not application state

`session.messages` is a movable, bounded transcript window. Features must not
rediscover durable or live state by scanning whichever messages happen to be
resident. State needed continuously—current prompt, current agent shell
command, pending prompts, counts, first-message identity—belongs in explicit
session fields or a deliberately bounded endpoint.

Recent-window rendering logic may inspect resident messages. Whole-transcript
features such as prompt recall or global search need their own indexed query;
they must never revive a full-hydration branch or silently treat the resident
window as the complete transcript.

## Whole-conversation overview rail

The overview rail is a bounded whole-conversation feature, not a virtualizer
layout projection. On pane activation the browser makes one request:

```text
GET /api/sessions/{id}/overview?buckets=200
```

`buckets` accepts `1..=512`. The response partitions stable global message
positions into equal ranges and returns each range's message count, dominant
semantic kind, user-authored count, and marker-presence flag, plus marker
positions and current transcript freshness metadata.

The backend computes the same map whether the full transcript or only the
bounded retained tail is resident. Persisted messages carry compact
kind/author metadata in a transactionally maintained one-byte-per-message
session blob; the endpoint therefore reads one small row rather than parsing or
hydrating message bodies. The repeated JSON bucket contract is served with HTTP
compression.

Buckets, markers, click targets, and the viewport indicator all use global
message position as their only coordinate system. The indicator interpolates
the visible interval from `messageStartIndex`, resident message count, and
scroll fraction. A click maps directly to a global position and then to the
single `around=` history request above.

The exact viewport range remains position-linear, while a 24-pixel outlined
handle centered on that range keeps the current location visible even when the
honest range would otherwise project to only one or two pixels.

The rail deliberately has no dependency on virtualizer layout snapshots,
measured or estimated pixel heights, focus state, mounted pages, or transcript
residency. Pixel measurement remains solely in the transcript virtualizer.
The overview refetches when `messageCount` or `sessionMutationStamp` changes.

## Core Model

### Pages

Messages are grouped into fixed-size pages.

- Constant: `VIRTUALIZED_MESSAGES_PER_PAGE`
- Current value: `8`
- Builder: `buildMessagePages(...)`

Each page stores:

- page index
- `[startIndex, endIndex)` message range
- page messages
- whether a trailing inter-page gap should be included

The virtualizer reasons about whole pages as the mounted unit.

### Page layout

Each page has a height:

- measured height, if the page has already rendered and reported one
- estimated height, otherwise

`buildPageLayout(...)` converts page heights into:

- `tops[]` - page start offsets
- `totalHeight` - virtual document height

That layout is used to:

- find the visible page range
- compute top and bottom spacers
- derive fallback search positioning for messages outside the mounted band

## Mounted Range Policy

### Working range

The steady-state mounted target is `workingMountedPageRange`.

It is computed from:

- current `scrollTop`
- viewport height
- a mounted reserve above the viewport
- a mounted reserve below the viewport
- one extra page below as bottom-edge hysteresis

Current reserves:

- `ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 3`
- `ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS = 3`
- `ACTIVE_MOUNTED_EXTRA_PAGES_BELOW = 2`

So active reading keeps several viewports of real DOM around the visible area
instead of waiting until the user is already on a band edge.

### Active scroll

During active user scroll, mounted-range updates are grow-oriented:

- incremental upward scroll can grow the start of the mounted band
- incremental downward scroll can grow the end of the mounted band
- the opposite side is not trimmed during the gesture

This avoids exposing spacer space during normal reading.

Large upward wheel deltas are prewarmed before the scroll write paints: the
virtualizer projects the wheel target and grows the mounted band above when that
target would otherwise land in the top spacer. `SessionPaneView` tags its
parent-owned wheel scroll writes as `incremental`, so a large wheel delta is not
misclassified as a seek and trimmed back while the gesture is still active.

The edge-growth math uses actual rendered page coverage as a cap on stale page
height estimates. That lets compact command-heavy pages prepend multiple bands
in one frame when their stored estimates are still too tall. A layout guard also
checks actual mounted DOM bounds during scroll cooldown and prepends pages if
the first mounted page has fallen below the viewport top. This mirrors the
existing bottom-edge guard for compact pages that shrink below their estimates.

### Idle compaction

Mounted-range compaction is deferred until scroll idle.

- cooldown constant: `USER_SCROLL_ADJUSTMENT_COOLDOWN_MS`
- current value: `200`

Once input settles, the mounted band is allowed to shrink back toward
`workingMountedPageRange`.

## Deferral and the Live Tail

`SessionConversationPage` in `ui/src/panels/AgentSessionPanel.tsx` decides *what*
the virtualizer renders, and it is the layer that keeps an actively streaming turn
responsive. It sits above the paging model that the rest of this document describes.

### Message bulk is deferred; the tail is not

The transcript body flows through `useDeferredValue(session.messages)`. During a
live turn the assistant streams tokens continuously, and re-rendering the whole
(virtualized, often heavy-Markdown) transcript at high priority on every tick would
starve interaction. Deferral keeps the previously rendered transcript on screen
while React prepares the new one at low priority.

Pure deferral would make streaming itself invisible, so the newest messages are
always spliced back in undeferred:

```
baseVisibleMessages = includeUndeferredMessageTail(deferredMessages, session.messages)
```

The bulk history lags under load; the live tail is always current. That is the
whole trick — defer the expensive history, never the part the user is watching.

### Pending prompts are the live tail — never defer them

Queued follow-ups (`session.pendingPrompts`) render pinned to the live turn through
`PendingPromptCard`. They are read from the **immediate** `session.pendingPrompts`,
never from a deferred copy.

This is a load-bearing rule, not an optimization. The queue is tiny and changes
only when a prompt is queued or dequeued — never per streamed token — so there is
nothing to defer for. But if it *is* deferred (a `useDeferredValue(pendingPrompts)`),
the continuous `session.messages` stream starves that low-priority update: it never
commits until the stream stops, so a queued prompt stays invisible during the exact
turn it was queued behind and only appears once that turn is stopped. That was a
shipped regression (introduced by a "responsiveness" refactor that deferred both
lists); see invariant 7. Note that `act()` in tests flushes deferred values
synchronously, so unit tests cannot reproduce this starvation — it only appears
under real continuous streaming.

## Render Flow

The render output is:

1. top spacer
2. mounted pages
3. bottom spacer

Only pages inside `mountedPageRange` are rendered as message cards.

Mounted pages are wrapped in `MeasuredPageBand`, which reports the full
rendered page height back to the virtualizer.

## Measurement

### Page measurement

Each mounted page is measured as a whole.

The measured height includes:

- slot heights
- in-page message gaps
- the trailing inter-page gap when the page is not the last one

Measurements are stored in `pageHeightsRef`.

### Heavy content inside mounted pages

Mounted pages always render heavy content immediately.

That includes:

- highlighted code
- heavy Markdown subtrees
- expanded prompts

Inside the mounted band, placeholder-to-real-content transitions are not
desirable because they change page height after the page is already part of
active reading.

## Scroll Behavior

### Native wheel / touch scroll

Normal wheel and touch movement are treated as incremental reading.

The browser owns the visible motion; the virtualizer reacts by growing the
mounted band and updating spacer geometry. It should not continuously rewrite
the live scroll position during ordinary reading.

### Native-scroll ownership lease

The pane and virtualizer classify each native scroll frame through one
node-scoped lease in `message-stack-scroll-sync.ts`. Its owners and lifetimes
are deliberately bounded:

- wheel: 120 ms
- touch: the 1200 ms bottom-follow window, including post-`touchend` inertia
- pointer/scrollbar thumb: 5 s, released earlier by pointer up/cancel,
  `lostpointercapture`, or window blur
- focus: 400 ms, and only when focus enters a control outside the visible band
- browser-owned keyboard motion (currently Space): the 1200 ms bottom-follow
  window, so a long page-sized native animation keeps its landing authority

A movement-capable input claims or replaces the lease. A same-burst boundary
tick that cannot move the viewport does not clear a valid landing lease.
Every consumer may peek at the same lease without mutating it. Only the
virtualizer's native listener, which owns the per-frame native delta, may revoke
a lease whose declared direction conflicts with that delta. Ordinary React
listener re-registration never clears the lease; true node detach or unmount
does.

The capture-phase wheel arbiter records rejected residual `WheelEvent` objects
in a `WeakSet`, so the later node and React listeners make the same no-authority
decision. The normalized delta and layout-sensitive nested-scroller verdict are
also cached on that native event, avoiding repeated ancestor/style walks across
capture, bubble, React, and virtualizer listeners. A decaying opposite wheel
tail may be suppressed briefly after upward keyboard navigation; a later tick
whose magnitude increases is treated as a deliberate reversal and immediately
takes authority. A separate one-shot virtualizer-position marker identifies an
exact anchor correction. It owns only the first native frame after the write
and is cleared by a newer user-scroll generation, so it cannot be replayed by a
later reader movement. Listener order is therefore: pane capture arbitration,
native node observers, then React root-delegated handlers and normalized user
intent.

Prelude-less native reader movement (for example a thumb drag, touch inertia,
or browser navigation) advances the shared user-scroll generation. A prepend
restore may suppress exactly one matching geometry tick using a generation- and
geometry-bound token; the token expires on the next input or native tick.
Finally, a detached viewport is rewound from the physical bottom only when a
still-live canceled bottom-follow or superseded wheel token identifies that
late frame. Mere absence of a native owner is not enough. Both pane and
virtualizer reattach a detached reader only after owned forward movement lands
at the exact physical bottom; entering the wider sticky-bottom band never
manufactures bottom authority. A viewport-immobile downward boundary input
also preserves any pending prepend generation/token because it did not move the
reader.

### Keyboard `PgUp` / `PgDown`

Session transcript page navigation is custom.

Ownership split:

- `SessionPaneView` is the single keyboard-intent producer. It classifies the
  real key once, including keys that target `document.body` while the browser's
  active scroller remains the transcript.
- `session-pane-body-keyboard-ownership.ts` tracks which mounted pane owns those
  body-targeted scroll keys without granting ownership to the composer/dialogs.
- Before native motion, the pane emits the node-scoped, non-bubbling
  `MESSAGE_STACK_USER_SCROLL_INTENT_EVENT`. The virtualizer consumes it when
  `viewportCanMove` is true. An immovable upward intent may also carry
  `detachFromBottomAtBoundary` when older history will hydrate, preventing the
  prepend measurement from replaying stale bottom authority. History demand
  consumes boundary intent and defers any page request to a microtask so every
  synchronous authority listener observes the gesture first.
- `SessionPaneView.scroll.ts` applies deterministic `PageUp` / `PageDown` deltas and
  emits `MESSAGE_STACK_SCROLL_WRITE_EVENT` with `scrollSource: "user"` plus
  explicit `scrollKind` metadata. The virtualizer has no independent keydown
  listener; adding a second producer would double-detach and double-request.
- Plain `ArrowUp` / `ArrowDown` use the same app-owned write path with one
  immediate 40 px step. The pane prevents Blink's native multi-frame keyboard
  animation so page measurement cannot repin between animation frames.
- Pane boundary commands (`Home` / `End`, macOS `Command+ArrowUp/ArrowDown`,
  and Windows/Linux control-key shortcuts) own
  their single bounded start/tail request and publish the user-owned seek write,
  rather than also publishing ordinary normalized pagination intent.
- Shift selection-extension chords inside the transcript—including
  `Shift+PageUp/PageDown` and Ctrl/Cmd+Shift boundary variants—stay
  browser-owned, whether focus is on transcript content or `document.body`;
  the pane neither prevents them nor publishes scroll authority. Composer
  shortcuts remain pane-owned because the composer is outside the message
  stack.

`Home` requests the bounded start page whenever
`resolveHasOlderSessionHistory` sees `hasOlderHistory === true` in the current
window metadata. Missing availability does not authorize pagination; neither
hydration status nor a difference between total and resident message counts is
a substitute. Newer-page demand likewise uses explicit `hasNewerHistory`.
The pane gives up tail-follow immediately, then applies the
top seek after that request settles so stale restore work cannot win the write.
The completion is guarded by a pane-local navigation generation both before it
schedules and inside its animation frame. Any later manual transcript gesture
or opposite boundary command invalidates the older completion, so out-of-order
start/tail responses cannot overwrite the reader's newer viewport intent.

The jump is a fixed fraction of the viewport height:

- `SESSION_PAGE_SCROLL_VIEWPORT_FACTOR`
- current value: `0.85` (with a minimum 160 px jump)

This avoids browser-defined page-jump behavior and keeps keyboard page
navigation closer to the wheel-scroll model.

### Search

When session search activates a message:

- if the target message is mounted, scroll targets the real DOM slot
- otherwise the virtualizer falls back to an estimated target from page layout

### Bottom follow

There are two bottom-follow policies in the current system:

1. **Virtualizer bottom pin**
   - used when the transcript is already near bottom
   - keeps the viewport pinned as page heights settle

2. **Pane-level jump to latest**
   - owned by `SessionPaneView.tsx`
   - used for explicit jump-to-bottom and some prompt-send cases

For prompt send and pinned assistant updates:

- if the pane is already near bottom, `SessionPaneView` keeps the lightweight
  smooth follow with the `bottom_follow` scroll-write kind
- otherwise it uses the stronger settled jump-to-bottom path

That split keeps active bottom-follow visually pleasant when already pinned,
but still reliable when the pane is away from bottom.

## Important Refs And State

Important refs:

- `pageHeightsRef`
- `shouldKeepBottomAfterLayoutRef`
- `isDetachedFromBottomRef`
- `skipNextMountedPrependRestoreRef`
- `lastUserScrollInputTimeRef`
- `lastUserScrollKindRef`
- `pendingMountedPrependRestoreRef`

Important state:

- `viewport`
- `layoutVersion`
- `scrollIdleVersion`
- `mountedPageRange`
- `isMeasuringPostActivation`

## Invariants

These rules should remain true:

1. Mounted transcript pages are the live reading surface.
2. Spacer math is allowed to describe unseen space, not replace mounted reading.
3. Active reading is grow-first; trimming belongs to idle.
4. Keyboard page jumps are deterministic and owned by the transcript, not the
   browser default page-scroll path.
5. Heavy content inside mounted pages should render directly.
6. Bottom-follow logic must stop immediately once the user explicitly scrolls
   away from the latest content.
7. Pending prompts are part of the live tail and must render from the immediate
   `session.pendingPrompts` whenever the resident window is the live tail. Never
   wrap the pending-prompt queue in `useDeferredValue` — the continuous message
   stream starves the deferred update and queued prompts vanish until the turn
   stops. When `hasNewerHistory` is true, hide live-only cards and show **Jump to
   latest** instead of splicing those cards below stale history.
8. The bottom of a historical window is not the live tail. Reattachment requires
   one bounded latest-page request before bottom-follow can resume.
9. Bottom-pin authority is resolved when a scroll write executes, not when an
   anchor or saved position is captured. Once bottom-follow is active, delayed
   restores must target the current real DOM bottom; they may not replay a
   previously recorded `scrollTop`.
10. Resident-prefix reveal or trimming never counts as a new response. The
    bottom indicator is armed only by live-tail content or pending-prompt
    advancement while detached.
11. Keyboard scroll intent has one producer (`SessionPaneView`). All consumers
    listen on the exact scroll node; virtualizer authority changes synchronously,
    while history page requests wait until the current event dispatch completes.

## Known Limitations

### Upward reading is still the sensitive path

The path that still deserves the most scrutiny is:

1. go to the bottom
2. `PgUp`
3. continue reading upward through a long conversation

The current implementation is much more stable than earlier revisions, but
upward prepend remains more sensitive than downward append.

### Unseen space is still estimated

Pages outside the mounted band still rely on estimated heights.

Those estimates affect:

- spacer sizes
- virtual total height
- search fallback positioning
- initial mounted-range decisions

That is acceptable for unseen content, but it is still the main approximation
in the system.

### Page identity is index-based

Page keys still include page start/end indices plus message ids.

That is workable, but insertions ahead of a page can still invalidate
downstream page identity more aggressively than a purely stable boundary key.

## Cleanup Candidates

These are the parts worth simplifying next.

1. **Bottom-follow ownership**
   - bottom behavior is split between the virtualizer and `SessionPaneView`
   - that split is currently intentional, but still more complex than ideal

2. **Mounted-range policy naming**
   - `visiblePageRange`
   - `workingMountedPageRange`
   - `mountedPageRange`
   are the right three concepts, but deserve short inline comments near the
   declarations because they are easy to conflate when editing the file

3. **Page identity**
   - current page keys are pragmatic, not ideal
   - a more stable page identity would make measurement retention easier to
     reason about

4. **`SessionPaneView` transcript scroll policy**
   - page jumps, prompt-send follow, sticky-bottom, and settled bottom restore
     all live there
   - the behavior is correct enough today, but the ownership surface is broad

## Possible Improvements

1. **Unify bottom-follow strategy**
   - either keep the current split but document it inline more aggressively
   - or move more of the transcript-specific follow policy behind one owner

2. **Stabilize page identity**
   - reduce unnecessary page-height invalidation after insertions

3. **Separate upward and downward range policies more explicitly**
   - upward prepend remains more fragile than downward append
   - separate helpers would make that asymmetry easier to maintain

4. **Browser-level regression coverage**
   - bottom -> first `PgUp`
   - repeated `PgUp`
   - long upward wheel read-through
   - long top-to-bottom downward read-through
   - prompt send while near bottom vs far from bottom
