# Session Flicker / Scroll Instability

This document tracks the long-running session flicker / scroll-instability
problem in the virtualized transcript view.

The issue has improved over time, but it is not resolved. Keep this file as the
dedicated working record for reproduction notes, root-cause findings, and
fix-specific acceptance criteria. Do not close the issue based on partial
improvements or narrower test coverage.

## Latest fixed finding: first `PageUp` from the bottom

The reproducible first-`PageUp` flicker from the bottom was caused by a heavy
assistant markdown card changing component shape at scroll start. While idle at
the bottom, assistant text could render as eager `MarkdownContent`; the first
manual scroll then disabled the immediate-heavy preference and remounted the
same message through `DeferredHeavyContent`, temporarily replacing measured
content with a placeholder. That changed the mounted page height during the
native page scroll.

The current fix keeps non-streaming assistant markdown mounted through the
deferred wrapper and passes the immediate-render preference into that wrapper
instead of switching component types. The virtualized stack also suspends
deferred heavy activation during direct user-scroll / page-jump cooldowns with
`data-deferred-render-suspended="true"`, then dispatches
`termal:deferred-render-resume` after the cooldown so near-viewport heavy blocks
activate only after scroll geometry has settled.

## Latest fixed finding: upward wheel can expose the top spacer

Mouse-wheel scroll upward could briefly reveal a large blank spacer when a
large delta moved the viewport into virtual space before the native `scroll`
event gave React a chance to prepend the next page band. The downward path
already had an actual-DOM coverage guard for compact pages that exposed the
bottom spacer; the upward path now has the symmetric protection:

- wheel input projects the upward target and prewarms pages above the mounted
  band before the parent-owned scroll write paints
- `SessionPaneView` marks parent-owned wheel scroll writes as `incremental`, so
  large wheel deltas do not get reclassified as seek jumps and trim the
  prewarmed band during the same gesture
- coverage calculations cap stale estimated page heights with the smallest
  currently rendered page height, allowing compact pages to prepend enough DOM
  before the viewport reaches the spacer
- during active-scroll cooldown, the virtualizer checks the first mounted page's
  real DOM top and prepends pages if it has fallen below the viewport top
- prepends still use the existing scroll-height restore so replacing estimated
  spacer space with real page DOM does not steal the user's wheel progress

## Current user-visible symptoms

### 1. `PageUp` scroll can degrade from page-sized movement into tiny steps

Observed behavior:

- The first `PageUp` starts as a large smooth move, roughly one viewport.
- Then the movement can degrade into very small increments, effectively
  "line-by-line" behavior.
- In bad cases the viewport appears to get trapped bouncing between two
  positions:
  - offset A
  - offset B
  - A -> B -> A -> B ...
- Visually this reads as flicker, even though the root problem is unstable
  repeated scroll-offset correction.

### 2. Mouse-wheel scroll can jump once after a discrete move

Observed behavior:

- Wheel scrolling is not the same failure mode as `PageUp`.
- Instead of a repeated visible loop, it usually performs a discrete move and
  then applies one correction jump.

This likely means the same unstable geometry / anchor logic is involved, but the
browser input shape differs:

- wheel: discrete input, then one correction
- `PageUp`: smooth animation, leaving more time for repeated correction

### 3. Visible overlap / jumbling of conversation cards during scroll

Observed behavior:

- During scroll through long sessions, some rows visibly overlap for a frame.
- This strongly suggests that visible rows are first laid out using estimates
  and then remeasured at a different real height.
- The overlap is not just cosmetic; it is likely the geometry source feeding
  the unstable scroll corrections.

### 4. `flushSync` warning appears in the console during the affected paths

Observed warning:

- `Warning: flushSync was called from inside a lifecycle method. React cannot flush when React is already rendering.`

This does not prove root cause by itself, but it confirms that the viewport
sync path is currently trying to force synchronous React updates from a timing
context that React considers unsafe.

## Confirmed architecture involved

The current repros touch all of the following:

- `ui/src/SessionPaneView.tsx`
  - page scroll command handling
  - sticky-bottom state
  - settled scroll-to-bottom loop
  - custom `notifyMessageStackScrollWrite()` dispatches

- `ui/src/panels/VirtualizedConversationMessageList.tsx`
  - estimated-height virtualized layout
  - per-row `ResizeObserver` measurement
  - anchor-preserving `scrollTop` rewrites
  - viewport sync path using `flushSync`
  - direct-user-scroll cooldown tracking

- `ui/src/panels/conversation-virtualization.ts`
  - `getAdjustedVirtualizedScrollTopForHeightChange`
  - estimated vs measured height math

- `ui/src/message-cards.tsx`
  - heavy content rendering paths that can affect final row height

## Things that are likely true

### 1. Visible rows are changing height after they enter the viewport

This is the strongest current signal.

The overlap screenshots are consistent with:

1. a row enters view with estimated height
2. actual rendered height differs
3. later rows are temporarily painted at stale `top` positions
4. the virtualizer then corrects layout and/or `scrollTop`

### 2. `PageUp` smooth scrolling gives the bug time to happen repeatedly

If the browser is animating toward a smooth-scroll target while the app is also
recomputing row layout and correcting `scrollTop`, the two systems can fight for
multiple frames.

This cleanly fits the observed difference between:

- wheel: one move + one correction
- `PageUp`: one large move, then repeated tiny corrections / A-B oscillation

### 3. The viewport sync path is too aggressive

The current architecture uses a custom scroll-write event and, in some async
paths, forces a flushed viewport update. The `flushSync` lifecycle warning means
that at least some of those dispatches are landing inside React layout work.

Even if this is not the original cause, it increases the chance of visible
instability.

### 4. Keyboard paging intent is not cleanly unified with the virtualizer's
manual-scroll protection

`PageUp` is handled in `SessionPaneView`, while the virtualized list separately
tracks direct user scroll intent. If those two systems do not share the same
source of truth, the measurement / anchor logic can keep "helping" while the
user is still manually paging.

## Things that are NOT sufficient explanations by themselves

### 1. Older-message prepend is not required for the current repro

The current bad `PageUp` behavior does not require loading older messages at the
top. That path may still have issues, but it is not needed to explain the main
reported failure.

### 2. Intersection-driven deferred placeholder activation inside the
virtualized list is not the primary culprit

The virtualized transcript already short-circuits deferred-heavy activation for
cards inside `.virtualized-message-list`, so the older main repro was not simply
"placeholder swapped to real content later" by IntersectionObserver activation.
The fixed first-`PageUp` repro was a different placeholder path: assistant
markdown switched from eager render to the deferred wrapper when scroll state
changed.

The more plausible source is:

- estimate -> measured height
- syntax / markdown / image / iframe stabilization
- repeated row resize after mount

## Working hypotheses

These are the active hypotheses worth validating in order:

### Hypothesis A: smooth `PageUp` animation is fighting measurement-driven
anchor correction

Expected failure shape:

1. `PageUp` starts a smooth native scroll
2. newly visible rows measure to different heights
3. anchor-preserving code rewrites `scrollTop`
4. browser animation continues toward its original smooth-scroll target
5. app corrects again
6. viewport oscillates between two nearby offsets

### Hypothesis B: the visible-window measurement path can still mutate row
geometry repeatedly after first paint

Expected failure shape:

1. rows are mounted with estimated height
2. measured height changes once or more
3. visible rows overlap for a frame
4. `scrollTop` correction runs against changing geometry

### Hypothesis C: custom programmatic scroll sync is firing during React layout
work and amplifying instability

Expected failure shape:

1. programmatic scroll write dispatches custom event
2. listener triggers flushed viewport sync
3. React warns because the flush lands during lifecycle/layout work
4. extra synchronous commits increase visible flicker

### Hypothesis D: manual paging does not reliably arm the same
"user is actively scrolling" guard used for wheel scrolling

Expected failure shape:

1. user presses `PageUp`
2. pane-level handler scrolls transcript
3. virtualizer does not fully treat that as direct manual scroll
4. geometry-correction logic remains active during the page motion

### Hypothesis E: top-edge height correction has no hysteresis once a tall row
has just crossed above the viewport

Expected failure shape:

1. a tall row's bottom clears the viewport top by only a few pixels
2. a late measurement lands for that row
3. `getAdjustedVirtualizedScrollTopForHeightChange()` treats it as fully above
   the viewport and adds the full height delta to `scrollTop`
4. the viewport jumps forward right as the user passes the row

This matches the current repro better than an index-selection bug. The likely
fault line is not "wrong item" but a boundary-condition decision made with stale
or too-sharp fold math.

### Hypothesis F: successive `ResizeObserver` callbacks are using stale
committed prefix tops

Expected failure shape:

1. row A changes height and mutates the measured-height ref immediately
2. before React commits the rebuilt layout, row B reports a height change too
3. anchor math for row B still reads the last committed `layout.tops`
4. row B's correction uses stale prefix sums even though live measured heights
   above it have already changed
5. viewport jumps or oscillates because the scroll write is computed from mixed
   generations of geometry

This is distinct from the top-edge hysteresis issue. The failure is not that
the wrong row index is chosen; it is that the right row is evaluated against an
out-of-date committed top.

## Reproduction notes to preserve

Keep these as the canonical repros unless a better one replaces them:

### Repro 1: long conversation, `PageUp`

1. Open a long session with many mixed message types.
2. Start near the bottom.
3. Press `PageUp`.
4. Observe:
   - first move is roughly a page
   - subsequent movement can degrade into tiny steps
   - viewport may oscillate between two positions

### Repro 2: long conversation, wheel scroll

1. Open the same kind of long session.
2. Wheel upward near the bottom / through heavy content.
3. Observe:
   - one discrete manual move
   - then one correction jump

### Repro 3: overlapping rows while scrolling

1. Scroll through a long session containing heavy / tall content.
2. Watch row boundaries closely.
3. Observe:
   - overlapping text/cards for one frame
   - then layout correction

### Repro 4: pass a tall item while scrolling upward

1. Open a long session containing a very tall message/card.
2. Scroll upward until that item's bottom has only just crossed above the
   viewport top.
3. Continue the same gesture.
4. Observe:
   - viewport suddenly jumps ahead
   - jump happens at the fold-crossing moment, not randomly later
   - behavior suggests late height correction near the viewport boundary

## Acceptance criteria for a real fix

Do not mark this resolved until all of the following are true:

1. `PageUp` moves by a stable page amount throughout the full transcript.
2. `PageUp` never degrades into line-by-line stepping.
3. No visible A-B offset oscillation occurs during paging.
4. Wheel scroll no longer performs a post-move correction jump.
5. Conversation cards do not overlap during normal scrolling.
6. No `flushSync` lifecycle warning appears in the console for session scroll
   paths.
7. Fix holds across:
   - plain text sessions
   - Markdown-heavy sessions
   - Mermaid / rich-render sessions
   - long command / diff output sessions

## Suggested next debugging work

Before changing behavior again, add instrumentation for:

1. every `scrollTop` write source
   - native scroll
   - `PageUp` / `PageDown`
   - sticky-bottom settle loop
   - measurement-based anchor correction
   - search-hit scroll

2. row height transitions
   - estimated height
   - first measured height
   - subsequent measured height changes

3. viewport oscillation detection
   - detect repeated A-B-A-B writes within a small time window

4. `flushSync` call provenance
   - identify exactly which programmatic write paths hit the warning

## Rule for future fixes

Do not accept fixes that only improve one symptom while leaving the others
untested. This bug has already had multiple partial improvements. The remaining
issue is systemic and needs end-to-end verification, not another narrow local
patch.
