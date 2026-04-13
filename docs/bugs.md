# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

## Terminal stream parser treats partial SSE chunks as complete frames

**Severity:** High - streamed terminal output can fail intermittently on normal network chunk boundaries.

`processTerminalSseBuffer()` parses a buffer even when no `\n\n` frame delimiter has arrived. Because `ReadableStream` chunks can split anywhere, the frontend can parse incomplete SSE metadata or partial JSON and reject an otherwise healthy terminal stream.

**Current behavior:**
- `readTerminalCommandEventStream()` appends each decoded chunk to a buffer and calls `processTerminalSseBuffer()`.
- When `frameEnd < 0` and `flush` is false, the parser can still treat the remaining buffer as a full frame.
- A split `output` or `complete` JSON payload can throw from `JSON.parse()`.

**Proposal:**
- Leave trailing partial data in the buffer until a full frame delimiter arrives.
- Only parse a trailing frame during EOF flush.
- Add tests where SSE frames and JSON payloads are split across multiple chunks.

## Terminal streaming can queue output without backpressure

**Severity:** High - chatty commands or slow clients can grow backend memory without bound.

The terminal stream route uses an unbounded Tokio channel for stdout/stderr events. Output storage is capped, but the live event queue is not, so a producer can outrun a slow SSE consumer.

**Current behavior:**
- `run_terminal_command_stream()` creates an unbounded channel for terminal stream events.
- Reader threads forward every streamable stdout/stderr chunk into that channel.
- Slow clients can accumulate unbounded pending events in memory.

**Proposal:**
- Replace the unbounded event pipe with bounded flow control, output coalescing, or a drop/truncate policy that preserves final response correctness.
- Add coverage for large-output commands and slow stream consumers if practical.

## Remote terminal SSE proxy accumulates an unbounded pending frame buffer

**Severity:** High - a hostile or broken remote can push the local backend toward OOM while holding a remote terminal permit.

`forward_remote_terminal_stream_response()` reads from the remote HTTP response into a `pending: Vec<u8>` and only drains bytes once `find_sse_frame_delimiter` locates `\n\n` or `\r\n\r\n`. Combined with `remote_post_response_without_timeout()`, a remote that returns `200 OK` with `Content-Type: text/event-stream` and then dribbles bytes that never contain a frame delimiter grows `pending` without bound. This is distinct from the unbounded event-channel bug — that bug is about local producers outrunning slow consumers, while this one is pure remote-input memory exhaustion that does not require a slow SSE consumer at all. The `TERMINAL_OUTPUT_MAX_BYTES = 512 KiB` cap only protects locally-executed shells.

**Current behavior:**
- `pending` grows by every 8 KiB read from the remote.
- There is no per-frame, per-read, or per-stream size cap on the proxy path.
- A hostile remote can pin the remote terminal permit and local memory until OOM.

**Proposal:**
- Cap `pending.len()` at a bounded multiple of `TERMINAL_OUTPUT_MAX_BYTES` and return `ApiError::bad_gateway(...)` on overflow.
- Apply the same cap to the trailing-pending check after the read loop.
- Mirror a sanity cap in the frontend SSE buffer so it cannot accumulate matching memory.

## Remote terminal SSE proxy forwards output without a cumulative byte cap

**Severity:** High - a hostile or broken remote can stream unlimited output through the local backend.

`TERMINAL_OUTPUT_MAX_BYTES` caps the captured output for locally-executed terminal commands, but the remote SSE proxy path forwards `TerminalOutputStreamPayload.text` verbatim with no cumulative bytes counter. Because the live stream channel is already unbounded (tracked separately), a remote that streams gigabytes of `output` events drives both the local mpsc queue and the eventual frontend `TerminalCommandResponse.stdout`/`stderr` string without limit.

**Current behavior:**
- `handle_remote_terminal_sse_frame` decodes each `output` frame and sends it onto the local event channel.
- No cumulative byte counter; the local 512 KiB cap applies only to local command execution.
- The final `complete` event deserializes the remote `TerminalCommandResponse` verbatim with no further bounds check.

**Proposal:**
- Track total bytes forwarded in `forward_remote_terminal_stream_response` and stop forwarding `output` events once the per-stream cap is exceeded.
- Mark the final response `outputTruncated: true` and surface a truncation note in the UI.
- Add coverage for remotely-forwarded output that exceeds the cap.

## Remote terminal SSE error frames can spoof local HTTP status codes

**Severity:** High - a hostile or compromised remote can forge local error semantics, including triggering the "local backend unavailable" UI state.

`handle_remote_terminal_sse_frame` reads `TerminalStreamErrorPayload.status` (a `u16` chosen by the remote) and builds an `ApiError::from_status(...)` that is then sent as the local `error` SSE frame. A hostile remote can synthesize `401`, `403`, `502`, or `503`, and the frontend's `createBackendUnavailableError` (in `ui/src/api.ts`) unconditionally trips when it sees certain statuses, falsely telling the user the **local** backend is down with `restartRequired: true`. `annotate_remote_terminal_429` only prefixes the message — it does not gate which statuses can be propagated.

**Current behavior:**
- The remote chooses any `u16` status and the local proxy forwards it unchanged into `ApiError`.
- A spoofed 502/503 triggers the local backend-unavailable UI.
- A spoofed 401/403 can confuse auth-related client behavior.

**Proposal:**
- Always return `ApiError::bad_gateway(payload.error)` for stream-embedded error frames and put the remote's code in the message body.
- Or clamp the propagated status to a small allowlist (`400`, `404`, `408`, `429`, `500`, `502`, `504`) and collapse anything else to `502`.
- Audit the 429 propagation path so legitimate remote 429s still flow through `annotate_remote_terminal_429`.

## SSE client disconnect does not cancel the spawned terminal worker

**Severity:** Medium - disconnecting from a running streamed command leaks processes, FDs, and concurrency permits until the command finishes on its own.

When the SSE consumer disconnects (tab closed, fetch aborted), axum drops the `Sse` body and `event_rx`. The detached `tokio::spawn` task and its inner `spawn_blocking` runner do not observe this drop. The local branch blocks in `process.wait()` (or the no-timeout loop), and the remote branch blocks in `response.read()`. `event_tx.send(...)` returns `Err` that is silently discarded. The semaphore permit is held for the full remaining lifetime. Clicking away from four streamed commands is enough to exhaust the local terminal semaphore for everyone else.

**Current behavior:**
- `tokio::spawn { spawn_blocking { ... } }` ignores `event_rx` lifetime.
- Disconnected clients still hold their permit until natural completion.
- Remote-forwarded commands additionally keep a TCP connection open.

**Proposal:**
- Observe `task_tx.is_closed()` between reads, or carry a `tokio::sync::CancellationToken`/drop-guard wired to the SSE stream's `Drop`.
- On disconnect, kill the child via `process_tree.kill(...)` and abort the forwarding read loop.
- Pair with the tracked no-timeout bug so an abandoned stream releases resources immediately instead of after the watchdog expires.

## Terminal stream route error contract diverges from the JSON route

**Severity:** Medium - the same class of failure returns HTTP 4xx on `/api/terminal/run` but HTTP 200 + SSE error frame on `/api/terminal/run/stream`.

`run_terminal_command` returns all failures (validation, scope resolution, workdir resolution, spawn errors) as direct HTTP 4xx/5xx. `run_terminal_command_stream` returns only validation and rate-limit errors as HTTP, while workdir resolution, scope resolution, and shell spawn failures happen inside `tokio::spawn` and are funneled through an SSE `error` event under HTTP 200. A network-level observer, a third-party HTTP client written against the docs, or a logging/metrics pipeline cannot treat the two routes equivalently.

**Current behavior:**
- Pre-spawn errors on the streaming route: HTTP 400/429 like the JSON route.
- Post-spawn errors on the streaming route: HTTP 200 + `event: error` frame.
- The same bad-workdir request returns 400 from the JSON route and 200 from the streaming route.

**Proposal:**
- Resolve workdir and scope synchronously in the outer async function before creating the SSE stream and spawning, so every pre-spawn failure still returns as HTTP.
- Document in `docs/architecture.md` which error classes flow through HTTP vs SSE error frames.

## Remote terminal stream HTML fallback can double-execute non-idempotent commands

**Severity:** Medium - a 200 + `text/html` response from a remote's streaming route silently triggers a second execution via the JSON fallback.

If the remote `/api/terminal/run/stream` returns `200 OK` with `Content-Type: text/html`, `run_terminal_command_stream` silently falls back to `remote_post_json_with_timeout` against `/api/terminal/run`. A `200` response implies the remote has already accepted and may have started executing the command; the fallback then runs it a second time. This is low risk for idempotent commands but unsafe for `git push`, `rm`, `npm publish`, or any other command with side effects. The 404/405 branch is safe because those statuses guarantee no execution started.

**Current behavior:**
- Fallback triggers on 404, 405, OR 200-with-text/html.
- 200 + HTML from a streaming endpoint re-invokes the command on the JSON route.
- Users running non-idempotent commands can experience double execution.

**Proposal:**
- Limit the fallback to `NOT_FOUND` and `METHOD_NOT_ALLOWED` only.
- On 200 + non-`text/event-stream` content type, return `ApiError::bad_gateway("remote returned unexpected content type for terminal stream")` without re-executing.

## Remote terminal SSE proxy lives in `src/api.rs` instead of `src/remote.rs`

**Severity:** Medium - ~115 lines of new SSE wire-format parsing sit in the API handler layer, breaking the documented "transport details live in remote.rs" boundary.

`src/api.rs` explicitly states it stays "intentionally thin" and that transport details live elsewhere, but the new streaming endpoint added `forward_remote_terminal_stream_response`, `handle_remote_terminal_sse_frame`, `parse_terminal_sse_frame`, and `find_sse_frame_delimiter` directly to the handler file. A future remote-stream consumer (e.g. a remote events relay) would either duplicate the parser or reach back into `api.rs`.

**Current behavior:**
- `api.rs` now owns frame delimiter scanning, SSE field parsing, and remote response buffering.
- `remote.rs` already encapsulates `decode_remote_json`, `request_with_optional_timeout`, and other remote transport concerns.
- Two sibling parsers (Rust + TypeScript) now live in different layers.

**Proposal:**
- Move the SSE helpers into `remote.rs` (or a new `remote_sse.rs`) as a typed helper: `forward_remote_sse_stream<E: DeserializeOwned, C: DeserializeOwned>(response, on_event, on_complete)`.
- Let the API handler stay at routing and scope-resolution concerns only.

## Terminal history `IN`/`OUT` labels announce inside `aria-live` region

**Severity:** Medium - screen reader users hear "I N" / "O U T" on every new history entry.

The new IN/OUT row layout in `TerminalHistoryItem` renders `<span class="terminal-io-label">IN</span>` and `OUT` decorative text inside the history container that declares `role="log" aria-live="polite"`. Assistive technology announces "I N echo hello 12:00:00 - Failed O U T permission denied" on every command. The neighboring `terminal-prompt` `$` already uses `aria-hidden="true"` for the same reason, so this is inconsistent with the rest of the panel.

**Current behavior:**
- `terminal-io-label` spans are not marked `aria-hidden`.
- Each new history entry triggers a screen-reader announcement that includes the decorative labels.
- The `terminal-prompt` span is already `aria-hidden="true"`.

**Proposal:**
- Add `aria-hidden="true"` to both `terminal-io-label` spans.
- Add a regression assertion in the TerminalPanel tests that the decorative labels are not part of the live announcement.

## TerminalPanel appendTerminalOutput silently routes unknown streams to stderr

**Severity:** Medium - a malformed-but-parseable `output` event misclassifies text instead of erroring.

`processTerminalSseBuffer` casts `JSON.parse(parsed.data) as TerminalCommandOutputEvent` with no runtime validation. `appendTerminalOutput` then checks `output.stream === "stdout"` and falls into the stderr branch for everything else. A payload like `{"stream": "warn", "text": "..."}` silently lands in stderr. A numeric `text` value becomes `"5"` via template-string coercion instead of being rejected.

**Current behavior:**
- The SSE output payload is cast without validation.
- Unknown `stream` values fall into the stderr branch via the `else` in `appendTerminalOutput`.
- Type coercion masks malformed payloads.

**Proposal:**
- Validate `parsed.stream === "stdout" || parsed.stream === "stderr"` and `typeof parsed.text === "string"` before dispatching.
- Surface unknown or malformed payloads through the error SSE path instead of silently routing them.

## Terminal stream reader does not cancel the underlying body on error exit

**Severity:** Medium - thrown errors release the stream reader lock but leave the HTTP body half-read until garbage collection.

`readTerminalCommandEventStream` calls `reader.releaseLock()` in `finally` but never `reader.cancel()`. On a thrown error path — malformed JSON in an output event, an explicit error event, or a missing completion — the stream is left un-cancelled. The HTTP connection stays parked until GC reaps it.

**Current behavior:**
- `finally` releases the reader lock but does not cancel the underlying stream.
- Error exits leave the connection in a half-read state.
- Network resources are held longer than needed on failures.

**Proposal:**
- In the error/throw paths, call `await reader.cancel().catch(() => {})` before `releaseLock()`.
- Or refactor to `for await (const chunk of body)` which cleans up automatically on `break`/`throw`.

## Terminal stream readers can emit output after completion

**Severity:** Medium - late reader output can break terminal stream event ordering.

The streaming output readers can continue forwarding chunks after the main terminal path has emitted the final `complete` event. This can happen when reader joins time out and the reader threads are deliberately detached, but the streaming hook remains live.

**Current behavior:**
- `join_terminal_output_reader()` can detach reader threads after its timeout.
- `read_capped_terminal_output_into_with_stream()` can continue sending output events from those detached readers.
- `send_terminal_stream_result()` can emit `complete` before those late output events stop.

**Proposal:**
- Disable live streaming once the reader-join timeout path is taken, or guarantee output readers are drained/stopped before `complete` is emitted.
- Add coverage proving no `output` events arrive after `complete`.

## Remote terminal stream fallback misses non-SSE legacy responses

**Severity:** Medium - mixed-version remotes can fail instead of falling back to JSON terminal execution.

The architecture docs say remote streaming falls back to `/api/terminal/run` when a remote does not support `/api/terminal/run/stream`. The implementation only falls back for 404, 405, or obvious HTML responses, so a successful non-SSE response is parsed as a stream and fails.

**Current behavior:**
- Remote stream calls use `/api/terminal/run/stream` first.
- Fallback is limited to 404, 405, or content types that look like HTML.
- A successful non-`text/event-stream` response is passed into SSE parsing.

**Proposal:**
- Treat successful non-SSE content types from `/api/terminal/run/stream` as legacy-compatible fallback cases.
- Retry `/api/terminal/run` before attempting to parse SSE frames.
- Add remote compatibility coverage for non-SSE stream responses.

## Terminal complete events are not validated before success

**Severity:** Medium - error-shaped completion payloads can be treated as successful terminal results.

The frontend casts every `complete` SSE payload to `TerminalCommandResponse` without runtime validation. If the backend emits an error-shaped payload on a `complete` event, the terminal panel can mark the command as done instead of surfacing the error.

**Current behavior:**
- `processTerminalSseBuffer()` parses `complete` event data and casts it to `TerminalCommandResponse`.
- It does not check for required terminal response fields or detect `{ error, status }` payloads.
- Error-shaped completion payloads can flow to the successful terminal result path.

**Proposal:**
- Validate decoded `complete` payloads before returning success.
- Route error-shaped completion payloads through the existing terminal stream error path.

## Terminal command input has no visible focus indicator

**Severity:** Medium - keyboard users can lose track of focus in the terminal panel.

The terminal command input removes its focus outline without adding an equivalent replacement. That regresses basic keyboard accessibility for the primary terminal input.

**Current behavior:**
- `.terminal-command-input:focus` sets `outline: none`.
- No matching `:focus-visible` or replacement focus treatment is provided.

**Proposal:**
- Restore a visible focus style for `.terminal-command-input:focus-visible`.
- Match the app's existing focus ring or border treatment.

## Terminal stream parser tests miss chunk and error cases

**Severity:** Medium - the riskiest terminal stream parser paths can regress without failing tests.

The current frontend stream parser and terminal panel streaming tests cover only simple happy paths. They do not exercise chunk buffering, CRLF delimiters, error events, missing completion, remote compatibility fallback, repeated output callbacks, or stderr routing.

**Current behavior:**
- `runTerminalCommandStream` tests use intact SSE frames.
- No test splits a frame or JSON payload across multiple `ReadableStream` chunks.
- Error, truncated-stream, stderr, repeated-output, and remote proxy behavior are not pinned.

**Proposal:**
- Add tests for split chunks, CRLF delimiters, stream `error` events, missing `complete`, and remote fallback behavior.
- Add terminal panel tests that emit multiple output callbacks, including stdout and stderr, before command completion.
- Add remote-scoped backend stream proxy tests for successful SSE and JSON fallback.

## Project deletion leaves orchestrator project references dangling

**Severity:** Medium - removing a project can leave persisted orchestrator state pointing at a missing project.

The project delete path removes the project and detaches sessions, but it does not reconcile other project-bound state. `OrchestratorInstance.project_id` can still reference the deleted project after the transaction, leaving the state model internally inconsistent.

**Current behavior:**
- `delete_project()` removes the project record and clears `session.project_id`.
- Orchestrator instances tied to that project keep their original `project_id`.
- Later orchestrator UI/actions can encounter a missing project reference.

**Proposal:**
- Block project deletion while active or persisted orchestrator instances reference the project, or cascade-clean those references in the same delete transaction.
- Add a regression test for the chosen behavior.

## Remote-backed project deletion can orphan remote projects

**Severity:** Medium - deleting a non-local project locally can leave matching project state on the remote backend.

Remote projects are created through the remote proxy flow, but the new project delete path only mutates local state. Removing a remote-backed project can therefore orphan the remote project binding and create duplicate or stale remote project state later.

**Current behavior:**
- `delete_project()` treats local and remote-backed project records the same.
- The local project is removed, but no delete is sent to the remote backend.
- Recreating the project can leave remote state behind or create another remote project.

**Proposal:**
- Proxy deletion to the remote backend when the project uses a non-local remote, or reject deletion for remote-backed projects until remote deletion is implemented.
- Add coverage for remote-backed project deletion or rejection.

## Project delete handler can update UI state after unmount

**Severity:** Medium - an in-flight project deletion request can update React state after the app unmounts.

`handleProjectMenuRemoveProject()` awaits `deleteProject()` and then updates application state without re-checking `isMountedRef.current`. Other async handlers in the app guard this pattern to avoid stale post-unmount updates.

**Current behavior:**
- The handler calls `adoptState()`, `resetRemovedProjectSelection()`, and `setRequestError(null)` after awaiting the delete request.
- The error path can call `reportRequestError()` after the same await.
- Neither path checks whether the component is still mounted.

**Proposal:**
- Return early after the await if `isMountedRef.current` is false before applying state updates or reporting errors.

## Standalone control surfaces can keep stale project scope after deletion

**Severity:** Medium - deleting a selected project can leave standalone control-surface workspace state out of sync.

The delete flow resets stored selected project ids, but it does not route standalone control-surface panes through the same workspace rescope logic used by manual project-scope changes. A standalone Files, Git, Sessions, or Projects surface can keep stale pane scope or root state until another user action forces a rescope.

**Current behavior:**
- `resetRemovedProjectSelection()` falls selected project ids back to All projects.
- It does not call the same workspace update path used by `handleControlSurfaceProjectScopeChange()`.
- Standalone control surfaces can remain scoped to a deleted project's derived workspace state.

**Proposal:**
- Centralize project-scope reset so project deletion updates both the selected filter state and workspace pane scope/root state.
- Add UI coverage for deleting the selected project from a standalone control surface.

## Project delete route is missing from architecture docs

**Severity:** Low - the documented HTTP API surface is out of sync with the backend routes.

The staged changes add `DELETE /api/projects/{id}`, but `docs/architecture.md` does not list the new route in the project API section.

**Current behavior:**
- The backend exposes `DELETE /api/projects/{id}`.
- The API table omits the route.

**Proposal:**
- Add `DELETE /api/projects/{id}` to `docs/architecture.md` with its response shape and behavior.

## File changes collapse button silently mutates state under search

**Severity:** High - the collapse toggle behaves as a no-op button while a search forces expansion, but the click still flips the internal state — and the change surfaces later once the search is cleared.

`FileChangesCard` computes `isFilesExpanded = !canExpandFiles || filesExpanded || isSearchExpanded`. When `searchQuery` is non-empty, `isFilesExpanded` is forced `true`, the collapse button is still rendered with `aria-expanded={true}` and the label "Collapse changed files", and clicking it sets `filesExpanded` to whatever the user toggled. The click has no visible effect while the search is active, but after the user clears the search the card snaps to whichever state the hidden toggle landed on. The user has silently mutated the card's persistent expansion state without feedback.

**Current behavior:**
- The collapse button remains rendered and clickable while `isSearchExpanded` is true.
- Clicks flip `filesExpanded` with no visible effect.
- Clearing the search reveals the now-unexpected state.
- `aria-expanded` reports the forced value, not the user-controllable state.

**Proposal:**
- Disable or hide the collapse button while `isSearchExpanded` is true.
- Or short-circuit the click handler when the search is forcing expansion.
- Have `aria-expanded` track the user-controlled `filesExpanded` rather than the union.

## Terminal stream endpoint is missing `error` event documentation

**Severity:** Low - the API surface of record does not describe the full SSE event contract.

The `docs/architecture.md` row for `POST /api/terminal/run/stream` lists `output` and `complete` events but not `error`. The backend emits `event: error` with a `{error, status}` payload (see `terminal_command_sse_event` in `src/api.rs`), and the frontend handles it via `createTerminalStreamEventError`. Third parties or future TermAl revisions implementing the protocol have an incomplete contract.

**Current behavior:**
- Docs describe `output` and `complete` events only.
- `error` events are emitted and consumed but undocumented.
- Payload schemas for `output` (`{stream, text}`) and `error` (`{error, status}`) are not documented.

**Proposal:**
- Expand the architecture row (or add a small "Terminal stream events" subsection mirroring the existing `/api/events` SSE doc) to enumerate all three event types and their payload schemas.
- Document which error classes flow through HTTP status vs embedded `error` events.

## Terminal complete-event serialization fallback emits the wrong event name

**Severity:** Low - if `TerminalCommandResponse` ever fails to serialize, the fallback produces a corrupt `complete` frame instead of routing through the error path.

`terminal_command_sse_event` handles a `serde_json::to_string(&response)` failure by emitting an `{error, status}` payload under `event: complete`. Both the local frontend and the remote proxy parser deserialize the frame as a `TerminalCommandResponse` and silently produce an object with missing fields. The path is currently unreachable (plain-owned `TerminalCommandResponse` always serializes), but it is fragile — a future field with fallible serialization would produce a degenerate success state instead of a visible error. The already-tracked "Terminal complete events are not validated before success" bug addresses the frontend side of the same contract violation; this is the symmetric backend-side issue.

**Current behavior:**
- `Complete` arm's serialization fallback emits `event: complete` with an error-shaped payload.
- The frontend and remote proxy treat it as a successful completion.

**Proposal:**
- Emit `Event::default().event("error").data(...)` on serialization failure.
- Or assert serializability statically and crash in tests instead of producing a corrupt success frame.

## SSE frame parsers have minor spec-compliance gaps

**Severity:** Low - the Rust SSE parser and the frontend SSE parser disagree on corner cases.

`parse_terminal_sse_frame` in `src/api.rs` returns `None` when a frame has `data:` lines but no `event:` field; the SSE spec says the default event name is `"message"`, and the frontend `parseSseFrame` defaults to that. `find_sse_frame_delimiter` in `src/api.rs` recognizes `\n\n` and `\r\n\r\n` but not bare `\r\r`; the frontend's `normalizeSseBuffer` replaces all `\r` with `\n` before parsing. Neither gap is reachable with axum's producer today, but a spec-compliant third-party remote would break through the proxy.

**Current behavior:**
- Rust parser silently drops frames without `event:` (spec default `"message"`).
- Rust delimiter scanner does not recognize `\r\r`.
- Frontend parser is more forgiving than backend parser in both directions.

**Proposal:**
- Default `event_name` to `"message"` in `parse_terminal_sse_frame` and route unknown event types through the existing `_ => Ok(None)` arm.
- Add a `\r\r` match to `find_sse_frame_delimiter`, or document that TermAl streaming SSE only supports LF and CRLF framing.

## File changes collapse tests miss boundary and search cases

**Severity:** Low - file-list collapse behavior can regress at threshold and search edge cases.

The file changes card now collapses long file lists and auto-expands when search is active. Current coverage only clicks expand on a seven-file message, leaving the threshold and search-triggered expansion behavior unpinned.

**Current behavior:**
- A seven-file changed-files card has a collapse/expand test.
- Exactly-six-file behavior is not covered.
- Search-query auto-expansion is not covered.

**Proposal:**
- Add a test proving exactly six files render expanded without the collapse control.
- Add a test proving a non-empty search query auto-expands long file lists.

## Implementation Tasks

- [ ] Add React Testing Library coverage for project deletion:
  open the project row context menu, confirm removal, mock `api.deleteProject`,
  and assert fallback to All projects plus relevant scope reset behavior.
- [ ] Add terminal stream parser edge-case tests:
  cover split SSE chunks, CRLF-delimited frames, stream error events,
  missing completion, complete-payload validation, and remote JSON fallback
  behavior.
- [ ] Add terminal panel streamed-output tests:
  cover multiple output callbacks before completion, including stdout append
  behavior and stderr rendering.
- [ ] Add backend remote terminal stream proxy tests:
  cover successful remote SSE output plus fallback to `/api/terminal/run`
  when the remote stream route is unavailable or returns non-SSE content.
- [ ] Add file changes collapse boundary tests:
  cover exactly six changed files and search-triggered auto-expansion for
  long file lists.
- [ ] Add file changes collapse round-trip and click-through-search tests:
  click expand then collapse and assert the list disappears, and verify that
  clicking the toggle while a non-empty `searchQuery` forces expansion does
  not silently mutate the underlying state.
- [ ] Add Rust unit tests for the four pure terminal stream helpers:
  cover `parse_terminal_sse_frame` (multi-line `data:`, missing event name,
  comment lines, CR trimming), `find_sse_frame_delimiter` (LF/CRLF/mixed/
  split-across-window), `terminal_output_delta_locked` (emitted-bytes
  advance, short-circuit when nothing new arrived), and
  `terminal_streamable_utf8_prefix_len` (incomplete trailing multibyte with
  and without the flush flag).
- [ ] Add Rust integration tests for terminal stream 429 and error paths:
  exhaust the local and remote terminal semaphores against
  `/api/terminal/run/stream` and assert 429 responses, and exercise a command
  that fails so an `event: error` SSE frame is emitted and the response is
  drained cleanly without a `complete` frame.
- [ ] Tighten terminal stream integration test assertions:
  continue reading until the SSE stream closes (instead of breaking on first
  `complete`), assert exactly one `complete` frame and no `error` frame, and
  compare the completed response stdout to the exact expected text.
- [ ] Tighten TerminalPanel streaming assertions to exact stdout text:
  replace `findByText(/done/)` with an exact-text assertion against the
  `pre.terminal-output-stdout` element so a concatenation-on-complete
  regression (instead of replacement) would fail the test.
- [ ] Anchor TerminalPanel substring text assertions to `<pre>` selectors:
  follow the existing rate-limit test pattern
  (`findByText("...", { selector: "pre.terminal-output-stdout" })`) for the
  stdout/stderr/mounted/remount assertions so a misrouted output regression
  cannot pass the test.
- [ ] Add frontend test for multiple buffered output frames in one chunk:
  enqueue two `output` SSE frames in a single `ReadableStream` chunk and
  assert `processTerminalSseBuffer` drains both.

## Known Design Limitations

These are deliberate design tradeoffs, not bugs, but are recorded here so
they stay visible to future contributors and can be revisited if the
tradeoff space changes.

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
  join) ≈ 70s; on the remote-proxy path the same ~70s inner budget fits
  inside the 90s `REMOTE_TERMINAL_COMMAND_TIMEOUT` envelope with ~20s of
  slack. Tuning `TERMINAL_OUTPUT_READER_JOIN_TIMEOUT` should account for
  both budgets and the 2× sequential multiplier.
- Reader threads write into an `Arc<Mutex<TerminalOutputBuffer>>` shared
  with the main thread (see `read_capped_terminal_output_into` and
  `join_terminal_output_reader` in `src/api.rs`). Each reader signals
  completion via a `sync_channel(1)` so `join_terminal_output_reader`
  blocks in `recv_timeout` instead of polling — the happy-path wake is
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
leak — the thread exits when its target exits, no data captured up to
the timeout is lost, and on Windows the Job Object prevents the scenario
entirely — and closing it would require platform-specific pipe-
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
consumed for a request that then fails deep inside the blocking task —
or vice versa. Both sides fail closed (mismatches return safely as
`ApiError`), but the 429 counters can transiently diverge from what is
actually in flight on each budget.

**Accepted tradeoff.** Closing this race would require snapshotting the
full resolution (scope, not just a boolean) before acquiring the permit,
which means running `ensure_remote_project_binding` — a blocking
`reqwest::send` on the first-time-bind path — on the async worker
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
bypassing `std::process::Child`'s encapsulation — either with a
crate-level extension trait or a direct `CreateProcess` call that
mirrors stdlib's stdio plumbing. That is a substantially larger
refactor than the ~10μs the snapshot costs in practice, so the current
implementation is left as-is with a prominent comment documenting the
reason.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
