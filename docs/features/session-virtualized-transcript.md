# Feature Brief: Session Virtualized Transcript

This document describes how the session transcript virtualizer works today.

Primary implementation:

- `ui/src/panels/VirtualizedConversationMessageList.tsx`

Supporting helpers:

- `ui/src/panels/conversation-virtualization.ts`
- `ui/src/message-cards.tsx`
- `ui/src/ExpandedPromptPanel.tsx`

## Purpose

The session transcript can contain long conversations, large command output,
heavy Markdown, diffs, and expanded prompts. Rendering the full conversation all
at once is too expensive, but the active scroll experience still needs to feel
like normal browser scrolling through real DOM.

The virtualizer therefore separates the transcript into:

1. **Mounted pages** — real DOM the browser scrolls through directly.
2. **Unseen space** — top and bottom spacer heights that stand in for pages that
   are not mounted.

The mounted DOM is the authoritative scroll surface. Virtual geometry only
describes content outside the mounted band.

## Core Model

### Pages

Messages are grouped into fixed-size pages.

- Constant: `VIRTUALIZED_MESSAGES_PER_PAGE`
- Builder: `buildMessagePages(...)`

Each page stores:

- page index
- `[startIndex, endIndex)` message range
- page messages
- whether a trailing inter-page gap should be included

The virtualizer measures and reasons about whole pages, not individual rows, as
the primary mounted unit.

### Page layout

Each page has a height:

- measured height, if the page has already rendered and reported one
- estimated height, otherwise

`buildPageLayout(...)` converts page heights into:

- `tops[]` — page start offsets
- `totalHeight` — virtual document height

That layout is used to:

- find the visible page range
- compute top and bottom spacers
- derive fallback scroll targets for search / initial placement when a page is
  not yet mounted

## Render Flow

### 1. Viewport state

The virtualizer tracks:

- `viewport.height`
- `viewport.scrollTop`
- `viewport.width`

This state is synchronized from the actual scroll container.

### 2. Visible page range

The current `visiblePageRange` is computed from:

- page tops
- page heights
- current `scrollTop`
- viewport height

This is the minimal page range that intersects the viewport.

### 3. Mounted page range

The DOM mounts a larger `mountedPageRange` around the visible range.

Mounted range goals:

- keep enough real DOM above the viewport for upward scroll
- keep enough real DOM below the viewport for downward scroll
- avoid frequent prepend/append churn
- still allow the DOM band to shrink back toward the desired range

There are two mounted-range drivers:

1. **Desired range from virtual layout**
   - a buffered range derived from `visiblePageRange`
   - used as the normal steady-state target

2. **DOM-edge growth**
   - checks how close the viewport is to the first/last mounted page in actual
     DOM coordinates
   - grows the mounted band early when the viewport approaches those edges

DOM-edge growth is the preferred path during active manual scroll because it is
based on the real mounted band, not only on estimated layout.

### 4. Spacer rendering

The render output is:

1. top spacer
2. mounted pages
3. bottom spacer

Only pages inside `mountedPageRange` are rendered as message cards.

## Measurement

### Page measurement

Each mounted page is wrapped in `MeasuredPageBand`.

The page band:

- observes itself and its mounted message slots with `ResizeObserver`
- measures the total rendered height of the page
- reports that height back to the virtualizer

The page height includes:

- slot heights
- in-page message gaps
- the trailing inter-page gap when the page is not the last one

### Heavy content inside mounted pages

Mounted pages always render heavy content immediately.

That includes content that would otherwise use deferred placeholders, such as
highlighted code or heavy Markdown subtrees. Inside the mounted transcript band,
placeholder-to-real-content transitions are not desirable because they change
page height after the page is already part of active scrolling.

The rule is:

- if a message is inside the mounted transcript band, render the real content

## Scroll Behavior

### Browser-driven active scroll

While the user is scrolling through mounted content, the browser is expected to
own the visible motion. The virtualizer should not continuously "correct" the
scroll position based on message/page estimates.

### Prepending pages above the viewport

When the mounted range grows upward, the virtualizer:

1. captures the first visible mounted message
2. mounts the new pages above
3. restores that same message to the same viewport offset

This prevents the visible transcript from sliding downward when new DOM is
inserted above the current reading position.

This path uses `pendingMountedRangeAnchorRef`.

### Deferred page-layout corrections

If a page measurement arrives during active manual scroll, the virtualizer does
not immediately apply all layout fallout. Instead it:

1. records the measured page height
2. waits for the direct-scroll cooldown to elapse
3. captures a visible-row anchor
4. applies the deferred layout update
5. restores the anchor

This path uses `pendingDeferredLayoutAnchorRef`.

The intent is to let the user scroll through real mounted DOM first, then let
virtual space catch up once direct input has settled.

### Bottom pin

If the transcript is already near the bottom, page-height changes are allowed to
keep the viewport pinned to the latest content.

If the user scrolls away from the bottom, that pin is cleared by the native
scroll path.

### Search

When session search activates a message:

- if the message is already mounted, scroll targets the real mounted slot
- otherwise the virtualizer falls back to an estimated scroll target derived
  from the page layout

This keeps search responsive even when the target is outside the mounted band.

## State And Refs

Important long-lived refs:

- `pageHeightsRef`
  - measured heights for mounted or previously measured pages
- `pendingMountedRangeAnchorRef`
  - anchor used when prepending mounted pages
- `pendingDeferredLayoutAnchorRef`
  - anchor used when deferred layout updates are applied after scroll idle
- `shouldKeepBottomAfterLayoutRef`
  - sticky-bottom intent
- `lastUserScrollInputTimeRef`
  - used to distinguish active manual scroll from idle layout catch-up

Important state:

- `viewport`
- `layoutVersion`
- `mountedPageRange`
- `isMeasuringPostActivation`

## Invariants

These are the important behavioral rules for future work:

1. Mounted transcript pages are real DOM and define the live reading surface.
2. Spacer math must not become the primary source of truth for what the user is
   currently reading.
3. Upward prepend must preserve a visible-row anchor.
4. Deferred page-height corrections must preserve a visible-row anchor.
5. Heavy content inside mounted pages should not reintroduce placeholder-driven
   height changes.
6. The viewport must never escape the mounted page range for more than a commit;
   if it does, the mounted range must catch up immediately.

## Known Limitations

### Incremental upward reading is the sensitive path

Scrollbar seek / drag behavior is acceptable for the current implementation.
The path that still needs the most scrutiny is incremental upward reading from
the bottom of a long conversation:

- go to the bottom
- press `PgUp`
- continue scrolling upward
- judge whether the transcript remains visually stable

Anchor restoration currently uses an imperative `scrollTop` write. That keeps
the visible row stable, but it can still interrupt the browser's native upward
motion or land at a slightly different offset than expected when new pages are
prepended above the viewport.

### Unseen page geometry is still estimated

The virtualizer still needs estimated heights for pages outside the mounted DOM.
Those estimates do not control the live mounted surface, but they still affect:

- spacer sizes
- virtual total height
- search fallback positioning
- initial page-range decisions

### Fixed page size is a compromise

Small pages increase mount churn. Large pages reduce churn but increase DOM
cost and make each prepend/append more expensive.

The current page size is a pragmatic tradeoff, not a universal optimum.

## Possible Improvements

1. **Momentum-preserving prepend**
   - avoid immediate `scrollTop` anchor writes during native upward momentum
   - likely requires a prepend strategy that keeps the visible DOM stationary
     without directly overriding the browser's current animation

2. **Adaptive page sizing**
   - use smaller pages near very heavy content or larger pages for homogeneous
     lightweight transcripts

3. **Dedicated prepend / append policies**
   - upward growth is more sensitive than downward growth
   - the virtualizer may benefit from separate thresholds and batch sizes for
     prepend vs append

4. **Better instrumentation**
   - log mounted-range changes, visible-range changes, anchor offsets, and page
     measurement deltas in a debug mode
   - useful for validating remaining edge cases without relying only on visual
     repros

5. **Separate incremental reading from random-access seek**
   - treat wheel / `PgUp` / normal upward reading as the highest-fidelity path
   - let scrollbar seek remain a simpler "jump, mount, settle" flow
   - this matches how the transcript is currently evaluated in practice: the
     important quality bar is visual stability while reading upward from the
     bottom, not perfect pixel accuracy during arbitrary scrollbar seeks

6. **Search-target prewarm**
   - optionally mount the target page band before applying the search scroll, so
     search can rely on real DOM more often and on estimated fallback less often

7. **More regression coverage**
   - long upward wheel scroll
   - bottom → `PgUp` → continued upward read-through
   - long upward smooth/page scroll
   - prepend during very tall prompt / Markdown block
   - mounted-range escape recovery
   - momentum interruption detection in browser-level tests
