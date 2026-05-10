# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## `AppPreferences` `PartialEq` now allocates string compares

**Severity:** Note - `src/turns.rs:354-361`. Adding 4 owned `String` fields means every `AppPreferences` comparison now performs four byte-by-byte string compares instead of trivial enum/discriminant compares. The struct is compared in update flows to detect "no-op update" and rebroadcast suppression. Negligible at human cadence but the cost characteristic flips from O(1) to O(N strings).

**Current behavior:**
- `PartialEq` derived; comparison cost grew with the new fields.

**Proposal:**
- None needed; flag if `AppPreferences` ever ends up in a tight loop comparison.

## Command-file regular-file gate is check-then-open

**Severity:** Note - `src/api_files.rs:418, 562, 597`. Command discovery and resolver metadata now reject stable symlinks and non-regular files before opening, but the check is still separate from the subsequent file open. A command file swapped between the check and open can still be followed/read.

**Current behavior:**
- Stable symlinks and non-files under `.claude/commands/` are skipped.
- There is still a small TOCTOU window between file-type validation and opening.

**Proposal:**
- Bind validation to the opened handle where platform support allows it, e.g. no-follow open plus handle metadata checks.
- Or compare pre/post file metadata and treat mismatch as unavailable.

## Pending-prompts queue scrolls away from the pinned live tail

**Severity:** Low - `ui/src/styles.css:4567-4576`, `ui/src/panels/AgentSessionPanel.tsx:1437-1442`. After splitting `pendingPromptCards` into a sibling `<div className="conversation-pending-prompts">` instead of nesting inside `.conversation-live-tail`, the live tail keeps `position: sticky; bottom: 0` while the pending-prompts queue uses `display: grid` with no sticky positioning.

When the user scrolls up to read history, the live tail and its waiting indicator stay pinned, but the queued prompts disappear from view. Previously the column-reverse flex inside the live tail kept queued prompts visually adjacent to the live turn. The new layout drops that co-pinning silently.

**Current behavior:**
- `.conversation-pending-prompts` is a normal grid (no `position: sticky`).
- `.conversation-live-tail.is-pinned` stays sticky at the bottom.
- Queued prompts can scroll out of view while the live tail remains visible.

**Proposal:**
- Confirm whether the loss of co-pinning is intentional and document it next to the JSX/CSS.
- Or wrap both sections in a single sticky parent / set `position: sticky; bottom: <live-tail-height>` on the queue so it pins above the live tail.
- Add a code comment explaining the new DOM order vs. visual intent (the previous `column-reverse` had a comment that no longer applies).

## `dispatch_delegation_wait_resumes` errors are stderr-only without audit ledger

**Severity:** Low - `src/delegations.rs:1131-1154`. Dispatch errors are written to stderr only. A wait that was consumed but failed to dispatch leaves no structured trace in state, deltas, or a retained wait record.

Operators and the UI cannot tell that fan-in resume should have happened but did not.

**Current behavior:**
- Dispatch errors write to stderr only.
- The wait has already been removed.
- No audit ledger entry is created.

**Proposal:**
- Emit a structured warning event or retain dispatch error metadata.
- Or document the best-effort policy and recovery expectations.

## Telegram inline callbacks can dispatch actions to the wrong active project

**Severity:** High - `src/telegram.rs:1699` and `src/telegram.rs:2893`. Telegram digest inline buttons only carry the action id as `callback_data`, while the callback handler resolves the currently active project at click time.

If the linked chat opens a digest for project A, switches to project B, and then taps an older project-A action button, the action can be dispatched against project B. Approval, stop, fix, or review actions can therefore operate on a different project than the message the user clicked.

**Current behavior:**
- Digest keyboards store only `action.id` in Telegram callback data.
- Callback handling calls `resolve_telegram_active_project_id` before dispatch.
- Project switches between message render and callback click can retarget old buttons.

**Proposal:**
- Bind callback data to its digest context with a short server-side token or validated project/action payload.
- Reject stale or mismatched callbacks and refresh the digest for the project that produced the clicked message.

## Isolated delegation worktree creation is not transactional

**Severity:** Medium - `src/delegations.rs:336` and `src/delegations.rs:1775`. The API creates a detached git worktree before later fallible validation and before patch application is known to succeed.

If agent setup, parent revalidation, delegation fan-out/depth checks, or patch application fails after `git worktree add`, the request returns an error but can leave a registered worktree and directory behind. Retrying with the same requested `worktreePath` can then fail because the target is no longer empty.

**Current behavior:**
- `prepare_isolated_delegation_worktree` runs before `validate_agent_session_setup` and active delegation admission checks.
- `create_detached_git_worktree` has no rollback guard for later `git apply` or API rejection failures.
- Failed requests can leave filesystem and git-worktree side effects with no delegation record to clean them up.

**Proposal:**
- Move all non-side-effect validation and admission checks before worktree creation.
- Add a rollback guard after worktree creation that removes the git worktree and created parent directories on any later failure.

## Isolated delegation worktree snapshots omit untracked files

**Severity:** Medium - `src/delegations.rs:1764-1772`. The isolated worktree mirror captures staged and unstaged tracked changes with `git diff --cached --binary` and `git diff --binary`, but it does not include non-ignored untracked files.

A delegated `/review-local` build or review can run against a child worktree missing newly created source files from the parent workspace. That can produce false review findings or false build failures.

**Current behavior:**
- Staged tracked changes are applied to the child worktree.
- Unstaged tracked changes are applied to the child worktree.
- Non-ignored untracked files are silently absent from the child worktree.

**Proposal:**
- Include `git ls-files --others --exclude-standard` files with path validation and size limits.
- Or reject isolated-worktree delegation while unsupported untracked files are present and explain the limitation.

## Isolated delegation patch capture has no cumulative size cap

**Severity:** Medium - `src/delegations.rs:1764-1772`. The isolated-worktree path captures repository-wide binary diffs into memory before applying them to the child worktree.

Large tracked binary changes can make the delegation endpoint allocate large patch buffers, spend substantial CPU in `git apply`, and consume disk in the generated worktree.

**Current behavior:**
- `git diff --cached --binary` output is collected into a `Vec<u8>`.
- `git diff --binary` output is collected into a second `Vec<u8>`.
- No cumulative byte limit is enforced before patch application.

**Proposal:**
- Enforce a cumulative patch byte limit before creating or applying an isolated worktree.
- Return a clear 4xx error when dirty state is too large to materialize safely.

## Slash-command delegation is mouse-only while the palette is open

**Severity:** Medium - `ui/src/panels/AgentSessionPanel.tsx:2467`. The Delegate button is now enabled for selected agent slash commands, but the textarea still intercepts `Tab` and `Enter` while the slash palette is open.

Keyboard users can select and send the slash command, but cannot move focus to the now-enabled Delegate button with `Tab`; `Enter` sends instead of delegates. The new slash-command delegation flow is therefore effectively mouse-only.

**Current behavior:**
- `Tab` is prevented and routed to `handleComposerSend` whenever the slash palette is open.
- `Enter` also sends the selected slash command.
- No keyboard gesture delegates the selected slash command.

**Proposal:**
- Allow normal tab navigation when the selected slash item can be delegated.
- Or add an explicit keyboard command for delegating the active slash command and cover it with RTL tests.

## Agent command resolver failures are invisible in the composer

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:2388` and `ui/src/panels/AgentSessionPanel.tsx:2491`. Resolver errors are caught and handled only by refocusing the composer.

If the backend rejects a native slash command with an additional note, or the backend is unavailable, the user sees no validation or retry explanation. The draft remains, but there is no visible reason why pressing Enter or Delegate did nothing.

**Current behavior:**
- Resolver `catch` blocks refocus the composer only when the original session is still active.
- No error message is surfaced through the existing composer/session error channel.
- Native slash note rejection and backend-unavailable failures look like no-ops.

**Proposal:**
- Thread an `onComposerError` callback or local inline error state into `AgentSessionPanelFooter` / `SessionComposer`.
- Surface sanitized resolver failures the same way delegation spawn failures are surfaced.
- Add tests for native-slash note rejection and backend-unavailable resolver failure.

## Telegram selection cleanup can be lost on informational early returns

**Severity:** Low - `src/telegram.rs:1757` and `src/telegram.rs:2716`. Some command paths call `resolve_telegram_active_project_id`, which can normalize stale selected project/session state and mark the bot state dirty, then return `Ok(false)` after sending informational text.

Those cleanup mutations are not persisted when the function returns `false`, so stale Telegram selection state can survive a command that already detected and cleared it in memory.

**Current behavior:**
- Telegram free-text forwarding with no active session can return `Ok(false)` after active-project cleanup.
- `/session` with no arguments can return `Ok(false)` after active-project cleanup.
- Dirty cleanup state is lost unless another path persists later.

**Proposal:**
- Return the accumulated `dirty` value from informational early-return paths.
- Audit other early returns immediately after Telegram state-normalization helpers.

## Delegation action generation guard can drop the first action after a session switch

**Severity:** Low - `ui/src/SessionPaneView.render-callbacks.tsx:194`. `activeSessionGenerationRef` is advanced in a passive `useEffect`, so a delegation action started immediately after mount or session switch can capture the pre-effect generation.

When the async action settles after the effect increments the generation, the result is treated as stale and silently dropped even though the user acted in the current session.

**Current behavior:**
- `activeSessionGenerationRef` increments after paint in `useEffect`.
- Open/insert/cancel delegation actions capture the current generation at action time.
- A narrow post-switch timing window can make the first action no-op.

**Proposal:**
- Update the active-session id/generation ref in `useLayoutEffect` or another pre-interaction path.
- Add a regression test for an immediate delegation action after session switch.

## Active-baseline → settled transition can strand cursor when agent appends in-place to baselined message

**Severity:** Medium - `src/telegram.rs:2001-2009`. When an active session with `baseline_while_active=true` settles, the new transition baselines onto the latest message and clears the flag. The cursor's `resend_if_grown` is set to `false`, so an in-place text growth on the baselined message is NOT detected.

If the agent's response to the Telegram prompt appends to the existing message id (some agents stream in-place), `start_index = position_of_last + 1` skips that very message. The user's Telegram prompt was never "delivered" as a separate reply — the existing message was just baselined, and the relay shows nothing to the Telegram user.

**Current behavior:**
- Active-baseline → settled clears `baseline_while_active` and sets `resend_if_grown: false`.
- In-place text growth on the baselined message is invisible.
- Telegram user sees no reply when agent appends rather than emitting a new message.

**Proposal:**
- When transitioning from active-baseline to settled, also set `resend_if_grown: true` so an in-place text growth is detected and re-forwarded as the Telegram reply.
- Or document that mid-message append is not a supported producer pattern.

## Footer-send failure permanently loses the close marker with no retry

**Severity:** Medium - `src/telegram.rs:2208-2223`. Footer-send failure is converted to `Ok(sent_visible_content: true)` and logged. The footer is for visual closure. If a transient failure swallows the footer once, it is permanently lost for that turn. The footer's stated purpose ("user has no easy way to tell 'is the agent still typing or done?'") is silently undermined.

**Current behavior:**
- Footer-send failure → `Ok(sent_visible_content: true)`, log only.
- No retry on next poll.
- Permanent loss of close marker for that turn.

**Proposal:**
- Either persist a "footer pending" flag in the cursor so the next poll can retry.
- Or accept the loss but emit a heads-up message ("[turn complete; footer delivery failed]") on the success path.

## First-chunk failure can cause permanent retry loops

**Severity:** Low - `src/telegram.rs:2156-2162`. The chunk loop returns `Err(err)` when `sent_visible_content` is false (no chunks were successfully sent). The cursor was NOT updated. On retry, the relay will replay the first chunk. If the first chunk's send always fails (e.g., the chunk is malformed), the user will see permanent retry loops with no progress, no `sent_visible_content`, and no error escalation.

**Current behavior:**
- First-chunk failure → no cursor update → infinite retry.

**Proposal:**
- After N failed first-chunk attempts for the same `(message_id, chunk_index)`, advance the cursor anyway and surface a "[chunk N skipped: send failed]" line in chat.

## Armed Telegram delivery failure can leak digest-primary fallback into the same chat

**Severity:** Low - `src/telegram.rs:1668-1692`. `forward_relevant_assistant_messages` suppresses digest-primary fallback only after an armed forward sends visible content. If the armed session hits a first-chunk delivery error, it logs the failure but leaves `armed_sent_visible_content=false`, so an unrelated `primary_session_id` can still forward in the same poll.

**Current behavior:**
- Armed delivery failure marks the relay state dirty and logs the error.
- `armed_sent_visible_content` remains false.
- Digest-primary fallback can still forward another session's assistant text to the Telegram chat.

**Proposal:**
- Track "armed delivery attempted and failed" separately from "armed session only baselined/no visible content".
- Suppress digest-primary fallback after an armed delivery error.

## Telegram tests accumulate temp files in `$TMPDIR`

**Severity:** Note - `src/tests/telegram.rs:148-159`. `telegram_test_config()` writes `state_path` per test but never cleans up. Files accumulate in `$TMPDIR` across test runs.

**Current behavior:**
- Per-test temp file path generated.
- No cleanup.

**Proposal:**
- Use a `Drop` guard or `tempfile::NamedTempFile` so created files are reaped.

## `forward_new_assistant_message_outcome` is now ~256 lines with interleaved early-returns

**Severity:** Note - `src/telegram.rs:1916-2007`. The new pre-forward block has 3 separate `Active baseline` early returns and one merge into the main path; future contributors will struggle to trace which baseline shape is preserved across the merge.

**Current behavior:**
- Single function ~256 lines.
- Multiple interleaved early-return branches.

**Proposal:**
- Extract the active-baseline transition into a helper `transition_active_baseline_to_settled` that returns either the new cursor + position or an `OutcomeShortCircuit`.

## `assistant_forwarding_cursors` HashMap never reaped on session deletion

**Severity:** Low - `src/telegram.rs:392-395`. The new HashMap has no eviction/cleanup tied to session deletion; entries accumulate indefinitely in `~/.termal/telegram-bot.json`. A long-lived install accumulates cursor entries for every Telegram-touched session ever, including deleted ones.

**Current behavior:**
- HashMap entries persist beyond session lifetime.
- No reaping on session deletion.
- State file grows monotonically.

**Proposal:**
- Hook session deletion to remove the corresponding cursor.
- Or prune entries in the relay's existing cleanup paths whenever it observes a foreign session id absent from current state.

## Marker dialog-semantics test bundles 6+ behaviors in one `it()`

**Severity:** Note - `ui/src/panels/AgentSessionPanel.test.tsx:1024-1121`. The new "uses dialog semantics and local keyboard behavior" test combines six behaviors (dialog role, input focus/select, codepoint truncation, whitespace-disabled submit, button-keydown short-circuit, resize-doesn't-close-during-edit, trim, cancel restores focus, escape restores focus). One test asserting six behaviors fails opaquely.

**Current behavior:**
- 6+ assertions in one `it()`.
- Failure messages cluster at one line.

**Proposal:**
- Consider splitting once the test grows further.
- Current 6-in-1 is acceptable but the pattern should not expand.

## `MarkdownContent.test.tsx` fit-mode test only validates srcdoc against a NARROW diagram

**Severity:** Low - `ui/src/MarkdownContent.test.tsx:603-625`. The new test "lets preview callers shrink wide Mermaid frames..." validates fit-mode srcdoc CSS only against a narrow diagram (300×80 viewBox). The "shrinks wide Mermaid frames" claim in the test name is not directly proved — a wide-viewBox test (e.g., 2340×926) plus a constrained-parent wrapper would actually exercise the shrink-without-upscale contract end-to-end.

The companion test for default mode at line 627 uses a 2340-wide diagram; fit mode does not have an equivalent wide test.

**Current behavior:**
- Test uses 300×80 viewBox.
- Wide-diagram shrink contract not exercised.
- Test name promises behavior beyond what it asserts.

**Proposal:**
- Add a second fit-mode test using a wide viewBox like `viewBox="0 0 2340 926"` inside a constrained parent (e.g., `style={{ width: "400px" }}`).
- Assert the iframe shrinks while srcdoc contains the fit-mode SVG CSS.

## `buildMermaidDiagramFrameSrcDoc` has no unit test for the new fit-mode option

**Severity:** Low - `ui/src/mermaid-render.test.ts`. The new `buildMermaidDiagramFrameSrcDoc(svg, options)` API has no direct unit test (only integration coverage via `MarkdownContent.test.tsx`). The existing `mermaid-render.test.ts` covers `isMermaidErrorVisualizationSvg` and `renderTermalMermaidDiagram` but not the srcdoc builder.

**Current behavior:**
- Builder exercised only via integration test.
- Unit-level CSS-string assertion absent.
- A regression in builder isolation would not surface until rendering.

**Proposal:**
- Add a `describe("buildMermaidDiagramFrameSrcDoc")` block with one test per mode asserting on the returned string's CSS substrings (overflow rules, body display, svg max-width).

## `MarkdownDocumentView` is dead-reachable through the `RendererPreviewPane` `isMarkdownSource` branch

**Severity:** Note - `ui/src/MarkdownDocumentView.tsx`. SourcePanel only routes `RendererPreviewPane` when `!(isMarkdownSource && renderedMarkdownSegment)`. `renderedMarkdownSegment` is always non-null when `isMarkdownSource && fileState.status === "ready"`. `RendererPreviewPane` itself bails on `fileState.status !== "ready"`. So the inner `if (isMarkdownSource) return <MarkdownDocumentView .../>` branch in `RendererPreviewPane` cannot fire from this caller.

Threading `fillMermaidAvailableSpace` through dead code does not cost anything but obscures the actual call graph. Round 77 expanded the dead surface area by adding a new prop.

**Current behavior:**
- Dead branch threads a new prop.
- No test exercises the path.
- Live caller graph unclear.

**Proposal:**
- Either delete the dead branch in `RendererPreviewPane` (and inline `MarkdownDocumentView` into its only live caller, if any).
- Or explicitly document that the branch exists for future non-SourcePanel callers and add a test that exercises the path.

## `fillMermaidAvailableSpace` widens the whole Markdown shell without documenting that contract

**Severity:** Note - `ui/src/message-cards.tsx:3624-3662, 4356` and `ui/src/panels/markdown-diff-change-section.tsx:188-228`. The new `fillMermaidAvailableSpace` prop is undocumented while `isStreaming` (just below it) has a detailed multi-paragraph comment explaining contract and consumers. The name is Mermaid-specific, but enabling it adds `markdown-copy-shell-fill-mermaid`, and the CSS widens the whole markdown shell/copy area rather than only Mermaid blocks.

A future contributor cannot tell from the type alone whether the prop applies only to Mermaid, what "fill" means visually, which caller decides to enable it, or whether non-Mermaid prose/code/table layout is intentionally affected in source previews.

**Current behavior:**
- Prop has no JSDoc and reads as Mermaid-only.
- Enabling it also applies a full-width markdown shell/copy layout class.
- Sibling `isStreaming` has detailed contract documentation.

**Proposal:**
- Scope the full-width override to Mermaid blocks if the broader markdown layout change is accidental.
- Or rename/document the prop as a preview-wide full-width markdown mode, including the fit-to-frame Mermaid behavior, layout interaction with `markdown-copy-shell-fill-mermaid`, and call sites.

## `buildMermaidDiagramFrameSrcDoc` `fitToFrame` option lacks contract documentation

**Severity:** Note - `ui/src/mermaid-render.ts:80-123`. `buildMermaidDiagramFrameSrcDoc` now takes an `{ fitToFrame?: boolean }` options bag but no source comment explains what semantic the new mode means or which call site passes it.

The header section above the function describes "What this file owns" but the new fit-mode contract (when does a caller want it, what does it do to wide vs narrow diagrams, how does it interact with `getMermaidDiagramFrameStyle`'s 24px slack) is not documented in source.

**Current behavior:**
- Option-bag parameter added with no JSDoc.
- Inline implementation differs by mode but contract is implicit.

**Proposal:**
- Add a JSDoc block above the function explaining: default mode (overflow-x scroll for wide diagrams), fit mode (max-width:100% SVG to shrink wide diagrams without upscaling, used by source-preview pane), and the deliberate choice to leave `getMermaidDiagramFrameStyle` independent of the option.

## `iframeStyle` `useMemo` block-form rewrite is stylistic churn with no behavior change

**Severity:** Note - `ui/src/message-cards.tsx:1249-1257`. The `iframeStyle` `useMemo` was rewritten from a one-line ternary into a block-form arrow but is functionally identical. Both forms return the same value with the same dependency list `[readySvg]`.

This counts against the "preserve public behaviour exactly during refactors" guidance in CLAUDE.md (this is a feature round, but the unrelated refactor adds noise).

**Current behavior:**
- Block-form arrow with no behavioral difference.
- Pure stylistic churn in a Round 77 staged diff.

**Proposal:**
- Either revert the `iframeStyle` block to its original ternary.
- Or, if the block is preferred, lift the same style to other helpers in the file for consistency.

## `MarkdownDocumentView.tsx` lacks header comment despite Round 77 expanding its surface

**Severity:** Note - `ui/src/MarkdownDocumentView.tsx:1-2`. CLAUDE.md says modules should have header comments explaining what they own / don't own / split provenance. `MarkdownDocumentView.tsx` is not new but Round 77 expanded its contract by threading `fillMermaidAvailableSpace` from non-Markdown / Markdown source preview through this component.

Sibling files like `panels/source-renderer-preview.tsx` and `panels/markdown-diff-change-section.tsx` have detailed header comments.

**Current behavior:**
- File has no header comment.
- Round 77 expanded its public-prop surface.
- Documentation inconsistent with siblings.

**Proposal:**
- Add a brief header comment describing the read-only Markdown document chrome (header + scroll body) and pin the relationship to the editable preview path used by SourcePanel.

## `fillMermaidAvailableSpace` feature has no `docs/features/*.md` entry

**Severity:** Note - `docs/features/source-renderers.md` (or a new file). CLAUDE.md says feature briefs live under `docs/features/`. Round 77 introduces a meaningful behavior split (fit-to-frame Mermaid in source preview panes vs scroll-on-overflow elsewhere). A feature brief would document the intent ("smaller diagrams keep natural size; wide diagrams shrink to pane width without horizontal scrollbar in source preview") and the trade-off (24px vertical slack from `getMermaidDiagramFrameStyle` survives even though no scrollbar appears in fit mode).

**Current behavior:**
- No feature brief documents the dual mode.
- Implementation behavior split across multiple files without narrative.

**Proposal:**
- Add a section to `docs/features/source-renderers.md` covering the fit-to-frame Mermaid mode, when each call site picks it, and the residual aspect-ratio slack consideration.

## Telegram renderer tests don't exercise `> 12 sessions` overflow message

**Severity:** Low - `src/tests/telegram.rs:188-245`. `render_telegram_project_sessions` appends "More sessions exist in TermAl." when more than 12 project-scoped sessions are present. The test fixture uses 2 project-1 sessions; the overflow branch is structurally untested.

A regression that flipped `> 12` to `>= 12` or removed the trailing line would silently pass.

**Current behavior:**
- Test fixture has 2 sessions per project.
- >12 sessions case uncovered.

**Proposal:**
- Add a fixture with 13+ project-scoped sessions.
- Assert the overflow line is present plus that exactly 12 session entries appear.

## Composer height test asserts on `transition: "none"` writes without ordering pin

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:7789-7950`. The "keeps multiline composer height steady when deleting text inside a line" test asserts `heightWrites.toContainEqual({ value: "1px", transition: "none" })` and `heightWrites.toContainEqual({ value: "96px", transition: "none" })`. The full ordering is not validated, so a regression that re-orders the writes (e.g., setting `96px` first and then `1px`) would still pass even though the visible behavior would differ.

**Current behavior:**
- `toContainEqual` matches in any order.
- A regression that flipped write order would pass.

**Proposal:**
- Assert on the full `heightWrites` array order, not just `toContainEqual`.
- Or use `toEqual([...])` with the full expected sequence.

## `digest.deep_link` derivation now branch-local in two places (same triple-shadow as `primary_session_id`)

**Severity:** Note - `src/api.rs:308-484`. Round 76 added two new `let deep_link = Some(build_project_deep_link(...))` rebindings inside the `worktree_dirty` and idle branches. The outer `deep_link` (line 329-332) is now used only by the four upper branches. Same triple-shadow problem as `primary_session_id`. Wire-contract callers cannot tell from `deep_link` alone which target it points at.

**Current behavior:**
- Three layers of `deep_link` semantics in one function.
- Shadows in dirty/idle branches.
- Wire contract docs don't distinguish.

**Proposal:**
- As with `primary_session_id`, lift the per-branch `deep_link` derivation into a helper that takes the action-target session id explicitly.

## `TelegramStateSessionsResponse` lacks documentation tying it to `/api/state`

**Severity:** Note - `src/telegram.rs:1176-1204`. The struct is a narrow projection of `/api/state`, but unlike `TelegramSessionFetchResponse` (which has explicit doc comments at lines 1206-1210, 1239-1249), `TelegramStateSessionsResponse` has none. Future readers cannot tell why this struct exists, what fields it depends on, or whether `/api/state` is the intentional source.

**Current behavior:**
- No `///` doc on the struct.
- Sibling `TelegramSessionFetchResponse` has docs.
- Inconsistent documentation across the relay's wire projections.

**Proposal:**
- Add a doc comment matching the style of `TelegramSessionFetchResponse`: explain the relay's data needs, document why `/api/state` is used over a narrower endpoint, and pin the camelCase wire contract.

## `digest.primary_session_id` semantics drift between branches without wire-level documentation

**Severity:** Note - `src/api.rs:308-484`. Round 76 deliberately changed `primary_session_id` in `worktree_dirty` and `idle` branches to be the non-delegation prompt target, while `pending_approval`, `pending_interaction`, `error_session`, and `active_session` branches keep the original "most-relevant session" semantics. The wire contract `ProjectDigestResponse.primary_session_id` is unchanged and undocumented. Consumers cannot tell which definition applies in which branch.

**Current behavior:**
- Per-branch divergent semantics.
- Wire contract unchanged.
- Consumers cannot disambiguate.

**Proposal:**
- Document the per-branch contract on `ProjectDigestResponse.primary_session_id` in `wire_project_digest.rs`.
- Or split the wire field into `summary_session_id` (always source of `done_summary`) and `action_target_session_id` (always dispatch target).

## Three Telegram user-error formatters with three formats but shared truncation helper

**Severity:** Note - `src/telegram.rs:2299-2336`. `telegram_action_error_text` (multi-line with "Send /status" footer), `telegram_callback_action_error_text` (one-line "X failed: Y"), and `telegram_prompt_error_text` (multi-line "Could not forward that message." + detail). Three user-visible string formats mean a UI-level message-style consistency check is missing. A future contributor changing one might forget the others.

**Current behavior:**
- Three formatters with three formats.
- Shared truncation helper but divergent prefix/footer styles.
- No JSDoc-style block declaring contract for each.

**Proposal:**
- Either consolidate into a single `format_telegram_user_error(action_kind, err)` enum-driven formatter.
- Or add a JSDoc-style block above each declaring the contract.

## Project digest `primary_session_id` mixes summary source and action target semantics

**Severity:** Low - `src/api.rs:397-476`. The error, dirty, and clean idle digest branches can compute summary text/status from one session while replacing `primary_session_id` and `deep_link` with the non-delegation prompt target. Consumers cannot tell whether `primary_session_id` names the session that produced the digest summary/status or the session that should receive follow-up actions.

**Current behavior:**
- `primary_session_id` is used as the action target by `execute_project_action()`.
- The same field is also serialized to clients as the digest's primary/deep-link session.
- Error, dirty, and idle branches can derive summary/status context from a delegation child while returning the parent/non-delegation session id.

**Proposal:**
- Split the internal digest model into explicit `summary_session_id` and `action_target_session_id` fields.
- Document which field drives links, summaries, and prompt-producing actions.

## `SessionPaneView` pending-prompt scroll exemption misses `showWaitingIndicator` dependency

**Severity:** Low - `ui/src/SessionPaneView.tsx:2035`. The scroll effect now checks `showWaitingIndicator` inside the `onlyPendingPromptsChanged` branch, but the effect dependency list does not include `showWaitingIndicator`.

**Current behavior:**
- `showWaitingIndicator` can change through sending/busy state.
- The scroll effect can keep using a stale closure for the pending-prompt scroll exemption.
- Pending-prompt scroll behavior can be out of sync with the current render.

**Proposal:**
- Add `showWaitingIndicator` to the effect dependency array.
- Or derive the exemption only from values already included in the dependency list.

## `TelegramStateSession.status` is stringly-typed where parallel projection uses typed enum

**Severity:** Note - `src/telegram.rs:1199`. `TelegramStateSession.status: String` is stringly-typed where the parallel `TelegramSessionFetchSession.status` uses a typed `TelegramSessionStatus` enum with `#[serde(other)]`. The wire contract is `"active" | "idle" | "approval" | "error"` (per `wire.rs:712-719`) — closed enum on the server side.

A future server adds a new status variant (`"queued"`, `"timing_out"`) silently slips through as `"unknown"` via `telegram_state_session_status_label`, and there is no compile-time check that the relay handles all wire status values.

**Current behavior:**
- `TelegramStateSession.status` is `String`.
- `telegram_state_session_status_label` maps unknown to `"unknown"`.
- Two parallel projections of the same `Session.status` field have inconsistent typing rigor.

**Proposal:**
- Replace `String` with a re-used `TelegramSessionStatus` enum that uses `#[serde(other)]` (mirroring `TelegramSessionFetchSession`).

## Duplicate `let primary_session_id` rebindings in `src/api.rs` digest branches

**Severity:** Note - `src/api.rs:432-470`. Duplicate `let primary_session_id = ...` and `let deep_link = ...` rebindings inside both `worktree_dirty` and idle branches shadow the outer bindings. The function now has three different layers of `primary_session_id` semantics: the original "most-recent-by-activity-rank", the rebinding to `prompt_target_session_id`, and the proposed-actions test `prompt_target_session_id.is_some()` already done before rebinding.

**Current behavior:**
- Three layers of `primary_session_id` semantics in one function.
- Future readers will struggle to keep these straight.

**Proposal:**
- Lift the prompt-target-vs-primary distinction into a single named variable at the top of the function.
- Or extract the dirty/idle branches into helpers that take both sessions explicitly.

## `paneMessageContentSignaturesRef` lifetime divergence vs `paneContentSignaturesRef`

**Severity:** Note - `ui/src/SessionPaneView.tsx:672`. `paneMessageContentSignaturesRef` is a per-instance ref while `paneContentSignaturesRef` is hoisted to App.tsx and threaded through. When `SessionPaneView` remounts (e.g., on a layout split), `paneMessageContentSignaturesRef` resets to `{}` while `paneContentSignaturesRef` persists. After remount, `previousMessageContentSignature` will be `undefined` for one render but `previousSignature` will be the prior live value.

**Current behavior:**
- Two refs with divergent lifetimes for related concerns.
- Mismatch may be invisible today but the lifetime divergence is a footgun.

**Proposal:**
- Hoist `paneMessageContentSignaturesRef` to App.tsx and pass it through alongside `paneContentSignaturesRef`.
- Or document why the divergence is intentional with a header comment.

## `latest_project_prompt_target_session` selects without health check

**Severity:** Note - `src/api.rs:525-530`. `latest_project_prompt_target_session` selects the latest non-delegation session regardless of `status`, so a parent session in `Error` will be chosen as the prompt target while a healthy delegation child sits idle. Callers downstream of `dispatch_project_action` will retry against an errored session.

**Current behavior:**
- Picks structurally rather than by health.
- Errored parent overrides healthy non-parent.

**Proposal:**
- Document the structural-vs-functional design trade-off in the function `///` doc.
- Or additionally filter on `status != Error`.

## `truncate_telegram_text_chars` produces output longer than `max_chars` when `max_chars < 3`

**Severity:** Low - `src/telegram.rs:2275-2285`. For `max_chars = 2`, the function returns `".."` + `"."` = `"..."` (3 chars) instead of `<= 2` chars. Current call sites use 96/180/240, so no immediate impact, but the helper's contract is broken on small inputs.

**Current behavior:**
- Ellipsis suffix unconditionally appended after truncation.
- For `max_chars < 3`, output exceeds the limit.

**Proposal:**
- Branch on `max_chars >= 3` to apply the ellipsis suffix and otherwise return at most `max_chars` characters.

## `render_telegram_project_sessions` re-runs filter twice

**Severity:** Note - `src/telegram.rs:2202-2252`. Re-runs `state.sessions.iter().filter(...).count()` after the take-12 to detect "more sessions exist", duplicating the filter logic. Two scans of the same vec when a single capture would suffice.

**Current behavior:**
- Filter predicate runs twice.
- Negligible CPU on small N.
- Future drift risk if filter predicates diverge.

**Proposal:**
- Compute the filtered count once before the take(12) and reuse it.

## 3 `style.height` writes per resize cause unnecessary layout thrash

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:1726-1729`. `textarea.style.height = previousMeasuredHeight + "px"` reassigns the height immediately after setting it to `Math.max(minHeight, 1)`. The `void textarea.offsetHeight` reflow forces layout twice per resize when only one height is needed.

Each `resizeComposerInput()` call now causes 3 `style.height` writes (1px → previousMeasuredHeight → finalHeight) when shrinking is allowed. Combined with the rAF-coalesced `scheduleComposerResize`, busy typing will show up in DevTools layout-thrash profiles.

**Current behavior:**
- 3 height writes per resize.
- 2 forced reflows per resize.

**Proposal:**
- Skip the snapshot-and-restore intermediate write when `previousMeasuredHeight === nextHeight`.

## `useEffect` consumes `paneMessageContentSignaturesRef` before predicate fires

**Severity:** Low - `ui/src/SessionPaneView.tsx:1979-2076`. The lookup at line 1986-1989 is ref-based, so it doesn't trigger re-runs. But the assignment back-writes `paneMessageContentSignaturesRef.current[scrollStateKey] = visibleMessageContentSignature` happens BEFORE the early-return check at line 1990. If the early return fires, the back-write happened, but the side-effect logic that reads it is in a later branch.

This means the effect "consumes" the previous-message-content-signature once per call regardless of whether the predicate ran.

**Current behavior:**
- Back-write happens before predicate evaluation.
- Consecutive same-signature renders lose the previous value.

**Proposal:**
- Compute `onlyPendingPromptsChanged` BEFORE writing the next ref value.
- Or add a test asserting the predicate fires on the second render in a row when only the assistant text grew.

## anyhow chain in user-visible Telegram error messages may leak filesystem layout

**Severity:** Low - `src/telegram.rs:2299-2305`. `telegram_action_error_text` puts the (sanitized but possibly long) anyhow detail directly into the user-visible Telegram message after "Could not run X.\n". Anyhow error chains can include filesystem paths, internal session ids, project ids, and other state-shape information.

The sanitizer only redacts Telegram tokens. A backend error like `"failed to load session 7af3-...: file not found at /Users/foo/.termal/sessions/7af3.json"` ships filesystem layout to a Telegram chat. The Phase 1 trust model accepts local users, but the Telegram chat is by definition not local.

**Current behavior:**
- Full anyhow chain forwarded to Telegram chat.
- Sanitizer only redacts tokens.

**Proposal:**
- Either curate user-visible error text per ApiError kind (mapping known kinds to safe text).
- Or document the trust assumption in `docs/features/telegram-ui-integration.md`.

## Live-tail `null` transition not tested

**Severity:** Note - `ui/src/panels/AgentSessionPanel.tsx:1291-1298`. When neither `liveTurnCard` nor `pendingPromptCards` is present, `liveTail` is `null`. The `position: sticky` element is removed from the DOM when becoming null. Without a test that asserts no reflow loop, a regression that recreates the wrapper on every render (e.g., changing the `liveTail` ternary to always render the wrapper) is undetectable.

**Current behavior:**
- Live-tail renders conditionally.
- Transition out of the live-tail state not pinned by a test.

**Proposal:**
- Add a test that asserts the live-tail wrapper appears AND disappears as `showWaitingIndicator`/`pendingPrompts` toggle.

## `mermaid-demo.md` accidental edits: title typo and undefined node

**Severity:** Note - `docs/mermaid-demo.md:1, 9`. Title was changed from `# Mermaid Demo` to `# Mermaid Demoa` (typo) and the diagram edge `Edit --> Stop` was changed to `Edit --> Stop2` (an undefined node). Both edits look like accidental input rather than deliberate. The mermaid diagram now references an undefined node `Stop2` which Mermaid renders as a degenerate empty rectangle.

**Current behavior:**
- Title contains typo.
- Edge references undefined node.

**Proposal:**
- Revert if accidental.
- Or if intentional (e.g., testing fault-tolerance), add a comment in the file explaining the test scenario.

## `dispatch_project_action` error handling asymmetric between message and callback paths

**Severity:** Note - `src/telegram.rs:1442-1457`. `dispatch_project_action` failure in callback-query handler calls `answer_callback_query` THEN `send_message`. If `send_message` returns Err, it bubbles via `?` and the function returns Err — but `answer_callback_query` already fired (with `let _ =`).

The order acknowledges first then reports, which is correct (Telegram's callback_query MUST be answered within ~30s). But if `send_message` errors, the caller's surrounding loop logs the error AND has no way to know the user already saw the toast.

**Current behavior:**
- Acknowledge-then-report order.
- send_message error bubbles even though toast already fired.

**Proposal:**
- Document the "answer toast first, then explanatory message" intent inline.

## Telegram `/sessions` couples the relay to full `/api/state`

**Severity:** Note - `src/telegram.rs:1025, 2020-2240`. The relay calls full `/api/state` and reconstructs a Telegram-specific project-session list locally. That couples Telegram command behavior to the broad state snapshot shape instead of a narrow project-scoped session summary contract.

**Current behavior:**
- `get_state_sessions()` deserializes a subset of `/api/state`.
- Telegram-specific project/session filtering and rendering happens inside the relay.
- No source comment documents why this broad state contract is intentional.

**Proposal:**
- Prefer a narrow project-scoped session summary API/contract.
- Or add a source comment documenting why the relay intentionally owns this `/api/state` projection and which fields it depends on.

## `start_telegram_relay_runtime` parallel `spawning`/`running` booleans should be a state enum

**Severity:** Note - `src/telegram.rs:222-302`. Round 74 consolidated the relay state into `TelegramRelayRuntime` with parallel `spawning` and `running` booleans. The snapshot rule `running && !spawning` is implicit. A future contributor adding a new flag (e.g., `stopping`) needs to remember to combine all three correctly in the snapshot.

**Current behavior:**
- Two parallel booleans for state.
- Snapshot rule encoded inline.
- No enforcement of valid state transitions.

**Proposal:**
- Replace the two booleans with a `RelayState` enum (`Idle | Spawning | Running`) and centralize the `running()` accessor on it.

## `start_telegram_relay_runtime` `else` branch redundantly clears `spawning` after spawn succeeds

**Severity:** Low - `src/telegram.rs:294-301`. In the `else` branch (`spawn` succeeded), the parent thread re-acquires the mutex purely to clear `spawning = false`. The spawned thread can have already cleaned up state if it ran to completion before this branch fires; the parent's write is then redundant. Additionally, if the spawned thread starts running BEFORE this `else` branch acquires the lock, the snapshot reports `running && !spawning` momentarily.

**Current behavior:**
- Parent thread re-acquires mutex post-spawn to clear `spawning`.
- Spawned thread may have already completed full cleanup.
- Brief window where snapshot can show `running` true based on lock acquisition order.

**Proposal:**
- Move the `spawning = false` write into the spawned thread's first action (before the run loop starts) and drop the parent's `else` branch entirely.

## `from_ui_file` returns `Option<Self>` for three distinct disabled-relay reasons

**Severity:** Note - `src/telegram.rs:181-213`. The function returns `Option<Self>` for THREE distinct disabled-relay reasons (disabled flag, missing/empty token, missing/empty default project). The caller cannot tell why the relay isn't started. A typed reason would help diagnostics and let the UI surface a more accurate "Stopped" reason.

The new "Stopped" UI label is broad. If the user thinks they enabled the relay but configured an invalid project, they get the same "Stopped" copy as if they merely toggled the relay off.

**Current behavior:**
- Three disabled paths collapse to `None`.
- Caller cannot distinguish.

**Proposal:**
- Return a `Result<Self, RelayDisabledReason>` and route the reason through to status / preferences UI.

## `||` change in `resolveSessionRemoteId` flips behavior for `session.remoteId === ""`

**Severity:** Note - `ui/src/remotes.ts:32`. The contract change from `??` to `||` (closes round-73 ledger entry) is correct per the protocol, but it changes observable behavior for `session.remoteId === ""`. If any backend path or test fixture ever produces an empty string, routing flips from "session declares local" (pre-round-74) to "fall through to project" (round-74).

The protocol contract is unwritten in code — only the comment in `types.ts:290` references it. Backend regressions that emit `""` would now route remote-owned where they previously routed local.

**Current behavior:**
- `||` treats `""` as falsy.
- Behavior change is silent.
- Only the helper-level test catches it.

**Proposal:**
- Add a runtime assert (in dev) or a wire-decode validation that rejects `Session.remoteId === ""` so the contract is enforced at the boundary.

## SHA-256 fingerprint JSON intermediate held in memory until end-of-function

**Severity:** Note - `src/telegram.rs:338-352`. The SHA-256 fingerprint includes `bot_token` in its JSON-encoded input. The hash is one-way, but the JSON intermediate (`encoded`) is held in `Vec<u8>` until the `hasher.update(encoded)` call returns. A heap dump or coredump could expose the token in JSON form briefly.

**Current behavior:**
- JSON intermediate buffer holds the bot token until end-of-function.
- No `Zeroize` on the buffer.
- Marginal degradation of process secret hygiene.

**Proposal:**
- Either zeroize the `encoded` buffer after `hasher.update`.
- Or revert to per-field byte feeds (no intermediate JSON buffer) for the secret-bearing fields.

## `prune_telegram_config_for_deleted_project` reconcile path is `#[cfg(not(test))]`

**Severity:** Low - `src/telegram_settings.rs:243-264`. Round 74 wired the relay reconcile into `prune_telegram_config_for_deleted_project` (closes the round-73 deleted-project entry) but the reconcile path is `#[cfg(not(test))]`. The persistence side is tested, the reconcile side is not.

**Current behavior:**
- Reconcile call is `#[cfg(not(test))]`.
- Production restart path is structurally untested.

**Proposal:**
- Add a non-`cfg`-gated abstraction so a Rust test can verify the reconcile is invoked after a successful prune.

## `telegram_ui_file_requires_default_project_for_relay_config` bundles 3 scenarios

**Severity:** Low - `src/tests/telegram.rs:1058-1097`. Bundles three scenarios (no default, blank-only default, valid trimmed default) into one `it()`. Same anti-pattern flagged elsewhere in the bug ledger. Also missing: `bot_token` blank/missing variant; `enabled = false` with valid token + project; bot_token being whitespace-only.

**Current behavior:**
- Three scenarios bundled.
- Coverage gaps for the bot_token disable path.

**Proposal:**
- Split into per-case tests (`it.each` over `(file_config, expected_outcome)` pairs).

## "Stopped" preferences test covers only one combination, no negative cases

**Severity:** Low - `ui/src/preferences-panels.telegram.test.tsx:139-151`. The new "Stopped" test only fires for the exact `(inProcess, enabled, configured, !running)` combination. Adjacent paths are not pinned: `lifecycle: "manual"` with same other fields → should NOT show "Stopped"; `enabled: false` with same other fields → should NOT show "Stopped".

A regression that broadens the condition to `lifecycle === "inProcess" && configured` would still pass.

**Current behavior:**
- Single positive case covered.
- No negative permutations.

**Proposal:**
- Add `it.each` rows covering the four negative permutations and assert the label is NOT "Stopped".

## Wire `TelegramStatusResponse` lacks derived `state` field; "Stopped" derivation lives in UI

**Severity:** Low - `ui/src/api.ts:644-646` and `ui/src/preferences-panels.tsx:1205`. The new comment documents the intent but does not state the relationship: `lifecycle === "inProcess" && enabled && configured && !running` ≡ "Stopped". This protocol-level invariant lives entirely in UI code.

A backend refactor (e.g., adding a `failed: bool` field) cannot consult the wire contract to know the UI's "Stopped" derivation.

**Current behavior:**
- "Stopped" derivation lives in UI only.
- Wire contract has no derived `state` field.

**Proposal:**
- Add a derived `state: "stopped" | "polling" | "linked" | "configured" | "notConfigured"` to the wire response (single source of truth).
- Or document the derivation rules in the `TelegramStatusResponse` JSDoc.

## `session-reconcile.test.ts` doesn't cover `remoteId === undefined` on both sides

**Severity:** Note - `ui/src/session-reconcile.test.ts:1142-1212`. The two new tests pin the remote-owner reconciliation but assert via reference identity. They do not cover the case where `remoteId === undefined` on both sides (current local-only behavior) — the OLD fast-path retained `previous` exactly. Without a regression test, a refactor that accidentally changed the comparison semantics for `undefined === undefined` could flow through.

**Current behavior:**
- Two new tests cover changed-remoteId cases.
- No assertion for the undefined-on-both-sides fast-path.

**Proposal:**
- Add a sibling test that asserts `reconcileSessions(previous, next)` returns `previous` when `remoteId` is undefined on both sides AND stamps match.

## `remotes.test.ts` `it.each` refactor dropped `isLocalSessionRemote` empty-string-session case

**Severity:** Note - `ui/src/remotes.test.ts:60-80`. The `it.each` refactor splits the previous bundle (closes round-73 entry) but drops the `isLocalSessionRemote` empty-string-session case from the bundle. The contract change for `""` semantics is the load-bearing behavior change of round 74; only one of the two helpers is covered.

**Current behavior:**
- `it.each` covers `resolveSessionRemoteId` empty-string case.
- `isLocalSessionRemote` empty-string case dropped.

**Proposal:**
- Add `[{ remoteId: "" }, { remoteId: "ssh-lab" }, false]` to the `isLocalSessionRemote` it.each table.

## `delegation-result-prompt.test.ts` "scans findings, commands, and notes" bundles 3 field types

**Severity:** Note - `ui/src/delegation-result-prompt.test.ts:296-336`. The new "scans findings, commands, and notes for the longest indented tilde fence" test is good but bundles three field types (findings, commandsRun, notes) plus the `~~~~~~` outer fence assertion in one `it()`.

**Current behavior:**
- Three field types bundled.
- Per-field signal lost on regression.

**Proposal:**
- Split into per-field `it()` blocks.

## `TelegramRelayStatusSnapshot` directly maps to wire shape with no transformation layer

**Severity:** Note - `src/telegram.rs:216-220, 318-335`. The new `TelegramRelayStatusSnapshot` is internal but its fields directly populate the wire shape `TelegramStatusResponse { running, lifecycle, ... }`. The struct sits between the runtime accessor and the wire serializer with no transformation layer, so any change to the snapshot fields cascades into the wire contract.

**Current behavior:**
- Snapshot fields → wire fields, one-to-one.
- No transformation layer.

**Proposal:**
- Either rename the struct to a wire-prefixed name.
- Or add a thin builder method on `TelegramStatusResponse` that takes the snapshot explicitly.

## Supervised in-process Telegram relay status is untestable in production due to `#[cfg(test)]` fallback

**Severity:** Medium - `src/telegram.rs:220-331`. `telegram_relay_status_snapshot()` has a production implementation backed by the live relay runtime and a test fallback that always returns `running: false` / `lifecycle: Manual`. The wire-shape tests can assert `InProcess` serialization statically, but no integration test exercises the live status endpoint while the in-process relay is running.

**Current behavior:**
- Relay status snapshot has `#[cfg(not(test))]`/`#[cfg(test)]` parallel implementations.
- Tests always see `running: false` / `lifecycle: Manual`.
- Production behavior is structurally untested.

**Proposal:**
- Add a non-`cfg(test)` "test mode" environment variable that lets a Rust integration test boot the runtime in a no-op mode and assert `running` flips.
- Or refactor the runtime so the status accessors take a `&Self` parameter that tests can inject.

## CLI `termal telegram` mode and in-process relay are not mutually exclusive

**Severity:** Note - `src/main.rs:75-77, 115-116`. The `Mode::Telegram` (CLI) path invokes `run_telegram_bot()` directly, NOT the in-process runtime. If a user starts both `termal server` and `termal telegram`, both would race against `~/.termal/telegram-bot.json` state file with no coordination. Two separate processes hitting Telegram polling against the same `next_update_id` cursor will alternately leapfrog and lose updates.

**Current behavior:**
- `termal server` boots the in-process relay.
- `termal telegram` starts a separate bot directly.
- No mutual-exclusion check.

**Proposal:**
- Document the mutual-exclusion contract in `docs/features/telegram-ui-integration.md`.
- Or detect a running in-process relay (via the state file) and refuse to start the CLI mode.

## `TelegramRelayRuntime` is a file-level global rather than `AppState`-owned state

**Severity:** Note - `src/telegram.rs:220-331`. `TelegramRelayRuntime` and `TELEGRAM_RELAY_RUNTIME` are file-level globals (`LazyLock<Mutex<...>>`). `AppState` has no visibility into the relay's running state, so any future health-monitor, restart-on-error, or readiness-signaling logic ends up reading globals instead of methods on `AppState`.

**Current behavior:**
- Runtime state lives in module-level statics.
- Test injection is harder; production-vs-test parity is structural.

**Proposal:**
- Move the runtime into `AppState` and own its lifecycle on the state object.

## `reconcile_telegram_relay_from_saved_settings` is synchronous on main task at startup

**Severity:** Note - `src/main.rs:115-116`. The reconcile runs synchronously on the main task, blocking after "listening: http://" is printed but before the server starts accepting requests. With corrupt-file backup paths the reconcile could spend time on filesystem operations before the server is fully responsive.

**Current behavior:**
- Synchronous reconcile after server bind, before request handling.

**Proposal:**
- Spawn the reconcile as a `tokio::spawn` so the server responds immediately.

## Telegram relay stop/restart does not wait for old thread quiescence

**Severity:** Medium - `src/main.rs:145`, `src/telegram.rs:248-315` signal the Telegram relay to stop but do not join the old relay thread or otherwise wait until it has stopped using its captured config.

After shutdown, disable, or config retargeting, a relay that already passed its shutdown check can briefly continue polling or handling Telegram updates with the old bot/project configuration. During process shutdown this can also exit before update cursors or state-file work has quiesced.

**Current behavior:**
- Stop/restart flips a shutdown flag for the old relay.
- The old detached thread is not joined.
- Replacement or shutdown can proceed before the old relay is fully idle.

**Proposal:**
- Retain a relay `JoinHandle` and join with a bounded timeout during restart and graceful shutdown.
- Or gate update/action side effects on a runtime generation check immediately before each side effect.

## Telegram relay status can report running before initialization succeeds

**Severity:** Low - `src/telegram.rs:257-324`. `start_telegram_relay_runtime()` sets `runtime.running = true` before the spawned worker has completed Telegram bot initialization, then `telegram_relay_status_snapshot()` reports `running: runtime.running && !runtime.spawning` after the spawn call clears `spawning`.

That means `/api/telegram/status` can briefly report `running: true` while `run_telegram_bot_with_config()` is still blocked in startup work such as `getMe`, or is about to fail and clear the state.

**Current behavior:**
- Runtime state flips to `running = true` before the worker enters and completes bot initialization.
- `spawning` is cleared immediately after the OS thread is spawned, not after the relay is ready to poll.
- Status can present the relay as running before readiness is proven.

**Proposal:**
- Track a distinct `starting`/`ready` state in `TelegramRelayRuntime`.
- Or have the worker signal readiness only after initialization succeeds, then expose `running: true`.

## Remote delta replay fingerprints include ignored inbound `remote_id`

**Severity:** Low - `src/remote_routes.rs:527-529` fingerprints the full inbound `Session` for replay suppression even though `localize_remote_session` intentionally clears untrusted inbound `remote_id` before applying state.

Two same-revision remote `SessionCreated` events that differ only by attacker-chosen or stale `remoteId` are semantically identical after localization, but they can produce different replay keys and bypass duplicate suppression.

**Current behavior:**
- Replay key includes inbound `Session.remote_id`.
- Localization discards inbound `remote_id`.
- Same-revision duplicates that only differ by `remoteId` can republish duplicate local deltas.

**Proposal:**
- Fingerprint the normalized/localizable session payload, clearing `remote_id` before hashing.
- Add a same-revision replay test where only inbound `remoteId` differs.

## Telegram bot token is persisted as plaintext in `telegram-bot.json`

**Severity:** Medium - `TelegramUiConfig.bot_token` is serialized directly into `~/.termal/telegram-bot.json`.

Responses mask the token, but the full credential remains on disk and in temp/corrupt-backup write paths. Unix hardening sets `0600`; Windows is a P0 platform and currently has only a no-op permission hardening path. Backups, sync tools, or another local process can read the token from the settings file.

**Current behavior:**
- Saving Telegram settings writes the full bot token to `telegram-bot.json`.
- API responses return only a masked token.
- Windows file hardening does not apply an ACL or secret-store protection.

**Proposal:**
- Move the token to an OS secret store, or keep token configuration env-only until protected storage exists.
- If file persistence stays, add explicit Windows ACL handling and document backup/sync exposure.

## `App.scroll-behavior.test.tsx` mid-test mock override needs documenting comment

**Severity:** Note - `ui/src/App.scroll-behavior.test.tsx:1000-1028`. The test now overrides `scrollToMock.mockImplementation` mid-test to add stateful scrollHeight growth. The override persists for the remainder of the `it` block but is replaced by the next test's `mockScrollToAndApplyTop()` call. If a future contributor adds another assertion expecting the standard apply-top behavior in the same test, the stateful mock could mislead.

**Current behavior:**
- Mid-test mock override.
- No comment explaining the divergence.

**Proposal:**
- Add a one-liner comment near the mock override explaining why it deliberately diverges from the harness default.

## `wire_session_from_record` and `wire_session_summary_from_record` parallel paths still risk drift

**Severity:** Note - `src/state_accessors.rs:285-318`. Round 72 added comments to both helpers reminding callers to keep them in sync, but the structural risk remains: any new field added to wire `Session` must be remembered in the explicit struct literal at `wire_session_summary_from_record`. The first proposal (refactor to a single field list) was not adopted; the second (debug-assert summary equals full for shared fields) was also not adopted.

Comments are documentation-only mitigation — they don't fail when the contract drifts.

**Current behavior:**
- Round 72 added sync-reminder comments at both call sites.
- Summary form still lists fields explicitly; full form uses clone-and-modify.
- New `record.foo` fields can silently miss the summary path.

**Proposal:**
- Add a debug-assert that the summary form's output equals the full form's output for shared fields.
- Or refactor `wire_session_summary_from_record` to call `wire_session_from_record` and then strip messages/messages_loaded.
- Or introduce a separate `SessionSummary` wire struct that omits `messages`/`messages_loaded` (eliminates the duplicate field list naturally).

## `ascii_word_boundary_between` widens accept set vs. pre-refactor helpers

**Severity:** Low - `src/telegram.rs:508-521`. `ascii_word_boundary_between` returns `true` whenever EITHER `before` or `after` is non-alphanumeric. The pre-refactor `ascii_word_boundary_at` returned false when `before` was alphanumeric AND `current` was non-alphanumeric.

In practice `windows().enumerate()` only matches alphanumeric needles ("telegram"/"bot"), so the new `value[index]` byte will be alphanumeric and there's no current call-site regression — but the helper's name suggests a generic boundary check that no longer matches the original specialized behavior.

**Current behavior:**
- New helper widens the accept set.
- No current call-site regression.
- Helper name overstates generality.

**Proposal:**
- Document that this helper assumes the needle starts and ends with alphanumeric bytes.
- Or keep call-site-specific helpers.

## Test bypasses internal mutation invariants for `wire_sessions_expose_remote_owner_metadata`

**Severity:** Note - `src/state_accessors.rs:200-242`. The new test reaches into `state.inner.lock()` and directly mutates `inner.sessions[index].remote_id`. The test bypasses any normal mutation path (`session_mut_*`), so it doesn't exercise the mutation-stamp bookkeeping that real remote-proxy ingestion goes through.

**Current behavior:**
- Test directly mutates record fields.
- No public ingestion path exercised.

**Proposal:**
- Drive the same scenario through a public ingestion path (e.g., feed a remote state snapshot via `apply_remote_state_snapshot`).

## Cross-remote `remote_id` information leak in wire responses to remotes

**Severity:** Low - `src/wire.rs:490-491`, `src/state_accessors.rs:267`. Adding `remote_id: Option<String>` to wire Session means the field is now in every API response that returns a Session, including `/api/state`, session responses, SSE `SessionCreated` payloads, and responses we serve to remotes. If we proxy a session for remote A and remote B asks us for that proxy session, the wire would emit `remote_id: "remote-a-id"`, leaking our naming for A to B.

The `remote_id` is a local config alias (e.g., "ssh-lab"), not a credential, but it's now visible across remotes. Phase 1 trust model may waive this.

**Current behavior:**
- `remote_id` exposed in broad wire Session responses.
- `localize_remote_session` clears the field on inbound (correct).
- Outbound responses to remotes still include OUR alias for OTHER remotes.

**Proposal:**
- When serving wire Sessions to remotes (vs. to local UI), strip the `remote_id` field.
- Or explicitly document that local remote aliases/session-to-remote ownership are non-sensitive shared metadata under the Phase 1 trust model.

## No test for inbound attacker-chosen `remote_id` in `localize_remote_session`

**Severity:** Note - `src/remote_sync.rs:534`. The defensive clear of inbound wire `remote_id` is correct, but no test covers the case where a remote snapshot sends `remote_id` set to an attacker-chosen value. The `apply_remote_session_to_record` path should overwrite with the trusted `remote_id` from the connection, so this is safe only if that production ingestion path is exercised.

**Current behavior:**
- Defensive clear is correct.
- Existing coverage can set record metadata directly instead of feeding an inbound remote snapshot.
- No test exercises the attacker-claim case through the production localization/ingestion path.

**Proposal:**
- Add a Rust test that simulates a remote snapshot sending `Session` with `remote_id: Some("OTHER-REMOTE")` and asserts the resulting `record.remote_id` is the trusted connection id while embedded wire metadata is cleared.

## Bundled `delegation-commands.test.ts` `resolveComposerDelegationAvailability` test grew larger

**Severity:** Low - `ui/src/delegation-commands.test.ts:271-306`. The test bundles five separate availability outcomes into one `it()` block. Round 71 added a fifth assertion (the `projectId: null, remoteId: "ssh-lab"` case) extending the bundle. The first failing assertion masks subsequent ones.

This is already on the bug-ledger backlog as a P2 split task. Round 71 made the bundle larger.

**Current behavior:**
- Five outcomes bundled in one `it()`.
- Round 71 added the fifth case.

**Proposal:**
- Split into per-outcome `it()`s as the existing backlog item suggests.

## New active-color test still pins literal hex `#22c55e` indirectly

**Severity:** Note - `ui/src/panels/AgentSessionPanel.test.tsx:404-411`. Round 71 changed the test to assert `normalizeConversationMarkerColor("#22c55e")` rather than the literal hex (closes prior brittleness finding). But the test still passes the literal `"#22c55e"` to the normalizer, so the test fails if the normalizer is changed to reject `#22c55e`. The fix moved the brittleness one layer deep.

**Current behavior:**
- Test asserts via `normalizeConversationMarkerColor("#22c55e")`.
- Still hard-codes `"#22c55e"` as input.

**Proposal:**
- Construct a marker with a color produced by `DEFAULT_CONVERSATION_MARKER_COLOR` (the contract value) and assert that color round-trips through normalization.

## "keeps the draft when delegation spawn throws" doesn't assert busy state was cleared

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:7101-7128`. The `finally` arm clears `setIsDelegationSpawning(false)` only when mounted. The test exercises the throw path but does not assert `screen.queryByRole("button", { name: "Delegating..." })` is gone — only that the textarea retained the draft.

A regression that left the button stuck in "Delegating..." after a thrown error would not be caught here.

**Current behavior:**
- Tests draft preservation.
- Doesn't assert button label flipped back.

**Proposal:**
- Add `expect(screen.queryByRole("button", { name: "Delegating..." })).not.toBeInTheDocument()` or assert "Delegate" is back as the button name.

## `delegation-commands.ts` header comment doesn't reflect new composer helpers

**Severity:** Note - `ui/src/delegation-commands.ts:1-4`. Round 69 extracted composer-delegation helpers (`delegationTitleFromPrompt`, `createComposerDelegationRequest`, `resolveComposerDelegationAvailability`) into this file, but the file header still scopes the file to "Phase 2/3 delegation command surface that UI/MCP wrappers can bind to" — composer wiring helpers are not strictly transport.

Per CLAUDE.md the project enforces split-file headers describing what each file owns vs. doesn't own. The new public API surface is a distinct contract from "MCP wrapper command bindings".

**Current behavior:**
- Header still scopes file to MCP wrapper bindings.
- Composer-side helpers added without header update.
- Future readers won't know whether to add new composer helpers here or in the panel.

**Proposal:**
- Extend the file header to call out "composer-side delegation availability/title/request helpers".
- Link the consumer (`SessionPaneView.tsx`).

## `resolveComposerDelegationAvailability` round-trips `parentSession` in success outcome

**Severity:** Note - `ui/src/delegation-commands.ts:274-297`. The function accepts `parentSession: Session` and returns it back via the success branch (`{ outcome: "available", parentSession }`). The caller already holds a `Session`. This reads like the helper is producing data, but it's just a type-narrowing convenience. Future callers who already hold a `Session` may shadow it confusingly.

**Current behavior:**
- Caller passes `parentSession`.
- Success outcome returns the same `parentSession`.
- Round-trip is type-narrowing only.

**Proposal:**
- Drop `parentSession` from the success outcome (caller already has it) and only return `{ outcome: "available" }`.
- Or document why it is round-tripped (e.g., "narrowed for ergonomic destructure").

## `///` doc on `update_parent_delegation_card_locked` lacks cross-link to architecture doc

**Severity:** Note - `src/delegations.rs:1138-1142`. Round 69 spelled the full triple `(parent_session, agent_id, source)` (closes the round-68 finding), but the original proposal also asked for a cross-link to the architecture doc that describes the invariant. That part is not done.

**Current behavior:**
- Triple invariant documented in `///` doc.
- No `// see docs/...` cross-link.

**Proposal:**
- Add `// see docs/features/agent-delegation-sessions.md` (or whichever doc owns the invariant).

## `aria-busy` on a `<button>` element is unconventional

**Severity:** Note - `ui/src/panels/AgentSessionPanel.tsx:2398-2399`. `aria-busy` is specified for live regions / containers, not buttons. Most ATs do read it, but the load-bearing user-facing signal is the textual flip "Delegate" → "Delegating...". Combined with `disabled` the busy indication is functional, but `aria-busy` may be redundant noise on a button. Some screen readers double-announce as "busy, Delegating, button" which can be verbose.

**Current behavior:**
- `aria-busy={isDelegationSpawning}` on button.
- Text flips to "Delegating...".
- Some ATs may double-announce.

**Proposal:**
- Consider dropping `aria-busy` and relying on the text flip + `disabled`.
- If kept, this is fine.

## Session-switch race test doesn't assert no commit on the new session

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:7149`. The "does not clear the original draft when a delegation resolves after a session switch" test verifies the absence of `onDraftCommit("session-a", "")`. It does NOT assert that the new active session ("session-b") was NOT spuriously committed either. A regression that committed `("session-b", "")` to the wrong session would still pass.

**Current behavior:**
- Negative assertion on original session id.
- No assertion on new session id.

**Proposal:**
- Add `expect(onDraftCommit).not.toHaveBeenCalledWith("session-b", "")`.

## Unmount race test relies on console error suppression for `act` warnings

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:7102-7129`. "Ignores delegation completion after the footer unmounts" exercises only the unmount-during-await path. It does NOT verify (a) `setIsDelegationSpawning(false)` is gated by `isMountedRef.current` so the React `act` warning never appears (covered indirectly by lack of console error), or (b) `focusComposerInput()` is not called after unmount (a pending rAF could try to focus a detached node).

A regression that drops the `isMountedRef.current` check inside `finally` would not be caught directly — `act` warnings only surface in CI and may be flaky.

**Current behavior:**
- Unmount-during-await path covered.
- `isMountedRef.current` guard not directly asserted.
- `focusComposerInput()` post-unmount not verified.

**Proposal:**
- Assert `console.error` was not called with "act"/"unmounted" warnings during the test.
- Or stub `setIsDelegationSpawning` via spy and verify it isn't invoked post-unmount.

## Busy-state test relies on `Promise.resolve()` flush timing

**Severity:** Note - `ui/src/panels/AgentSessionPanel.test.tsx:6974-7008`. "Marks the delegation action busy while the spawn is in flight" relies on `await act(async () => { fireEvent.click(...); await Promise.resolve() })` to flush the busy-state commit. This is brittle to React batching changes; if a future React version delays the `setState` commit one more microtask, the test will assert the busy button before it appears.

**Current behavior:**
- Single `Promise.resolve()` flush.
- Synchronous `expect` after click.

**Proposal:**
- Use `await waitFor(() => expect(busyButton).toHaveAttribute("aria-busy", "true"))` instead of the immediate `expect`.

## `delegationTitleFromPrompt` and `resolveComposerDelegationAvailability` tests bundle multiple cases

**Severity:** Note - `ui/src/delegation-commands.test.ts:230-251` (`delegationTitleFromPrompt` bundles 4 cases) and `:271-293` (`resolveComposerDelegationAvailability` bundles 4 outcomes) each pack multiple cases into one `it`. Same bundled-test anti-pattern previously called out for the Rust telegram tests.

**Current behavior:**
- Two new test files bundle 4 cases each.
- Failure messages cluster.

**Proposal:**
- Split into single-case `it`s, or restructure as `it.each([...])`.

## `createComposerDelegationRequest` is composer-scoped but generic name

**Severity:** Note - `ui/src/delegation-commands.ts:254-268`. The helper accepts `Pick<Session, "agent" | "model">` and emits a fixed `mode: "reviewer"` / `writePolicy: { kind: "readOnly" }`. The current name implies "any composer-driven delegation" but the request shape is read-only-reviewer-only. A future consumer (e.g., MCP wrapper) wanting a non-reviewer or write-policy variant would mis-name intent if they reused this builder.

**Current behavior:**
- Composer-scoped name.
- Fixed reviewer/readOnly request shape.
- No comment guiding alternative builders.

**Proposal:**
- Keep the composer-scoped naming and add a note that other delegation kinds should add their own builder.
- Or rename to `createReadOnlyReviewerDelegationRequest` to make the constraint explicit.

## `enableLocalDelegationActions` is binary all-or-nothing without contract doc

**Severity:** Note - `enableLocalDelegationActions` is passed as a single all-or-nothing boolean at `ui/src/SessionPaneView.render-callbacks.tsx:107,154,400`. The wire/docs/UX could grow to allow Open without Cancel (e.g., "view but don't mutate" remote sessions); the binary flag forecloses that. No comment captures the design intent of "all three are routed together by construction."

**Current behavior:**
- One boolean gates all three actions (open, insert, cancel).
- Future contributors will be tempted to thread separate flags.
- Symmetry intent not documented.

**Proposal:**
- Document the all-or-nothing semantic in a comment.
- Or restructure as an action-config object the parent fills.

## Round 71 boundary helpers `ascii_word_boundary_at` and `ascii_word_boundary_after` are now byte-identical

**Severity:** Low - round 71 unified both helpers around `ascii_word_boundary_between(value[index-1], value[index])` (closing the round-67 ledger entry "no longer mirror each other"). However, the two functions now have **identical** call shapes — they differ only in their name. A reader sees `ascii_word_boundary_at(haystack, idx)` and `ascii_word_boundary_after(haystack, idx + needle.len())` and assumes different semantics, but the bodies are byte-identical.

`src/telegram.rs:492-521`. `ascii_word_boundary_after`'s `before`/`after` reference the byte BEFORE the call's `index` — the same pair as `ascii_word_boundary_at(value, index)`.

**Current behavior:**
- Both helpers delegate to `ascii_word_boundary_between` with identical argument shapes.
- Names suggest distinct semantics; bodies are identical.
- Misleading for future contributors.

**Proposal:**
- Inline both helpers into a single call at `ascii_bytes_contains_word_ignore_case`.
- Or keep one helper and have the other delegate with an explanatory comment about intent at the call site.

## `ascii_word_boundary_after` camelCase only handles lower→upper transitions

**Severity:** Low - `src/telegram.rs:502-510`. The boundary detection treats `botX` as separable but not `BOTx` — `BOTtoken` (a 3-letter uppercase tag followed by a lowercase tail) would not be recognized as a context match. A construct like `TELEGRAMbot ...` or `BOTtoken=...` would not match `bot` or `telegram` context. Unlikely in real config, but worth pinning.

**Current behavior:**
- `botToken` matches (lower→upper).
- `BOTtoken` does NOT match (upper→lower).
- Asymmetric camelCase handling.

**Proposal:**
- Either accept and document the asymmetry (camelCase boundaries are conventionally lower→upper).
- Or extend to also accept upper→lower and add a corresponding test case.

## Round 67 added 3 more bundled cases to `telegram_generic_token_redaction_requires_telegram_or_bot_word_context`

**Severity:** Note - round 67 added three more bundled cases (`telegram_bot: { token: ... }`, `telegram-bot token=...`, `telegramBot token=...`) into `src/tests/telegram.rs:782-796`. The bundled-test anti-pattern is already an open ledger entry that explicitly called out new bundle additions; the entry "Bundled telegram redaction tests keep growing despite the explicit anti-pattern flag" gets larger again with 8 total bundled assertions in this single test.

**Current behavior:**
- Round 67 added 3 more bundled assertions.
- 8 total bundled assertions in this single test.

**Proposal:**
- Promote each new namespaced-context case to its own `#[test]`.
- Or convert the bundle to a `(input, expected, name)` table+helper.

## `enableLocalDelegationActions` flag flips invalidate `MessageCard` memo

**Severity:** Low - three callbacks at `ui/src/SessionPaneView.render-callbacks.tsx:355-371` are passed as `enableLocalDelegationActions ? handler : undefined`. When the flag flips between renders, three new `undefined` slots vs. three stable function refs change the `MessageCard` props and re-render the entire parallel-agents card. `MessageCard` is `memo`-wrapped — passing `undefined` toggles invalidate the memo check on every flag flip.

**Current behavior:**
- Flag flip → three undefined props → memo invalidation → full card re-render.
- Acceptable today (project remoteId rarely flips).

**Proposal:**
- Memoize the three "disabled" undefined values as a single object.
- Or pass the flag itself through and let the consumer decide.

## Composite key used for React identity + pendingActionKeys but bare `agent.id` for handlers

**Severity:** Note - `ui/src/message-cards.tsx:2210-2219` uses `${agent.source}:${agent.id}` for React identity AND `pendingActionKeys`, but the per-row click handlers still pass the bare `agent.id` to `onOpenAgentSession`/`onInsertAgentResult`/`onCancelAgent`. Self-consistent today (only delegation rows have action buttons), but a future change that allows tool-source action handlers would bypass source disambiguation at the handler boundary.

**Current behavior:**
- React keys: `${source}:${id}`.
- Pending action keys: `${source}:${id}:${action}`.
- Handler args: bare `agent.id`.

**Proposal:**
- Either pass the composite identity to the handler.
- Or document the gating contract on `runAgentAction` / the per-action callbacks.

## Mixed-source same-id MessageCard test doesn't pin pending-key composite contract

**Severity:** Low - `ui/src/MessageCard.test.tsx:179-234`. The test asserts cancel works on the delegation row but does NOT assert: (a) no React duplicate-key warning across re-renders, (b) the tool row is unaffected by clicks on the delegation row, (c) the pending-action key disambiguates so a stale tool-source key doesn't interfere with the delegation cancel.

The pending-state contract `agent.source:agent.id:cancel` is the load-bearing fix but only the React-key collision is verified — a regression that flipped the pending-key composite back to bare id but kept the React `key` composite would still pass. Also: the test renders without `DeferredHeavyContentActivationProvider`.

**Current behavior:**
- React key collision verified.
- Pending-key disambiguation unverified.
- Test missing DeferredHeavyContentActivationProvider wrapper.

**Proposal:**
- Trigger two sequential actions on the same source/id pair, assert one is rejected as pending.
- Mirror the wrapping pattern from sibling tests for the provider.

## "Renders remote delegation progress as display-only" test under-covers regression surface

**Severity:** Low - `ui/src/SessionPaneView.render-callbacks.test.ts:586-622`. The new test only asserts no buttons render. It doesn't assert that the parent's three handler functions were never invoked, doesn't switch the flag mid-render, and doesn't test mixed-row scenarios. A regression that dropped only one action's guard would still pass — all three render no buttons together.

**Current behavior:**
- Single "no buttons rendered" assertion.
- No per-action regression coverage.
- No mid-render flag-flip test.

**Proposal:**
- Add an assertion `expect(params.onComposerError).not.toHaveBeenCalled()` after rendering.
- Exercise a flag flip mid-test.
- Or split into per-action coverage.

## `agent-delegation-sessions.md` cancel doc doesn't link wire status to UX phrases

**Severity:** Note - round-67's new paragraph at `docs/features/agent-delegation-sessions.md:213-217` documents that cancel responses can return `queued` and `running`, but doesn't mention that the wire-format status string is reused as the user-visible label inside the UI's "Delegation child session is unavailable (...)" message. The previous docs explicitly warned against branching on user-visible message text; this change moves to a curated set of phrases ("already X" / "still X") but the protocol-vs-UX relationship is undocumented. The mapping lives only in the TS source.

**Current behavior:**
- Wire status table updated.
- UX phrase mapping not cross-linked.

**Proposal:**
- Cross-link the wire `status` table to the UX label table.
- Or document the verb-form mapping ("queued / running → 'still'; completed / failed / canceled → 'already'") in the same section.

## "running with no child session" protocol contract unclear

**Severity:** Note - `ui/src/SessionPaneView.render-callbacks.tsx:213-238`. When `getDelegationStatusCommand` returns a missing `childSessionId`, the UI dispatches `onComposerError("...still running")`. The wire contract today is that delegations in `running` state DO have a child session, so the "still running" phrase implies an unexpected backend state.

**Current behavior:**
- UI surfaces "still running" for missing child session.
- Protocol contract unclear: is this state legitimate?

**Proposal:**
- Pin the protocol contract in `agent-delegation-sessions.md` (running implies childSessionId is present) and treat the case as a 5xx-equivalent UX.
- Or document the legitimate "running but child not yet attached" case.

## `clears pending parallel-agent actions when an action rejects` test doesn't verify rejection suppression

**Severity:** Low - the test at `ui/src/MessageCard.test.tsx:321-365` asserts the visible UI side-effect (button re-enabled) which is satisfied by the `.finally()` alone. Removing the `.catch(() => undefined)` would still pass this test (the `.finally()` runs and resets the pending state). The test silently observes the finally's side-effect, not the catch's noise-suppression role.

The actual round-65 fix it claims to validate is the `.catch(() => undefined)` that prevents an unhandled rejection.

**Current behavior:**
- Test asserts cancel button is re-enabled.
- Both `.finally()` and `.catch()` paths satisfy that assertion.
- A regression dropping the `.catch(() => undefined)` would still pass.

**Proposal:**
- Capture unhandled-rejection events via `process.on("unhandledRejection", ...)` (Node) or `window.onunhandledrejection` (jsdom).
- Assert no console error or no `vi.spyOn(console, "error")` was emitted.
- Or refactor the test name and assertion to describe what is being pinned.

## `debug_assert_eq!` for `agent.source` clobber check is no-op in release builds

**Severity:** Low - round-65 swapped the unconditional clobber for `debug_assert_eq!` at `src/claude.rs:581-585`. Production builds (release mode) will silently let a non-`Tool` value persist if a future code path ever drops a `Delegation`-sourced agent into this update branch. The contract is encoded but not enforced for release-mode users.

The previous round-64 review entry called out the clobber as future-proofing risk. Round-65's fix preserves the runtime behavior in release while documenting the intent in debug.

**Current behavior:**
- `debug_assert_eq!(agent.source, ParallelAgentSource::Tool, ...)`.
- Release builds skip the assertion.
- Future regression scenario silently succeeds in release.

**Proposal:**
- Upgrade to a release-mode guard that warns and resets, or errors before a mismatched source can drive UI routing.
- Keep the debug assertion only as a supplemental development check.

## SSE-envelope test only asserts the second `parallelAgentsUpdate` event, skips the first

**Severity:** Low - `src/tests/http_routes.rs:586-662` `state_events_route_streams_parallel_agents_update_sources` consumes the first `upsert` event via `let _ = next_sse_event(&mut body).await` and only asserts the second update event. The first publish (the `create` event) is silently swallowed.

A regression where source serialization works for updates but not creates would still pass.

**Current behavior:**
- First create event consumed without assertion.
- Second update event asserted (`tool` and `delegation` source).
- Create-path serialization regression would not surface.

**Proposal:**
- Also assert the first event's source values (parse it instead of `let _ = ...`).
- Or add a sibling test for the create path.

## `delegation_parent_card_update_ignores_tool_source_id_collision` only manually constructs the collision

**Severity:** Low - the new test at `src/tests/delegations.rs:655-746` constructs the collision by manually inserting a tool-source row with the same id as the delegation. The production paths that could create such a collision (Claude task path emitting a tool-source row with a delegation-id-overlapping uuid) are not exercised.

A regression in delegation-id generation (e.g., switches from uuid to deterministic source) could create real collisions and this test wouldn't catch it.

**Current behavior:**
- Test manually inserts a same-id collision.
- Production cross-path collision not exercised.

**Proposal:**
- Add a sibling test that drives both the Claude task path and the delegation creation path with overlapping ids.
- Or document the assumption that uuid id spaces don't collide deterministically.

## `ascii_bytes_contains_word_ignore_case` is O(n×m) byte scanning per candidate

**Severity:** Low - `src/telegram.rs:474-490`. The 96-byte window keeps the cost bounded, but the function runs over every prefix containing a generic `token=` substring inside log detail strings up to `MAX_LOG_DETAIL_CHARS = 256`. Practical cost is negligible today; flagging because the helper is now load-bearing for the Telegram redaction allowlist gate.

**Current behavior:**
- Per-candidate O(n×m) byte scan.
- 96-byte cap bounds the cost.
- No `memchr` or vectorized comparison.

**Proposal:**
- Document the O(n×m) bound and the 96-byte cap.
- Or use `memchr`/`bytes_eq_ignore_ascii_case` patterns if input ever grows.

## `session-reconcile.test.ts` "only source changes" test only exercises one direction

**Severity:** Note - `ui/src/session-reconcile.test.ts:1031-1064`. Exercises a `tool` → `delegation` source flip but not `delegation` → `tool` (the reverse). `reconcileParallelAgentsMessage` is a pure structural compare; both directions should produce a fresh reference. A regression that flipped only one direction is still possible to slip past.

**Current behavior:**
- One-direction source flip covered.
- Reverse direction unverified.

**Proposal:**
- Add the reverse case.
- Or note the symmetry assumption in the test.

## `cancelDelegation` `it.each` test pins running/completed/canceled as identical

**Severity:** Note - `ui/src/SessionPaneView.render-callbacks.test.ts:493-527` `it.each(["canceled", "completed", "running"])` lumps three statuses behind a single assertion. The user-visible UX for `running` (cancel acknowledged, child still running) deserves a different message than `completed` or `canceled`, but the test pins them as identical no-error outcomes — locking in the bugs.md-flagged inconsistency.

**Current behavior:**
- Three statuses asserted identically.
- The test will need updating when running gets its own UX.

**Proposal:**
- Add a comment explaining the test pin is the current-but-flagged behavior.
- Or split `running` into its own test scoped to the bugs.md follow-up.

## `#[non_exhaustive]` removal lacks comment about future re-add

**Severity:** Note - `src/wire_messages.rs:132-139`. Round 66 removed `#[non_exhaustive]` (correctly, since it's a no-op on a private enum). But the rationale isn't documented in the source. If `ParallelAgentSource` ever moves to a public module, the attribute would need to be restored for downstream consumers.

**Current behavior:**
- Attribute removed.
- No `///` comment notes the conditional re-add.

**Proposal:**
- Add a one-line `///` comment: "If this enum becomes pub, restore `#[non_exhaustive]` for downstream consumers."

## `(message_id, agent_id, source)` disambiguating key not documented in architecture

**Severity:** Note - `src/delegations.rs:1154-1156` and the wire contract for `parallelAgentsUpdate` already carry `source`. But `docs/architecture.md` doesn't mention that backend lifecycle updates filter by source. A frontend reconciler implementer might assume `agent.id` is unique within a `parallelAgents` message; the new test explicitly creates a same-id collision, which the docs don't reflect.

**Current behavior:**
- Backend filters by `(message_id, agent_id, source)`.
- Architecture doc describes the wire role of `source` but not the disambiguating-key invariant.

**Proposal:**
- Add a sentence to `docs/architecture.md` or the parallel-agent-source feature brief noting that `(message_id, agent_id, source)` is the disambiguating key, not `(message_id, agent_id)` alone.

## Bundled telegram redaction tests keep growing despite the explicit anti-pattern flag

**Severity:** Low - the bug-ledger entry "Round-64 added another bundled telegram redaction test" already calls out the bundled-test anti-pattern. Round 65 added two more cases to `..._respects_context_and_thresholds` (`telegram_adjacent_token_key`, `bot_adjacent_token_key`) and three more to `..._handles_escaped_and_telegram_specific_contexts` (`bearer_equals`, `authorization_equals`, `lower_bearer_equals`).

Each additional bundled case makes the failure message harder to interpret if a future regression touches one path. Three bundled tests now exhibit the pattern; round 65 added 5 more bundled cases on top of the prior round-64 additions.

The new `ambiguous_token_key` case (the `token=` bare-key behavior flip) is pinned only as one assertion deep in a bundle. Round-65 `telegram_adjacent_token_key` / `bot_adjacent_token_key` cases (the substring heuristic) similarly hide load-bearing checks inside the bundles.

**Current behavior:**
- Three bundled tests now exhibit the pattern.
- Round 65 added 5 more bundled cases.
- Failure messages still cluster at one line.

**Proposal:**
- Promote each behavior change to its own `#[test]`.
- Or use a small `(input, expected_redacted, name)` table fixture and one helper.

## `docs/architecture.md` `ParallelAgents` row blurs which agents emit which source

**Severity:** Note - the round-64 architecture doc at `docs/architecture.md:711` documents the contract but the "typical source" cell now reads "Delegation progress … or tool progress". Combined with the surrounding text describing `SubagentResult` as "Codex subagent/task results", a reader may infer that delegation-progress is Codex-only when in practice both Claude and Codex paths write `Tool` and the delegation runtime writes `Delegation` regardless of agent backend.

**Current behavior:**
- "typical source" cell reads as if delegations are Codex-specific.
- Both agent backends emit `Tool`-source.

**Proposal:**
- Add a parenthetical "(any agent backend)".
- Reword `SubagentResult` as agent subagent/task results, not Codex-only results.

## `docs/features/telegram-ui-integration.md` 422 disambiguation paragraph depends on human-readable error text

**Severity:** Medium - the 422 disambiguation paragraph at `:261-267` asks clients to branch on `error.message` text. The new compatibility note at `:269-273` describes a behavior change where validation now reports `unknown default Telegram session project` instead of `default Telegram session must belong to the default project` - exactly the kind of message-text change the prior paragraph warns clients about.

**Current behavior:**
- Paragraph 1: "Clients should use the error message".
- Paragraph 2: "We changed the error message; clients depending on the old text break."
- `ApiError` exposes only `{ error }`, with no stable machine-readable kind.

**Proposal:**
- Add a separate machine-readable error-code field.
- Or use distinct statuses for local request-shape failures versus Telegram token/auth failures.

## `scheduleLayoutRefresh` callback double-refreshes viewport snapshot per flush

**Severity:** Medium - the rAF callback at `ui/src/panels/conversation-overview-controller.ts:241-252` calls `refreshLayoutSnapshot()` (which already updates `viewportSnapshot` via `extractConversationOverviewViewportSnapshot(nextSnapshot)`) AND THEN `refreshViewportSnapshot()` (which makes a separate `getViewportSnapshot()` call and updates the same `viewportSnapshot` again). Two consecutive `setViewportSnapshot` calls per flush.

The two calls produce different values (one derived, one fresh). React 18 batches both within an effect, but `refreshLayoutSnapshot` and `refreshViewportSnapshot` are independent setters — either the second overwrites the first (waste) or vice versa.

**Current behavior:**
- Both setters called from the same rAF flush.
- Two `setViewportSnapshot` updates per scheduled refresh.
- Either is wasted depending on which value wins.

**Proposal:**
- Decide whether the layout-derived viewport snapshot or the fresh `getViewportSnapshot()` is the source of truth.
- If both are needed, fold into a single update path.

## Conversation overview viewport translation can reuse a stale same-size tail window or cross-session translation

**Severity:** Medium - tail-window viewport translation validates compatibility only by `messageCount` and lacks any session identity guard.

`ui/src/panels/conversation-overview-map.ts:299-333`. During streaming, the visible tail can shift from one 20-message window to another while the viewport snapshot still reports the same count, allowing an old translation offset to project the rail viewport marker onto the wrong transcript region. Additionally, a snapshot from a different session that happens to share the same `messageCount` could trigger the translation branch, projecting one session's viewport against another session's overview.

**Current behavior:**
- `viewportSnapshotTranslation` carries only `snapshotMessageCount`.
- `projectConversationOverviewViewport` accepts any later viewport snapshot with the same count.
- Same-size shifted tail windows can reuse a stale `sourceTopOffsetPx`.
- No `sessionId` equality guard; cross-session reuse is structurally possible.

**Proposal:**
- Include a cheap window identity, such as first/last message id or a layout window version, in both the translation and viewport snapshot.
- Add `sessionId` equality as part of the translation gate (and persist `sessionId` on the translation).
- Add regressions for a same-size shifted tail window and for cross-session viewport projection.

## Returning to bottom leaves stale virtualized scroll-kind classification

**Severity:** Medium - bottom re-entry clears the idle-compaction timer but leaves `lastUserScrollKindRef.current` set to `"incremental"`.

`ui/src/panels/VirtualizedConversationMessageList.tsx:2726`. The cleared idle timer is normally what expires scroll-kind state, so later native scrollbar movement without wheel/key/touch input can inherit the stale classification.

**Current behavior:**
- Native downward scroll near bottom sets `lastUserScrollKindRef.current = "incremental"`.
- The idle-compaction timer is cleared at the same boundary.
- Later native scrolls can reuse the cached scroll kind indefinitely.

**Proposal:**
- Clear `lastUserScrollKindRef` on bottom re-entry, or expire the one-tick override with a short timestamp/timer.
- Add a regression that returns to bottom, then performs a native scroll with no preceding wheel/key/touch input.

## `resolveViewportSnapshotTranslation` only happy-path tested

**Severity:** Medium - the new translation helper has six negative branches (`layoutSnapshot === null`, `layoutSnapshot.messageCount >= estimatedRows.length` (full-transcript no-op), `layoutSnapshot.messages.length === 0`, `firstRowIndex < 0` (orphan tail message id absent from full transcript), `!hasContiguousWindow`, drift case where `viewportSnapshot.messageCount !== snapshotMessageCount`). The new test at `conversation-overview-map.test.ts:452-506` covers only the happy path and reuses the layout snapshot as the viewport snapshot.

`ui/src/panels/conversation-overview-map.test.ts:452-506`. Each negative branch silently returns null and falls through to the legacy projection. The current happy-path test also does not prove that `projectConversationOverviewViewport` handles a newer live viewport snapshot independently from the layout snapshot. A regression that flipped any of these guards or stale-live-viewport handling would only surface as misaligned viewport markers in production.

**Current behavior:**
- One happy-path test exercises contiguous tail window with `messageCount < estimatedRows.length`.
- That test passes the same snapshot as both layout and viewport input.
- Negative branches silently fall through.
- Drift case (where viewport snapshot count differs from translation snapshot count) is uncovered.

**Proposal:**
- Add focused tests for each negative branch via `buildConversationOverviewProjection` then probing `projection.viewportSnapshotTranslation` for null in each case.
- Add a separate live viewport snapshot case with a different `viewportTopPx`.
- Add a drift-case test where viewport snapshot count differs from translation snapshot count and the legacy projection path is exercised.

## `resolvePrependedMessageCount` only happy path tested

**Severity:** Medium - the new pure helper has six branches (cross-session, empty previous window, no growth, partial overlap, no first-message match, contiguous match at index 0). Only the happy path (genuine prepend at startIndex>0) is exercised end-to-end via the prepend integration test.

`ui/src/panels/VirtualizedConversationMessageList.tsx:413-441`. The integration test combines layout, scroll, and DOM behavior, so a regression in the matcher (off-by-one in `maxStartIndex`, accepting a non-contiguous overlap) might be masked by the looser scroll-position assertion.

**Current behavior:**
- Helper is not exported; no direct unit test.
- Edge cases (empty before/after, single message, all messages new vs. all stale, partial overlap, session change) unverified.

**Proposal:**
- Export `resolvePrependedMessageCount` (or move it to a sibling module) and add unit tests for each branch with `MessageWindowSnapshot` fixtures.

## `mountedRangeWillChange` early-return not pinned by test

**Severity:** Medium - the new `mountedRangeWillChange || !preservedAnchorSlot` early-return at `VirtualizedConversationMessageList.tsx:1561-1567` is the load-bearing fix for the "single-frame visual jump" issue (which was removed from bugs.md by this round). The integration test was simultaneously rewritten to drop `await waitFor(...)` in favor of a synchronous assertion. There is no test that pins the new condition — i.e., that when a prepend forces a range change AND the anchor is mounted, no stale-rect scroll write is emitted before the followup effect re-anchors.

`ui/src/panels/VirtualizedConversationMessageList.test.tsx:484-492`. The test asserts the post-flush position, not the absence of the intermediate stale write. A regression that flipped back to the old `!preservedAnchorSlot` gate would still pass the new test (the followup effect catches up either way).

**Current behavior:**
- New test asserts post-flush scroll position synchronously.
- No assertion verifies the absence of an intermediate stale-rect scroll write.
- A regression to the prior gate would not be caught.

**Proposal:**
- Assert `harness.scrollWrites` between the prepend and the followup effect — specifically, that no scroll write lands at the stale `targetScrollTop` value computed from pre-mutation rects.
- Or use `hydrationScrollWrites.every(...)` to assert all writes track the final scroll position.

## rAF-coalesced `messageCount` refresh now lags `layoutSnapshot.messageCount` behind `messageCount` by one frame

**Severity:** Medium - the round-62 fix routes the `messageCount`-driven `refreshLayoutSnapshot` through the same rAF-coalesced scheduler as the steady-state effect. Coalescing is correct, but downstream consumers reading `layoutSnapshot.messageCount` synchronously inside the same React render now see a stale snapshot until the rAF flushes.

`ui/src/panels/conversation-overview-controller.ts:241-274`. The new "coalesces ready layout refreshes" test pins this behavior — `expect(layout-message-count).toHaveTextContent("90")` immediately after rerender to 120/140, then 140 only after `flushNextFrame`. The active-streaming case (per-chunk delta increments) is the most sensitive — every assistant chunk leaves snapshot consumers one rAF behind.

**Current behavior:**
- All `refreshLayoutSnapshot` calls go through rAF scheduling.
- Synchronous consumers see stale snapshots within the same React frame.
- The new test codifies the lag.

**Proposal:**
- Confirm whether downstream consumers (rail tail items, viewport projection) rely on synchronous freshness.
- If yes: add a fast-track path — when `messageCount - layoutSnapshot.messageCount > N`, refresh synchronously to avoid >1 rAF lag.
- Otherwise document the lag explicitly so future consumers know the snapshot lags by up to one rAF behind `messageCount`.

## Session-id guard inside scheduled refresh callbacks unverified

**Severity:** Medium - the new `scheduleLayoutRefresh` / `scheduleViewportRefresh` callbacks at `ui/src/panels/conversation-overview-controller.ts:247-274` capture `expectedSessionId = overviewSessionIdRef.current` and compare on dispatch. The guard is correct in principle but no test exercises a session change mid-rAF.

A regression that dropped the guard would still pass every existing test, but a session switch between schedule and flush would clobber the new session's snapshot with a stale read from the previous session. Additionally, the cleanup path only cancels via `cancelLayoutRefreshFrame` / `cancelViewportRefreshFrame`; if a `scheduleLayoutRefresh` was queued but already entered the rAF callback, only the session-id guard prevents a stale write.

**Current behavior:**
- Session-id captured at schedule time, compared at flush time.
- No test crosses the session-id boundary mid-rAF.
- A regression dropping the guard would still pass.

**Proposal:**
- Extend the controller test harness so a `rerender({ sessionId: "session-b", messageCount: 90 })` between the schedule and the flush asserts the layout snapshot is NOT updated to the stale session-a result.
- Document the lifetime contract for `overviewSessionIdRef` vs `isRailReady`.

## `markUserScroll` anchor speculation captures approximate touch offsets

**Severity:** Medium - speculative offset adjustment `viewportOffsetPx - inputScrollDeltaY` applied unconditionally on every input event. For touch events, `touchDeltaY` is the FINGER delta (not the scroll delta). When user touches a non-scrollable region, swipes within an iframe, or hits a scroll boundary, the anchor's `viewportOffsetPx` ends up off by the would-be delta.

`ui/src/panels/VirtualizedConversationMessageList.tsx:2767-2778`. The downstream prepended-restore effect uses this anchor as a scroll target.

**Current behavior:**
- Speculative offset applied to anchor on every input event with non-null delta.
- Touch deltas approximate scroll deltas.
- At scroll boundaries the speculation is wrong.

**Proposal:**
- Defer the speculative offset until the native scroll handler observes an actual `scrollTop` change.
- OR drop the speculation and re-capture the anchor inside the prepended-restore effect.

## `isPurePrepend` strict gate drops bottom-gap preservation when concurrent append happens

**Severity:** Medium - in streaming sessions hitting hydration, the user-near-bottom-escape-upward scenario is exactly when a new assistant chunk lands alongside the prepend — making `isPurePrepend` false. The bottom-gap signal is silently consumed.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1473-1479`. With any trailing growth, the bottom-gap path is bypassed and `pendingBottomGapAfterPrepend` is cleared without being applied.

**Current behavior:**
- Strict `isPurePrepend` gate.
- Concurrent append makes the gate false.
- Bottom-gap preservation silently consumed.

**Proposal:**
- Relax to `pureOrAppendingPrepend` allowing N appended messages alongside the prepend.
- OR re-store the bottom gap if the gate fails so the next layout effect can still consume it.

## `skipNextMountedPrependRestoreRef` cleared by new prepend effect — silently overrides user-scroll intent

**Severity:** Medium - the new prepend-anchor `useLayoutEffect` unconditionally writes `skipNextMountedPrependRestoreRef.current = false` whenever a prepend is detected. If user wheels (sets it true), then a transcript prepend fires before the prior effect drains, the skip flag is silently cleared.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1520-1521`.

**Current behavior:**
- `markUserScroll` sets `skipNextMountedPrependRestoreRef = true`.
- New prepend effect unconditionally clears it.
- User-scroll intent lost on prepend.

**Proposal:**
- Respect the skip flag if set; only clear when no prior intent exists.

## `pendingPrependedMessageAnchorRef.remainingAttempts = 3` magic number with no telemetry on exhaustion

**Severity:** Medium - if the anchor never re-mounts (e.g., user scrolls away during chained re-renders), `remainingAttempts` decrements to 0 and gives up — leaving `latestVisibleMessageAnchorRef` stale. No log when this exhausts.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1523-1529`. 3 is arbitrary with no test pinning the boundary.

**Current behavior:**
- Three retry attempts.
- Silent exhaustion if all fail.
- No telemetry signal.

**Proposal:**
- Log when exhaustion occurs, OR
- Make the anchor invalidate on user-scroll inside the followup effect.

## `latestVisibleMessageAnchorRef` capture re-runs on every native scroll tick

**Severity:** Medium - useLayoutEffect deps include `viewportScrollTop` (state). On every native scroll tick the viewport state updates → effect re-runs → `getBoundingClientRect()` over all mounted slots. For a 600+ message tail with mounted range covering 50+ slots, this is per-scroll-tick rect reads.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1645-1651`.

**Current behavior:**
- Anchor capture re-runs on every viewport scroll-state update.
- Each run does N `getBoundingClientRect()` reads.

**Proposal:**
- Throttle via rAF.
- OR only capture when prepend is imminent.

## `validate_and_normalize_telegram_config` orphan-session error message reorder is wire-visible

**Severity:** Medium - same input now produces a different error message. Round 60 reordered the `known_projects` check to run before the project-mismatch check; the user-visible error message changes from `"default Telegram session must belong to the default project"` to `"unknown default Telegram session project P_unknown"` for the case where session points to an unknown project.

`src/telegram_settings.rs:204-208`. Wrappers that case-match on the previous error string see different output.

**Current behavior:**
- Validation error precedence reordered.
- Error message changes for the same input.
- No documentation of the wire-shape change.

**Proposal:**
- Add a wire-shape change note in `docs/features/telegram-ui-integration.md`.
- Document the validation error precedence: known_projects > project-mismatch.

## `status-fetch-failed` priority drops mixed-instance signal silently

**Severity:** Medium - round 60 documents the priority rule, but `applyCurrentInstanceStatusBatchResponses` filters out responses whose `serverInstanceId` differs from the previous baseline. So if a status batch sees one fetch fail AND collected responses come from a NEW instance, the mixed-instance information is silently dropped: no `recoveryGroups` for the new-instance responses.

`ui/src/delegation-commands.ts:594-617` + `docs/features/agent-delegation-sessions.md:245-260`.

**Current behavior:**
- `status-fetch-failed` masks concurrent server restart.
- Wrappers receive `status-fetch-failed` packet without instance-change diagnostic.
- Doc claims status-fetch priority but doesn't note the partial-information loss.

**Proposal:**
- Document the partial-information loss in `agent-delegation-sessions.md` so wrappers know `status-fetch-failed` may hide a concurrent server restart.

## Tail-window thresholds drift apart: 101-message hydration vs 512-message render mode

**Severity:** Low - `SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101` triggers backend tail hydration of `SESSION_TAIL_FIRST_HYDRATION_MESSAGE_COUNT = 20`, but the UI tail-window only activates when `INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES = 512`. Sessions in [101, 512] messages get a 20-message tail hydration response while the conversation page does not enter tail-window mode.

`ui/src/panels/AgentSessionPanel.tsx:129-130`, `ui/src/app-live-state.ts:272-273`. The 101/512 asymmetry is undocumented; future tuning lands inconsistently.

**Current behavior:**
- Sessions ≥101 messages get a 20-message tail hydration.
- Sessions <512 messages render in normal (non-tail-window) mode.
- For sessions in [101, 512], demand-hydration UX is unreachable because tail-window is not active.

**Proposal:**
- Align the two thresholds, or add a contract comment explaining the deliberate asymmetry (e.g. why 101 is the hydration-shape threshold and 512 is the rendering-window threshold).

## `previousMessageWindowRef` lazy-init makes early-return unreachable

**Severity:** Low - the new lazy-init at `VirtualizedConversationMessageList.tsx:525-531` unconditionally sets `previousMessageWindowRef.current` to a non-null `MessageWindowSnapshot` during the first render. By the time the `useLayoutEffect` runs at line 1406, the ref is already non-null, so the `if (previousWindow === null) return;` early-return at line 1414 is unreachable.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1414-1416`. Correctness-neutral (the next downstream check `resolvePrependedMessageCount` handles the same-content case naturally), but the dead-code obscures intent.

**Current behavior:**
- First-render initializer sets ref to non-null.
- `if (previousWindow === null) return;` never fires.
- A reader thinks "there's a real first-mount short-circuit" when there isn't.

**Proposal:**
- Either remove the dead check, OR reset `previousMessageWindowRef.current = null` between session boundaries inside an effect to make the check live.

## `resolveViewportSnapshotTranslation` does an O(N+M) full-transcript pass per layout snapshot rebuild

**Severity:** Low - the helper uses `Array.prototype.findIndex` to find the first layout-snapshot message in `estimatedRows`, then `.every` to verify a contiguous window. For a 1000-message transcript with a 100-message tail, `findIndex` scans up to 900 entries and `.every` walks up to 100. Acceptable for current sizes, but `buildConversationOverviewProjection` runs on every layout snapshot change.

`ui/src/panels/conversation-overview-map.ts:903-942`. The active "messageCount-driven refreshLayoutSnapshot" effect (already in bugs.md as Medium) makes this snowball with the streaming layout-refresh cadence.

**Current behavior:**
- O(N) `findIndex` over full estimatedRows.
- O(M) `.every` over layoutSnapshot.messages.
- Runs on every layout snapshot rebuild during streaming.

**Proposal:**
- Build a `Map<messageId, rowIndex>` once for `estimatedRows` and reuse for the lookup.
- Or compare `layoutSnapshot.messages[0].messageId` to the trailing-N message ids (since the layout window is always near the end).

## `handleInsertParallelAgentResult` accepts any status without warning

**Severity:** Low - the handler at `ui/src/SessionPaneView.render-callbacks.tsx:217-250` accepts any `result.status` (including `"failed"`, `"canceled"`) and inserts the formatted prompt without warning. The formatter prefixes `Delegation result (failed) from child-1:` as a status disclaimer in the prompt body, but the user has no UI-level confirmation they're inserting a failed result.

A user clicking "Insert result" on an `error`-status agent gets the failure summary inserted as if it were guidance — fine for an aware user, confusing for an unaware one. Round-62 noted this; round-63 still threads any result.

**Current behavior:**
- Status disclaimer in prompt body only (no UI prompt).
- `status === "failed"` inserts failure summary as if it were guidance.
- Empty/whitespace summary falls back to "No summary provided." with no further hint.

**Proposal:**
- Confirm with the user via a "do you really want to insert a failed result?" pattern when `result.status !== "completed"`.
- Or surface a non-blocking notice via `onComposerError`.

## `runAgentAction` not memoized; recreated per render

**Severity:** Low - `runAgentAction` at `ui/src/message-cards.tsx:2120-2140` is defined inline inside the component without `useCallback`. It now reads pending state through refs, so the stale-closure bug is fixed, but the helper and per-row inline arrow handlers still get fresh identities on every render.

Acceptable today; flagging for future re-use when N grows.

**Current behavior:**
- `runAgentAction` recreated per render.
- Per-row click handlers also recreated.
- No `useCallback` memoization.

**Proposal:**
- If parallel-agent counts grow, memoize `runAgentAction` with `useCallback` and stable deps.
- Or extract `ParallelAgentRow` per existing tracked entry.

## messageCount-driven effect has no cleanup; pending rAF survives across rerenders

**Severity:** Low - the messageCount effect at `ui/src/panels/conversation-overview-controller.ts:446-457` calls `scheduleLayoutRefresh()` but has no cleanup that calls `cancelLayoutRefreshFrame()`. The schedule helper is idempotent (early-returns if a frame is pending), so this isn't a leak per se. But if the activation effect re-runs and resets `setIsRailReady(false)` while a frame is in-flight, the rAF callback fires after `isRailReady` is already false; the session-id guard catches it but only because the rail-build path bumps `overviewSessionIdRef.current = sessionId`. A rail-rebuild with the SAME sessionId would still flush.

**Current behavior:**
- messageCount effect schedules without cleanup.
- Session-id guard catches stale flushes only when sessionId differs.
- A same-sessionId rail-rebuild flushes the stale frame.

**Proposal:**
- Add a cleanup to the messageCount effect that calls `cancelLayoutRefreshFrame()`.
- Document the guard semantics around `overviewSessionIdRef`.

## Standalone Telegram redaction excludes generic `token` key context

**Severity:** Low - the standalone Telegram-token sanitizer no longer treats bare `token=` / `token:` as a token-bearing marker.

`src/telegram.rs:421-431` now allowlists Telegram-specific key names and bearer contexts, while `src/tests/telegram.rs:717-721` pins `token=<telegram-shaped-token>` as unredacted. Generic `token` keys are common in error payloads and debug logs; if Telegram returns or wraps a bot token under that key, `sanitize_telegram_log_detail` can expose it in stderr or `/api/telegram/test` error text.

**Current behavior:**
- `botToken=`, `telegramBotToken=`, `telegram_bot_token=`, and `TERMAL_TELEGRAM_BOT_TOKEN=` redact.
- `token=<telegram-shaped-token>` does not redact.
- The behavior improves false-positive precision but weakens secret-redaction recall for a common key name.

**Proposal:**
- Either restore bare `token` redaction in this Telegram-specific sanitizer.
- Or require an additional Telegram-adjacent context while still covering `token=` / `token:` payloads that can carry bot tokens.

## Wheel/scrollTop demand-hydration thresholds lack boundary-exact test coverage

**Severity:** Low - the wheel test fires `deltaY: -4` (below threshold) and asserts no hydration, then `deltaY: -120` with `scrollTop: 20_000` (above scrollTop ceiling) and asserts no hydration. The exact threshold boundary — `deltaY: -8` (should trigger) vs `-7` (should not), and `scrollTop: 160` vs `161` — is not pinned.

`ui/src/panels/AgentSessionPanel.tsx:340-351`, `ui/src/panels/AgentSessionPanel.test.tsx`. A future change to the constants would not surface as a failing test because no boundary-exact case is asserted.

**Current behavior:**
- `deltaY: -4` and `deltaY: -120` cases pinned.
- `deltaY: -7` (below threshold) and `-8` (at threshold) untested.
- `scrollTop: 160`/`161` boundary untested.

**Proposal:**
- Add `deltaY: -8` (just at threshold) and `deltaY: -7` cases.
- Add `scrollTop: 160` (at ceiling) vs `161` (above) cases.

## `SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101` boundary not tested

**Severity:** Low - `app-live-state.test.ts:1509-1547` was updated to pin the new `SESSION_TAIL_FIRST_HYDRATION_MESSAGE_COUNT = 20` count, but no test pins the boundary at `MIN_MESSAGES = 101`. A 100-message session should NOT trigger tail hydration; a 101-message session should.

`ui/src/app-live-state.ts:272`, `ui/src/app-live-state.test.ts:1509-1547`. A regression that lowered the trigger threshold to 21 (mistaking `MESSAGE_COUNT` for `MIN_MESSAGES`) would not be caught.

**Current behavior:**
- Test confirms tail hydration uses the new count.
- Boundary value (100 vs 101) is not exercised.

**Proposal:**
- Add a 100-message session test asserting `fetchSessionTail` is NOT called.
- Add a 101-message session test asserting it IS called.

## `update_telegram_config` pre-sanitize means response can drop fields the client never touched

**Severity:** Low - the new sanitize-before-patch step now silently scrubs stale persisted state on any unrelated config update (e.g., toggling `enabled`). The wire response then reflects the sanitized state, which differs from what the client posted.

`src/telegram_settings.rs:51`. A client that only patches `enabled: true` will receive a response missing `defaultProjectId` / `defaultSessionId` it never asked to clear. Clients with optimistic UI that diff request to response will diff incorrectly. There is no `metadata.cleared` field or response indicator that sanitization happened.

**Current behavior:**
- `enabled: true` patch may return a response missing fields the client never touched.
- No client-visible signal that sanitization occurred.
- Optimistic UIs cannot diff request vs response.

**Proposal:**
- Document the contract: "the response always reflects sanitized current-state and may drop fields the client never touched".
- Or add a `cleared` array (or similar) to the response so clients can know what changed.

## New `cancelLayoutRefreshFrame` / `cancelViewportRefreshFrame` cleanup not pinned by unmount-while-pending test

**Severity:** Low - the unmount path in `conversation-overview-controller.ts` now uses the new cancel helpers that read `*RefreshFrameIdRef.current`. The cleanup contract is that pending frames are cancelled on session switch and on unmount. The existing tests don't explicitly schedule a refresh, unmount before flushing, and assert `cancelAnimationFrame` was called for the pending frame id.

`ui/src/panels/conversation-overview-controller.ts:215-275`. A regression that dropped the cancel call inside the cleanup would only surface as a noisy log on session switch (the session-id guard catches the actual stale write), not a failed test.

**Current behavior:**
- Cleanup invokes new cancel helpers.
- No test asserts `cancelAnimationFrame` is called when unmounting with a pending frame.
- Session-id guard masks the visible regression.

**Proposal:**
- Add an explicit unmount-while-pending test that schedules a refresh, unmounts before flushing, and asserts `cancelAnimationFrame` was called with the pending frame id.
- Same for `sessionId` change mid-pending.

## `coalesces ready layout refreshes` test does not pin rapid-update upper bound

**Severity:** Low - the new test asserts that 2 message-count rerenders coalesce into a single frame. The production behavior is that an unbounded number of rerenders during a single rAF window all coalesce.

`ui/src/panels/conversation-overview-controller.test.tsx:292-401`. A regression that, say, leaked a frame per rerender after the first 2 would not surface — the assertion `frameCallbacks.size === 1` checks only the steady-state after each rerender.

**Current behavior:**
- 2-rerender coalescing pinned.
- 10+ rerenders in tight succession not pinned.
- Per-flush `getLayoutSnapshot` invocation count not verified.

**Proposal:**
- Loop ten (or twenty) rerenders without flushing, asserting `frameCallbacks.size` remains exactly 1.
- Spy on `getLayoutSnapshot` calls to assert it runs exactly once per flushed frame, not per rerender.

## `telegram_settings_load_defaults_only_for_missing_file` relies on platform-specific `io::ErrorKind`

**Severity:** Low - the new test creates a directory at the settings file path and expects `fs::read` to return a non-`NotFound` error so the code propagates instead of defaulting. The exact `io::ErrorKind` returned when reading a directory is platform- and libstd-version-dependent (Linux: `IsADirectory` on newer toolchains, `Other` previously; Windows: `PermissionDenied`/`Other`; macOS: `Other`).

`src/tests/telegram.rs:1187-1214`. Today the assertion is on `StatusCode::INTERNAL_SERVER_ERROR` and a substring of the human message, both of which depend only on the error being non-`NotFound`. If libstd ever maps directory-read on some platform to `NotFound` (unlikely), the test silently flips to asserting the default-on-missing path.

**Current behavior:**
- Test relies on directory-read error being non-`NotFound`.
- Specific `ErrorKind` shape is platform-dependent.
- Future libstd change could flip the behavior silently.

**Proposal:**
- Use a fixture that's reliably non-`NotFound` across platforms (e.g., write malformed JSON to assert the parse error path).
- Or assert directly on the lower-level `io::Error::kind()` to make the platform contract explicit.

## `ParallelAgentsCard` per-row inline arrow handlers regenerate identity per render

**Severity:** Low - `ui/src/message-cards.tsx:2158-2194` constructs three inline arrow functions per agent row (`() => onOpenAgentSession(agent.id)`, etc.) that regenerate identity on every render. `MessageCard` is `memo`-wrapped, but parent message identity is what stabilizes the card; agent-status updates re-render the whole list anyway.

Acceptable for the current N≤10ish parallel-agent count; flagging because the project guidelines call out "expensive render subtrees that regenerate DOM tree on every parent re-render."

**Current behavior:**
- Three inline arrows per agent row.
- Arrow identity changes per render.
- Acceptable at current parallel-agent counts.

**Proposal:**
- If parallel-agent counts grow, extract a `ParallelAgentRow` child component memoized on `(agent, callbacks)` and pass stable callbacks via `useCallback`.

## `MessageCard.test.tsx` parallel-agent test mounts without `DeferredHeavyContentActivationProvider`

**Severity:** Low - the new test at `ui/src/MessageCard.test.tsx:54-112` mounts `MessageCard` directly without wrapping it in `DeferredHeavyContentActivationProvider`. The parallel-agent card today does not require it (no Monaco etc.), but if it ever grows a deferred-render branch the test would silently bypass it.

**Current behavior:**
- Other heavy-content tests in this file wrap with `DeferredHeavyContentActivationProvider`.
- Parallel-agent test omits the wrapper.
- A future deferred-render branch in `ParallelAgentsCard` would silently bypass the gate in this test.

**Proposal:**
- Mirror the wrapping pattern from sibling tests for consistency.
- Or document that parallel-agent rendering deliberately doesn't require the provider.

## `~70` lines of `getBoundingClientRect` mock setup duplicated between adjacent `AgentSessionPanel.test.tsx` tests

**Severity:** Low - the new "hydrates a long-session tail after a native-scrollbar mousedown" test duplicates ~70 lines of `Object.defineProperty` and `getBoundingClientRect` boilerplate from the prior sibling test. Future changes to the rect contract need parallel edits in both tests.

`ui/src/panels/AgentSessionPanel.test.tsx:2193-2306`. Small drifts can produce silent test divergence.

**Current behavior:**
- Two adjacent tests duplicate the rect/scrollNode mock setup.
- Future contract changes require parallel edits.

**Proposal:**
- Extract a shared helper `installLongTranscriptScrollNodeMocks(scrollNode)` returning a cleanup function.

## Git file actions treat user paths as Git pathspecs instead of literals

**Severity:** Medium - file actions pass user-controlled path strings to Git pathspec commands. The `--` separator prevents option injection, but Git still expands pathspec magic and glob syntax.

`src/git.rs:275, 944`. A crafted filename such as `:(top)*.txt`, `*.rs`, or a path containing `[]` can make a single-file stage/revert/clean action affect more files than the selected row.

**Current behavior:**
- User-derived paths are collected as pathspec strings.
- `run_git_pathspec_command` appends them after `--`.
- Git pathspec magic and glob matching remain enabled.

**Proposal:**
- Force literal pathspec handling for all user-derived Git path args, for example via `GIT_LITERAL_PATHSPECS=1` or `:(literal)` wrapping.
- Add regression coverage for `*`, `?`, `[]`, and `:(top)` filenames.

## `ConversationOverviewViewportSnapshotTranslation` is not exported despite appearing on public projection shape

**Severity:** Note - the new local `type ConversationOverviewViewportSnapshotTranslation` is referenced by the public `ConversationOverviewProjection` interface (which IS exported), but the type itself is not exported. External consumers can read `projection.viewportSnapshotTranslation` only via inference and cannot import the type by name. There is also no contract comment explaining when translation is used vs the legacy path, what the units of `sourceTopOffsetPx` are, or the `messageCount` keying contract.

`ui/src/panels/conversation-overview-map.ts:85-91`.

**Current behavior:**
- Type is declared as a local `type`, not `export type`.
- No JSDoc on `viewportSnapshotTranslation`, `resolveViewportSnapshotTranslation`, or `projectConversationOverviewViewport`.
- Downstream callers in `ConversationOverviewRail.tsx` and tests must reach in via projection.

**Proposal:**
- Export the type to mirror the visibility of its container.
- Add a short module-level contract comment explaining "translation captures `documentTop − layoutTop` for a contiguous window so a viewport snapshot from the windowed virtualizer can map onto the full-transcript overview".
- Document the `snapshotMessageCount` re-use rule.

## `validate_and_normalize_telegram_config` and `sanitize_telegram_config_for_current_state` have overlapping responsibilities with implicit ordering

**Severity:** Note - `update_telegram_config` runs `validate_and_normalize_telegram_config` followed by `sanitize_telegram_config_for_current_state` on the success path. Both functions filter `subscribed_project_ids` against `known_projects`, but with different semantics — validate rejects unknown projects with `ApiError::bad_request`, sanitize silently drops them. The order matters and the layering contract is implicit.

`src/telegram_settings.rs:68-69`. No comment explains why both run on the write path.

**Current behavior:**
- `validate_and_normalize_telegram_config` strictly rejects unknown ids.
- `sanitize_telegram_config_for_current_state` silently drops them.
- Both run sequentially with no comment explaining the layering.

**Proposal:**
- Add a short comment explaining the intended layering (e.g., "Validate user input strictly, then sanitize against any state that may have changed since the user clicked save").
- Or merge the two passes if the use cases have converged.

## `telegram_token_boundary_byte` and `telegram_token_secret_byte` are byte-for-byte identical predicates

**Severity:** Note - both predicates resolve to `is_alphanumeric || _ | -`. The duplication is functionally correct (the secret loop greedily consumes every byte, so the post-loop boundary check is trivially satisfied) but is an attractive nuisance — a future contributor may try to tighten one without the other and reintroduce a redaction gap. The two predicates serve conceptually different roles (delimiter detection vs. content matching).

`src/telegram.rs:385-390`.

**Current behavior:**
- Boundary and secret predicates accept identical byte sets.
- No documentation of the subset/equality requirement.
- A change to one without the other would silently break redaction.

**Proposal:**
- Collapse into a single predicate with an explanatory `///` comment.
- Or add comments noting that the boundary and secret byte sets must remain a subset/equal for correctness.

## `standalone_telegram_bot_token_end` boundary uses `is_ascii_alphanumeric` only

**Severity:** Note - the new `telegram_token_boundary_byte` excludes `:` (fixing the colon-delimited case), but the boundary check uses `is_ascii_alphanumeric` only — non-ASCII alphanumeric prefix bytes (e.g., a Cyrillic letter immediately preceding the digit run from a Telegram error or interpolated chat title) are NOT boundary bytes, so a token whose left boundary is a multi-byte UTF-8 letter would still redact.

`src/telegram.rs:353-383`. Mostly defense-in-depth for log shapes that may not exist today; if an upstream library ever interpolates a chat title with a non-ASCII letter, the redactor will treat it as a delimiter.

**Current behavior:**
- Boundary detection accepts only ASCII alphanumeric.
- Non-ASCII letters before/after a candidate are treated as delimiters.
- Tightening only `:` was the targeted fix; not all callers were considered.

**Proposal:**
- Optionally treat any non-whitespace byte as a "boundary" (i.e. keep the strict-letter rule and additionally exclude `:` from the boundary set).
- Or document the chosen contract.

## `validate_and_normalize_telegram_config` clones bot token into snapshot copy

**Severity:** Note - the snapshot+restore fix uses `let mut normalized = config.clone()`, which clones the entire `TelegramUiConfig` including `bot_token: Option<String>`. The token now exists twice on the heap until `normalized` drops at function return. Validation never zeroes it. Consistent with the rest of the codebase but worth noting if you ever introduce a `Zeroizing<String>` wrapper for the bot token elsewhere.

`src/telegram_settings.rs:149-232`.

**Current behavior:**
- `clone()` creates a duplicate of `bot_token` for the duration of validation.
- No `Zeroizing` wrapper.
- Phase 1 single-user trust boundary makes this acceptable.

**Proposal:**
- Either accept (Phase 1 single-user stance).
- Or restructure as pure validation that does not own a copy of secrets — collect required mutations into a small `Normalization { default_project_id: Option<String>, push_subscriptions: Vec<String> }` value and apply after validation passes.

## `handleKeyDown` listener attached to `document` with `capture: true` fires for every keystroke globally

**Severity:** Note - the `AgentSessionPanel` keyboard demand-hydration listener is now in the capture phase, so it sees keystrokes on every element on the page. The `isTranscriptDemandKeyEventInScope` guard checks `composedPath().includes(scrollNode)` to filter, but the document-level capture listener fires for every keystroke during typing. `composedPath()` allocates an array per event.

`ui/src/panels/AgentSessionPanel.tsx:357-372`. Current scope filter prevents misfires; the cost is observable but probably acceptable.

**Current behavior:**
- `document.addEventListener("keydown", ..., { capture: true })`.
- Fires for every global keystroke including unrelated panels and modals.
- `composedPath()` filtering allocates per event.

**Proposal:**
- Consider attaching the listener to `node` directly (since composedPath also resolves through bubbles) and dropping `capture: true`.
- Or short-circuit on key first (only `ArrowUp`/`Home`/`PageUp` trigger expensive scope checks).

## `useSessionRenderCallbacks` gains three new required props without optional fallbacks

**Severity:** Note - `ui/src/SessionPaneView.render-callbacks.tsx:139-148` added `onOpenConversationFromDiff`, `onInsertReviewIntoPrompt`, `onComposerError` as required props. Any downstream caller of `useSessionRenderCallbacks` (today, only `SessionPaneView.tsx`) must thread these through.

Acceptable for now since there's a single caller; flagging for future re-use of the hook.

**Current behavior:**
- Three new required props added.
- No optional fallbacks.
- Hook is exported but only one caller.

**Proposal:**
- Either keep the required props (and document the hook is `SessionPaneView`-private).
- Or make the parallel-agent action callbacks themselves conditional and fall back to omission if any prop is undefined.

## Two readers see divergent `telegram-bot.json` view: API status sanitizes, file may have stale refs

**Severity:** Note - `telegram_status` reads the file and runs `sanitize_telegram_config_for_current_state` once, returning sanitized data without persisting it. `update_telegram_config` runs sanitize twice and persists. A client that calls `GET /api/telegram/status` and gets `defaultProjectId: null` (sanitized) but doesn't follow up with `POST /api/telegram/config` will see "phantom" data — the on-disk file still has the stale id.

`src/telegram_settings.rs:103-104`. A subsequent unrelated process reading `telegram-bot.json` directly (e.g., a `cargo run -- telegram` legacy path) gets a different view than the UI.

**Current behavior:**
- Status endpoint returns sanitized config without persisting.
- On-disk file may contain stale refs the API hides.
- Two readers with different views.

**Proposal:**
- Either auto-persist the sanitized form on read in `telegram_status` so the file matches what the API returns (with appropriate locking).
- Or document that the on-disk file is allowed to contain stale references that `GET /api/telegram/status` will hide.

## Single test bundles five Telegram redaction assertions

**Severity:** Note - `telegram_log_sanitizer_redacts_bot_tokens_and_truncates` bundles five independent assertions (URL redaction + truncation, standalone `=`-delimited, colon-delimited, benign three-shape passthrough, malformed `/bot/bot/`). Round-63 added another bundled test, `telegram_standalone_token_redaction_respects_context_and_thresholds`, with 11 disjoint assertions — the same anti-pattern. Same pattern bugs.md previously called out for the connection-test classifier. A failure deep inside makes the line number unhelpful.

`src/tests/telegram.rs:630-660` and `:662-722`. Plus round-63 added `telegram_settings_validation_does_not_partially_mutate_on_other_error_paths` which bundles three independent error paths.

**Current behavior:**
- Three bundled tests now exhibit the pattern.
- Failure messages cluster at one location each.

**Proposal:**
- Split each into per-shape `#[test]` functions.
- Or use a small `(input, expected_substring_present, expected_substring_absent)` table and one helper.

## Delegation result formatting remains coupled to command transport

**Severity:** Low - the hook at `ui/src/SessionPaneView.render-callbacks.tsx:13-20` imports `delegation-commands` and `delegation-result-prompt` directly, and the pure formatter at `ui/src/delegation-result-prompt.ts:11` imports `DelegationResultPacket` from `delegation-commands`.

The formatter now uses the stricter packet shape, which fixed the prior type-drift issue, but the dependency still points from pure prompt formatting into command transport. A future refactor to swap delegation transports requires re-wiring both the hook and the formatter.

**Current behavior:**
- Hook directly imports network-API module.
- Formatter imports a transport-owned packet type.
- Tests must mock the imports at the module level.
- Future transport swap requires hook rewrite.

**Proposal:**
- Pass `delegationActions: { open, insert, cancel }` as hook props (defaulting to the production wrappers in `SessionPaneView.tsx`).
- Move `DelegationResultPacket` to a neutral shared module such as `types.ts` or `delegation-result-types.ts`.
- Or expose a `DelegationActionContext` provider so consumers can override.

## Standalone redactor precision-vs-recall trade-off documented for future reviewers

**Severity:** Note - round-63 fundamentally inverted the standalone-token redactor: from "redact everything matching the shape" (round 62) to "redact only with key-context or bearer-context." The new gate trades recall for precision. The bug ledger pre-round-63 noted the prior behavior leaked under foreign-token false positives (Low) and unanticipated formats (Medium).

The new design fixes the false-positive Low (verified by `accessToken=` and `csrfToken =` tests) but leaves the Medium concern partially open: tokens still leak when they appear in unanticipated structures (JSON arrays without keys, free-prose mentions, code spans, YAML lists, anything where the standalone token isn't preceded by `=`/`:` + approved key, or the alphabetic word "bearer"). The `unanchored = "trace value <token>"` test fixture documents this as deliberate.

`src/telegram.rs:353-388`.

**Current behavior:**
- Closed-allowlist redaction.
- Tokens in unanticipated formats (no key context, no Bearer prefix) leak by design.
- Test fixture `unanchored` pins the trade-off.

**Proposal:**
- Add a comment to `redact_standalone_telegram_bot_tokens` explaining the precision-over-recall choice and listing the formats that intentionally leak so future contributors understand the design intent.
- Reconsider if telemetry shows real leaks.

## Copy/rename Git staging pathspec branch lacks coverage

**Severity:** Low - the new staging helper includes original paths only for `C` and `R` status codes, but the added regression test covers only the non-rename modified-file path.

`src/git.rs:273`. A regression that stops including the original path for copy/rename staging can leave source deletes or rename metadata unstaged without failing the current test.

**Current behavior:**
- `collect_git_stage_pathspecs` branches on the first status-code character.
- Tests cover the `M` behavior.
- Copy/rename behavior is not pinned.

**Proposal:**
- Add focused coverage for `Some("R")` and `Some("C")`.
- Prefer a real repo staging scenario that proves both old and new paths are staged together.

## `preserveGatewayErrorBody` masks backend-unavailable responses on empty gateway bodies

**Severity:** Medium - opted-in routes map every 502/503/504 response to `request-failed`, even when the body is empty or not an intentional third-party JSON error.

`ui/src/api.ts:1687-1689`. A real TermAl backend/proxy outage on a preserved route can bypass the established `backend-unavailable` path and lose restart/retry semantics.

**Current behavior:**
- `preserveGatewayErrorBody` forces 502/503/504 into `request-failed`.
- Empty or non-actionable bodies still use the preserved path.
- Callers cannot reliably distinguish intentional upstream errors from backend availability failures.

**Proposal:**
- Preserve gateway bodies only when the response contains a parseable, intentional JSON error payload.
- Fall back to `backend-unavailable` for empty, malformed, or otherwise non-actionable 5xx gateway bodies.

## Tail-window size policy is duplicated across frontend layers

**Severity:** Low - tail-first hydration and active transcript tail rendering both encode the same 20-message policy in separate private constants.

`ui/src/app-live-state.ts:272` and `ui/src/panels/AgentSessionPanel.tsx:130`. If `SESSION_TAIL_FIRST_HYDRATION_MESSAGE_COUNT` and `INITIAL_ACTIVE_TRANSCRIPT_TAIL_MESSAGE_COUNT` diverge, partial hydration and active transcript rendering can unexpectedly add or drop visible tail messages.

**Current behavior:**
- The hydration layer requests a 20-message tail.
- The active transcript render path slices a 20-message tail independently.
- No shared policy or cross-reference ties the two values together.

**Proposal:**
- Centralize the tail-window size in a shared UI policy module.
- Or add explicit comments documenting why the two constants must match or may intentionally differ.

## `useInitialActiveTranscriptMessages` mutates `hydrationRef` during render

**Severity:** Medium - render-time side effect on a ref. Works but fragile under concurrent mode (`useTransition`/`Suspense`) — a render that's discarded would still leave the ref in its mutated state, prematurely flipping `hydrated = true` on a discarded render path.

`ui/src/panels/AgentSessionPanel.tsx:228-236`. Lines 221-226 reset on session change; lines 234-236 set `hydrated = true` in early-eligibility branch.

**Current behavior:**
- Two ref mutations during render body.
- React 18 concurrent rendering or Suspense can discard renders.
- Discarded render's ref mutations persist.

**Proposal:**
- Hoist the session-id-change reset into a `useEffect` (with the trade-off of one stale render after the change).
- Or document the render-mutation as deliberate and known-fragile under Suspense.

## "in-flight Telegram test unmounts" test asserts only `consoleError`, doesn't actually pin the unmount guard

**Severity:** Medium - React 18+ removed the "Can't perform a state update on an unmounted component" warning entirely, so removing the `isMountedRef` checks would not cause the test to fail.

`ui/src/preferences-panels.telegram.test.tsx:162-189`. The test reads as effective coverage for the `isMountedRef` guard but actually catches no regression because no warning fires under React 18. A regression making `setError` always swallow would also pass.

**Current behavior:**
- Test asserts `expect(consoleError).not.toHaveBeenCalled()`.
- Under React 18, no warning fires regardless of the guard.
- Removing the guard would not cause the test to fail.

**Proposal:**
- Spy on the test promise's then-handler (or wrap `setError`/`setIsTesting` via mock) to assert they aren't invoked post-unmount.
- OR remount and verify state is freshly initialised.
- Add a positive control: same flow stays mounted, error DOES surface.

## Two cancellation patterns coexist in `TelegramPreferencesPanel`: `cancelled` flag for initial-fetch, `isMountedRef` for handlers

**Severity:** Low - same component has two patterns for the same concern ("drop late updates after unmount"). Future maintainers may copy the wrong one.

`ui/src/preferences-panels.tsx:1229-1263`. The fetch-status `useEffect` uses its own `cancelled` closure flag while the three async handlers use `isMountedRef`.

**Current behavior:**
- Initial-fetch effect uses `cancelled` flag.
- `handleSave`/`handleTestConnection`/`handleRemoveBotToken` use `isMountedRef`.
- Two patterns side-by-side.

**Proposal:**
- Consolidate on one pattern. `isMountedRef` reads cleaner for fire-and-forget click handlers; `cancelled` flags read cleaner for effect-scoped fetches; both are fine, but pick one per file.

## PATCH docstring does not cover `subscribed_project_ids`

**Severity:** Medium - round 56's docstring at `src/wire.rs:1056-1060` reads "Nullable string fields use the same tri-state PATCH convention". `subscribed_project_ids: Option<Vec<String>>` (line 1067) is NOT a string field, has NO `deserialize_nullable_marker_field`, and silently accepts `null` as no-op. A reader of just the docstring + struct will conclude all `Option<...>` fields share tri-state semantics.

**Current behavior:**
- Docstring documents tri-state for "string fields".
- `subscribed_project_ids` is `Option<Vec<String>>` and behaves differently.
- Round 56's docstring widens the surface for confusion rather than narrowing it.

**Proposal:**
- Tighten the docstring to "string fields decorated with `deserialize_nullable_marker_field`" + an explicit note that `subscribed_project_ids` differs, OR
- Migrate `subscribed_project_ids` to the marker pattern.

## `prepare_assistant_forwarding_for_telegram_prompt` race window between cursor capture and POST send

**Severity:** Medium - the new prepare/apply split correctly avoids mutate-before-success, but widens the cursor-capture-to-apply window across a network round-trip. If the agent emits new assistant text between T0 (capture) and T1 (POST returns), the T0 baseline marks the freshly-emitted message as already-forwarded.

`src/telegram.rs:890-894`. The pre-round-55 `arm_assistant_forwarding_for_telegram_prompt` had the same fundamental race but a much narrower window (no network call between cursor read and state write).

**Current behavior:**
- T0: `prepare_*` reads cursor.
- T1: `send_session_message` POST returns.
- T2: `apply_*` commits T0 cursor to state.
- An assistant message emitted between T0 and T1 is silently marked as "already forwarded".

**Proposal:**
- Re-fetch the cursor right before applying, not at the prepare step.
- Or capture `latest` AFTER the POST returns (since the goal is "baseline as of after this prompt is sent").

## `prune_telegram_config_for_deleted_project` failure is `eprintln!`-only

**Severity:** Medium - if pruning fails, `delete_project` returns success and on-disk Telegram config retains stale references. The on-read sanitize masks them, but `validate_and_normalize_telegram_config` REJECTS unknown subscribed project ids on save — so a future `update_telegram_config` will fail with "unknown Telegram project `<deleted>`" with no obvious recovery path.

`src/session_crud.rs:525-530`. The eprintln logs the failure but the API surface returns success.

**Current behavior:**
- `prune_telegram_config_for_deleted_project` failure is logged to stderr.
- `delete_project` returns success regardless.
- Next `update_telegram_config` fails with confusing error.

**Proposal:**
- Either persist Telegram config inside `commit_locked` (atomic), or have `validate_and_normalize_telegram_config` strip unknown ids on write rather than reject, or escalate the prune failure to a 5xx instead of swallowing.

## `src/telegram.rs` past 1500-line architecture rubric threshold

**Severity:** Medium - file now exceeds 1766 lines after round 56. CLAUDE.md asks for smaller modules.

`src/telegram.rs`. Round 56 added `backup_corrupt_telegram_bot_file`, `telegram_command_mentions_other_bot`, and digest-failure branches on top of the round-55 baseline. Mixes: HTTP client, TermAl client, wire types, command parser, digest renderer, assistant-forwarding cursor logic, corrupt-file backup helper, and the relay loop. `telegram_settings.rs` already extracted the UI surface; the next natural cut is `telegram_relay.rs` + `telegram_clients.rs` + `telegram_wire.rs`.

**Current behavior:**
- One file owns seven concerns now.
- Continued growth pattern across recent rounds.

**Proposal:**
- Split into 2-3 modules mirroring the api.rs/wire.rs split shape.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

## `ui/src/preferences-panels.tsx` past 2000-line natural split point

**Severity:** Medium - `TelegramPreferencesPanel` now has three async handlers (handleSave, handleTestConnection, handleRemoveBotToken) each with `if (!isMountedRef.current)` guards repeated 3×. Pattern duplication is wide enough that a `useUnmountSafeAsync` helper would cut ~60 lines.

`ui/src/preferences-panels.tsx:1226-1430`. CLAUDE.md asks for smaller modules; the panel duplicates 30-40 line handler bodies with the same guard pattern.

**Current behavior:**
- Three async handlers each with three `if (!isMountedRef.current)` checkpoints (start of catch, end of try, finally).
- Pattern duplication; future maintainer copies the shape into a fourth handler.

**Proposal:**
- Extract `useUnmountSafeAsync` hook returning a `runSafe(asyncFn, { onSuccess, onError, onFinally })` wrapper, OR
- Split `TelegramPreferencesPanel` into form-state component + inner mount-safe handler module.

## `validate_and_normalize_telegram_config` mixes pure validation, mutation, and mutex acquisition

**Severity:** Medium - holds the state mutex while iterating + mutating caller-owned config. Same anti-pattern that the new prepare/apply assistant-forwarding split fixed elsewhere this round.

`src/telegram_settings.rs:148-227`. The rename from `validate_telegram_config` documents the dual responsibility but the `&mut TelegramUiConfig` signature still buries it.

**Current behavior:**
- One function holds state mutex, iterates `inner.projects`/`inner.sessions`, mutates caller-owned config.
- State held across multiple checks, allocations, and conditional mutations.

**Proposal:**
- Split into `pure_validate_telegram_config(...) -> Result<TelegramConfigNormalization, ApiError>` + `apply_telegram_config_normalization(...)` outside the lock.

## `POST /api/telegram/test` 429 missing `Retry-After` header / cooldown duration

**Severity:** Low - body says "Try again in a moment." but doesn't include a numeric duration nor an HTTP `Retry-After` header.

`src/telegram_settings.rs:361`. The cooldown is `TELEGRAM_TEST_COOLDOWN = Duration::from_secs(2)`; HTTP-aware clients can't act on a structured rate-limit signal.

**Current behavior:**
- 429 status.
- Body: "Telegram connection tests are rate-limited. Try again in a moment."
- No `Retry-After` header.

**Proposal:**
- Attach a `Retry-After: 2` header, or add a stable `retryAfterSeconds` field to the error contract.

## `subscribed_project_ids: Option<Vec<String>>` lacks `deserialize_nullable_marker_field` — `null` no-ops

**Severity:** Medium - inconsistent with sibling PATCH fields. With default serde, `null` deserializes to `None` (treated identically to "field absent"), so `{"subscribedProjectIds": null}` silently means "do not update".

`src/wire.rs:1063`. There is no test asserting the `null`-as-no-op behavior.

**Current behavior:**
- Other PATCH fields use `deserialize_nullable_marker_field`.
- `subscribed_project_ids` does not.
- `null` and absent are indistinguishable.

**Proposal:**
- Add a regression test asserting `{"subscribedProjectIds": null}` is no-op, OR
- Switch to `deserialize_nullable_marker_field` with `Vec::new()` interpretation for explicit-null for symmetry.

## `get_session_tail` skips remote-proxy hydration that `get_session` performs

**Severity:** High - the new tail-first path silently degrades for remote-proxy sessions, returning an empty tail instead of triggering upstream hydration.

`src/state_accessors.rs:386-401`. `get_session` (lines 341-374) detects an unloaded remote-proxy session (`record.remote_id.is_some() && record.remote_session_id.is_some() && !record.session.messages_loaded`) and synchronously calls `hydrate_remote_session_target` to fetch from the upstream remote. `get_session_tail` does not perform this check — it returns whatever messages the local cache happens to hold (typically zero for an unhydrated remote proxy), with `messages_loaded=false`. For a 200-message remote-proxy session never opened locally, the tail-first path returns an empty tail. The frontend then proceeds to the full fetch, which DOES trigger upstream hydration. So the user sees: empty pane → full transcript pop, with a wasted round-trip in between. The architecture doc claim at `docs/architecture.md:217-229` ("For unloaded remote-proxy sessions, the same route synchronously calls the owning remote's…") becomes false specifically when `?tail=N` is appended.

**Current behavior:**
- `get_session_tail` reads `record.session.messages` directly, no remote-proxy hydration branch.
- For remote proxies with empty local cache, response carries `messages: []`, `messages_loaded: false`.
- Frontend's `shouldStartTailFirstHydration` triggers based on `messageCount >= 101` — a remote proxy whose summary advertises a large `messageCount` is exactly the case routed here.
- Empty tail falls through to "stale" (because `responseSession.messages.length > 0` guard), only working by accident, then full fetch triggers hydration.

**Proposal:**
- Either (a) route `get_session_tail` through the same remote-proxy hydration branch that `get_session` uses (preferred — single source of truth), or (b) reject `?tail=N` on unloaded remote-proxy sessions with a typed error so the frontend skips tail-first hydration for them, or (c) document explicitly that `?tail=N` is local-only and have `shouldStartTailFirstHydration` skip remote-proxy sessions.
- Add Rust test asserting tail-fetch against an unhydrated remote-proxy session triggers upstream hydration (or returns the typed gating error).

## `messageUpdated`/`textDelta`/`textReplace` for missing-prefix message IDs silently degrade to `appliedNeedsResync` after partial adoption

**Severity:** High - the partial-tail post-condition (`messagesLoaded: false`, `messages.length: N`, `messageCount: M > N`) creates a real gap window where deltas targeting messages in the unloaded prefix are silently dropped on the floor as metadata-only.

`ui/src/live-updates.ts:472-512, 530-540, 589-599, 644-654`. After tail adoption, the local session has, e.g., `messages.length: 100`, `messageCount: 150` — gap between messages 1-50 (missing) and 51-150 (present). When a `messageUpdated` SSE delta arrives for `message-30`, `findMessageIndex` returns -1, the code calls `applyMetadataOnlySessionDelta` and returns `appliedNeedsResync`. The metadata advances but the textual update is dropped. The classifier no longer distinguishes "message id missing because session not yet hydrated" (the pre-tail-first invariant: `messages.length === 0`) from "message id missing because it's in the partial-tail prefix gap." Before this change, that branch was reachable only when zero messages were loaded; the comment-on-record relied on the assumption "no messages → resync recovers." Now the partial tail keeps the gap permanently arming this codepath until the full fetch lands, and during active streaming with retries failing the text update on `message-30` may be lost in a new risk window.

**Current behavior:**
- After partial tail adoption, the local session has a real prefix gap.
- Deltas targeting missing-prefix message IDs degrade to `appliedNeedsResync`.
- Resync schedules eventually recover, but during active streaming the textual update is dropped during the recovery window.
- No explicit tracking of "this message ID is in our gap, not just generally missing".

**Proposal:**
- Track which message IDs are present in the local transcript explicitly (or the transcript-coverage range as a `[startIndex, endIndex)` tuple), so the "this index is in our gap, ignore" branch is tested directly rather than inferred from `findMessageIndex === -1 + messagesLoaded === false`.
- Or refuse to enter partial state — keep the tail-first response but immediately invalidate it if any `messageUpdated`/`textDelta` for an unloaded-prefix index arrives before the full fetch lands.
- Add coverage where (a) tail adopts partial, (b) `messageUpdated` for `message-30` (in gap) is dispatched, (c) full fetch lands with the updated message-30 text. Today the textual update is lost; verify the full fetch carries the correct text.

## Dead code: full-fetch `"partial"` outcome in `startSessionHydration` is unreachable

**Severity:** High - the runtime branch at `app-live-state.ts:1361-1366` is unreachable; the type-level exhaustiveness check passes, but a future maintainer reasoning about it has no signal that the code is dead.

`ui/src/app-live-state.ts:1278-1390`. `allowPartialTranscript` is only set on the tail-fetch request context (line 1305 spreads `requestContext` and adds `allowPartialTranscript: true`). The full-fetch path uses `fullRequestContext` which never sets `allowPartialTranscript` (line 1338 captures from `captureHydrationRequestContext`). `classifyFetchedSessionAdoption` at `session-hydration-adoption.ts:290-297` only returns `"partial"` when `requestContext.allowPartialTranscript === true`. So the full fetch can never produce `"partial"`. The current `case "partial": shouldRetryHydration = true; break;` is misleading defensive code that the exhaustiveness `_exhaustive: never` validates but no test ever exercises.

**Current behavior:**
- `case "partial":` arm in the full-fetch outcome switch sets `shouldRetryHydration = true` and breaks.
- The arm cannot fire because `allowPartialTranscript` is never set on the full-fetch context.
- Type system passes because both switches share the same return type.

**Proposal:**
- Either (a) add a `console.warn` + `assert.fail`-style early dev-mode signal so a future contract change surfaces immediately, (b) split `AdoptFetchedSessionOutcome` into a tail-only and full-only variant so the type system enforces unreachability (e.g., `type FullFetchOutcome = Exclude<AdoptFetchedSessionOutcome, "partial">`), or (c) add an explanatory comment naming why this is dead today and what would make it live.

## `SESSION_TAIL_HYDRATION_MAX_MESSAGES = 500` silent cap with no signal to caller

**Severity:** Medium - `message_limit.min(SESSION_TAIL_HYDRATION_MAX_MESSAGES)` truncates without a status code, header, or response field. Future callers cannot detect that they got a different prefix than they asked for.

`src/state_accessors.rs:213`. Today the only caller hard-codes 100, so dormant. But the contract is "you can ask for any N; you get back min(N, 500) messages with no indication you were capped." A future caller (Telegram digest, mobile, batch export, or the same UI raised to 1000 for richer first-paint) cannot distinguish "I asked for 800, got 500 because cap" from "I asked for 800, the session has 500" without recomputing from `message_count` minus `messages.length` minus a guess at where `start_index` landed.

**Current behavior:**
- `tail=600` returns 500 messages with no diagnostic.
- No `Content-Range`-style header or response field.
- Frontend has no way to detect the cap was applied.

**Proposal:**
- Either (a) reflect the cap in the response (e.g., `messages_window: { offset, limit, total }` field on `SessionResponse`), (b) reject `tail` values above the cap with `ApiError::bad_request` naming the limit so callers know to coordinate, or (c) at minimum cross-reference the cap constant in the frontend constant's doc-comment so a bump on either side prompts review.

## Three hard-coded constants encode the same architectural invariant with no cross-references

**Severity:** Medium - `SESSION_TAIL_HYDRATION_MAX_MESSAGES = 500` (backend cap), `SESSION_TAIL_FIRST_HYDRATION_MESSAGE_COUNT = 100` (frontend request size), `SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101` (frontend trigger threshold) live in different files with no cross-references.

`src/state_accessors.rs:38` and `ui/src/app-live-state.ts:272-273`. Implicit relationships: the threshold (101) is "MESSAGE_COUNT + 1" so we only tail-first for sessions where tailing actually saves bytes. The backend cap is silent — if the frontend ever asks for tail=600, the backend silently truncates to 500 and the frontend learns nothing about the cap. A future change to MESSAGE_COUNT might forget to update MIN_MESSAGES (breaks the "only tail if it would actually shorten" invariant) or might exceed MAX_MESSAGES (silently capped without diagnostic).

**Current behavior:**
- Three hard-coded constants with implicit relationships.
- No comment cross-references between them.
- A change to one risks silently breaking the others.

**Proposal:**
- Either derive `MIN_MESSAGES = MESSAGE_COUNT + 1` from `MESSAGE_COUNT` in code, OR add JSDoc cross-references naming the related constants and the architectural invariant.
- Have the backend report the cap in the response (see entry above).

## Wire projection layer owns `messages_loaded` SEMANTIC field for partial case

**Severity:** Medium - `wire_session_tail_from_record` decides `messages_loaded` based on whether the slice covers the whole transcript AND the source is loaded. This is wire-semantics decision (UI uses `messagesLoaded: false` to mean "still adopt me, but don't trust messages.length === messageCount") that lives in the projection helper.

`src/state_accessors.rs:210-219`. The wire layer's job is "single source of truth for the JSON shape" — but `messages_loaded` here is becoming a SEMANTIC field, not a shape field. `get_session_tail` is the only caller, but the next time someone needs partial transcripts (e.g., a "show me messages around message-X" range fetch), they'll either reuse this helper with a new caller (coupling unrelated wire-projections) or duplicate the logic.

**Current behavior:**
- `wire_session_tail_from_record` encodes "tail only counts as fully loaded if it covers the whole transcript AND the source is loaded".
- This is semantic flag manipulation, not pure shape projection.
- Future range-fetch callers must reuse or duplicate.

**Proposal:**
- Either move `messages_loaded` decision into the route handler (keeping the projection pure-shape), OR formalize a `partial_transcript_loaded` distinction in the wire shape itself (`transcriptLoaded: "full" | "partial-tail" | "summary"`) and have the frontend act on the typed value rather than inferring from `messagesLoaded === false && messages.length > 0`.

## `SessionHydrationRequestContext` is a four-flag bag with non-obvious mutual exclusions

**Severity:** Medium - two booleans (`allowDivergentTextRepairAfterNewerRevision`, `allowPartialTranscript`) plus three metadata fields. The flags have non-obvious interactions encoded in call-site logic, not the type.

`ui/src/session-hydration-adoption.ts:16-23`. `allowDivergentTextRepairAfterNewerRevision === true` means the request is for a divergence repair, which `shouldStartTailFirstHydration` deliberately excludes from tail-first. That exclusion lives at `app-live-state.ts:771-773`, not in the type. A reader of `SessionHydrationRequestContext` sees two unrelated flags and has to chase to the call sites to learn they're never simultaneously true.

**Current behavior:**
- Four-flag context bag.
- Mutual exclusions encoded as call-site early-returns.
- Type system doesn't enforce the contract.

**Proposal:**
- Convert to a discriminated union — `type SessionHydrationRequestContext = ({ kind: "fullSession" } | { kind: "partialTail" } | { kind: "textRepair" }) & SharedMetadata`. Classifier dispatches on `kind`; call sites can never set inconsistent flags.

## `hydratedSessionIdsRef.current.add(sessionId)` invariant has three call sites

**Severity:** Medium - after this change, `add` is called at three places (tail "adopted", early-return after partial-then-already-hydrated, full "adopted"). The invariant — "add when fully hydrated and we won't run another hydration" — is encoded by repetition.

`ui/src/app-live-state.ts:1308, 1335, 1359-1361`. Worse, the `partial` outcome at line 1310 deliberately does NOT add to the set, because the session is not fully hydrated yet. A future reader scanning for "where do we mark hydrated" sees three places and must read each branch to understand the implicit "and partial is not hydrated" rule. If a fourth state is added (e.g., "tail returned the whole transcript because backend has fewer messages than the limit AND messages_loaded was true"), the question "do we add to hydratedSessionIdsRef here?" has no automatic answer.

**Current behavior:**
- Three call sites for the "fully hydrated" mark.
- One outcome (partial) deliberately omits the mark.
- The invariant is encoded by repetition.

**Proposal:**
- Either (a) extract a small `markFullyHydrated(sessionId)` helper that wraps the add + clearHydrationRetry pair (already paired at all three sites), OR (b) compute "is this session fully hydrated" from session state at use sites and stop tracking it in a separate ref.

## `?tail=0` returns an empty array with `messages_loaded: false`, indistinguishable from "still loading"

**Severity:** Medium - for a populated session, response is `messages: [], messages_loaded: false, message_count: N>0` — the frontend treats this as the metadata-only/awaiting-hydration case and schedules a retry. So `?tail=0` is a no-op DOS pattern: the caller gets nothing useful and triggers refetch loops.

`src/api.rs:127-128` + `src/state_accessors.rs:212-217`. There is no test or documentation defining whether `tail=0` is intended (sane: "give me just the metadata", invalid: "rejected as `bad_request`", or oversight). Frontend's `Math.max(0, Math.floor(messageLimit))` clamp at `ui/src/api.ts:528` defensively allows it.

**Current behavior:**
- `tail=0` accepted at the route boundary.
- Backend returns empty array with `messages_loaded: false` for populated sessions.
- Frontend classifier treats response as "stale" (since `messages.length > 0` guard fails).
- A retry is scheduled, calling tail=0 again — refetch loop.

**Proposal:**
- Decide and document: either treat `tail=0` as "metadata only, messages_loaded: false" explicitly (add a test pinning the shape), or reject `tail=0` as `bad_request` so callers do not accidentally enter a refetch loop.
- Tighten `fetchSessionTail` clamp to `Math.max(1, Math.floor(messageLimit))` if the latter.

## `Query<GetSessionQuery>` parse failure bypasses project `ApiError` envelope

**Severity:** Medium - `?tail=foo` / `?tail=-1` / `?tail=99999999999999999999` returns Axum's default plain-text rejection, not the project's `{ "error": ... }` shape.

`src/api.rs:135`. Pre-existing pattern across `Query<T>` handlers (`api_review.rs`, `api_files.rs`, `api_git.rs`); same as the already-tracked Telegram JSON body rejection. The new endpoint inherits the gap. Frontend `createResponseError` in `ui/src/api.ts` falls back to a generic message.

**Current behavior:**
- Malformed `?tail` value triggers Axum's default `400 Failed to deserialize query string: ...` plaintext.
- Frontend cannot parse this through the project's error envelope.
- Pre-existing pattern across many `Query<T>` handlers.

**Proposal:**
- Add an `api_query_rejection` helper analogous to `api_json_rejection` and switch all `Query<T>` handlers to `Result<Query<T>, QueryRejection>` to match the project envelope.
- Track separately if scope-expansion concern.

## `get_session_tail` JSON serialization runs on tokio worker, not `spawn_blocking`

**Severity:** Medium - the `get_state` handler at `src/api.rs:107-122` documents why it serializes inside `spawn_blocking`. The new tail path reintroduces the anti-pattern.

`src/api.rs:131-143`. With a 500-message ceiling, a single tail response can serialize multiple MB on the worker. A script repeatedly hitting `?tail=500` against a session containing large messages can pin a worker for noticeable durations. Realistic impact under Phase 1 single-user trust is low, but the precedent contradicts the deliberate `get_state` rewrite.

**Current behavior:**
- `get_session_tail` runs inside `run_blocking_api`, but `Json(response)` serialization happens on the tokio worker thread.
- For 500 large messages, the tokio worker can stall for noticeable durations.

**Proposal:**
- Mirror the `get_state` pattern: serialize the response inside `spawn_blocking` and return `Vec<u8>` with an explicit `application/json` header.

## `?tail=N` query parameter is not documented in `docs/architecture.md`

**Severity:** Medium - the new parameter, the `messages_loaded` invariant, the silent cap at 500, and the local-only scope are nowhere documented.

`src/api.rs:121-141`. `docs/architecture.md:191` describes `GET /api/sessions/{id}` without mentioning the query parameter. `docs/metadata-first-state-plan.md:758` even says "Pagination of `GET /api/sessions/{id}` is a non-goal" — contradicting the new tail parameter without an update. Frontend's `classifyFetchedSessionAdoption` and `adoptFetchedSession` treat tail responses as "partial" via `allowPartialTranscript: true` — this contract should be pinned in docs.

**Current behavior:**
- `?tail=N` query parameter implemented but undocumented.
- `metadata-first-state-plan.md` still claims pagination is a non-goal.

**Proposal:**
- Update `docs/architecture.md` to document `?tail=N`, the messages_loaded invariant, the silent cap at `SESSION_TAIL_HYDRATION_MAX_MESSAGES = 500`, the local-only scope, and the tail/full revision-may-differ contract.
- Update `docs/metadata-first-state-plan.md` to clarify the tail-first hydration carve-out is not pagination.

## `fullRequestContext` recapture pattern is silently load-bearing but undocumented

**Severity:** Low - line 1338 recaptures the request context after partial adoption mutated `sessionsRef`. Without this recapture, the full fetch would compare against pre-tail metadata and likely classify as `stale`. A future "simplification" back to the original `requestContext` would silently break the classifier.

`ui/src/app-live-state.ts:1338-1339`. The metadata fields (`messageCount`, `sessionMutationStamp`) on the new local state come from the partial-adopted tail. The full fetch's `classifyFetchedSessionAdoption` needs the post-partial values. No comment explains this.

**Current behavior:**
- `fullRequestContext = captureHydrationRequestContext(sessionId, options) ?? requestContext;`
- The recapture is needed for correctness but undocumented.

**Proposal:**
- Add a one-line comment: "// Recapture so the classifier sees post-tail-adoption metadata; partial-adoption mutated sessionsRef."

## `SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101` undocumented "why 101?"

**Severity:** Low - reading the file in isolation, "why 101?" is not obvious. The threshold is "fetch the tail when fetching the full transcript would be ≥1 message wasteful". Note the marginal benefit at 101 messages: tail saves 1 message but adds a round-trip.

`ui/src/app-live-state.ts:272-273`. Future maintainers may bump this to a "round number" without realizing the constant of 100 in the message count is what makes 101 the natural threshold. Bigger question: at exactly 101 messages, is the round-trip cost worth saving 1 message? Probably not — a more pragmatic threshold (e.g., 300+) would amortize the round-trip cost over much more saved data.

**Current behavior:**
- `SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101` is undocumented.
- The relationship to `MESSAGE_COUNT = 100` is implicit.
- At 101 messages, the round-trip arguably costs more than tailing saves.

**Proposal:**
- Add a one-line comment: "// Trigger tail-first when at least one message would be saved by skipping it (i.e. message count exceeds the tail's window)."
- Consider raising the threshold (e.g., `>=300`) where the round-trip cost is amortized over much more saved data.

## Tail-then-full sequence doubles HTTP request volume for sessions ≥101 messages

**Severity:** Low - the frontend always pairs `fetchSessionTail(100)` with `fetchSession(...)` for sessions where `messageCount >= 101`. Phase 1 local-only is fast. Future remote-host or flaky-network scenarios pay this tax.

`ui/src/app-live-state.ts:1278-1390`. Over SSH this matters more than over HTTP loopback. Combined with the High-severity "remote-proxy hydration skipped" entry, the worst case is: tail-first (returns empty for unhydrated remote proxy) + full-fetch (triggers remote hydration, returns full transcript) — two round-trips for what could have been one.

**Current behavior:**
- Two HTTP calls per visible-session hydration for sessions ≥101 messages.
- Phase 1 local-only is fast.
- Combined with remote-proxy issue, worst case is 2× wasted traffic.

**Proposal:**
- Once remote routing is sorted, consider returning the full transcript in the same response for sessions under a "small-enough" threshold.
- Or have the client skip the tail-first request when the remote round-trip cost would dominate.

## Telegram settings updates live outside the app state/revision model

**Severity:** Medium - Telegram settings are user-visible configuration, but saves bypass `StateInner`, `commit_locked()`, snapshots, revisions, and SSE.

`src/telegram_settings.rs:30` updates `~/.termal/telegram-bot.json` directly through the Telegram settings endpoint. That means one browser tab can save config while other tabs keep stale settings until they manually refetch, and future relay lifecycle work will need to reconcile app state with a separate settings file.

**Current behavior:**
- Telegram settings updates do not bump the app revision.
- `/api/state` and SSE do not carry the changed config.
- Other open clients cannot observe settings changes through the normal state model.

**Proposal:**
- Store Telegram UI config in durable app state and mutate it through `commit_locked()`.
- If `telegram-bot.json` remains necessary for adapter interop, mirror committed state to that file behind a documented boundary.

## Telegram settings and relay state can overwrite each other in `telegram-bot.json`

**Severity:** Medium - the UI settings endpoint and Telegram relay both read-modify-write the same JSON file, and the settings mutex only protects one process.

`src/telegram_settings.rs:20` defines a process-local mutex, while `src/telegram.rs` can still run in the standalone `cargo run -- telegram` process and write the same file. Concurrent `/api/telegram/config` saves and relay cursor persistence can lose either UI-owned token/config fields or runtime-owned `chatId` / `nextUpdateId` fields. Atomic file replacement prevents partial files, but it does not serialize read-modify-write cycles across processes.

**Current behavior:**
- Settings saves and relay state persistence share one file.
- Writes are read-modify-write operations without cross-process serialization.
- The process-local mutex does not coordinate server and standalone relay modes.
- Last writer wins if the two processes read old state and then save different halves.

**Proposal:**
- Split UI config and runtime cursor/chat state into separate files, or guard all writers with an OS-level file lock.
- Add cross-process interleaving coverage proving config and runtime state both survive competing writes.

## `POST /api/telegram/test` rate limiting is per token and can be bypassed

**Severity:** Medium - a script can rotate bogus token values to fan out outbound requests and grow the rate-limit cache.

`src/telegram_settings.rs:355` keys the cooldown by token hash. Phase 1 single-user local mitigates the practical risk, but a local caller can submit many unique invalid tokens and bypass the per-token cooldown while causing repeated outbound requests to Telegram.

**Current behavior:**
- Token validation enforces maximum length but not Telegram token shape before the network call.
- Repeated tests of the same token are throttled.
- Unique token values bypass the cooldown.
- The in-memory rate-limit map can grow until retention cleanup.

**Proposal:**
- Validate Telegram token shape before network calls.
- Add a global/concurrent cap and a bounded cache.
- Rate-limit the endpoint independently of token identity.

## Telegram routes and tail-session hydration are missing from the architecture endpoint table

**Severity:** Low - newly implemented client-visible API behavior is not reflected in the central REST endpoint documentation.

`src/main.rs:233` registers `/api/telegram/status`, `/api/telegram/config`, and `/api/telegram/test`, but `docs/architecture.md` does not list these routes in its endpoint table. The same table still describes `GET /api/sessions/{id}` as a full-session fetch even though the new `?tail=N` query can intentionally return a partial local transcript with `messagesLoaded: false`.

**Current behavior:**
- The feature brief mentions Telegram endpoints.
- The architecture REST table omits methods, status/error semantics, and response shapes for the implemented Telegram routes.
- The session-fetch contract does not document `tail`, the tail cap/semantics, or the partial-response shape.

**Proposal:**
- Add the Telegram routes to `docs/architecture.md` with methods, request/response shapes, and error semantics.
- Document `GET /api/sessions/{id}?tail=N`, including when `messagesLoaded` is false and why callers must treat that response as a tail window rather than a full transcript.

## Telegram settings UI belongs behind a focused module boundary

**Severity:** Low - the Telegram panel adds a large independent API workflow to the already broad preferences panel module.

`ui/src/preferences-panels.tsx:1214` adds several hundred lines of Telegram settings state, effects, API calls, payload shaping, and rendering to a file that already owns multiple preferences panels.

**Current behavior:**
- Telegram settings lifecycle/config UI lives inside `preferences-panels.tsx`.
- Fetch/save/test behavior and render structure are coupled to the broad preferences module.

**Proposal:**
- Extract Telegram settings UI and its fetch/save/test hook into a dedicated preferences or telegram-settings module.

## `useInitialActiveTranscriptMessages` mutates a ref during render

**Severity:** Medium - the new long-session tail-window hook writes `hydrationRef.current.sessionId` and `hydrationRef.current.hydrated = true` during render, breaking React 18 Strict Mode / concurrent rendering invariants.

`ui/src/panels/AgentSessionPanel.tsx:236-285`. The hook is part of the long-session tail-window path that activates only on transcripts above ~512 messages. Concurrent renders can flip `hydrated: true` before the actual commit, causing the windowing optimization to be skipped on first paint of large sessions. Worse, the second render re-keys the ref, potentially losing the "I started hydrating" intent. Most hooks in `panels/` use `useState` for derived-from-prop state with explicit reset effects.

**Current behavior:**
- `if (hydrationRef.current.sessionId !== sessionId) { hydrationRef.current = { hydrated: false, sessionId }; }` mutates during render (line 242-247).
- `if (!isTailEligible && messages.length > INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES) { hydrationRef.current.hydrated = true; }` mutates during render (line 255-257).
- Strict Mode double-invoke fires the mutation twice without committing.

**Proposal:**
- Convert to `useState` with `useEffect` reset.
- Or use the React-docs "derived state" pattern: `const [prevSessionId, setPrev] = useState(sessionId); if (prevSessionId !== sessionId) { setPrev(sessionId); setHydrated(false); }`.
- Add Strict Mode coverage proving the windowing path still activates after a double-render.

## Active-transcript tail-window hook overlaps with `VirtualizedConversationMessageList`'s bottom-mount path

**Severity:** Medium - two layers (panel + virtualizer) gate "skip work for the tail" with different thresholds and different effects on dependent UI.

`ui/src/panels/AgentSessionPanel.tsx:175-286 useInitialActiveTranscriptMessages` windows messages to the last 96 before passing them to `ConversationMessageList` → `VirtualizedConversationMessageList`. The virtualizer's `preferInitialEstimatedBottomViewport` (round 53 addition) mounts the bottom range without rendering all messages above. The hook drops messages from React's perspective entirely (so `messageCount` becomes 0 → overview rail hides via `messageCount: isInitialTranscriptWindowActive ? 0 : visibleMessages.length` at line 804), while the virtualizer would just not mount unused slabs. A future reader changing the threshold has two places to keep in sync.

**Current behavior:**
- Hook drops messages above a 512-message session threshold, returning a 96-message tail.
- Virtualizer mounts only the bottom-of-viewport range via `preferInitialEstimatedBottomViewport`.
- Overview-rail gating uses `messageCount: 0` when the hook is windowing, hiding the rail.

**Proposal:**
- Move all "long session initial mount" logic into the virtualizer alone, then drop the hook.
- Or document the layer split with a header comment naming which problem each layer owns and why two layers exist.

## Telegram settings HTTP API split across three routes diverges from `/api/settings` convention

**Severity:** Medium - every other settings surface uses `POST /api/settings` returning `StateResponse` with SSE broadcast; Telegram uses `GET /api/telegram/status` + `POST /api/telegram/config` + `POST /api/telegram/test` returning `TelegramStatusResponse` with no broadcast.

`src/main.rs:233-235`. The `/test` route reasonably stays separate (genuinely a side-effecting outbound call). But splitting the GET/POST status+config into its own route is a divergence from the established pattern. The split also means none of the rest of the codebase's settings infrastructure (revision bumping, SSE broadcast, partial-payload merging via `UpdateAppSettingsRequest`) applies. A future caller scripting via the API has two patterns to learn.

**Current behavior:**
- Existing settings flow through `POST /api/settings` returning `StateResponse` (broadcast via SSE).
- Telegram settings use three new routes returning custom `TelegramStatusResponse` (not broadcast).
- The divergence is unexplained in code or docs.

**Proposal:**
- Fold the Telegram config bag into `UpdateAppSettingsRequest` with a `telegram: Option<UpdateTelegramConfigRequest>` field, returning `StateResponse` like every other setting.
- Or document explicitly in `docs/features/` why Telegram is intentionally separated (e.g., "secret tokens kept out of the broadcast snapshot").

## `validate_telegram_config` does TOCTOU between in-memory validation and on-disk persistence

**Severity:** Low - the validation reads `inner.projects` and `inner.sessions` while holding the state mutex, releases the lock, then `persist_telegram_bot_file(&file)?` writes. Between release and write, another thread could delete the validated project, leaving a persisted config that references a now-missing project.

`src/telegram_settings.rs:138-208`. The lock is correctly NOT held across I/O — that's the right call — but the TOCTOU window means the next status fetch will silently strip the dropped project ID via `sanitize_telegram_config_for_current_state`, which can be surprising to the user who just clicked Save. The read-time sanitize covers the symptom but not the underlying inconsistency.

**Current behavior:**
- Validation acquires the mutex briefly, then drops it.
- Persistence runs without holding the mutex.
- A concurrent project deletion between validation and persistence persists a stale reference.

**Proposal:**
- Add a header comment explaining the TOCTOU model and the sanitize-on-read recovery path.
- Or run `sanitize_telegram_config_for_current_state` after `validate_telegram_config` so the persisted file matches what the next read would return.

## `TelegramPreferencesPanel` does not memoize handlers, diverging from sibling preference panels

**Severity:** Low - `projectOptions` and `sessionOptions` are memoed, but `updateDraft`, `toggleProject`, `handleSave`, `handleTestConnection`, and the inline `onChange` lambdas at lines 1797, 1822, 1834 are recreated on every render. The two `ThemedCombobox` controls receive new function identity on every keystroke. Pattern divergence with `RemotePreferencesPanel` and other sibling panels in the same file.

`ui/src/preferences-panels.tsx:1214-1971`. A future reader copy-pasting from one panel to another now has two patterns to choose from.

**Current behavior:**
- Handlers are recreated on every render.
- Sibling preference panels in the same file memoize handlers.
- ThemedCombobox children receive new identity on every keystroke.

**Proposal:**
- Stabilize handlers via `useCallback`.
- Or document explicitly that the panel intentionally avoids memoization. Either is fine; consistency is the architectural goal.

## `src/telegram_settings.rs` module header doesn't enumerate critical invariants

**Severity:** Low - the header explains the file format transition but does not document the two-writer race, validation TOCTOU, divergent lock-error handling, or sanitize-on-read recovery model.

`src/telegram_settings.rs:1-9`. The header describes "the relay loop still reads the legacy flat runtime fields … the file format below keeps those fields flat and adds a `config` object", but does not document: (a) the two-writer race with the standalone CLI relay, (b) the validation TOCTOU window, (c) why the lock-error handling diverges from project convention, or (d) the sanitize-on-read recovery model. This is the entry point for the next reader who needs to extend the module (e.g., the Phase 1 in-process relay lifecycle).

**Current behavior:**
- Header enumerates the file-format transition but no invariants.
- Future readers risk regressing the implicit contracts.

**Proposal:**
- Extend the header to enumerate (a) what owns what in the file, (b) coordination assumptions between writers, (c) lock-failure / IO-failure recovery model.

## `persist_telegram_bot_state` reads-then-writes the file unconditionally on every state change

**Severity:** Low - the relay polls every `TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS` (5s default) and writes whenever `dirty`. The new logic adds a `fs::read` + `serde_json::from_slice` round-trip on every persist, doubling syscalls.

`src/telegram.rs:190-205`. Modest cost on its own. More concerning: if the file is concurrently being rewritten by the HTTP route, `fs::read` could observe a partial write (since `fs::write` truncates and rewrites without atomicity), and the relay would silently `unwrap_or_default()` — meaning a corrupt-read is treated as "first ever persist" and the next write erases the `config` portion. Pairs with the existing "Telegram settings and relay state can overwrite each other" entry.

**Current behavior:**
- Each persist does `fs::read` + parse + merge + `fs::write`.
- A partial-read mid-concurrent-write silently degrades to defaults.
- The next write erases legitimate config.

**Proposal:**
- Combine with the atomic-write fix on the existing two-writer-race entry.
- Distinguish "file does not exist" (legitimate first-run) from "file exists but unparseable mid-write" (warn + retry).

## No length validation on `default_project_id`, `default_session_id`, or `subscribed_project_ids`

**Severity:** Low - `bot_token` now has a 256-char cap (round 55), but the project/session id fields and the subscribed list count remain uncapped.

`src/telegram_settings.rs:30-56` + `src/wire.rs:1012-1029`. Phase 1 single-user trust boundary makes this practically unexploitable, but the absence of any sanity check on these remaining fields is worth noting for symmetry with the bot-token cap.

**Current behavior:**
- No `MAX_PROJECT_ID_LEN` or `MAX_SUBSCRIBED_PROJECTS` cap.
- A multi-MB JSON body in those fields is accepted up to the global 10MB limit.

**Proposal:**
- Add `MAX_PROJECT_ID_LEN` (256 bytes), `MAX_SUBSCRIBED_PROJECTS` cap.
- Reject in `update_telegram_config` with `ApiError::bad_request`.

## `SessionPaneView` `paneScrollPositions` in deps adds no reactivity

**Severity:** Low - the dependency on the dictionary identity is stable across renders for the same `pane.id`; mutations inside the dictionary do not trigger the effect. False reactivity impression for future readers.

`ui/src/SessionPaneView.tsx:1869-1900`. Either drop the dep with an `eslint-disable` comment explaining why, or capture the dependency narrowly (e.g., `paneScrollPositions[scrollStateKey]?.shouldStick`).

**Current behavior:**
- `paneScrollPositions` dict identity is stable across renders.
- Mutations inside the dict don't trigger the effect.
- The dep gives a false impression of reactivity.

**Proposal:**
- Drop the dep with an `eslint-disable` comment, or narrow to the specific value being read.

## `ConversationOverviewRail` per-segment fresh handlers and aria-label per render

**Severity:** Low - up to 160 segment buttons each get fresh `onClick`/`onKeyDown` arrow functions per render, plus a fresh `aria-label` string from `overviewSegmentLabel(segment, projection.items)` (an O(n) lookup against `projection.items.length`).

`ui/src/panels/ConversationOverviewRail.tsx:267-289`. Acceptable today, but as transcripts grow this is the next hot spot if rail rebuilds churn.

**Current behavior:**
- Each render creates 160 arrow functions and 160 aria-label strings.
- aria-label computation is O(n) against `projection.items`.

**Proposal:**
- Memoize per-segment handlers via a single delegated handler that reads the segment index from `data-conversation-overview-index`.
- Cache aria-labels alongside the segments.

## `ThemedCombobox` `useEffect` deps include `activeIndex`, tearing down listeners per keystroke

**Severity:** Low - the outside-pointer/keyboard handler effect re-attaches the global `pointerdown`/`keydown` listeners every time `activeIndex` changes (every ArrowUp/ArrowDown).

`ui/src/preferences-panels.tsx:1782-1859`. Functionally correct, but wasteful. If the same keystroke that triggered the change also fires a synthetic `keydown`, ordering between "old listener cleanup" and "new listener registration" is invisible to React.

**Current behavior:**
- Effect deps `[activeIndex, isOpen, onChange, options]` rebuild listeners per keystroke.
- Each open menu sees attach/detach churn.

**Proposal:**
- Move `activeIndex` into a ref synchronized with the state update; drop it from deps.
- Or split the effect into "attach listeners once when open" + "read activeIndex from a ref".

## `AgentSessionPanel.tsx` exceeds 2000-line architecture rubric threshold

**Severity:** Note - `ui/src/panels/AgentSessionPanel.tsx:1475` and `ui/src/panels/AgentSessionPanel.test.tsx`. The panel remains over the documented TSX file-size budget, and the composer resize/transition behavior is now a local state machine inside `SessionComposer`.

This review adds and exercises multiple rAF/transition refs plus cancellation/restore ordering in the same component. The behavior is UI-local, but the ordering contract is subtle enough that future changes are hard to reason about inside the broader panel file.

**Current behavior:**
- `AgentSessionPanel.tsx` is 2605 lines.
- `AgentSessionPanel.test.tsx` is 8677 lines.
- Composer auto-resize and transition restoration share state across several refs and rAF callbacks.

**Proposal:**
- When touching this area again, extract textarea sizing/transition behavior into a focused hook such as `useComposerAutoResize`.
- Keep targeted tests for resize scheduling, transition restoration, and session-switch cleanup with that hook.

## `scheduleConversationOverviewRailBuild` module-level FIFO queue is shared across all controller instances

**Severity:** Note - a slow rail build in pane A delays pane B by one frame; cleanup story across module reloads / HMR is subtle.

`ui/src/panels/conversation-overview-controller.ts:33-87`. The module-level `pendingConversationOverviewRailBuildTasks: ConversationOverviewRailBuildTask[]`, `conversationOverviewRailBuildFrameId: number | null`, and `nextConversationOverviewRailBuildTaskId: number = 1` are shared across all controllers/sessions/panes. Acceptable per the rAF cadence (60Hz = 16ms/frame). The cleanup logic (cancel-on-empty-queue, splice-by-task-id) is correct but the global-state coupling means HMR / module reloads have subtle behavior.

**Current behavior:**
- All rail builds across all panes serialized through one global FIFO queue.
- One slow task delays all subsequent panes by one frame.
- Module-level globals make cleanup-across-HMR subtle.

**Proposal:**
- Defer (no concrete bug today). Consider in a future round whether per-pane queues would simplify reasoning, especially as multi-pane scenarios become more common.

## CSS context-menu pattern duplicated between pane-tab and conversation-marker variants

**Severity:** Low - two near-third "context menu" features now share ~80% of the same CSS shell; the third copy will be the trigger for extraction but it should be promoted to a `.context-menu` family before then.

`ui/src/styles.css:3981-4023` (new `.conversation-marker-context-menu*`) and `:2506-2546` (existing `.pane-tab-context-menu*`). Same `position: fixed`, z-index ordering, `color-mix(in srgb, var(--surface-white) ...)` background pattern, `box-shadow: 0 20px 40px color-mix(in srgb, var(--ink) 14%, transparent)`, hover/focus blue mix, `*-item-danger` red. Differences are only `min-width`, `border-radius` (custom 1rem vs `var(--control-radius)`), padding values, and `border: 1px solid var(--line)` vs unbordered. The pattern is reusable as a `.context-menu` / `.context-menu-item` / `.context-menu-item-danger` family.

**Current behavior:**
- Two near-duplicate context-menu CSS blocks.
- Small variations are unique-to-call-site.
- Future third instance would copy a third near-duplicate.

**Proposal:**
- Promote the shared shell + item rules into a base `.context-menu` set.
- Let `.pane-tab-context-menu` and `.conversation-marker-context-menu` carry only their unique tweaks (`min-width`, `border-radius`, `border`, separator).
- Defer if the variations are deliberately divergent — but mark this as a known cluster so the third instance triggers extraction.

## `SessionPaneView.tsx` near-bottom early-out is captured at `isSending` flip, not reactive

**Severity:** Low - the catchup branch never schedules for the started-near-bottom-then-scrolled-away case, contrary to what the comment promises.

`ui/src/SessionPaneView.tsx:2024`. The effect has dependency array `[isSending, pane.viewMode, scrollStateKey]`, and `isMessageStackNearBottom()` reads from `messageStackRef.current` (not reactive). The effect's near-bottom decision is therefore evaluated only at the moment `isSending` flips true. If the user starts the send near bottom but scrolls away while the request is in flight, the catchup branch (`scheduleSettledScrollToBottom` via `followLatestMessageForPromptSend`) never schedules.

**Current behavior:**
- Near-bottom snapshot captured only at `isSending` true→false transition.
- User scrolling away during the in-flight request bypasses catchup.
- Comment overstates the guarantee ("schedule the settled-poll catchup here to bring the user's prompt into view once it lands").

**Proposal:**
- Add a reactive signal (e.g., a derived `nearBottomAtSendStart` captured into the effect's deps via a ref-based subscription).
- Or update the comment to match the actual behavior ("when the user is near bottom AT THE TIME isSending toggled, defer entirely to the post-message-land effect").

## `src/tests/telegram.rs` header documents fewer pinned axes than the file currently covers

**Severity:** Note - test ownership header is stale relative to the +285-line additions.

`src/tests/telegram.rs:1-12` describes test ownership as "pin two pieces of that adapter: `parse_telegram_command` ... and `render_telegram_digest` / `build_telegram_digest_keyboard`". The +285-line additions now cover assistant-forwarding partial-progress, no-baseline forwarding, unknown char-count re-forwarding, unknown-session-status gating, error classification, log sanitization, and prompt byte-limit — these go beyond what the header claims.

**Current behavior:**
- Header at lines 1-12 covers two pinned axes.
- File now pins many additional axes.

**Proposal:**
- Update the header to enumerate the additional pinned axes (or summarize as "Telegram relay test surface: command parsing, digest rendering, assistant forwarding, error classification, log sanitization, prompt limits").
- Or split the file if the assistant-forwarding family becomes its own pinned axis.

## `messageCreatedDeltaIsNoOp` lacks semantic-change negative coverage

**Severity:** Medium - the identical-replay tests do not prove the no-op predicate still material-applies when the message payload changes while metadata stays equal.

`ui/src/live-updates.ts:320` compares the existing message payload and metadata to decide whether a `messageCreated` replay is no-op. Current tests cover identical duplicate replay behavior, but do not keep id/index/preview/count/stamp the same while changing a semantic message payload field. An over-broad predicate could drop a real `messageCreated` update while the current tests still pass.

**Current behavior:**
- Duplicate identical `messageCreated` replay coverage exists.
- Semantic-change negative cases are missing for same id/index/metadata.
- Same-id pending prompt cleanup interaction is not directly pinned.

**Proposal:**
- Add negative cases in `live-updates.test.ts` for same id/index/metadata with changed message payload.
- Add coverage for same-id pending prompt cleanup so no-op detection cannot skip required prompt removal.

## Near-bottom prompt-send early return lacks direct scroll coverage

**Severity:** Medium - the prompt-send stutter fix is not directly pinned by a near-bottom pending-POST test.

`ui/src/SessionPaneView.tsx:2024` returns early when the message stack is already near bottom so the old-bottom smooth scroll does not race the later post-message scroll. Existing scroll coverage pins the far-from-bottom catch-up path, but not this near-bottom skip. A regression could reintroduce the old-target smooth scroll and visible stutter without failing the current suite.

**Current behavior:**
- Near-bottom sends skip the old-bottom smooth-scroll effect.
- Far-from-bottom prompt catch-up is covered.
- No test starts near bottom, keeps the POST pending, grows `scrollHeight`, and asserts no old-target smooth scroll fires before the prompt lands.

**Proposal:**
- Add an `App.scroll-behavior.test.tsx` case that starts near bottom, sends with a pending POST, grows `scrollHeight`, and asserts no old-target smooth scroll occurs before the prompt lands.

## Telegram command suffix parsing conflates foreign-bot commands with unknown commands

**Severity:** Medium - commands addressed to another bot can make TermAl respond, while valid suffixed setup commands can be ignored.

`src/telegram.rs:790` treats `parse_telegram_command_for_bot` returning `None` as an unknown command and sends help, even when the reason is "this command was addressed to a different bot." The unlinked setup branch still uses `parse_telegram_command(text)` without the resolved bot username, so `/start@termal_bot` and `/help@termal_bot` can be ignored in standard Telegram group-command form.

**Current behavior:**
- Foreign-bot suffixes and unknown commands share the same `None` outcome.
- Linked chats can receive TermAl help for commands addressed to another bot.
- Unlinked suffixed `/start@termal_bot` / `/help@termal_bot` are not parsed with the bot-aware parser.

**Proposal:**
- Return a typed command parse outcome such as parsed / unknown / foreign-bot.
- Ignore foreign-bot commands.
- Use the bot-aware parser in the unlinked `/start` / `/help` path once the username is known.

## Telegram JSON parsing paths lack sample-shape coverage

**Severity:** Low - error envelope and session-status serde behavior can regress while current tests that construct internal structs still pass.

`src/telegram.rs:567` and related Telegram/TermAl response parsing now include behavior for Telegram error envelopes and `TelegramSessionStatus` values. The tests mostly construct internal Rust structs directly, so sample JSON with `error_code`, unknown status, or missing status can drift from the actual API shapes without being caught.

**Current behavior:**
- Classifier/status tests mostly use constructed Rust values.
- Telegram error-envelope JSON parsing is not pinned with sample payloads.
- TermAl session status parsing for unknown/missing status is not pinned with sample payloads.

**Proposal:**
- Add sample JSON deserialization tests for Telegram error envelopes.
- Add TermAl session status sample JSON tests for known, unknown, and missing status values.

## Telegram-forwarded text has no per-chat rate cap

**Severity:** Medium - any linked chat can still fan out prompt submissions quickly enough to create a burst of local backend and agent work.

`src/telegram.rs:1654-1666` now rejects Telegram prompts above `MAX_DELEGATION_PROMPT_BYTES = 64 * 1024` before calling `forward_telegram_text_to_project`, but accepted prompts are still not rate-limited per chat. Command and callback actions dispatch backend work at `src/telegram.rs:1633` and `src/telegram.rs:1710`. A linked chat can submit many below-limit prompts or action commands in quick succession, each becoming local backend work and possibly an agent turn.

**Current behavior:**
- Oversized Telegram prompts are rejected by UTF-8 byte length.
- Below-limit prompts and action commands are forwarded unchanged.
- No per-minute or burst cap exists per linked chat before backend work starts.
- The default 1-second poll cadence can ingest those bursts quickly.

**Proposal:**
- Add a per-minute / per-chat prompt and action-command rate cap so a linked chat cannot fan out N HTTP calls per second.

## Telegram relay forwards full assistant text to Telegram by default

**Severity:** Medium - assistant replies can include code, local file paths, file contents, or secrets and are sent to a third-party service without an explicit opt-in.

`src/telegram.rs:1151-1160`. The relay chunks and forwards the full settled assistant message body to Telegram once the session is no longer active. This goes beyond the compact project digest and sends arbitrary model output off-machine by default.

**Current behavior:**
- The Telegram digest path is compact, but settled assistant messages are forwarded in full.
- Assistant text may contain local workspace details or user-provided secrets.
- Users enabling the relay do not get a separate opt-in for full-content forwarding.

**Proposal:**
- Make full assistant text forwarding an explicit opt-in setting.
- Keep digest-only forwarding as the default for Telegram integrations.
- Document the third-party content exposure and add any practical redaction/truncation before full forwarding.

## Telegram `getUpdates` batch processing is unbounded and re-runs on poll-iteration panic

**Severity:** Medium - a Telegram update burst (real attack, retry storm, or accidental flood) becomes a multiplicative wave of HTTP calls into the local backend, and a panic mid-batch re-runs the same updates on the next poll.

`src/telegram.rs:44-78` and `src/telegram.rs:304`. The relay accepts the entire `Vec<TelegramUpdate>` Telegram returns and walks each update through `handle_telegram_update`, which can issue multiple outbound HTTP calls per update (digest fetch, send_message, action dispatch, session fetch). State is persisted once at the end of the iteration; a panic mid-batch leaves `next_update_id` un-advanced and Telegram resends the same batch on the next poll, amplifying the effect.

**Current behavior:**
- `getUpdates` does not pass an explicit `limit`, so Telegram returns up to its server-side default (100).
- A 100-update batch can fan out to several hundred backend HTTP calls.
- Mid-batch panic loses all per-update state (including advanced `next_update_id`), and Telegram replays.

**Proposal:**
- Cap `getUpdates` `limit` (e.g., 25) on the request side.
- Persist `next_update_id` per update inside the batch loop rather than once at the end.
- Add a per-iteration backoff after errors so a sustained failure does not tight-loop.

## `preferStreamingPlainTextRender` prop name and "Three render paths" comment are stale after the unified-render refactor

**Severity:** Medium - public-shaped prop semantics drift silently from a removed implementation; future readers see a name describing code that no longer exists.

`ui/src/message-cards.tsx:311, 327, 359-388`. The `StreamingAssistantTextShell` render path was removed; the prop now functionally means "this is the live-streaming assistant message". The leading comment block ("Three possible render paths...") describes the old three-path implementation, contradicted by the next paragraph saying "always renders through `<DeferredMarkdownContent>`". Test names and the prop signature carry the old name.

**Current behavior:**
- `preferStreamingPlainTextRender` is the only render-path discriminator but its name suggests a removed plain-text shell.
- The leading comment describes three paths; only one survives.

**Proposal:**
- Rename to `isStreamingAssistantTextMessage` (matching the local `isStreamingAssistantMessage` variable already used inside `MessageCardImpl`).
- Replace the "Three possible render paths" sentence with the current single-path explanation.
- Pure refactor, no behavior change.

## CSS bubble `width: fit-content` transition causes horizontal layout reflow at turn end

**Severity:** Medium - the "stable component subtree across stream → settle" goal is partially undermined by a CSS-driven layout jump.

`ui/src/styles.css:4448-4452`. `:has(.markdown-table-scroll)` applies `width: fit-content; max-width: min(96rem, 96%)` to the bubble. With the new `deferAllBlocks: true` policy a streaming bubble has NO `.markdown-table-scroll` until the turn settles (the table sits in `.markdown-streaming-fragment` instead). When streaming ends, the bubble's effective `max-width` jumps from default `42rem` to `96rem` AND its `width` switches to `fit-content` — the bubble grows wider, producing a visible horizontal reflow at the same moment the React subtree was supposed to be stable.

**Current behavior:**
- During streaming the bubble follows the prose-default sizing (`42rem` cap).
- On settle the `:has(.markdown-table-scroll)` selector engages and the bubble jumps to `fit-content` / 96rem cap.

**Proposal:**
- Anticipate `width: fit-content` while streaming if a `|`-line has been seen (e.g., a class on the streaming-fragment placeholder that triggers the same selector).
- Or: document the layout shift as accepted and add a regression that asserts bubble width remains stable across the stream→settle transition so any future drift is visible in tests.

## Mermaid aspect-ratio sizing can clip constrained diagrams

**Severity:** Medium - constrained Mermaid iframes can hide diagram content instead of only removing blank frame space.

`ui/src/mermaid-render.ts:134-152` sizes Mermaid iframes with `aspectRatio: ${frameWidth} / ${frameHeight}` + `height: auto`. The change fixes the wide-blank-frame regression, but the iframe document still keeps the SVG at intrinsic width while hiding vertical overflow. When `max-width: 100%` constrains the iframe, the frame height can shrink without the inner SVG scaling with it, so wide or tall diagrams can lose bottom content instead of simply removing unused whitespace.

**Current behavior:**
- The outer iframe height is driven by CSS `aspect-ratio` and constrained width.
- The inner Mermaid SVG can remain at intrinsic dimensions.
- The iframe hides vertical overflow, so constrained diagrams can be clipped at the bottom.

**Proposal:**
- Keep explicit intrinsic height for horizontally-scrollable diagrams, or make the inner SVG scale with the iframe width before using aspect-ratio sizing.
- Add visual/regression coverage for constrained wide and tall diagrams.

## Asymmetric `orchestrator_auto_dispatch_blocked` between two persist-failure rollback sites

**Severity:** Medium - an Error session can remain auto-dispatch-eligible after a runtime-exit commit failure while disk and memory disagree.

`src/session_lifecycle.rs:449` defensively sets `record.orchestrator_auto_dispatch_blocked = true` on persist-failure rollback in the stop-session path. `src/turn_lifecycle.rs:455` does NOT mirror that defensive set in the runtime-exit rollback path, and the inner block at `turn_lifecycle.rs:413` has already explicitly cleared the flag to `false` before the failed `commit_locked`. Net effect: if the runtime-exit commit fails, the session in-memory state is `SessionStatus::Error` with the "Turn failed: …" message, but the orchestrator can still observe it as eligible for auto-dispatch.

**Current behavior:**
- `stop_session` rollback sets `orchestrator_auto_dispatch_blocked = true` defensively.
- `handle_runtime_exit_if_matches` rollback leaves the flag at whatever the inner block last wrote (`false`).
- An Error session with a failed persist commit can still be re-dispatched.
- The new tests do not pin `orchestrator_auto_dispatch_blocked` in either rollback path.

**Proposal:**
- Either mirror the `session_lifecycle.rs` defensive set (`true`) in `turn_lifecycle.rs`, or document why the asymmetry is intentional.
- Tighten the persist-failure tests to also pin `orchestrator_auto_dispatch_blocked`, `runtime`, `runtime_stop_in_progress`, and the "stopped/failed" message presence.

## Mermaid fallback loader lives in the message card renderer

**Severity:** Low - Mermaid fallback loading and cache ownership are mixed into an already large rendering component.

`ui/src/message-cards.tsx:182` now owns dynamic import failure classification and global fallback script loading, while `mermaid-render.ts` already owns Mermaid rendering configuration and queueing.

**Current behavior:**
- Mermaid fallback loader/cache logic lives in `message-cards.tsx`.
- Rendering, fallback loading, and message-card composition are coupled in the same large module.

**Proposal:**
- Move fallback loading into `mermaid-render.ts` or a small `mermaid-loader.ts`.
- Keep message cards calling a single render helper.

## Markdown diff change-block grouping rules duplicated between renderer and index builder

**Severity:** Medium - the change-navigation index walker copies the renderer's grouping rules; future drift between the two will silently desynchronize navigation stops from rendered blocks.

`ui/src/panels/markdown-diff-view.tsx:508-526` and `ui/src/panels/markdown-diff-change-index.ts:60-87`. Both walks have identical logic: skip `normal`, gather consecutive non-`normal` segments, break at the same `current.kind === "added" && next.kind === "removed"` boundary, and produce identical id strings (`segments.map(s => s.id).join(":")`). The renderer then re-derives the same id and looks it up in a `Map<id, index>` the navigation code built from the index walker's output. The header comment in `markdown-diff-change-index.ts:46-54` explicitly acknowledges "the rule is duplicated here so the navigation index does not drift from what the user sees" — i.e., the only thing keeping the two walks in sync is the test suite.

**Current behavior:**
- Renderer (`renderMarkdownDiffSegments`) and index builder (`computeMarkdownDiffChangeBlocks`) walk the same segment array twice with identical grouping rules.
- The navigation index is recovered by id-lookup against a Map built from the index walker's output.
- Any future change to the grouping rules (e.g., a third break-rule for a new segment kind) must be made in both places.

**Proposal:**
- Have `renderMarkdownDiffSegments` consume the precomputed `changeBlocks` directly. Iterate (`normal` segment OR `changeBlocks[changeBlockCursor]`); the renderer emits the editable section for normals and the `<section>` wrapper for the next change-block, advancing `changeBlockCursor` after each.
- Single source of truth for grouping rules in `computeMarkdownDiffChangeBlocks`; the navigation index becomes the literal cursor position, no Map lookup needed, and the renderer's per-render Map allocation goes away.

## Concurrent shutdown callers can flip `persist_worker_alive` before the join owner finishes

**Severity:** Medium - the documented "flag flips only after worker join" contract is not true when two `AppState` clones call `shutdown_persist_blocking()` concurrently.

`shutdown_persist_blocking()` takes the worker handle out of `persist_thread_handle`, releases that mutex, and then blocks in `handle.join()`. A second concurrent caller can enter while the first caller is still joining, see `None`, and run the idempotent branch that stores `persist_worker_alive = false`. That lets a concurrent `commit_delta_locked()` take the synchronous fallback while the worker may still be doing its final drain/write, reopening the dual-writer persistence race the round-13 ordering was meant to close.

**Current behavior:**
- The first shutdown caller owns the join handle but does not hold the handle mutex while joining.
- A second shutdown caller treats `None` as "already stopped" even if the first caller is still waiting for the worker to stop.
- The second caller can publish `alive == false` before the worker has actually exited.

**Proposal:**
- Serialize the full shutdown transition so no caller can observe the stopped state until the join owner has returned from `handle.join()` and stored `persist_worker_alive = false`.
- Alternatively replace the `Option<JoinHandle>` state with an explicit `Running` / `Stopping` / `Stopped` state so only the join owner can transition from stopping to stopped.

## Rendered diff regions reset document-level Mermaid/math budgets

**Severity:** Medium - splitting rendered diff preview into one `MarkdownContent` per region weakens existing browser-side render-budget guards.

The rendered diff view now maps every renderable region to its own `MarkdownContent`. `MarkdownContent` counts Mermaid fences and math expressions per rendered document, so this split resets `MAX_MERMAID_DIAGRAMS_PER_DOCUMENT` and `MAX_MATH_EXPRESSIONS_PER_DOCUMENT` for each region instead of for the full diff preview. A crafted or simply large diff with many Mermaid/math regions can render far more expensive diagrams/equations than the previous single synthetic-document path allowed.

**Current behavior:**
- Each rendered diff region gets an independent Mermaid/math budget.
- The whole rendered diff preview no longer has one aggregate render cap.

**Proposal:**
- Compute aggregate Mermaid/math counts before mapping regions and apply a document-level fallback when the aggregate exceeds the cap.
- Or pass a shared render-budget context/override into each region-level `MarkdownContent`.

## Post-shutdown persistence writes still leave a post-collection-pre-join window

**Severity:** Medium - round-13 closed the dual-writer file race, but a narrow gap remains between the worker's final `collect_persist_delta` and `handle.join()` returning.

Round 13 moved the `persist_worker_alive` flip from BEFORE the Shutdown signal to AFTER `handle.join()` returns. That closed the dual-writer hazard (concurrent fallback writes racing the worker's still-in-progress final drain on the same persistence path). However, after the worker captures its final delta but before `handle.join()` returns, a concurrent `commit_delta_locked` will observe `alive == true`, bump `inner.mutation_stamp`, and return without persisting. That mutation is not picked up by the worker (already past collection) nor by the sync fallback (flag still true). `commit_locked` and `commit_persisted_delta_locked` are unaffected because they call `persist_internal_locked` which itself errors and falls back when the channel becomes disconnected.

**Current behavior:**
- The dual-writer file race is closed by round 13.
- A narrow window between the worker's final `collect_persist_delta` and `handle.join()` returning still exists; `commit_delta_locked` calls in that window observe `alive == true` and return without persisting.
- `commit_locked` / `commit_persisted_delta_locked` infer fallback from `persist_tx.send` failure, but `persist_tx` only disconnects when the LAST `AppState` clone drops its sender; with multiple clones (which is the production shape), `send` succeeds silently into a worker that has exited.

**Proposal:**
- Either (a) serialize the worker's final drain with sync fallback by holding `inner` for the worker's collect-and-write final iteration, or (b) require callers to quiesce non-HTTP producers before invoking `shutdown_persist_blocking`.
- Add a regression that races a late `commit_delta_locked` with the worker's final collection and proves the final persisted state is the latest `StateInner`.
- Add an explicit `persist_worker_alive` Acquire check to `persist_internal_locked` so all four commit variants share one shutdown contract.

## Duplicate remote delta hydrations fall through to unloaded-transcript delta application

**Severity:** Medium - duplicate in-flight hydration callers receive `Ok(false)`, which every delta handler treats as "no repair happened; continue applying the delta".

The in-flight map suppresses duplicate `/api/sessions/{id}` fetches, but it does not coordinate the waiting delta handlers. For a summary-only remote proxy, a concurrent text delta or replacement can still run against missing messages and trigger a broad `/api/state` resync; a message-created delta can partially mutate an unloaded transcript before the first full hydration finishes.

**Current behavior:**
- The first delta for an unloaded remote session starts full-session hydration.
- A duplicate delta for the same remote/session sees the in-flight key and returns `Ok(false)`.
- Callers continue into the narrow delta path as if no hydration was needed.

**Proposal:**
- Return a distinct outcome such as `HydrationInFlight`, or have duplicates wait/queue behind the first hydration.
- After the first hydration completes, re-check the session transcript watermark before applying queued or retried deltas.
- Add burst/concurrent same-session delta coverage proving only one remote fetch occurs and duplicate deltas do not mutate unloaded transcripts.

## Text-repair hydration lacks live rendering regression coverage

**Severity:** Medium - the lower-revision text-repair adoption path is covered only by a classifier unit test.

The new adoption rule is intended to fix the user-visible bug where the latest assistant message stays hidden until an unrelated focus, scroll, or prompt rerender. The current coverage proves the pure classifier returns `adopted`, but it does not prove the live hook requests the flagged hydration, adopts the lower-revision session response after an unrelated newer live revision, flushes the session slice, and renders the repaired text immediately.

**Current behavior:**
- `classifyFetchedSessionAdoption` has a unit test for divergent text repair after a newer revision.
- No hook or app-level regression drives `/api/sessions/{id}` through the live-state path and asserts immediate transcript rendering.

**Proposal:**
- Add a `useAppLiveState` or `App.live-state.reconnect` regression where text-repair hydration is requested, a newer unrelated live event advances `latestStateRevisionRef`, the session response resolves at the original request revision, and the active transcript updates without any extra user action.

## Timer-driven reconnect fallback can stop after `/api/state` progress before SSE proves recovery

**Severity:** Medium - a fallback snapshot can refresh visible UI while the live EventSource transport is still unhealthy.

`ui/src/app-live-state.ts:2068` disables `rearmUntilLiveEventOnSuccess` when a same-instance `/api/state` response makes forward revision progress, unless the recovery path is the manual-retry variant. A successful `/api/state` fetch proves that polling can reach the backend and can repair visible state, but it does not prove the SSE stream has reopened or can deliver later assistant deltas. If the transport remains broken, a later live message can stay hidden until another reconnect/error/user action restarts recovery.

**Current behavior:**
- Timer-driven reconnect fallback asks to keep polling until live-event proof.
- Same-instance `/api/state` forward progress disables that live-proof rearm path for non-manual recovery.
- UI state can look refreshed while the EventSource transport is still unconfirmed.

**Proposal:**
- Split "snapshot refreshed UI" from "transport recovered" in the reconnect state machine.
- Keep reconnect polling armed until `confirmReconnectRecoveryFromLiveEvent()` runs from a data-bearing SSE event, unless a cause-specific recovery path intentionally documents a different contract.
- Add a regression that adopts same-instance `/api/state` progress through the timer-driven reconnect path, keeps SSE unopened/unconfirmed, advances timers, and asserts another fallback poll is scheduled.

## Remote hydration in-flight cleanup can race with the RAII guard

**Severity:** Low - clearing `remote_delta_hydrations_in_flight` by key can remove or later invalidate a newer in-flight hydration for the same remote/session.

The remote hydration guard removes its `(remote_id, session_id)` key on drop. `clear_remote_applied_revision` can also remove keys for a remote while an older hydration guard is still alive. If a later hydration inserts the same key after that cleanup, the older guard can drop afterward and remove the newer marker, allowing duplicate hydrations despite the guard.

**Current behavior:**
- In-flight hydration entries are keyed only by `(remote_id, session_id)`.
- Remote continuity cleanup can remove a live key while the guard that owns it is still alive.
- A stale guard drop cannot distinguish its own entry from a newer entry with the same key.

**Proposal:**
- Store a unique token or generation per in-flight entry and remove only when the token still matches.
- Or avoid clearing live in-flight markers during remote continuity reset; let the owning guard retire its own marker.
- Add cleanup tests covering overlapping guards and per-remote cleanup.

## Lagged force-adopt marker clearing on EventSource reconnect lacks coverage

**Severity:** Low - the frontend now clears an armed lagged recovery marker on EventSource error/reconnect, but no test pins that boundary.

The new baseline guard covers same-stream stale recovery after a newer delta, but a separate hazard is an old `lagged` marker surviving across a closed EventSource into a new stream. The implementation clears the marker on reconnect/error cleanup, yet no regression proves a stale lower/same-instance state on the new stream cannot be force-adopted.

**Current behavior:**
- `clearForceAdoptNextStateEvent` runs during EventSource error/reconnect cleanup.
- Existing lagged tests do not cross an EventSource boundary.

**Proposal:**
- Add a reconnect test that dispatches `lagged`, triggers `error`, opens a new EventSource, and sends a lower/same-instance state that must not be force-adopted.

## Remote hydration dedupe coverage bypasses the production burst path

**Severity:** Low - the current duplicate-hydration test manually seeds the in-flight map instead of driving real bursty remote deltas.

The test pins the duplicate branch, but it would not catch a regression where the first real hydration leaks the guard, where a successful hydration does not clear the marker, or where multiple actual same-session delta handlers still issue duplicate remote session fetches.

**Current behavior:**
- The test inserts an in-flight key directly.
- It does not prove the first production hydration inserts and clears the guard.
- It does not prove bursty same-session deltas issue only one remote session fetch.

**Proposal:**
- Add coverage for a successful hydration path that asserts the guard is removed afterward.
- Add a burst/concurrent same-session delta case that asserts only one remote session fetch is issued.

## `apply_remote_state_if_newer_locked` `force: bool` parameter is unnamed at call sites

**Severity:** Low - seven call sites pass `false` and one passes `true`; readers cannot tell what `force` means without consulting the function signature.

`apply_remote_state_if_newer_locked` was extended with a `force: bool` parameter so that `apply_remote_lagged_recovery_state_snapshot` can bypass the same-revision replay gate. The parameter is correct, but the convention scales poorly: a future caller that copies a neighbouring `false` from any of the seven existing sites will inherit the gated behaviour without realising the parameter exists, and a future maintainer who needs the bypass at a different site will have to re-derive what the boolean means.

**Current behavior:**
- `apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None, false)` appears at seven call sites.
- One new call site passes `true` for lagged-recovery force-apply.
- The doc-comment on the function explains the parameter, but the call sites do not self-document.

**Proposal:**
- Replace `force: bool` with a typed `enum SnapshotApplyMode { GateBySnapshotRevision, ForceApplyAfterLagged }` (or similar). All existing call sites become `SnapshotApplyMode::GateBySnapshotRevision`; the lagged-recovery site reads `SnapshotApplyMode::ForceApplyAfterLagged` and self-documents.
- Optional: also push the bypass-gate into a tiny inline comment at the lagged-recovery site naming the upstream invariant (`api_sse.rs::state_events` yields `state` immediately after `lagged` within one `tokio::select!` arm).

## SSE recreation control plane is split between `sseEpoch` state and `pendingSseRecreateOnInstanceChangeRef`

**Severity:** Medium - two coordination mechanisms for one concern, increases regression risk and reduces debuggability.

`forceSseReconnect()` sets `pendingSseRecreateOnInstanceChangeRef.current = true` synchronously and the consume happens inside `adoptState` only when `fullStateServerInstanceChanged` is true. This adds a second control plane for SSE reconnection alongside the existing `sseEpoch` state, with the ref-vs-state ordering being load-bearing (synchronous `setSseEpoch` would tear down the in-flight probe). The pattern is documented inline, but ref state is not visible in React DevTools or in any state diff, so a subsequent maintainer reading the SSE transport effect cannot see the gate that determines whether the effect re-runs. The Round 8 comment in the doc-block notes this exact split-plane pattern was reverted before for the same reason. There is also no clear-on-no-instance-change reset path: if `forceSseReconnect()` fires but the recovery probe response comes back as same-instance (false alarm), the flag stays armed and could fire on a much later legitimate restart.

**Current behavior:**
- `forceSseReconnect()` mutates a ref invisible to DevTools.
- The flag is consumed only inside the `fullStateServerInstanceChanged` branch of `adoptState`.
- A successful recovery probe with no instance change leaves the flag armed indefinitely.
- The flag-on-adopt ordering relative to `setSseEpoch` is not pinned by a load-bearing test (current tests assert the recreate happens but not the ordering).

**Proposal:**
- Lift the gate into a state-driven shape (e.g., a single `sseReconnectReason` state with `instanceChangeAfterAdopt` as one of its values), so the SSE reconnection trigger is visible in React DevTools.
- Or add a load-bearing test that fails if the consume-on-adopt ordering is reversed.
- Either way, clear `pendingSseRecreateOnInstanceChangeRef` on any `adoptState` success that does not change the instance, so a false-alarm `forceSseReconnect()` cannot fire on a later legitimate restart.

## Sticky shutdown tests bypass `/api/events` stream wiring

**Severity:** Medium - helper-level tests can pass while the production SSE handler still hangs during shutdown.

The new tests validate the sticky `watch` shutdown helper directly, but they do not exercise `state_events` or the `/api/events` route using that signal. A future regression in the stream's pre-loop checks or select wiring could keep long-lived SSE connections open and block graceful shutdown while the helper tests still pass.

**Current behavior:**
- Tests cover the shutdown signal helper before/after registration.
- They do not hold or open `/api/events` streams and assert termination through the route handler.

**Proposal:**
- Add route-level SSE shutdown tests for shutdown-before-connect and shutdown-after-initial-state.
- Wrap both in timeouts so missed shutdown delivery fails loudly.

## Shutdown signal registration errors can look like real shutdown

**Severity:** Medium - `src/main.rs:147-166`. The new `shutdown_signal()` helper ignores `tokio::signal::ctrl_c().await` errors, and on Unix the SIGTERM branch completes immediately if `tokio::signal::unix::signal(...)` returns `Err`.

Those error paths should be diagnostics or startup failures, not successful shutdown triggers. If signal registration fails, the server can exit immediately after startup with little context.

**Current behavior:**
- Ctrl+C signal errors are discarded with `let _ = ...`.
- Unix SIGTERM registration failure makes the `terminate` future complete.
- The `tokio::select!` cannot distinguish a real shutdown signal from a signal-listener setup failure.

**Proposal:**
- Make signal setup fallible during startup and return an error if registration fails.
- Or log the registration/await error and park that branch with `std::future::pending::<()>().await` so it cannot trigger shutdown.

## Final shutdown persist failure exits without retry

**Severity:** Medium - `src/app_boot.rs:270-275`. The normal persist worker records failures and retries with backoff, but a shutdown tick sets `should_exit_after_tick` and breaks after the first final attempt even if that attempt failed.

A transient SQLite lock, disk hiccup, or I/O error during graceful shutdown can still drop pending mutations. The new drain logs the failure, but the process continues toward exit as though the final state reached disk.

**Current behavior:**
- `retry_state.record_result(&result)` records the final failure.
- `should_exit_after_tick` still breaks the loop immediately.
- Pending changed sessions can remain only in memory when the process exits.

**Proposal:**
- On shutdown, exit only after a successful final persist.
- Or use a bounded retry/timeout policy and return/log a shutdown failure outcome that clearly says durability was not confirmed.
- Add a test covering `Err` followed by `Ok` after `PersistRequest::Shutdown`.

## Triplicate `requestStateResync + startSessionHydration` recovery pattern in delta handler

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. Three near-identical recovery sites within ~110 lines of the same handler perform the same `requestStateResync({ rearmOnFailure: true }) + startSessionHydration(delta.sessionId)` pair. The `appliedNeedsResync` branch knows `delta.sessionId` is statically a string; the other two branches add a runtime guard (`"sessionId" in delta && typeof delta.sessionId === "string"`) — the type narrowing is subtly different at each site.

A future fourth recovery branch would need to update three sites; collapsing into a helper subsumes the gate and centralizes the contract comment.

**Proposal:**
- Extract `function triggerRecoveryForDelta(delta: DeltaEvent)` that performs the resync and conditional hydration.
- Replace the three call sites with the helper. Centralize the contract comment.

## Two backend Lagged branches duplicate the lagged-marker emission

**Severity:** Low - `src/api_sse.rs:182-200, 204-215`. The state-receiver and delta-receiver Lagged branches now both yield `lagged` followed by a recovery state snapshot built via `state_snapshot_payload_for_sse(state.clone()).await`. The branches are byte-identical apart from comments. The third Lagged branch (`file_receiver` at line 221) deliberately doesn't recover — so a 2-of-3 helper is still warranted for the asymmetric maintenance risk: a future change that grows one branch (e.g., a tracing log, structured `data` body, or `revision` hint on the marker) needs to be mirrored manually on the other.

**Proposal:**
- Extract a helper that yields the marker + recovery snapshot. The `async_stream::stream!` macro doesn't compose cleanly with helpers that themselves yield, so consider a named local closure or document the invariant explicitly.
- Or, accept the duplication and add cross-referencing comments naming both branches.

## Per-session hydration burst has no cooldown beyond in-flight deduplication

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. The new `startSessionHydration(delta.sessionId)` calls trigger `GET /api/sessions/{id}` (full transcript fetch) on every problematic delta. `hydratingSessionIdsRef` deduplicates concurrent fetches per session, but it does not rate-limit successive fetches: once a hydration completes, the next problematic delta on the same session immediately schedules another full transcript fetch. On a flaky network with bursty deltas, a hydration→delta→hydration loop is possible, each iteration shipping the entire transcript over the wire.

**Current behavior:**
- In-flight dedup via `hydratingSessionIdsRef` collapses simultaneous calls to one round-trip.
- After completion, the next problematic delta immediately schedules another fetch with no cooldown.
- Phase-1 local-only deployment makes this practically free; future remote-host or flaky-network use exposes the storm risk.

**Proposal:**
- Add a per-session cooldown timestamp ("don't re-hydrate the same session within Nms of the last completed hydration unless the new delta carries a revision strictly greater than the one that started the previous hydration").
- Or document the burst as intentional given the local-only deployment cost; add a comment naming the trade-off so future reviewers don't keep flagging it.

## Watchdog-inversion tests don't assert the "Waiting for the next chunk of output…" affordance state

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx:3439` and `ui/src/App.live-state.watchdog.test.tsx:625`. The two recent inverted tests assert that the recovered text becomes visible, but say nothing about the "Waiting for the next chunk of output…" affordance. After the recovery snapshot adopts (the deltas test's snapshot has `status: "idle"`, the watchdog test's stays `status: "active"`), the affordance state is the most user-visible signal of whether recovery actually replaced the wedged UI vs just rendered the recovered text somewhere on the page.

**Proposal:**
- In `deltas.test.tsx`: add `expect(screen.queryByText("Waiting for the next chunk of output...")).not.toBeInTheDocument();` after the assertion that the recovered chunk is visible (recovery snapshot is idle, affordance should disappear).
- In `watchdog.test.tsx`: add an assertion clarifying expected affordance state for the still-active recovery (the assistant chunk now sits at the boundary, so the affordance should NOT be present).

## Rendered Markdown diff navigation does not scroll when there is exactly one change

**Severity:** Low - prev/next buttons can appear to do nothing for the common "one changed block" case.

`MarkdownDiffView` and `RenderedDiffView` intentionally skip the initial scroll so restored parent scroll position is preserved. Navigation scrolls from a `useEffect` keyed on the current index. When there is exactly one change/region, pressing next or previous computes the same index, React bails out of the state update, and the scroll effect does not run. The controls remain enabled but cannot bring the lone target into view.

**Current behavior:**
- Initial mount skips scrolling by design.
- One-change/one-region navigation resolves to the same index.
- No separate "navigation requested" signal exists to force a scroll to the same target.

**Proposal:**
- Drive the scroll side effect from a navigation request counter, or call an explicit scroll helper from the prev/next handlers.
- Cover both `MarkdownDiffView` and `RenderedDiffView` one-target cases.

## Rendered diff region navigation has no explicit scroll-container layout contract

**Severity:** Low - the new region-navigation ref may target a wrapper that is not the actual scroll container.

`RenderedDiffView` introduces `.diff-rendered-view-scroll` and queries it for `data-rendered-diff-region-index` targets, but the changed CSS does not give that wrapper an explicit flex/overflow contract, and the component does not adopt the existing `source-editor-shell source-editor-shell-with-statusbar` layout used by the Monaco and Markdown diff modes. If the parent remains the real scroller, `scrollIntoView()` may work inconsistently and the statusbar can diverge from the rest of the diff editor surface.

**Current behavior:**
- `RenderedDiffView` owns a new internal scroll ref.
- `.diff-rendered-view-scroll` has no explicit overflow/flex sizing.
- The rendered diff footer is not wrapped in the established editor shell/statusbar structure.

**Proposal:**
- Either adopt the existing editor-shell/statusbar layout contract or add explicit CSS that makes `.diff-rendered-view-scroll` the intended scroll container.
- Add a focused layout/navigation regression for rendered-region scrolling.

## Post-commit hardening helpers have no automated production-path coverage

**Severity:** Low - `src/persist.rs:213-227`. `verify_persist_commit_integrity` is `#[cfg(not(test))]`-only because it depends on production SQLite path hardening. The post-commit contract - redirection remains fatal, owner-only chmod/mode verification remains fatal unless `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS` is set - has no direct automated coverage.

**Proposal:**
- Expose a testable seam (e.g., inject the hardening function via a closure or trait), OR
- Add a Linux-only integration test that creates a real chmod-failing scenario.

## Watchdog wake-gap stops-after-progress invariant is not pinned

**Severity:** Low - `ui/src/backend-connection.test.tsx`. No direct negative-case test pins that watchdog wake-gap reconnect probes (which do NOT set `pendingBadLiveEventRecovery`) STOP after same-instance snapshot progress without a data-bearing SSE event. The cause-specific flag's whole premise is that wake-gap probes can stop while parse/reducer-error probes keep polling, but only the polling-continues side is pinned.

**Proposal:**
- Add a regression that triggers a watchdog wake-gap reconnect (no parse/reducer error), receives a same-instance progressed `/api/state` snapshot, advances `RECONNECT_STATE_RESYNC_MAX_DELAY_MS`, and asserts `countStateFetches()` did not increment.

## `app-live-state.ts` reconnect state machine continues to grow

**Severity:** Low - `ui/src/app-live-state.ts:2504 lines`. TS utility threshold (1500) exceeded; new `pendingBadLiveEventRecovery` adds another flag-shaped piece of reconnect bookkeeping. The reconnect/resync state machine inside `useEffect` now coordinates 6+ pieces of cross-cutting state.

**Proposal:**
- Extract a `ReconnectStateMachine` (or similar) module that owns the flag set + transitions and exposes named events (`onSseError`, `onSseReopen`, `onBadLiveEvent`, `onSnapshotAdopted`, `onLiveEventConfirmed`).
- Defer to a pure code-move commit per CLAUDE.md.

## `select_visible_session_hydration_fallback_error` lacks integration coverage

**Severity:** Low - `src/state_accessors.rs:351-369`. Unit tests pin the helper and typed local-miss fallback in isolation, but no integration-style test asserts the public `get_session` path returns 404 to the caller when a recoverable remote error is followed by a `not_found` fallback. A future refactor that drops the selector call from the `or_else` chain would not be caught.

**Current behavior:**
- Selector is unit-tested but the wiring that makes the new behavior reach a caller is not pinned.

**Proposal:**
- Add an integration-style test that drives the public `AppState::get_session` path through a recoverable remote hydration miss followed by a vanished cached summary, and asserts the response is `404 session not found` / `LocalSessionMissing` rather than the original recoverable remote error.

## `useAppSessionActions` ref cluster has grown from 1 to 4 to feed the rejected-action classifier

**Severity:** Medium - `ui/src/app-session-actions.ts:316-356`. `useAppSessionActions` now requires `latestStateRevisionRef`, `lastSeenServerInstanceIdRef`, `projectsRef`, and `sessionsRef` because of the inline `classifyRejectedActionState` call site. The ref count grew from 1 → 4 in a few rounds, all to feed one classifier function.

**Current behavior:**
- Every new evidence dimension for stale action snapshots pushes another ref into this hook.
- App.tsx, the test harness, and the hook signature all need editing whenever a new dimension is added.
- Same anti-pattern the resync-options ref cluster had before extraction.

**Proposal:**
- Pass a single `actionStateClassifierContextRef: MutableRefObject<{ revision, serverInstanceId, projects, sessions }>` (or a memoized snapshot getter) so adding a new evidence dimension does not require touching the hook signature, the caller, and the test harness.
- Defer to a dedicated commit per CLAUDE.md.

## `connectionRetryDisplayStateByMessageId` two-stage memoization is correct but threaded through ~4 stability hops

**Severity:** Medium - `ui/src/SessionPaneView.tsx:858-895`. The retry-display memoization now uses `signature → ref-cached map → useCallback wrapper → useSessionRenderCallbacks deps → MessageCard renderer identity`. The map identity stability invariant is load-bearing for `SessionBody` memoization but only documented sparsely. A future change to retry-display semantics needs to be threaded through ~4 separate stability hops.

**Current behavior:**
- Hand-rolled signature-stable memo bridges to a renderer that already had its own deps tax.
- Reviewers have flagged this as "complex invariant without nearby comments" several rounds in a row.

**Proposal:**
- Extract the signature-stable memo into a small `useStableMapBySignature` hook in a sibling utility module so the pattern is reusable and named.
- Or memoize directly on `(messages, status)` and accept one rebuild per message-list change — `MessageCard` is already memoized below the `SessionBody` memo gate.

## Directory-level state hardening retains a TOCTOU window after symlink check

**Severity:** Low - `src/persist.rs:146-149`. Round-15 carryover. `harden_local_state_directory_permissions` calls `reject_existing_state_directory_redirection_unix` (which uses `fs::symlink_metadata`), then `harden_local_state_permissions(path, 0o700)` — which uses path-based `fs::set_permissions` and `fs::metadata`, both of which follow symlinks. An attacker able to replace the directory between the two calls would get the chmod redirected through the symlink. The matching file path now uses `O_NOFOLLOW + fchmod`, but the directory path has not been migrated.

**Current behavior:**
- File-level chmod is symlink-safe (O_NOFOLLOW + fchmod).
- Directory-level chmod is not.
- Mitigated by Phase-1 single-user threat model (only the user controlling `~/` could plant the symlink).

**Proposal:**
- Open the directory with `O_DIRECTORY | O_NOFOLLOW`, then `fchmod` on the resulting fd; or use `fchmodat(AT_FDCWD, path, mode, AT_SYMLINK_NOFOLLOW)`.

## Non-optimistic user-prompt display causes 100-300ms felt lag on every Send

**Severity:** Medium - `ui/src/app-session-actions.ts:851-895` and `ui/src/app-live-state.ts:1283-1385`. The composer is non-optimistic: clicking Send clears the textarea, fires `await sendMessage(...)`, and then runs `adoptState(state)` against the full `StateResponse` returned by the POST. The "you said X" card only appears after the round-trip plus the heavy `adoptState` walk completes.

`adoptState` re-derives codex, agentReadiness, projects, orchestrators, workspaces, and walks transcripts on the main thread. On a focused active session this lands in the 100-300ms range every send (longer when an active turn is mid-stream). The codebase has already self-diagnosed the path in `docs/prompt-responsiveness-refactor-plan.md` but no optimistic-insert fix has landed.

The lag compounds with two existing tracked bugs ("Focused live sessions monopolize the main thread during state adoption", "Composer drafts have three authoritative stores") but is itself a separable contributor.

**Current behavior:**
- User clicks Send -> textarea clears -> POST fires -> response returns -> `adoptState` walks -> card paints.
- Total delay: round-trip (typically 30-100ms locally) + adoptState (50-200ms on focused live sessions) = visible 100-300ms gap.
- During the gap the session shows neither the user prompt nor the composer text.

**Proposal:**
- Insert an optimistic user-message card in `handleSend` before `await sendMessage(...)`, keyed by a temp id.
- When the POST response arrives or the SSE `messageCreated` delta lands (whichever is first), reconcile by id (swap temp id for server-assigned `messageId`).
- This collapses the round-trip and the adoptState walk out of the felt-lag path simultaneously.
- Cross-link to `docs/prompt-responsiveness-refactor-plan.md` and decide whether this is a standalone fix or folds into the larger refactor.

## `applyDeltaToSessions` duplicates the "lookup first, metadata-only fallback when missing" pattern across five non-created delta types

**Severity:** Low - `ui/src/live-updates.ts:329-599`. The reordered `messagesLoaded === false` branch (apply to in-memory message when present, fall back to metadata-only only when `messageIndex === -1`) is now repeated five times across `messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, and `parallelAgentsUpdate`. The previous code had the same shape duplicated five times in the wrong order; the new code is now the right order duplicated five times. A future sixth retained-non-created delta type will need to re-derive the same flow.

**Current behavior:**
- Each branch independently re-implements `findMessageIndex` -> `if (-1 && !messagesLoaded) metadata-only` -> `if (-1) needsResync` -> type-narrow -> apply.
- The existing duplication is what let the fallback land in the wrong order originally; the next protocol addition has the same cliff.

**Proposal:**
- Extract a `tryApplyMetadataOnlyFallbackForMissingTarget(session, sessionIndex, sessions, delta)` helper (or similar) that centralizes the missing-target/unhydrated decision so each delta type calls a single helper instead of inlining the same branch.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

## Production SQLite persistence is bypassed in the test build

**Severity:** Medium - `src/app_boot.rs:229`. The runtime persistence changes now depend on SQLite schema setup, startup load, metadata writes, per-session row updates, tombstone cleanup, and cached delta persistence, but `#[cfg(test)]` still routes the background persist worker through the old full-state JSON fallback.

Many production SQLite helpers in `src/persist.rs` are `#[cfg(not(test))]`, so existing persistence tests can pass while the real runtime SQLite write/load/delete behavior remains unexercised. The newest post-commit hardening policy (`verify_persist_commit_integrity`, fatal owner-only permission verification, cache invalidation reset, and fatal pre-transaction redirection checks) is part of that production-only surface.

**Current behavior:**
- Test builds bypass `persist_delta_via_cache` and related SQLite write paths.
- Production SQLite load/save helpers are mostly compiled out under `cargo test`.
- Current tests cover retry bookkeeping and legacy JSON fixtures, but not the runtime SQLite persistence contract or the post-commit hardening decisions.

**Proposal:**
- Make the SQLite persistence path testable under `cargo test` with temp database files.
- Add coverage for full snapshot save/load, delta upsert, metadata-only update, hidden/deleted session row removal, and startup load from SQLite.
- Add coverage for post-commit permission failures, cache invalidation reset, and fatal redirection/reparse checks.
- Keep legacy JSON fixture tests separate from production runtime persistence tests.

## `SessionPaneView.tsx` and `app-session-actions.ts` past architecture file-size thresholds

**Severity:** Low - `ui/src/SessionPaneView.tsx` is now 3,160 lines and `ui/src/app-session-actions.ts` is 1,968 lines, both past the architecture rubric Â§9 thresholds (~2,000 for TSX components, ~1,500 for utility modules). The round-11 extractions of `connection-retry.ts`, `app-live-state-resync-options.ts`, `session-hydration-adoption.ts`, and `SessionPaneView.render-callbacks.tsx`, plus the later `action-state-adoption.ts` split, reduced these files but left them over their respective thresholds.

The companion `app-live-state.ts` entry already exists; this captures the two related Phase-2 candidates that emerged after the round-11 splits.

**Current behavior:**
- `SessionPaneView.tsx` mixes pane orchestration with reconnect-card / waiting-indicator / retry-display orchestration.
- `app-session-actions.ts` still mixes action handlers with optimistic-update and adoption-outcome side-effect wiring.
- Both files now have natural extraction boundaries with their own existing direct unit-test coverage.

**Proposal:**
- Pure code move per CLAUDE.md, in dedicated split commits (one per file).
- For `SessionPaneView.tsx`: candidate is the reconnect-card / waiting-indicator computation cluster.
- For `app-session-actions.ts`: candidate is the optimistic-update + adoption-outcome side-effect cluster now that pure stale target evidence has moved out.

## `App.live-state.deltas.test.tsx` past 2,000-line review threshold

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx`. File is now 3,435 lines and 18 `it` blocks after this round's cross-instance regression coverage, well past the architecture rubric Â§9 ~2,000-line threshold for TSX files. The header already lists three sibling files split out (`reconnect`, `visibility`, `watchdog`), establishing the per-cluster split pattern.

The newest tests still cluster around hydration/restart races and cross-instance recovery, which is a coherent split boundary. Pure code move per CLAUDE.md.

**Current behavior:**
- Single test file mixes hydration races, watchdog resync, ignored deltas, orchestrator-only deltas, scroll/render coalescing, and resync-after-mismatch flows.
- 18 `it` blocks; the newest coverage adds another cross-instance state-adoption scenario.
- Per-cluster grep tax growing.

**Proposal:**
- Pure code move: extract the 4–5 hydration-focused tests into `ui/src/App.live-state.hydration.test.tsx`, mirroring the sibling-split pattern.
- Defer to a dedicated split commit; do not couple with feature changes.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is now 2,435 lines after this round. The architecture rubric Â§9 sets a pragmatic ~1,500-line threshold for TypeScript utility modules. The hydration adoption helpers have moved out, but the module still mixes retry scheduling, profiling, JSON peek helpers, and the main state machine.

**Current behavior:**
- Single module mixes hydration matching, retry scheduling, profiling, JSON peek helpers, and the main state machine.
- Per-cluster grep tax growing with each round.

**Proposal:**
- Defer to a dedicated pure-code-move commit per CLAUDE.md.
- Extract `hydration-retention.ts` (or `session-hydration.ts`) containing `hydrationRetainedMessagesMatch`, `SESSION_HYDRATION_RETRY_DELAYS_MS`, `SessionHydrationTarget`, `SessionHydrationRequestContext`, and the matching unit tests.

## `AgentSessionPanel.test.tsx` past 5,000-line review threshold

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx`. File is now 5,659 lines (+511 this round), past the project's review threshold for test files. The added blocks cluster naturally by concern — composer memo coverage, scroll-following coverage, ResizeObserver fixtures — and would extract cleanly into siblings without behavioral change.

The adjacent `App.live-state.*.test.tsx` split (April 20) is the precedent for per-cluster `.test.tsx` files. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `AgentSessionPanel.test.tsx` mixes composer, scroll, resize, and lifecycle clusters.
- Per-cluster grep tax growing with each replay-cache-adjacent feature round.

**Proposal:**
- Pure code move: extract into `AgentSessionPanel.composer.test.tsx`, `AgentSessionPanel.scroll.test.tsx`, `AgentSessionPanel.resize.test.tsx` (matching the App.live-state cluster shape).
- Defer to a dedicated split commit; do not couple with feature changes.

## `src/tests/remote.rs` past the 5,000-line review threshold

**Severity:** Low - `src/tests/remote.rs` is now 9,202 lines after this round's +471-line addition, well past the project's review-threshold for test files. The new replay-cache work clusters cohesively between lines ~2,810 and ~4,040 (the `RemoteDeltaReplayCache` shape helper, the `local_replay_test_remote` / `seed_loaded_remote_proxy_session` / `assert_delta_publishes_once_then_replay_skips` / `assert_remote_delta_replay_cache_shape` / `test_remote_delta_replay_key` helpers, and the `remote_delta_replay_*` tests).

The growth is incremental across many rounds of replay-cache hardening, not a single landing — but extracting the cluster keeps the rest of the file's per-test density manageable. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `src/tests/remote.rs` mixes hydration tests, orchestrator-sync tests, replay-cache tests, and protocol-shape tests.
- Per-cluster grep is harder than necessary; future replay-cache work continues to grow the file.

**Proposal:**
- Extract the replay-cache cluster (lines ~2,810–4,040) into `src/tests/remote_delta_replay.rs` as a pure code move — including the helpers and all `remote_delta_replay_*` tests.
- Defer to a dedicated split commit; do not couple with feature changes.

## `SourcePanel.tsx` is growing along a separable axis

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` grew from ~803 to 1119 lines in this round (+316). It is approaching but has not crossed the ~2,000-line scrutiny threshold. The new responsibility (rendered-Markdown commit pipeline orchestration: collect — resolve ranges — check overlap — reduce edits — re-emit with EOL style) is meaningfully separable from the existing source-buffer/save/rebase/compare orchestration. It has its own state (`hasRenderedMarkdownDraftActive`, `renderedMarkdownCommittersRef`), pure helpers already split into `markdown-commit-ranges`/`markdown-diff-segments`, and a clean parent-callback interface.

**Current behavior:**
- SourcePanel owns two distinct orchestration responsibilities in one component.

**Proposal:**
- No action this commit. Consider extracting a `useRenderedMarkdownDrafts(fileStateRef, editorValueRef, setEditorValueState, ...)` hook in a follow-up, owning `renderedMarkdownCommittersRef`, `hasRenderedMarkdownDraftActive`, `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleRenderedMarkdownSectionDraftChange`.
- The hook would expose a small surface for SourcePanel to consume and keep the file under the scrutiny threshold.

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

## `CodexUpdated` delta carries a full subsystem snapshot despite the "delta" name

**Severity:** Medium - `src/wire.rs::DeltaEvent::CodexUpdated { revision, codex: CodexState }` publishes the entire `CodexState` on every rate-limit tick and every notice addition. The architectural contract the codebase otherwise respects is "state events for full snapshots, delta events for scoped changes". `CodexUpdated` is small today (rate_limits + notices capped at 5), but the naming invites future bulky additions to `CodexState` (login state, model-availability maps, per-provider metadata) to be broadcast in full on every tiny change.

**Current behavior:**
- The variant ships a full `CodexState` payload.
- Two publish sites in `src/session_sync.rs` send the complete snapshot even when only the rate limits changed.
- Wire name and shape set a precedent for "delta = tiny changes" that this variant violates.

**Proposal:**
- Split into narrower variants: `CodexRateLimitsUpdated { revision, rate_limits }` and `CodexNoticesUpdated { revision, notices }`. The two call sites in `session_sync.rs` already pick their publish trigger, so split dispatch is straightforward.
- Alternatively, add a source-level comment on the `CodexUpdated` variant stating that `codex` is intentionally the full subsystem snapshot and any future field addition to `CodexState` must reconsider whether a narrower event is needed.

## `DeferredHeavyContent` near-viewport activation now deferred by one paint

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder — content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ~1 paint even when the card is already near viewport on mount.
- The deferral was added as part of the `allowDeferredActivation` cooldown gate (to avoid layout thrash during active scrolls).
- Near-viewport mount activation now produces a one-frame placeholder flicker in place of the previous zero-frame activation.

**Proposal:**
- Use `useLayoutEffect` when `allowDeferredActivation === true` (or for the near-viewport branch generally). Keep the `requestAnimationFrame` in the IntersectionObserver entry path for rapid-entry de-dupe.
- Alternatively, add a targeted comment explaining the deliberate trade-off if the new behavior is intended.

## `"sessionId" in delta` poll-cancel branches are not extensible

**Severity:** Low - `ui/src/app-live-state.ts:1613, 1633` handle delta-event poll cancellations by structurally checking `"sessionId" in delta`. The two `revisionAction === "ignore"` / `"resync"` branches each hard-code the knowledge that only `SessionDeltaEvent` variants carry `sessionId`. Adding a third non-session delta type requires remembering to update both branches, and a new session-scoped delta that uses a different key (e.g. `sessionIds: string[]`) would silently miss both gates.

**Current behavior:**
- Two branches each run `"sessionId" in delta && typeof delta.sessionId === "string"`.
- The `SessionDeltaEvent` exclude type in `ui/src/live-updates.ts:76` exists but is not used here.

**Proposal:**
- Extract a `cancelPollsForDelta(delta: DeltaEvent)` helper that switches on `delta.type` (or uses the same `SessionDeltaEvent` narrowing). Call it from both branches.
- That also centralizes the "which deltas cancel which polls" contract in one place.

## `prevIsActive`-in-render replaced with post-commit effect delays the first-activation measurement pass

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:426-432` converted the `prevIsActive !== isActive` render-time derived-state update into a post-commit `useEffect`. Under the previous pattern, a session switching from `isActive: false — true` flipped `setIsMeasuringPostActivation(true)` during render, so the first frame rendered the measuring shell with the correct `preferImmediateHeavyRender` value. The new effect defers that flip to after commit — the first paint of the newly-active session briefly shows `isMeasuringPostActivation: false`, flipping to the measurement shell only on the next render.

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

## Composer sizing double-resets on session switch

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:918-931` runs `resizeComposerInput(true)` synchronously inside a `useLayoutEffect` keyed on `[activeSessionId]`, and a following `useEffect` keyed on `[composerDraft]` schedules another resize via `requestAnimationFrame` on the same first render. The rAF resize is redundant because the synchronous one already measured the new metrics.

**Current behavior:**
- Layout effect resets cached sizing state and calls `resizeComposerInput(true)` synchronously.
- Draft effect schedules a second `requestAnimationFrame` resize on the same first render.
- First render of any newly-activated session does two resize passes instead of one.

**Proposal:**
- Track a "just-resized-synchronously" flag set in the layout effect and checked at the top of `scheduleComposerResize`, or gate the draft effect with a prev-draft ref so the "initial draft equals committed" case is a no-op.

## Duplicated `Session` projection types in `session-store.ts` and `session-slash-palette.ts`

**Severity:** Low - `ComposerSessionSnapshot` (`ui/src/session-store.ts:36-83`) and `SlashPaletteSession` (`ui/src/panels/session-slash-palette.ts:51-65`) each re-pick overlapping-but-non-identical field sets from `Session`. Three `Session`-like shapes now exist (`Session`, `ComposerSessionSnapshot`, `SlashPaletteSession`) with no compile-time check that additions to `Session` reach both projections — a new agent setting added to `Session` could silently default to `undefined` in consumers that read through either projection.

**Current behavior:**
- Both projection types declare field lists by hand.
- No `Pick<Session, ...>` derivation; nothing fails to compile when `Session` grows a new field.

**Proposal:**
- Derive both types via `Pick<Session, ...>`, or express `SlashPaletteSession` as `Omit<ComposerSessionSnapshot, ...>` where their field sets differ.
- Colocate the derivations in `session-store.ts` so the projection contract is visible in one place.

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

## Conversation cards overlap for one frame during scroll through long messages

**Severity:** Medium - `estimateConversationMessageHeight` in `ui/src/panels/conversation-virtualization.ts` produces an initial height for unmeasured cards using a per-line pixel heuristic with line-count caps (`Math.min(outputLineCount, 14)` for `command`, `Math.min(diffLineCount, 20)` for `diff`) and overall ceilings of 1400/1500/1600/1800/900 px. For heavy messages — review-tool output, build logs, large patches — the estimate is 20–40% under the rendered height, so `layout.tops[index]` for cards below an under-priced neighbour places them inside the neighbour's rendered area. The user sees the cards painted on top of each other for one frame, until the `ResizeObserver` measurement lands and `setLayoutVersion` rebuilds the layout.

An initial attempt to fix this by raising estimates to a single 40k px cap (and adding `visibility: hidden` per-card until measured) was reverted after it introduced two worse regressions: (1) per-card `visibility: hidden` combined with the wrapper's `is-measuring-post-activation` hide left the whole transcript empty for a frame whenever the virtualization window shifted before measurements landed; (2) raising the cap made the `getAdjustedVirtualizedScrollTopForHeightChange` shrink-adjustment huge (40k estimate − 8k actual = −32k scrollTop jump), so slow wheel-scrolling through heavy transcripts caused visible scroll jumps of tens of thousands of pixels. The revert restores the one-frame overlap as the known limitation.

**Current behavior:**
- Initial layout uses estimates that badly under-price long commands / diffs.
- First paint places subsequent cards overlapping the under-priced one for one frame.
- Next frame, `ResizeObserver` fires, `setLayoutVersion` rebuilds, positions correct.
- Visible to the user as a brief "jumble" during scroll.

**Proposal:**
- Proper fix likely needs off-screen pre-measurement (render the card in a hidden measure-only tree, read `getBoundingClientRect` height, then place in the layout) rather than a formula-based estimate. This is a bigger change than a single pure-function tweak.
- Alternative: batch-measurement pass when the virtualization window shifts — hide the wrapper briefly, mount the newly-entering cards, wait for all their measurements, then reveal.
- Not: raise the estimator cap. Large overshoots trade one visible artifact for a worse one.

## Hard kill (SIGKILL, power loss) can still lose the last un-drained persist write

**Severity:** Low - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window — the persist channel carried a full-state clone — so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

The message is not hidden; it is genuinely gone from SQLite. No amount of frontend re-rendering will bring it back.

**Current behavior:**
- Active-turn deltas (e.g., streaming assistant text, `MessageCreated` at the end of a turn) commit through `commit_persisted_delta_locked`, which only signals the persist thread.
- The persist thread acquires `inner` briefly, collects the delta, and writes to SQLite.
- Between "signal sent" and "row written" there is a small time window (usually sub-millisecond, but can stretch under contention) during which a hard kill of the backend loses the mutation.
- On backend restart + SSE reconnect, the browser's `allowRevisionDowngrade: true` adoption path applies the persisted state. The persisted state is missing the un-drained mutation, so the in-memory latest message is overwritten and disappears.

**Proposal:**
- The user-initiated restart path (Ctrl+C / SIGTERM) is now covered by the graceful-shutdown drain — see the preamble.
- For the residual hard-kill case (SIGKILL, power loss): consider opt-in synchronous persistence for the last message of a turn — the turn-completion commit (`finish_turn_ok_if_runtime_matches`'s `commit_locked`) could flush synchronously before returning, trading a few ms of latency on turn completion for zero-loss durability of the final message.
- Or accept and document this as a known Phase-1 limitation in `docs/architecture.md` (background-persist durability contract: at most one un-drained mutation may be lost on hard kill).

## SSE state broadcaster can reorder state events against deltas

**Severity:** Medium - under a burst of mutations, a delta event can arrive at the client before the state event for the same revision, triggering avoidable `/api/state` resync fetches.

Before the broadcaster thread, `commit_locked` published state synchronously (`state_events.send(payload)` under the state mutex), so state N always hit the SSE stream before any follow-up delta N+1. Now `publish_snapshot` enqueues the owned `StateResponse` to an mpsc channel and returns; the broadcaster thread drains and serializes on its own schedule. `publish_delta` remains synchronous. A caller that does `commit_locked(...)?` + `publish_delta(...)` can therefore race: the delta hits `delta_events` before the broadcaster drains state N. The frontend's `decideDeltaRevisionAction` requires `delta.revision === current + 1`; if state N hasn't advanced `latestStateRevisionRef` yet, the delta is treated as a gap and the client fires a full `fetchState`.

**Current behavior:**
- `publish_snapshot` is async (channel + broadcaster thread).
- `publish_delta` is sync.
- Client can observe delta N+1 before state N.
- Extra `/api/state` resync fetches fire under sustained mutation bursts.
- Correctness preserved (resync fixes the view), but behavior is chatty and pushes load onto `/api/state` — which is exactly the path we just made cheaper.

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

- Replace the unbounded queue with a single-slot latest mailbox or bounded channel.
- Drop or overwrite superseded snapshots before they can accumulate in memory.
- Add a burst test that publishes multiple large snapshots while the broadcaster is delayed and asserts only the latest snapshot is retained.

## Implementation Tasks

- [ ] P2: Cover frontmatter parser edge cases directly:
  add a `strip_markdown_frontmatter` unit-test module covering opening `---` without a closing terminator (returns original content), `description: |` multi-line value, malformed `key: value: extra` lines, very large frontmatter, and trailing whitespace on the closing `---`.
- [ ] P2: Cover `rejects_invalid_agent_command_delegation_metadata` parametrically:
  parameterize over each error branch in the metadata parser: `mode: invalid_value`, `writePolicy.kind: bogus`, `enabled: not-a-bool`, `enabled: true` without `mode`, `enabled: true` without `writePolicy.kind`, `prefix:` without `strategy: prefixFirstArgument`, `strategy: bogus`.
- [ ] P2: Cover adversarial `PromptTemplate` source-vs-name mismatch for delegation defaults:
  add `prompt_template_delegate_resolution_does_not_use_metadata_when_source_path_mismatches_name` — a PromptTemplate whose source ends with `/.claude/commands/review-local.md` but whose name is `audit` must not inherit metadata defaults.
- [ ] P2: Cover frontmatter freshness contract:
  once resolver metadata is cached from the command-listing pass, add a Rust test
  that lists/resolves a command, edits the on-disk frontmatter, resolves again,
  and asserts the second resolve follows the documented snapshot/cache contract
  instead of silently mixing old prompt content with freshly re-read metadata.
- [ ] P2: Cover delegation tests through `update_app_settings` normalization:
  add at least one delegation test that goes through `state.update_app_settings(...)` with a non-canonical model string instead of mutating `inner.preferences.default_codex_model` directly, then creates a delegation and asserts the child uses the canonicalized form.
- [ ] P2: Cover frontend default-model forwarding for all model-picker agents:
  add parameterized tests for Claude, Codex, Cursor, and Gemini covering custom default forwarding, the `default` sentinel omission, and at least one settings-panel Apply/Reset interaction.
- [ ] P2: Cover Claude default-model validation:
  assert app settings reject leading-hyphen and control-character Claude model values, or assert the eventual Claude CLI args use a safe `--model=<value>` form.
- [ ] P2: Cover CRLF command frontmatter parsing:
  add a command fixture using `---\r\n` frontmatter and assert description extraction, body stripping, and argument-hint preservation still work on Windows-style files.
- [ ] P2: Cover Telegram relay update-before-digest ordering:
  extract a testable relay loop iteration or fake Telegram/TermAl clients, then
  assert pending updates are processed before digest sync and that one loop
  iteration emits at most one digest/assistant-forwarding pass.
- [ ] P2: Cover pending-prompts visibility without a live turn:
  render `SessionConversationPage` with `showWaitingIndicator: false` and a non-empty `pendingPrompts` array, then assert `.conversation-live-tail` is absent and the pending-prompt cards live inside `.conversation-pending-prompts`. Prevents a regression that re-introduces the old `liveTurnCard || pendingPromptCards.length > 0` condition.
- [ ] P2: Cover `DelegationWaitResponse` JSON serialization:
  round-trip a `DelegationWaitResponse` through `serde_json::to_value` and assert the camelCase keys `resumePromptQueued` and `resumeDispatchRequested` are present for one busy-parent and one idle-parent scenario, and round-trip a `DeltaEvent::DelegationWaitConsumed` to assert `reason` is always emitted with the expected value.
- [ ] P2: Replace the duplicative malformed-wait persistence test:
  rewrite `removing_parent_consumes_unsatisfied_wait_even_when_targets_are_not_terminal` so it writes a stale wait whose owner is missing into the persistence file, restarts via `AppState::new_with_paths`, and asserts boot reconciliation drops it. As written, it bypasses validation via `inner.delegation_waits.push` and duplicates a scenario already covered by `removing_delegation_parent_consumes_pending_wait_with_parent_removed_reason`.
- [ ] P2: Cover true-value delegation wait response booleans through the command layer:
  add `resumePromptQueued: true` and `resumeDispatchRequested: true` cases for `resume_after_delegations` and reviewer-batch wait propagation so fields cannot be hard-coded false.
- [ ] P2: Decouple `delegation_wait_reconciles_after_restart_recovery` from recovery copy:
  avoid asserting the exact `"TermAl restarted before this turn finished"` string from `src/messages.rs`; assert stable wait/delegation identifiers or finish the child with a deterministic result packet before simulated shutdown.
- [ ] P2: Cover edge cases in delegation finding parsing:
  add focused tests for `- None` filtering, no-separator findings mapping to the current fallback behavior, and multi-word severities such as `Code Style src/foo.rs:42 - msg`.
- [ ] P2: Switch resolver Rust temp-dir cleanup away from `unwrap()`:
  replace end-of-test `fs::remove_dir_all(root).unwrap()` cleanup in `src/tests/agent_commands.rs` with `let _ = fs::remove_dir_all(...)` so assertion failures do not leak temp dirs or mask the original failure.
- [ ] P2: Cover Telegram dirty-state cleanup on informational early returns:
  seed stale selected project/session state, drive the no-active-session free-text path and `/session` with no arguments, and assert cleared selection state is persisted instead of returning `Ok(false)`.
- [ ] P2: Cover `SessionPaneView` isolated-worktree delegation option pass-through:
  trigger a delegated `/review-local` command through the component boundary and assert `spawnDelegationCommand` receives `writePolicy: { kind: "isolatedWorktree", ownedPaths: [] }`.
- [ ] P2: Cover omitted `isolatedWorktree.worktreePath` JSON:
  add a serde or route-level test using `{ "kind": "isolatedWorktree", "ownedPaths": [] }` and assert the backend accepts the omitted path and generates a TermAl-owned worktree path.
- [ ] P2: Strengthen Telegram sessions chunking assertions:
  replace negative assertions against text that never appears in the fixture with reconstructed-chunk assertions that every expected session id/name is preserved.
- [ ] P2: Cover keyboard delegation for selected slash commands:
  add RTL coverage proving keyboard users can delegate an active agent slash command while the palette is open, either through normal tab focus or an explicit delegation shortcut.
- [ ] P2: Cover visible composer errors for resolver failures:
  reject a resolver request for native-slash notes or backend-unavailable responses and assert the composer surfaces a user-visible sanitized error without clearing the draft.
- [ ] P2: Cover real composer-to-overview focus detection:
  render the real composer/overview path or assert the real composer emits `data-conversation-composer-input`, so `ConversationOverviewRail` deferral does not depend only on synthetic test fixtures.
- [ ] P2: Cover active-baseline same-message growth after Telegram prompt settlement:
  arm a Telegram prompt behind an active turn, settle with the same assistant message id grown in place, and assert the reply forwards or the unsupported behavior is explicitly pinned.
- [ ] P2: Cover first-chunk Telegram forward failure:
  force the first chunk of a long assistant message to fail and assert bounded retry/escalation behavior instead of an endless replay loop.
- [ ] P2: Cover armed-delivery failure suppressing digest-primary fallback:
  make an armed session fail before sending visible content and assert an unrelated digest primary is not forwarded in the same poll.
- [ ] P2: Cover emitted OrchestratorsUpdated localized remote ownership:
  drive remote delta application end-to-end and assert the emitted localized sessions clear inbound `remote_id` before replay-key normalization/fingerprinting.
- [ ] P2: Cover pinned live-tail queued prompt order:
  render a pinned live turn with at least two queued prompts and assert the live card is closest to the composer without reversing queued prompt FIFO order.
- [ ] P2: Cover fit-to-frame Mermaid preview behavior with wide diagrams:
  add a `MarkdownContent` test using `fillMermaidAvailableSpace` with a wide Mermaid `viewBox` inside a constrained parent, and assert the fit-mode iframe `srcdoc` plus frame sizing contract.
- [ ] P2: Cover editable SourcePanel Mermaid fit-mode iframe wiring:
  drive the `preserveMermaidSource` branch through SourcePanel or `MarkdownContent` and assert the Mermaid iframe `srcdoc` contains fit-mode SVG CSS, not just wrapper classes.
- [ ] P2: Add Telegram settings API/security regressions:
  cover plaintext token-at-rest exposure, corrupt-backup permission hardening, Windows ACL/secret-store fallback behavior, global/concurrent rate limiting that cannot be bypassed by rotating token strings, and bounded rate-limit cache retention.
- [ ] P2: Cover post-validation Telegram settings sanitization:
  delete a project/session after validation but before the second sanitize path, or extract a deterministic helper seam, and assert the persisted response cannot retain stale references. The current stale-reference test at `src/tests/telegram.rs:1573` seeds invalid state before validation, so removing the post-validation sanitize in `src/telegram_settings.rs:73` would still pass.
- [ ] P2: Add Telegram settings file concurrency regressions:
  simulate UI config save racing relay state persistence across separate processes or an OS-lock harness, assert atomic writes prevent partial JSON reads, and assert token/config plus `chatId`/`nextUpdateId` are not lost.
- [ ] P2: Add Telegram preferences panel RTL coverage:
  cover API error display, stale default-session clearing, default-project auto-subscription, `inProcess` running/stopped lifecycle labels including stopped-over-linked precedence, AppDialogs Telegram tab path, and StrictMode-mounted save/test/remove flows proving post-await UI updates still land.
- [ ] P2: Cover Telegram `/sessions` state contract and command dispatch:
  add a serde sample for the `/api/state` projection (`projectId`, `messageCount`) and a command-path test proving `/sessions` calls the state endpoint and sends the rendered list.
- [ ] P2: Cover Telegram callback action failure handling:
  drive a dispatch failure through `handle_telegram_callback_query` or an extracted seam and assert callback answer text, chat error text, and no digest refresh.
- [ ] P2: Add `messageCreatedDeltaIsNoOp` semantic-change negatives:
  keep id/index/preview/count/stamp equal while changing message payload and assert a material apply; include same-id pending prompt cleanup coverage.
- [ ] P2: Add near-bottom prompt-send early-return scroll coverage:
  start near bottom, send with a pending POST, grow `scrollHeight`, and assert no old-target smooth scroll fires before the prompt lands.
- [ ] P2: Add virtualized bottom re-entry scroll-kind expiry coverage:
  return to bottom, cancel idle compaction, then issue a native scroll without wheel/touch/key prelude and assert stale `lastUserScrollKindRef` classification cannot leak.
- [ ] P2: Add Telegram startup-message coverage:
  assert the no-chat startup message points to `TERMAL_TELEGRAM_CHAT_ID` / trusted state binding rather than first-touch `/start`.
- [ ] P2: Add reconnect-specific gapped session-delta recovery coverage:
  arm reconnect fallback polling, reopen SSE, dispatch an advancing stamped `textDelta`/`textReplace` across a revision gap, and assert live text renders before snapshot repair while recovery remains pending until authoritative repair succeeds.
- [ ] P2: Add equal-revision gap repair snapshot adoption coverage:
  skip a non-session revision, optimistically apply a later session delta, then return `/api/state` at the same revision and assert the skipped global state is adopted instead of rejected as stale.
- [ ] P2: Add production SQLite persistence coverage:
  make the SQLite runtime persistence path available under `cargo test`, then cover temp-database full snapshot save/load, delta upsert, metadata-only update, hidden/deleted row removal, and startup load.
- [ ] P2: Add Windows state-path redirection coverage:
  cover SQLite main-file symlinks, sidecar symlinks, and `.termal` directory junction/symlink cases behind Windows-gated tests.
- [ ] P2: Add post-shutdown persistence ordering coverage:
  race a late background commit against `shutdown_persist_blocking()` and prove the final persisted state reflects the latest `StateInner`, not an older worker-drained delta.
- [ ] P2: Add concurrent shutdown idempotency race coverage:
  call `shutdown_persist_blocking()` concurrently from two `AppState` clones and assert `persist_worker_alive` cannot flip false until the join owner has returned.
- [ ] P2: Add graceful-shutdown open-SSE coverage:
  cover both shutdown-before-connect and shutdown-after-initial-state through `/api/events`, and assert the stream exits within a timeout so the persist drain is reached.
- [ ] P2: Add shutdown persist failure retry coverage:
  force the final shutdown persist attempt to fail once and then succeed, and assert the worker does not exit before the successful write.
- [ ] P2: Add non-send action restart live-stream delta-on-recreated-stream coverage:
  the round-13 fix proves `forceSseReconnect()` is called on cross-instance `adoptActionState` recovery, but does not dispatch live deltas through the recreated EventSource. Submit an approval/input-style action after backend restart, then dispatch assistant deltas on the new `EventSourceMock` and assert they render in the active transcript bubble.
- [ ] P2: Add live text-repair hydration rendering regression:
  drive the live-state hook or app through text-repair hydration after an unrelated newer live revision and assert the active transcript renders the repaired assistant text without scroll, focus, or another prompt.
- [ ] P2: Add AgentSessionPanel deferred-tail component regressions:
  cover switching from a non-empty deferred transcript to an empty current session, and same-id updated assistant text through the rendered component path (`useDeferredValue`, pending-prompt filtering, and the virtualized list), not only the exported helper.
- [ ] P2: Cover full-transcript demand inputs beyond upward wheel:
  add table-style cases for `scrollTop <= 160`, `ArrowUp`/`Home`/`PageUp`, touch drag-up demand, and the ctrl-wheel negative branch on the long-session tail-window path.
- [ ] P2: Add lagged-marker EventSource reconnect-boundary regression:
  dispatch `lagged`, trigger EventSource error/reconnect, then send a lower/same-instance state on the new stream and assert the old marker cannot force-adopt it.
- [ ] P2: Add remote hydration dedupe production-path coverage:
  drive bursty same-session remote deltas through the production hydration path, assert only one remote session fetch is issued, and assert the in-flight guard is cleared after successful hydration.
- [ ] P2: Add failed manual retry reconnect-rearm regression:
  cover manual retry hitting a transient failure, then the next scheduled attempt adopting a newer same-instance snapshot while polling still continues until SSE confirms.
- [ ] P2: Add timer-driven reconnect same-instance-progress live-proof regression:
  trigger the non-manual reconnect fallback path, adopt a same-instance `/api/state` snapshot with forward progress while SSE remains unopened/unconfirmed, advance timers, and assert fallback polling continues until a data-bearing live event confirms recovery.
- [ ] P2 watchdog wake-gap stop-after-progress regression:
  trigger watchdog wake-gap recovery, adopt same-instance `/api/state` progress, and assert no additional reconnect polling occurs before a later live event.
- [ ] P2: Cover the index clamp-on-shrink branch in `MarkdownDiffView` and `RenderedDiffView`:
  re-render the parent with a smaller `regions`/`segments` array while `currentChangeIndex`/`currentRegionIndex` points past the new end and assert the counter snaps to "Change/Region 1 of N" while prev/next still wrap correctly. Today the existing prev/next tests only exercise wrap-around at full length; the `current >= changeCount/regionCount` clamp branch in the `useEffect` is unexercised.
- [ ] P2: Add rendered diff render-budget coverage:
  create many Mermaid/math rendered regions and assert the preview applies the same document-level caps as a single `MarkdownContent` document.
- [ ] P2: Add single-target rendered diff navigation coverage:
  assert prev/next scrolls the only Markdown diff change and the only rendered diff region even though the selected index does not change.
- [ ] P2: Cover large Markdown editing deferral by character count:
  add a low-line Markdown document over `LARGE_MARKDOWN_FULL_RENDER_CHAR_LIMIT` and assert the editable-section deferral state plus opt-in transition. The current test covers only the line-count trigger.
- [ ] P2: Route the new lagged-recovery reconnect test through the textDelta fast-path it documents:
  the new `App.live-state.reconnect.test.tsx` test exercises the revision-gap branch (the `messageCreated` delta omits `sessionMutationStamp` so it falls into the resync fallback). Add `sessionMutationStamp` so the delta routes through the matched-stamp fast-path that the surrounding `handleDeltaEvent` comment is most concerned about, OR rename the test to clarify it covers the revision-gap branch specifically and add a sibling test for the textDelta fast-path.
- [ ] P2: Split the bad-live-event + workspaceFilesChanged test into isolated arrange-act-assert phases:
  `ui/src/backend-connection.test.tsx:1225-1261` co-fires the stale `delta` and the `workspaceFilesChanged` event in one `act()`. The assertion `countStateFetches() === hydratedStateFetchCount` is satisfied if either side skips confirmation, so the test cannot pinpoint which side regressed. Dispatch `workspaceFilesChanged` alone first and assert no fetch fired; then add the stale delta separately and re-assert.
- [ ] P2: Add frontend stop/failure delta-before-snapshot terminal-message coverage:
  dispatch cancellation/update deltas before the same-revision snapshot and assert appended stop/failure terminal messages remain rendered without relying on a later unrelated refresh.
- [ ] P2: Replace `function scrollIntoView() { ... = this; }` capture pattern in `AgentSessionPanel.test.tsx`:
  `AgentSessionPanel.test.tsx:1515, 1568, 1680` rely on `this`-binding inside a method-form function. Future swc/esbuild config that arrow-rewrites methods or any "use strict" tightening would silently break the capture. Use `vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(function (this: HTMLElement) { scrolledNode = this; })` and rely on `mockRestore()` to clean up.
- [ ] P2: Tighten `expectRequestErrorDeferredUpdatesOnly` to assert deferred-update payloads:
  `app-session-actions.test.ts:158-172` checks shape (every call is a function, never null) but never invokes the deferred functions. A regression where the deferred function returns a stale or `null` next state would still pass. Invoke each captured updater with a fixed `prev` and assert the resulting next state.
- [ ] P2: Add doc-comment to `clear_active_turn_file_change_tracking` enumerating callers and intent:
  the helper is now shared across normal-completion sites (`src/session_interaction.rs`, `src/state_accessors.rs`, etc.) and the two new persist-failure rollback sites (`src/session_lifecycle.rs:450`, `src/turn_lifecycle.rs:455`). A future "preserve grace deadline" tweak motivated by one purpose would silently change the other. Document the unconditional-wipe contract so readers see both intents.
- [ ] P2: Add Rust persist-failure rollback negative-coverage tests:
  `src/tests/session_stop.rs:560`, `src/tests/session_stop_runtime.rs:897` only cover the failure path. Add a sibling test that runs the same setup with succeeding persistence and asserts the post-stop record retains its expected (non-cleared) fields, proving the cleanup is gated on the failure branch and that the helper is not unconditionally clearing on every commit.
- [ ] P1: Add Telegram-relay unit tests for the pure helpers introduced in `src/telegram.rs`:
  cover `chunk_telegram_message_text` (empty, exact-3500-char, under-limit, no-newline-in-window hard-split, newline-in-window soft-split, multi-byte / emoji char-vs-UTF16-unit, trailing-newline preservation), `telegram_turn_settled_footer` for `idle` / `approval` / `error` / unknown-status arms, `telegram_error_is_message_not_modified` against the Telegram error wording, and a serde-decode round-trip for `TelegramUpdate` / `TelegramChatMessage` against a real-shape `getUpdates` JSON snapshot to pin the snake_case contract.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P1: Add direct splitter tests for `splitStreamingMarkdownForRendering(text, { deferAllBlocks: true })`:
  `ui/src/markdown-streaming-split.test.ts` currently has zero direct coverage for `deferAllBlocks`. Cover closed pipe-table followed by prose, closed fence followed by prose, closed math followed by prose, multiple closed blocks (cut at the earliest), nested constructs, and identity behavior on inputs containing no block constructs.
- [ ] P1: Pin the "no remount across streaming → settled transition" claim with DOM-identity assertions:
  `ui/src/MarkdownContent.test.tsx:1456` and `ui/src/MessageCard.test.tsx:245-285` assert the rendered shape but do not assert that React preserved the same DOM nodes across the rerender. Capture a stable child DOM node (e.g., a paragraph from the settled prefix) before flipping `isStreaming` to false, then assert `container.contains(savedNode)` and reference equality after the rerender.
- [ ] P2: Restore discrimination in `expectRenderedMarkdownTableContains`:
  `ui/src/App.live-state.deltas.test.tsx:296-315` was relaxed to accept either `.markdown-table-scroll table` or `.markdown-streaming-fragment` after `deferAllBlocks: true` shipped. The relaxation silently passes if a streaming table never settles. Split into two helpers (`expectStreamingTableFragmentContains` for active-streaming phase, `expectSettledTableContains` for after-turn-end) called at the right phases, or advance the test through whatever signal flips `isStreaming` to false before the assertion.
- [ ] P2: Move the Mermaid bundle-URL assertion out of the `appendChild` spy:
  `ui/src/MarkdownContent.mermaid-fallback.test.tsx:54` asserts inside the mock implementation; failure traces point at the spy rather than the test body. The post-render assertions at lines 77-78 (`expect(appendedScripts).toHaveLength(1); expect(appendedScripts[0]?.src).toBe(expectedBundleSrc);`) already pin the contract; delete the inline `expect()`.
- [ ] P2: Pin the heavy-content gate is bypassed during streaming:
  `ui/src/MessageCard.test.tsx:245-284` confirms shape but does not assert `.deferred-markdown-placeholder` is absent during streaming. Add an assertion for an `isStreaming` assistant message regardless of size, and pair with a long-enough streaming message (over the heavy threshold) confirming the gate stays bypassed.
- [ ] P2: Add a wire-projection round-trip test for `TelegramSessionFetchMessage`:
  the parallel narrow projection of `Message` in `src/telegram.rs:481-515` will silently desync if `wire_messages.rs` renames the discriminator or the `Text` variant fields — serde will deserialize into `Other` and the relay will go silent on text messages. Round-trip a representative `Message::Text` payload through `TelegramSessionFetchMessage` and assert the `Text` arm matched.
- [ ] P2: Add app-level watchdog coverage for replayable appliedNoOp deltas:
  `ui/src/live-updates.test.ts` now pins reducer-level `appliedNoOp` for `messageCreated`, `textReplace`, `messageUpdated`, `commandUpdate`, `parallelAgentsUpdate`, and marker replays, and `App.live-state.deltas.test.tsx` covers `textReplace` plus duplicate `messageCreated`. Add siblings for the remaining no-op replay types and assert the watchdog still fires within `LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 3000` ms.
- [ ] P2: Add genuine-divergence reconciliation coverage for same-revision unknown-session deltas:
  `docs/architecture.md` now documents that session creation advances the main revision, and `ui/src/app-live-state.ts` cross-links that contract. Add a coverage test that (a) sets the client up with a session list missing `session-X`, (b) dispatches a same-revision session delta for `session-X` (where `latestStateRevisionRef.current === delta.revision`) — asserting NO immediate `/api/state` fetch, then (c) dispatches the next authoritative `state` event including `session-X` and asserts it adopts cleanly.
- [ ] P2: Reset module-level rail-build FIFO state between overview controller tests:
  `pendingConversationOverviewRailBuildTasks`, `nextConversationOverviewRailBuildTaskId`, and `conversationOverviewRailBuildFrameId` persist across Vitest workers. Today only one test exists in `conversation-overview-controller.test.tsx`, but the test is built around tightly-counted frame flushes — order-dependency surface area is real. Either expose a test-only reset or use `vi.resetModules()` at the suite boundary.
- [ ] P2: Cover overview activation cancellation when the controller unmounts mid-pending:
  mount an overview-controller harness, flush one or two frames so a queued second-rAF or FIFO task is alive, then unmount and flush the remaining frames asserting that `setIsRailReady(true)` was never observed and the FIFO is empty. The current test never unmounts mid-pending so the rAF cancellation paths and FIFO splice-by-task-id are unexercised.
- [ ] P2: Finish splitting the remaining marker-menu create/remove test:
  the marker-menu coverage now has focused cases for keyboard trigger, portal cleanup, scroll/resize close, explicit trigger contract, and clamp fallback. The original create/remove test still combines add/remove, Escape focus restore, ArrowDown navigation, and rect-based clamp behavior; split the remaining assertions if it grows again.
- [ ] P2: Add `ConversationOverviewRail` compact-mode keyboard navigation coverage:
  cover `Enter`, `Space`, `ArrowDown`, `ArrowUp`, `Home` in compact mode (the existing `End` test only covers one key path). The `Enter`/`Space` path goes through `navigateToSegmentIndex(currentIndex)` and is meaningfully different from the arrow-key path; the arrow-key path resolves the *current* segment from `viewportProjection.viewportTopPx`. Cover the `viewportProjection.viewportTopPx → currentItem → currentIndex → findOverviewSegmentIndexForItemIndex` chain so a regression that broke it is caught.
- [ ] P2: Pin the `ConversationOverviewRail` compact-vs-per-segment threshold boundary:
  the threshold is hard-coded at 160 (`CONVERSATION_OVERVIEW_COMPACT_SEGMENT_THRESHOLD`). The existing test covers `5` segments (per-segment) and `220 commandMessages` (compact), but `buildConversationOverviewSegments` collapses same-kind runs so 220 messages can produce far fewer segments. Construct messages designed to produce exactly 160 vs. 161 distinct segments and assert per-segment vs. compact rendering at the boundary.
- [ ] P2: Cover the `ConversationOverviewRail` pointer cancellation path:
  the rail's `pointerdown → pointercancel` sequence is uncovered. `finishRailDrag` resets `suppressNextClickRef = false` for `pointercancel`, but no test exercises the cancel branch. On touch hardware, browsers fire `pointercancel` when a long-press triggers a context menu or scroll gesture; a regression that mishandled cancel would silently swallow the user's next click on a segment.
- [ ] P2: Cover the `ConversationOverviewRail` zero-height viewport floor:
  `Math.max(8, viewportProjection.viewportHeightPx)` at line 313 protects the visual indicator from disappearing on extreme zoom. Add a `viewportHeightPx: 0` snapshot rerender and assert `height: "8px"` on the indicator.
- [ ] P2: Cover the `setTimeout` fallback in `scheduleConversationOverviewIdleCallback`:
  the existing `conversation-overview-controller.test.tsx` harness installs an idle-callback shim. The new `scheduleConversationOverviewIdleCallback` (`conversation-overview-controller.ts:96-119`) has a `setTimeout` fallback for environments without `requestIdleCallback` (Safari, embedded webviews) that is uncovered. Add a second test that explicitly deletes `window.requestIdleCallback` and `window.cancelIdleCallback` before render, advances `setTimeout` with `vi.useFakeTimers()`, and asserts the rail still becomes ready after the fallback delay.
- [ ] P2: Tighten the long-active-session `VirtualizedConversationMessageList` test:
  the new "mounts a long active transcript from the estimated bottom window" test asserts `slot count <= 16`. Add a tighter lower bound (e.g., `>= 4`) or assert `getLayoutSnapshot().mountedPageRange.startIndex > 0` so a regression returning an empty mounted range is caught.
- [ ] P2: Restructure the `bottom_pin` mount test to exercise unmount→mount transition:
  the new "bottom-pin mounts the bottom range without starting a boundary reveal" test (`VirtualizedConversationMessageList.test.tsx:572-600`) asserts the bottom message renders, but `getByText("message-48")` was already true before the `notifyMessageStackScrollWrite` because `waitFor` ran first. Restructure: scroll to top so message-48 is unmounted, dispatch `bottom_pin`, then assert message-48 mounts AND the boundary-reveal attribute is absent — exercising both halves of the contract.
- [ ] P2: Prove deferred-tail-window initial render starts from newest messages:
  extend `AgentSessionPanel.test.tsx:1493-1525` so the first render includes a newest-tail message and excludes an early message, then assert the early/full transcript appears only after the scheduled hydration delay.
- [ ] P2: Add intermediate-checkpoint assertion to the deferred-tail-window test:
  `AgentSessionPanel.test.tsx:1493-1525` asserts only that the rail is absent immediately and present after 1.5s of advanced timers. A regression that made the rail render *immediately* (defeating the deferral) would still pass the post-advance assertion. Add an intermediate `act(() => vi.advanceTimersByTime(100))` checkpoint asserting the rail is *still* absent. Or assert directly on a transcript-hydration symptom (count of mounted message slots is small at t=0 and large at t=1500ms).
- [ ] P2: Add `get_session_tail` boundary and error coverage:
  `src/tests/http_routes.rs:138` covers only happy path (`tail=3` on 5 messages). Add cases for (a) `tail=0` on a populated session asserting `messages: [], messages_loaded: false`, (b) `tail >= len(messages)` where `start_index == 0` asserting `messages_loaded` propagates from the source, (c) `tail > SESSION_TAIL_HYDRATION_MAX_MESSAGES = 500` asserting the silent cap (and ideally fail-loud after the cap-disclosure fix lands), (d) missing session id 404, (e) hidden session, (f) `?tail=abc` malformed value, (g) tail-then-full id-overlap proving the wire-level guarantee.
- [ ] P2: Add `get_session_tail` remote-proxy hydration coverage:
  pair with the High-severity bug entry. Once remote-proxy routing is fixed (or explicitly gated), add a Rust integration test that calls `?tail=N` against an unhydrated remote-proxy session and asserts either upstream hydration triggers or the typed gating error fires. Today an empty tail with `messages_loaded: false` is the silent-degrade behavior.
- [ ] P2: Cover the four uncovered `tailAdoptOutcome` branches in `app-live-state.test.ts`:
  the new "adopts a large-session tail" test exercises only `partial`. Add one focused case per remaining outcome: (a) `adopted` (tail returns full transcript when backend has fewer than 100 messages with `messages_loaded: true` — short-circuits the full fetch), (b) `restartResync` (different `serverInstanceId`), (c) `stateResync` (revision gap), (d) `stale` (lower revision). Each branch has distinct side effects (`hydratedSessionIdsRef.add`, `hydrationRestartResyncPendingRef.current = true`, `requestActionRecoveryResyncRef`, fall through to full fetch).
- [ ] P2: Cover the tail-fetch mismatch path in `app-live-state.test.ts`:
  add a test where `fetchSessionTail` resolves with `session.id !== sessionId` (e.g., `"session-2"` while request was for `"session-1"`); assert `requestActionRecoveryResyncRef.current()` fires once, `fetchSession` is not called, `hydrationMismatchSessionIdsRef` contains the original id.
- [ ] P2: Cover the tail-then-full race-detection early-return:
  app-live-state.ts:1333 (`attemptedTailHydration && !sessionStillNeedsHydration`) handles a real race but is uncovered. Simulate: tail mock resolves; during `act`, dispatch a delta or other path that completes hydration before the full fetch runs. Assert no full fetch is issued and the session is added to `hydratedSessionIdsRef`.
- [ ] P2: Cover concurrent SSE delta during tail-then-full window:
  between the tail response and the full fetch, dispatch a synthetic `messageCreated`/`textDelta` MessageEvent. Assert: (a) the delta is correctly applied to the partial-tail messages (or correctly degrades to needs-resync if the delta targets a missing-prefix message), (b) the full fetch eventually adopts and the resulting transcript reflects both the delta and the full-load.
- [ ] P2: Cover threshold boundaries for `shouldStartTailFirstHydration`:
  test at `messageCount = 100` (no tail), `101` (tail), `102` (tail). Today the only test creates 150 messages. A regression flipping `>=` to `>` or dropping the `messageCount === undefined` fallback would not be caught.
- [ ] P2: Cover `shouldStartTailFirstHydration` skip predicates:
  five separate skip conditions (lines 767-783): `allowDivergentTextRepairAfterNewerRevision === true`, session not found, `messagesLoaded !== false`, `messages.length > 0`, `messageCount` below threshold. Add at least one test per skip condition.
- [ ] P2: Verify partial-adoption preserves `messageCount`:
  add `expect(sessionsRef.current[0]?.messageCount).toBe(150)` after the partial-adoption assertion in `app-live-state.test.ts`. A regression that drops or zeros `messageCount` in the partial path would still let the `messages.length === 100` assertion pass while breaking message-count rendering.
- [ ] P2: Cover `check_telegram_test_rate_limit` cooldown expiration and 60s retain TTL:
  current test covers only "first OK, second rejected". Add cases for (a) third call after >2s passes (cooldown actually expires), (b) entries pruned after 60s `TELEGRAM_TEST_RATE_LIMIT_RETAIN`. Use injected clock or `tokio::time::pause`.
- [ ] P2: Cover `prune_telegram_config_for_deleted_project` no-op early-return:
  call against a config that doesn't reference the target project; assert no disk write happens (file mtime unchanged or `Ok(())` returned without invoking persistence). The `telegram_configs_equal` early-return is currently uncovered.
- [ ] P2: Strengthen forwarder armed-priority assertion:
  the `telegram_forwarder_drains_armed_session_before_digest_primary` test asserts only `sent_texts == ["Telegram-originated reply", footer]`. This indirectly proves session-2 wasn't sent, but doesn't track which `session_id` values were passed to `get_session`. Track in the fake and assert exclusion explicitly.
- [ ] P2: Cover `backup_corrupt_telegram_bot_file` cross-fs fallback:
  current `telegram_state_persist_backs_up_malformed_existing_file` covers the rename success path but does NOT cover the `fs::copy` + `fs::remove_file` fallback. Extract `corrupt_telegram_bot_file_backup_path` + the rename-then-copy fallback to a helper accepting a `rename_fn: impl Fn(...)` and inject a forced-failure rename in a unit test.
- [ ] P2: Cover `write_telegram_bot_file` atomic-rename + temp-cleanup:
  round 56's rewrite (`telegram_settings.rs:417-427`) introduced `let result = (|| {...})(); if result.is_err() { let _ = fs::remove_file(&temp_path); }`. A failure inside the closure that leaves a stale `.tmp` file would be a silent disk-leak regression. Add a test that injects a `write_all` failure and asserts the temp file is unlinked.
- [ ] P2: Cover Windows `MoveFileExW` Telegram settings replacement:
  add a `#[cfg(windows)]` test that writes an existing Telegram settings file plus temp file, invokes the replacement/persist path, and asserts final content plus temp cleanup without delete-before-rename behavior.
- [ ] P2: Cover `forward_new_assistant_message_if_any` armed-clear branch for non-matching session:
  the helper `clear_forward_next_assistant_message_session_id` checks session_id equality at the implementation but no test exercises a non-match (i.e., armed for session-A, called with session-B).
- [ ] P2: Add `isConversationVirtualized` short-conversation gate test in `AgentSessionPanel.test.tsx`:
  round 57's behavioral change has no test for the < 80 message branch. Add a `makeTextMessages(5)` session test that clicks a marker and asserts `findMountedConversationMessageSlot` was used (or assert `virtualizerHandleRef.current` was never accessed because the prop was undefined). Pin the contract.
- [ ] P2: Cover `updateTelegramConfig` PATCH tri-state body shape (omitted vs null vs string):
  `api.test.ts:235-263` exercises only `{ botToken: null, defaultSessionId: null }`. Add cases: omitted (no key in body) and explicit string (`botToken: "123:abc"`).
- [ ] P2: Add AppDialogs Telegram tab path test:
  `AppDialogs.test.tsx` contains no `telegram` references. Open the settings dialog with `settingsTab="telegram"` and assert the panel renders with initial fetch.
- [ ] P2: Cover all 7 demand-driven hydration listeners in `AgentSessionPanel.test.tsx`:
  current test exercises only `wheel` of seven listeners (scroll, wheel, keydown, touchstart, touchmove, touchend, touchcancel). Add scroll-near-top (scrollTop = 50, fire scroll, expect hydration), keydown (ArrowUp/Home/PageUp dispatch), touchstart+touchmove with positive deltaY (pull-down) producing hydration, and a NEGATIVE control: `event.ctrlKey` wheel with `deltaY: -120` should NOT hydrate.
- [ ] P2: Assert one-shot hydration in demand-driven hydration test:
  after the wheel triggers hydration, fire wheel again and assert no further state churn (e.g., rendering doesn't re-mount, listeners are torn down because the effect's early-return fires after `hydrated` is true).
- [ ] P2: Assert listeners removed on unmount in demand-driven hydration test:
  spy on `node.removeEventListener` and assert each event name is removed on unmount/sessionId change. A leak that retains a listener on a detached scrollNode would not be caught.
- [ ] P2: Cover touch hydration semantics in `AgentSessionPanel.test.tsx`:
  dispatch touchstart at clientY=100, then touchmove at clientY=50 (negative delta, pull-up) → no hydration. Then touchstart at clientY=100, touchmove at clientY=200 (pull-down) → hydration. Verify touchend resets `lastTouchClientY` so the next touchstart starts fresh.
- [ ] P2: Cover post-arrival demand hydration:
  in `AgentSessionPanel.test.tsx`, fire a demand gesture after message arrival to prove the retained listener still hydrates the transcript.
- [ ] P2: Cover same-size tail-window overview translation shifts:
  build or render two different 20-message tail windows with the same `messageCount` and assert stale viewport translation is not reused across first/last message id changes. Include a cross-session case where one session's snapshot is queried against another's projection at the same `messageCount`.
- [ ] P2: Cover `resolveViewportSnapshotTranslation` negative branches:
  add focused tests for null layout snapshot, full-transcript no-op (`layoutSnapshot.messageCount >= estimatedRows.length`), empty layout messages, missing first row in estimated rows, and non-contiguous window. Add a separate live viewport snapshot case with a different `viewportTopPx`, plus a drift-case test where viewport snapshot count differs from translation snapshot count.
- [ ] P2: Cover `resolvePrependedMessageCount` branches directly:
  export the helper (or a thin shim) and add unit tests for cross-session, empty previous window, no growth, partial overlap, no first-message match, and contiguous match at index 0.
- [ ] P2: Pin `mountedRangeWillChange` early-return absence of stale-rect scroll write:
  during the prepend integration test, capture `harness.scrollWrites` between the prepend and the followup effect and assert no scroll write lands at the stale `targetScrollTop` value computed from pre-mutation rects.
- [ ] P2: Split bundled Telegram sanitizer assertions:
  break `telegram_log_sanitizer_redacts_bot_tokens_and_truncates` and `telegram_standalone_token_redaction_respects_context_and_thresholds` into focused per-shape tests so URL redaction, key contexts, bearer contexts, thresholds, and false-positive avoidance fail independently.
- [ ] P2: Add wheel/scrollTop demand-hydration boundary tests:
  add `deltaY: -7` (below threshold) and `deltaY: -8` (at threshold) cases, plus `scrollTop: 160` vs `161` cases to pin the constants.
- [ ] P2: Add `SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101` boundary test:
  add a 100-message session test asserting `fetchSessionTail` is NOT called, and a 101-message session test asserting it IS called.
- [ ] P2: Replace bundled `..._handles_escaped_and_telegram_specific_contexts` test with per-shape `#[test]`s:
  promote each of the nine assertions (escaped JSON, Bearer `:`, lowercase bearer-colon, Bearer `=`, `Authorization=Bearer`, lowercase bearer-equals, snake-case key, camel-case key, env-var key) to its own `#[test]`. Same for the prior bundled sanitizer tests and the `ambiguous_token_key` behavior flip.
- [ ] P2: Cover Telegram relay active-project reconciliation:
  start an in-process relay with subscribed projects but no default and assert startup fails or status exposes the effective `activeProjectId`; delete a project used by a running relay and assert the relay is stopped or restarted without the deleted id.
- [ ] P2: Cover Telegram relay runtime lifecycle seam:
  add an injectable or testable relay runtime so startup from saved settings, implicit first subscribed-project fallback, invalid/missing config stop, config-save start/stop/restart, deleted-project reconciliation, runtime status `running: true` + `inProcess`, and graceful-shutdown stop are covered despite the production path's `#[cfg(not(test))]` guards.
- [ ] P2: Cover Telegram relay stop/restart quiescence:
  simulate disable or config retarget while an old relay is in flight and assert stale-generation polling/action handling cannot continue after status reports the replacement or stopped state.
- [ ] P2: Add component-level session tab tooltip remote-owner coverage:
  render tab tooltips for projectless, missing-project, missing-remote, and conflicting session/project remote proxy sessions whose summaries carry `remoteId`, complementing formatter-level coverage for session-owner precedence.
- [ ] P2: Cover remote-sync embedded remote-owner clearing:
  seed a remote snapshot session with attacker-chosen `remoteId`, localize it, and assert trusted `SessionRecord.remote_id` metadata is preserved while the embedded `record.session.remote_id` is cleared and local wire projections re-emit only trusted ownership.
- [ ] P2: Pin `event.target === node` mousedown guard with negative case:
  add a sibling test that fires `mouseDown` on a child of `scrollNode` (e.g., a virtualized message slot) and asserts hydration does NOT occur. Round-63's isolated test only fires mouseDown directly on the scrollNode.
- [ ] P2: Cover MessageCard pending-action across all action types:
  the existing pending-action test only covers cancel. Add sibling tests for `Open session` and `Insert result`, asserting duplicate clicks on the same action are suppressed while unrelated action buttons keep their documented per-action pending behavior.
- [ ] P2: Cover ParallelAgentsCard tool-source no-callback assertion:
  in addition to the existing "no buttons rendered" assertion, also assert that `onCancelParallelAgent` / `onOpenParallelAgentSession` / `onInsertParallelAgentResult` were NOT called with the tool agent's id (defense in depth).
- [ ] P2: Add `ParallelAgentsCard` pending-action unmount coverage:
  click an async action, unmount before the promise settles, resolve/reject the promise, and assert the pending-state cleanup cannot update after unmount.
- [ ] P2: Pin session-id guard in scheduled refresh callbacks:
  extend the controller test harness so a `rerender({ sessionId: "session-b", messageCount: 90 })` between a schedule and the flush asserts the layout snapshot is NOT updated to the stale session-a result, and the new session's later flush wins.
- [ ] P2: Add unmount-while-pending tests for new rAF schedulers:
  schedule a refresh, unmount before flushing, and assert `cancelAnimationFrame` was called for the pending frame id. Same for a `sessionId` change mid-pending.
- [ ] P2: Pin rapid-update upper bound for layout-refresh coalescing:
  loop ten (or twenty) rerenders without flushing, asserting `frameCallbacks.size` remains exactly 1 across the burst. Spy on `getLayoutSnapshot` calls to assert it runs exactly once per flushed frame.
- [ ] P2: Stabilize `telegram_settings_load_defaults_only_for_missing_file` against platform `io::ErrorKind`:
  switch from a directory fixture (which returns platform-dependent kinds) to malformed JSON or asserts directly on `io::Error::kind()`.
- [ ] P2: Cover Git literal pathspec handling:
  after forcing literal pathspec behavior, add regression coverage for filenames containing `*`, `?`, `[]`, and `:(top)` so single-file Git actions cannot expand to other files.
- [ ] P2: Cover copy/rename staging pathspecs:
  add focused coverage for `collect_git_stage_pathspecs(..., Some("R"))` and `Some("C")`, preferably through a real repo scenario proving old and new paths are staged together.
- [ ] P2: Stabilize `/api/telegram/test` 422 discrimination:
  add a stable error code/kind or distinct statuses for local JSON-shape failures versus Telegram token/auth failures, then update the feature brief and tests to avoid message-text parsing.
- [ ] P2: Pin the 1024-char error message in token validation test:
  the new 1024-char case asserts only `status == BAD_REQUEST`. Add `assert!(long_err.message.contains("at most 256 characters"));` to ensure the limit is named even on extreme oversize.
- [ ] P2: Cover the `kind: "request-failed"` doc gap:
  document `ApiRequestErrorKind` post-round-58: round 58's `preserveGatewayErrorBody` makes `kind: "request-failed"` no longer a strict status-class signal — it can now appear with status 502/503/504 alongside non-5xx status codes. Update `ApiRequestErrorKind` JSDoc and feature-brief contract for wrappers using `error.status` for status-class triage.
- [ ] P2: Add 5xx empty-body fallback to `extractError` for `preserveGatewayErrorBody` callers:
  for empty 502 body, `extractError` returns `"Request failed with status 502."` — more confusing than the prior `"The TermAl backend is unavailable."`. Either fall through to backend-unavailable copy when `raw` is empty AND `status >= 502`, or have `extractError` return a sentinel that the caller can detect for a fallback.
- [ ] P2: Capture unhandled-rejection events in the rejected-action MessageCard test:
  the existing test asserts only the visible button-re-enabled side-effect (which `.finally()` alone satisfies). Add `process.on("unhandledRejection", ...)` capture or `vi.spyOn(console, "error")` to pin the `.catch(() => undefined)` round-65 fix.
- [ ] P2: Assert SSE-envelope source in the `parallelAgentsUpdate` create event:
  parse the first event in `state_events_route_streams_parallel_agents_update_sources` instead of consuming via `let _ = ...`, asserting `agents[0].source` and `agents[1].source`. Or add a sibling test scoped to the create path.
- [ ] P2: Cover production-path tool/delegation id collision:
  add a Rust test that drives both the Claude task path and the delegation creation path with overlapping ids (or document the assumption that uuid id spaces don't collide deterministically). The current test manually inserts the collision.
- [ ] P2: Cover `reconcileParallelAgentsMessage` source-flip in both directions:
  add a `delegation` → `tool` source-flip case alongside the existing `tool` → `delegation` case, or note the symmetry assumption in the test.
- [ ] P2: Split `cancelDelegation` `it.each` running/completed/canceled identical-pin into focused tests:
  add a comment explaining the test pin is the current-but-flagged behavior, or split `running` into its own test scoped to the bugs.md follow-up.
- [ ] P2: Pin pending-key composite contract in mixed-source MessageCard tests:
  trigger two sequential actions on the same source/id pair, assert one is rejected as pending. Add a test that flips a tool row to a delegation row across rerenders and asserts no row reuse. Also wrap with `DeferredHeavyContentActivationProvider` per sibling test pattern.
- [ ] P2: Strengthen "renders remote delegation progress as display-only" test:
  add `expect(params.onComposerError).not.toHaveBeenCalled()` after rendering, exercise a flag flip mid-test, or split into per-action coverage so a regression that drops only one of the three guards surfaces.
- [ ] P2: Clean up AgentSessionPanel `act(...)` warnings:
  targeted AgentSessionPanel Vitest still emits React `act(...)` warnings around async rerenders/events; identify the warned updates and wrap or await them so timing-sensitive failures are not hidden by noisy test output.
- [ ] P2: Strengthen race-condition delegation tests:
  the session-switch race test should also assert `expect(onDraftCommit).not.toHaveBeenCalledWith("session-b", "")` (negative on new session id). The unmount race test should assert `console.error` was not called with `act`/`unmounted` warnings, or stub `setIsDelegationSpawning` to verify it isn't invoked post-unmount.
- [ ] P2: Replace immediate `expect` with `waitFor` in busy-state delegation test:
  `await waitFor(() => expect(busyButton).toHaveAttribute("aria-busy", "true"))` instead of the synchronous expect after `Promise.resolve()`. Removes brittleness against future React batching changes.
- [ ] P2: Split bundled `delegation-commands.test.ts` tests:
  split `delegationTitleFromPrompt` (4 cases) and `resolveComposerDelegationAvailability` (4 outcomes) into single-case `it`s or restructure as `it.each([...])`.
