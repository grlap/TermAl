# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

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



## Search-band range merging mutates mounted-page state during render

**Severity:** High - `mergeRanges(...)` reuses and mutates the first input range object, so an overlapping search band can mutate `mountedPageRange` React state during render.

`ui/src/panels/VirtualizedConversationMessageList.tsx` now merges the live
viewport range with the pinned search-hit range, but `mergeRanges(...)`
initializes `mergedRanges` with `sortedRanges[0]` and then updates
`currentRange.endIndex` in place. When the first merged range is
`mountedPageRange`, that mutation writes directly into the current React state
object while the component is rendering. The same object also feeds mounted-band
reconciliation and scroll-anchor bookkeeping, so the resulting corruption can
be hard to reproduce and harder to reason about.

**Current behavior:**
- `mergeRanges(...)` copies the input array but not the `VirtualizedRange`
  objects inside it.
- Overlapping ranges update `currentRange.endIndex` in place.
- When the first merged range is `mountedPageRange`, React state is mutated
  during render instead of producing a fresh derived range.

**Proposal:**
- Make `mergeRanges(...)` fully immutable by cloning the first range before
  storing it in `mergedRanges`.
- Keep all later merged ranges cloned as well so the helper never mutates an
  input object.
- Add a focused regression that overlaps the viewport band with the pinned
  search band and asserts the original `mountedPageRange` object is unchanged.

## Focused live sessions monopolize the main thread during state adoption

**Severity:** Medium - a visible, focused TermAl tab with an active Codex session can spend multiple seconds of an 8 s sample on main-thread work even when no requests fail and no exceptions fire.

A live Chrome profile against the current dev tab showed no runtime exceptions, no failed network requests, and no framework error overlay, but the page still burned about `6.6 s` of `TaskDuration`, `0.97 s` of `ScriptDuration`, `372` style recalculations, and several long tasks above `2 s` while Codex was active. The hottest app frames were `handleStateEvent(...)` in `ui/src/app-live-state.ts`, `request(...)` / `looksLikeHtmlResponse(...)` in `ui/src/api.ts`, `reconcileSessions(...)` / `reconcileMessages(...)` in `ui/src/session-reconcile.ts`, `estimateConversationMessageHeight(...)` in `ui/src/panels/conversation-virtualization.ts`, and repeated `getBoundingClientRect()` reads in `ui/src/panels/VirtualizedConversationMessageList.tsx`. A second targeted typing profile pointed the same way: 16 simulated keystrokes averaged only about `1.0 ms` of synchronous input work and about `11 ms` to the next frame, while `handleStateEvent(...)` alone still consumed about `199 ms` of self time. Narrower composer-rerender and adoption-fan-out regressions have been fixed separately, but the remaining profile still points at broader whole-tab churn.

**Current behavior:**
- A visible, focused active session still produces repeated long main-thread tasks while Codex is working or waiting for output.
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

## `/api/state` success responses still pay full text + HTML-sniff cost

**Severity:** Medium - `ui/src/api.ts::request(...)` still routes successful JSON snapshots through `response.text()` and `looksLikeHtmlResponse(...)` before `JSON.parse(...)`, so busy reconnect/resync flows burn CPU proportional to payload size even when the backend returns correct JSON.

The profiler-backed active-session round surfaced `looksLikeHtmlResponse(...)` and `request(...)` among the hottest app frames during state-resync activity. That work is avoidable on the common path: successful `/api/state` responses already advertise JSON, but the client still allocates the full body as text, lowercases/trims/scans it for HTML, and only then parses it as JSON. On large metadata or transcript-bearing snapshots, that adds extra string churn exactly when the main thread is already busy.

**Current behavior:**
- `request(...)` always reads the entire body as text, runs `looksLikeHtmlResponse(raw, contentType)`, and only then `JSON.parse(raw)`.
- `fetchState()` inherits that path during live resync and reconnect work, so every successful snapshot pays the extra full-body text handling cost.
- The hot path therefore does HTML-fallback detection work even when the response is already a normal `application/json` success.

**Proposal:**
- Treat successful JSON responses as JSON-first and reserve whole-body text scanning for error cases or obviously wrong content types.
- Keep the dev-server HTML fallback detection, but move it onto a narrow path that does not penalize healthy successful snapshots.
- Add explicit coverage for JSON success, HTML fallback, malformed JSON, and non-JSON error bodies so the cheaper fast path does not weaken the existing safety checks.

**Plan:**
- In `request(...)`, branch on `response.ok` plus JSON-like content types and parse with `response.json()` immediately on the happy path.
- Restrict `looksLikeHtmlResponse(...)` to responses whose content type is already suspicious or whose JSON parse failed, using at most a bounded prefix probe when the content type is missing.
- Add targeted tests for `/api/state` success, Vite/dev-server HTML fallback, 404 text responses, and malformed JSON so the API helper stays robust while the fast path gets cheaper.

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

## State snapshots still include full session transcripts on the wire

**Severity:** Medium - `/api/state` response bodies and SSE state broadcasts still include every session's full `messages` vector. The serialization CPU cost is now off the mutex and off the tokio workers (broadcaster thread + `spawn_blocking`), but the payload size itself is unchanged.

`snapshot_from_inner_with_agent_readiness` continues to clone every visible `Session` with its full `messages` vector into `StateResponse`. The HTTP `/api/state` handler and SSE state publisher then serialize those full transcripts even when the frontend only needs session metadata. Reconnect and tab-restore payloads still scale with total transcript size; individual active-prompt latency is unblocked (per the delta-persist and broadcaster fixes above), but network/client time to apply a full-state snapshot still scales.

**Current behavior:**
- `/api/state` returns all visible sessions with all historical messages (serialized inside `spawn_blocking`, so no tokio worker stall, but the response body is still O(all messages)).
- `publish_state_locked` builds the same full transcript snapshot for SSE state events (serialized on the broadcaster thread).
- The dedicated `GET /api/sessions/{id}` route exists, but state snapshots do not defer to it.
- The frontend already has `Session.messagesLoaded?: boolean` scaffolding that treats `false` as "needs hydrate" â€” forward-compat for the planned backend change.

**Proposal:**
- Make state snapshots metadata-first: include session shell fields and mark transcript-bearing sessions as `messagesLoaded: false` with an empty `messages` array.
- Keep `GET /api/sessions/{id}` as the authoritative full-transcript route, and keep session-create/prompt flows returning enough data that the active prompt UI remains reliable.
- **Before landing** (per the earlier revert): audit every `commit_locked` caller and ensure a matching `publish_delta` exists for any state change that adds/edits messages, so stripped state events do not drop the change.
- Add backend and App-level regression coverage proving `/api/state` omits transcripts, session hydration restores the full transcript, and metadata snapshots do not clear an already-hydrated active session.

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

## Lazy hydration effect: missing retry guard, over-eager deps, unreconciled replace

**Severity:** Medium - the new hydration path has several bugs that will materialize once the backend starts emitting `messagesLoaded: false` sessions.

Three distinct issues in and around the new `useEffect(... fetchSession ...)` in `ui/src/App.tsx`:
1. The dep array includes `activeSession?.messages.length`, causing the effect to re-run on every SSE `textDelta` token for the active session. Today the body short-circuits via the hydrated-set, so no correctness issue â€” but the deps are a footgun for any future real work added to the effect.
2. The async IIFE only guards against unmount. If the user switches away mid-fetch and the response's `session.id !== sessionId`, the code calls `requestActionRecoveryResyncRef.current()`. A transient server race can loop mismatch â†’ resync â†’ refetch â†’ mismatch.
3. `adoptCreatedSessionResponse` (and `live-updates.ts`'s `sessionCreated` reducer) raw-replace an existing session without per-message identity preservation via `reconcileSession`. If SSE `sessionCreated` materializes the session before the API response lands (or vice versa), memoized `MessageCard` children see new identities and remount.

**Current behavior:**
- Deps: `activeSession?.id`, `activeSession?.messages.length`, `activeSession?.messagesLoaded`.
- Mismatch branch triggers action-recovery resync without a "tried once" marker.
- Raw `[...previousSessions, created.session]` / `replaceSession(..., delta.session)` on the `existingIndex !== -1` branch.

**Proposal:**
- Drop `activeSession?.messages.length` from the dep array; comment the deliberate exclusion.
- Add a `hydrationMismatchSessionIdsRef` (or count attempts) to avoid re-firing after one mismatch until an authoritative state event arrives.
- Route the existing-session replace branch through `reconcileSession` (or a similar identity-preserving merge) so memoized children keep stable identity.


## Implementation Tasks

- [ ] P2: Add end-to-end recovery-open intent coverage in `useAppLiveState`:
  queue overlapping `requestActionRecoveryResyncRef` opens, adopt snapshots in
  stages, and assert each session opens only when the authoritative session list
  actually contains it.
- [ ] P2: Add a zero-height measurement regression for transcript virtualization:
  force every mounted message slot to report `0` height on the first pass and
  assert the virtualized list keeps a stable mounted window instead of
  collapsing to gap-only page heights.
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
