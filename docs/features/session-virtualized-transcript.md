# Feature Brief: Session Virtualized Transcript

This document describes how the session transcript virtualizer works today.

Primary implementation:

- `ui/src/panels/VirtualizedConversationMessageList.tsx`

Supporting owners:

- `ui/src/panels/conversation-virtualization.ts`
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
- `ACTIVE_MOUNTED_EXTRA_PAGES_BELOW = 1`

So active reading keeps several viewports of real DOM around the visible area
instead of waiting until the user is already on a band edge.

### Active scroll

During active user scroll, mounted-range updates are grow-oriented:

- incremental upward scroll can grow the start of the mounted band
- incremental downward scroll can grow the end of the mounted band
- the opposite side is not trimmed during the gesture

This avoids exposing spacer space during normal reading.

### Idle compaction

Mounted-range compaction is deferred until scroll idle.

- cooldown constant: `USER_SCROLL_ADJUSTMENT_COOLDOWN_MS`
- current value: `200`

Once input settles, the mounted band is allowed to shrink back toward
`workingMountedPageRange`.

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

### Keyboard `PgUp` / `PgDown`

Session transcript page navigation is custom.

Ownership split:

- `SessionPaneView.tsx` intercepts `PageUp` / `PageDown`
- it applies a fixed `scrollTop` delta itself
- it emits `MESSAGE_STACK_SCROLL_WRITE_EVENT` with optional explicit
  `scrollKind` metadata so the virtualizer can classify the write correctly

The jump is a fixed fraction of the viewport height:

- `SESSION_PAGE_JUMP_VIEWPORT_FACTOR`
- current value: `0.45`

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

For prompt send specifically:

- if the pane is already near bottom, `SessionPaneView` keeps the lightweight
  smooth follow
- otherwise it uses the stronger settled jump-to-bottom path

That split keeps prompt send visually pleasant when already pinned, but still
reliable when the pane is away from bottom.

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
