# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

## In-flight terminal command results can be lost across panel remounts

**Severity:** Medium - running command output can disappear from the visible
terminal after tab switches or remounts.

`TerminalPanel` starts `runTerminalCommand()` from component-local state while
also caching history in the module-level `terminalHistoryById` Map. If the
panel unmounts while a command is running, a later mount rehydrates the running
entry as `Interrupted`, while the original async completion resolves into the
old component instance. The visible terminal can therefore miss the real
command result.

**Current behavior:**
- switching away from a terminal tab can unmount the active `TerminalPanel`
- remounting converts cached `running` entries to `Interrupted`
- the backend command may still complete, but the remounted panel does not
  receive that completion as canonical state

**Proposal:**
- make terminal history/result updates come from one lifecycle-safe store
- guard late async completions after unmount
- rehydrate remounted panels from canonical terminal state, not stale local
  snapshots
- add a regression test that switches/remounts during an in-flight command and
  still shows the eventual result

## Remote terminal commands skip local validation before proxying

**Severity:** Medium - invalid terminal requests can produce remote transport
errors instead of deterministic local validation errors.

`run_terminal_command` checks for a remote project/session scope before it
trims and validates the command. Empty or oversized commands for remote-scoped
requests are proxied first, so an unavailable remote backend can mask the
expected `400` validation error and invalid requests still incur remote work.

**Current behavior:**
- remote-scoped terminal requests route to the remote backend before command
  validation
- empty and oversized commands are validated locally only on the non-remote path
- remote connectivity can change the error returned for the same invalid input

**Proposal:**
- trim and validate `request.command` before remote scope routing
- reuse the sanitized command for both local and remote execution
- add remote-scoped validation tests for empty, oversized, and valid multibyte
  commands

## Terminal timeout kills only the top-level shell, not the subprocess tree

**Severity:** High - runaway terminal commands can outlive the advertised
60s timeout and keep consuming CPU, memory, and file descriptors.

`run_terminal_shell_command` relies on `SharedChild::kill` to terminate the
child on timeout, but does not kill the process tree. On Windows only
`powershell.exe` is terminated; grandchildren such as `cargo.exe`, `node`, or
anything started from PowerShell continue running past the deadline. On Unix
`sh` is terminated with `SIGKILL` but backgrounded children (`&`), `setsid`
detached children, and other grandchildren inherit stdout/stderr and keep
executing. These inherited pipes then block the output-reader threads and
compound the existing output-reader waiter-thread leak.

**Current behavior:**
- timeout path calls `SharedChild::kill` only against the top-level shell
- grandchildren inherit stdio pipes and continue running past the 60s cap
- the post-kill wait can still return `None` because grandchildren hold pipes
- the reader-thread leak bug is a direct consequence of this tree-kill gap

**Proposal:**
- on Windows, assign the child to a Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so closing the handle terminates the tree
- on Unix, set a new session/process group in `pre_exec` and kill with
  `killpg(-pgid, SIGKILL)` on the timeout path
- closing the process tree also releases the inherited pipes, which unblocks
  the output readers without the waiter-thread workaround

## No concurrency or rate limiting on `/api/terminal/run`

**Severity:** Medium - any same-origin caller can saturate the tokio blocking
pool and block every other blocking API endpoint.

`run_terminal_command` spawns a blocking task per request with up to a ~70s
worst-case budget and nothing bounds concurrent in-flight commands. A burst
of requests (from a buggy pane, orchestrator loop, MCP tool, or third-party
code with same-origin access) can occupy every worker in tokio's default 512
blocking thread pool. Combined with the tree-kill and reader-thread leaks,
a long TermAl session that runs background commands can grow OS threads,
FDs, and blocking-pool usage monotonically, starving git, filesystem reads,
and other blocking handlers.

**Current behavior:**
- no concurrency limit on `/api/terminal/run`
- each in-flight request holds a blocking worker for up to ~70s worst case
- saturation starves other blocking endpoints
- backgrounded children compound the effect by leaking additional threads

**Proposal:**
- add an `Arc<tokio::sync::Semaphore>` on `AppState` bounding concurrent
  terminal commands (e.g. 4 in-flight)
- acquire before `spawn_blocking` and release on completion or error
- return a deterministic 429/503 or queue short requests with a bounded wait

## Terminal history Map grows without bound as tabs are opened and closed

**Severity:** Low - slow memory leak over long sessions.

`TerminalPanel` persists command history in a module-level
`terminalHistoryById` Map keyed by `terminalId`. When a terminal tab is
closed, its Map entry is never removed. Over a long session with many
terminal tabs opened and closed, orphaned entries accumulate. Each entry
can hold command output up to 512KB per command.

**Current behavior:**
- closing a terminal tab does not delete its Map entry
- orphaned entries persist for the lifetime of the browser tab
- only explicit "Clear" removes entries for the active terminal

**Proposal:**
- delete the Map entry on component unmount via a cleanup effect
- or prune stale entries when the workspace reconciles closed tabs

## Terminal output-reader timeout can leak waiter threads

**Severity:** Low - repeated commands with inherited output pipes can slowly
consume process resources.

`join_terminal_output_reader` wraps a reader `JoinHandle` in another spawned
thread and waits with `recv_timeout`. If a command exits but a background child
keeps stdout or stderr open, the timeout path returns to the request, but the
waiter thread remains blocked on the original reader join handle indefinitely.

**Current behavior:**
- terminal output readers have a bounded wait at the request layer
- timeout returns an empty string with `truncated = true`
- the helper thread waiting on the reader join handle has no cancellation path

**Proposal:**
- replace the detached waiter with a cancellation-aware or polling-based join
  strategy
- ensure timeout cleanup can terminate or release blocked output readers
- add coverage for output-reader timeout behavior with an inherited pipe

## Terminal panel remounts when project scope appears or disappears

**Severity:** Medium - transient terminal input can be lost when the derived
project scope changes.

`App.tsx` renders the active terminal tab through two different subtree shapes:
one branch wraps `TerminalPanel` in a scope selector section, and the other
renders `TerminalPanel` directly. When `shouldRenderTerminalProjectScope`
flips because project/session lookup data arrives or changes, React remounts
the panel instead of updating it in place. The terminal history survives via
the module cache, but local draft state such as the current command and workdir
input does not.

**Current behavior:**
- scope resolution changes can remount the active terminal panel
- command drafts and workdir edits are reset on that remount
- the same terminal tab can present different component trees over time

**Proposal:**
- keep a stable terminal panel wrapper and toggle scope controls inside it
- or preserve the terminal draft state across scope transitions explicitly
- add a regression test for scope changes that should not discard typed input

## Terminal scroll-to-bottom effect yanks viewport on every history update

**Severity:** Low - users inspecting earlier output are scrolled back to the
bottom whenever new output or new entries arrive.

`TerminalPanel` runs a `useEffect` that sets `scrollTop = scrollHeight`
whenever `history` changes. If the user scrolls up to read earlier command
output, then a running command emits more stdout or a new entry is appended,
the effect forces the scroll container back to the bottom. Active text
selections inside rendered `<pre>` history items can also break on the scroll.

**Current behavior:**
- every history state change forces the container to the bottom
- scrolling up to read prior output is overridden by newer entries or in-flight
  stdout updates
- cross-entry text selections can break when the viewport jumps

**Proposal:**
- use a sticky-bottom pattern: capture `wasAtBottom` before commit and only
  auto-scroll when the user was already pinned
- optionally surface a "scroll to latest" pill when new output arrives above
  the viewport

## Scoped terminal tabs can share outer pane scroll state

**Severity:** Low - terminal tabs with the same workdir but different
session/project scope can inherit each other's outer scroll metadata.

`SessionPaneView` builds the terminal `scrollStateKey` from pane id and
`activeTerminalTab.workdir`. `openTerminalInWorkspaceState` intentionally keeps
terminal tabs distinct by workdir plus session/project scope, so two terminals
can point at the same path while targeting different sessions or projects. The
outer pane scroll cache and related indicator state then collide for those
distinct tabs.

**Current behavior:**
- terminal scroll state is keyed by pane id and workdir only
- scoped terminal tabs with the same workdir share that key
- switching between them can reuse the wrong outer scroll metadata

**Proposal:**
- key terminal pane scroll state by `activeTerminalTab.id`
- or include the full scope tuple alongside the workdir in the key
- add a regression test with two same-workdir terminal tabs in different scopes

## TerminalPanel handleSubmit uses a stale-closure isRunning guard

**Severity:** Low - two synchronous submit events can each start a new command
before React commits the running state.

`handleSubmit` reads `isRunning` from the render-time closure and calls
`setHistory` to append a new running entry. Between the render that triggered
the submit and React's next commit, the closure's `isRunning` stays `false`.
Two synchronous submits (rapid click, keyboard then click, or test harnesses
firing two events in one tick) can both pass the `!isRunning` check and each
dispatch its own `runTerminalCommand` call. The disabled button attribute
catches typical double-clicks but the in-function guard is ineffective.

**Current behavior:**
- disabled attribute blocks typical UI double-clicks
- the `!isRunning` check inside `handleSubmit` reads a stale closure value
- two synchronous submits can each append a running entry and start a command

**Proposal:**
- track running state in a `useRef` and read/write it synchronously at the
  top of `handleSubmit` before any `await`
- reset the ref in the try/finally branch
- add a regression test that fires two synchronous submit events

## Terminal shell child can be orphaned when SharedChild::new fails

**Severity:** Low - rare error path leaves an unsupervised child process and
blocked reader threads.

`run_terminal_shell_command` spawns stdout and stderr reader threads before
it calls `SharedChild::new(child)`. `SharedChild::new` internally runs
`try_wait`, which can return an IO error. On failure the raw `Child` value
is dropped and `Child::drop` does not kill the process, so the spawned shell
runs unsupervised. The reader threads stay blocked on the inherited pipes
until the orphan terminates on its own.

**Current behavior:**
- reader threads spawn before `SharedChild::new`
- a `SharedChild::new` error drops the raw child without killing it
- reader threads remain blocked on the orphan's pipes

**Proposal:**
- call `SharedChild::new` first and spawn reader threads only after it succeeds
- on any early spawn error path, explicitly `let _ = child.kill();` before
  returning
- ensure every error path that abandons the child also releases its pipe
  handles

## Terminal Unix shell uses `sh -lc` and re-sources login profile per command

**Severity:** Low - per-command latency and unexpected environment side effects
on Unix terminals.

`build_terminal_shell_command` invokes `sh -lc <command>` on Unix. The `-l`
flag makes sh a login shell that sources `/etc/profile` and the user's
profile files on every invocation. This adds measurable latency (often tens
of milliseconds or more depending on user config), can mutate environment
state per command, and is unusual for programmatic command execution. Most
programmatic tooling uses `sh -c` (or `bash -lc` when login semantics are
explicitly required for nvm/rbenv shims).

**Current behavior:**
- every terminal command re-sources the user's login profile on Unix
- per-command latency is higher than `sh -c`
- environment side effects can differ between commands in the same session

**Proposal:**
- switch to `sh -c "$command"` by default
- if interactive PATH semantics are intentional, document it and prefer
  `bash -lc` for consistency with the typical nvm/rbenv setups
- verify on Linux and macOS that command execution PATH is still sane after
  the change

## Empty terminal workdir returns confusing "must stay inside project" error

**Severity:** Low - trivial input validation surfaces as a scope error that
misleads users about the actual problem.

`run_terminal_command` does not trim or reject an empty `workdir` string
before calling `resolve_project_scoped_requested_path`. With
`ScopedPathMode::ExistingPath`, an empty path joins onto the server's own
cwd, which usually fails the containment check with a "must stay inside
project" error. The error is technically correct but misleading — the real
issue is a missing workdir.

**Current behavior:**
- empty workdir joins with the server's current directory
- the containment check rejects the result as out-of-scope
- the user sees a scope error rather than a missing-workdir error

**Proposal:**
- trim `request.workdir` and reject empty values with
  `ApiError::bad_request("terminal workdir cannot be empty")`
- apply the same check on the remote proxy branch before forwarding

## TerminalPanel workdirDraft sync effect can clobber user input mid-edit

**Severity:** Low - reconciles can reset the workdir draft while the user is
typing.

`TerminalPanel` syncs `workdirDraft` from the `normalizedWorkdir` prop in a
`useEffect` keyed on the prop alone. If an SSE reconcile or parent re-render
changes the prop while the user has the workdir input focused and partially
edited, the effect overwrites the draft with the incoming prop value,
discarding what the user typed.

**Current behavior:**
- prop updates blindly overwrite `workdirDraft`
- partially typed workdir edits are lost on reconcile
- the user sees their input reset without warning

**Proposal:**
- gate the sync on the input's focus state and a dirty flag so typed edits
  are preserved
- or key the draft by the prop only on mount and rely on local edits thereafter

## Terminal remote proxy timeout margin is smaller than its doc-comment claims

**Severity:** Low - a command that hits the local timeout plus reader joins
can race the remote proxy deadline and surface as a spurious `bad_gateway`.

`REMOTE_TERMINAL_COMMAND_TIMEOUT = 75s` is documented as "exceeds
`TERMINAL_COMMAND_TIMEOUT` so the remote backend can time out and respond
before the local proxy gives up", implying a generous margin. The actual
worst-case server processing is `60s (wait) + 0.25s (post-kill wait) + 2 × 5s
(reader joins) = ~70.25s`, leaving roughly 4.75s for network round-trip,
JSON decoding, and HTTP scheduling — not the implied 15s headroom. A brief
network hiccup on a slow remote link can return `bad_gateway` even though
the remote completed correctly.

**Current behavior:**
- remote proxy deadline: 75s
- worst-case server processing: ~70.25s
- remaining budget for network/JSON: ~4.75s
- doc-comment implies but does not state the full math

**Proposal:**
- bump `REMOTE_TERMINAL_COMMAND_TIMEOUT` to at least 90s
- or reduce `TERMINAL_OUTPUT_READER_JOIN_TIMEOUT` to 2s
- update the doc-comment on `REMOTE_TERMINAL_COMMAND_TIMEOUT` to state the
  real relationship (child wait + post-kill wait + reader joins + network
  margin) so future tweaks do not erode the budget silently

## Implementation Tasks

- [ ] P2: Add terminal in-flight remount regression test:
  start a command, remount or switch away from the terminal panel, resolve the
  request, and assert the visible terminal shows the final result.
- [ ] P2: Add remote-scoped terminal validation ordering tests:
  verify empty and oversized commands return local validation errors before any
  remote proxy attempt, and valid multibyte commands continue past validation.
- [ ] P2: Add terminal output-reader timeout cleanup coverage:
  simulate an inherited stdout/stderr pipe and assert the timeout path reports
  truncation without leaving unbounded blocked helpers.
- [ ] P2: Add `formatTerminalResult` signal-exit branch test:
  cover `exitCode: null` with `timedOut: false` to assert the
  `"Exit signal in ..."` label path.
- [ ] P2: Add terminal `stderr` and `outputTruncated` rendering tests:
  confirm stderr renders separately from stdout and that the truncation
  warning appears when `outputTruncated: true`.
- [ ] P2: Add positive multibyte terminal command validation test:
  send a 20K multibyte-char command with a valid in-scope workdir and
  assert it passes the character-length check (proving chars, not bytes).
- [ ] P2: Add `read_capped_terminal_output` invalid UTF-8 test:
  feed raw invalid byte sequences and assert `from_utf8_lossy` produces
  replacement characters.
- [ ] P2: Add terminal API round-trip tests:
  cover `runTerminalCommand` request serialization and a backend
  success-path response so stdout, stderr, and exit-code mapping stay locked.
- [ ] P2: Add backend terminal command runner coverage:
  run a trivial local command in a temp workdir and assert success, exit code,
  stdout, timeout state, and normalized workdir; add remote-proxy branch
  coverage if the harness can support it.
- [ ] P2: Add App-level terminal tab integration coverage:
  open Terminal from a session pane and verify the scoped embedded panel renders
  with `showPathControls={false}` while preserving command plumbing.
- [ ] P2: Add terminal workspace reconciliation tests:
  exercise `normalizeWorkspaceStatePaths` and `reconcileWorkspaceState`
  for `kind: "terminal"` tabs after reload and session changes.
- [ ] P2: Add scoped terminal scroll-state regression test:
  open two terminal tabs with the same workdir but different session/project
  scope and assert their pane scroll keys do not collide.
- [ ] P2: Add docked terminal render coverage:
  render `TerminalPanel` with `showPathControls={false}` and verify the
  alternate embedding mode still runs commands while hiding the workdir form.
- [ ] P2: Add terminal timeout/kill-path test:
  exercise `run_terminal_shell_command` with an injected short timeout and a
  sleep command, and assert `timed_out: true`, `success: false`, and no
  orphaned child process on Windows and Unix.
- [ ] P2: Reset `terminalHistoryById` between TerminalPanel tests:
  export a test-only helper from `TerminalPanel.tsx` to clear the module map
  and call it in `afterEach`, then add tests for cross-remount history
  restore and the "Interrupted" reconciliation branch for a stale `running`
  entry.
- [ ] P2: Add TerminalPanel sticky-bottom scroll test:
  assert the scroll container does not yank to the bottom when the user has
  scrolled up and new history entries arrive mid-run.
- [ ] P2: Add TerminalPanel double-submit regression test:
  fire two synchronous submit events before React commits and assert only
  one `runTerminalCommand` call happens.
- [ ] P2: Add terminal Windows verbatim-prefix workspace reconcile test:
  mirror the existing filesystem `\\?\C:\repo` restore test for terminal
  tabs so Windows reload handling is locked in alongside filesystem and
  git-status.
- [ ] P2: Add terminal null-workdir open test:
  cover `openTerminalInWorkspaceState(..., null, ...)` and the
  separate-tabs case where `findTerminalTab` is skipped because the workdir
  is null.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
