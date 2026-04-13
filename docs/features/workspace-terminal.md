# Feature Reference: Workspace Terminal

## Status

Implemented.

TermAl's terminal panel is a scoped command runner for the current session or
project. It is not a full PTY emulator. Each submitted command runs through the
backend, streams output back to the panel, and is kept in that terminal tab's
history.

## UX

- Terminal tabs can be opened from workspace context.
- Each terminal tab has its own history store keyed by tab id.
- History survives ordinary tab switches and pane moves while the terminal tab
  remains live.
- Closing a terminal tab prunes its history unless a command is still running or
  the panel is still mounted.
- The working directory is editable per terminal tab.
- If a terminal loses both session and project scope, the panel shows that it is
  no longer associated with a live target.

## Backend API

```text
POST /api/terminal/run
POST /api/terminal/run/stream
```

The JSON route returns the final command result. The streamed route returns SSE
frames:

```text
event: output
data: { "stream": "stdout" | "stderr", "text": "..." }

event: complete
data: { "command": "...", "stdout": "...", "stderr": "...", "exitCode": 0, ... }

event: error
data: { "error": "...", "status": 502 }
```

Validation, scope resolution, bad workdirs, and local concurrency-cap failures
return ordinary HTTP errors before the SSE response starts. Failures discovered
after the stream starts are sent as `event: error`.

## Routing

Terminal requests can include session and/or project scope. The backend resolves
that scope the same way as file and git operations:

- local project/session -> run locally
- remote project/session -> proxy to the owning remote through the SSH tunnel

Remote terminal streaming prefers the remote streamed endpoint. It falls back to
the remote JSON endpoint only when the stream endpoint returns 404 or 405, which
keeps older local development remotes usable without accidentally double-running
a command.

## Limits

- Local and remote destinations each allow four in-flight terminal commands.
- Commands longer than 20,000 characters are rejected.
- Workdirs longer than 4,096 characters or containing NUL bytes are rejected.
- Captured stdout/stderr are capped and marked truncated when the cap is hit.
- Remote streamed frame size and cumulative forwarded output are bounded before
  forwarding to the browser.

## No Production Timeout

Terminal commands intentionally do not have a production watchdog timeout.

The terminal panel is used for long-running foreground workflows such as:

- `flutter run`
- dev servers
- watch tasks
- REPL-like tools
- long integration commands

A timeout would terminate commands users expect to keep alive. Instead, TermAl
uses concurrency caps, output caps, and stream cancellation behavior.

## Cancellation

Local streamed terminal runs observe SSE disconnects. When the browser drops the
stream, the backend cancels the run and kills the local process tree.

Remote streamed runs release the local forwarding permit promptly on browser
disconnect. The remote body reader can remain blocked on a stalled remote socket
until the remote emits bytes or closes; this accepted design limitation is
tracked in `docs/bugs.md`.

## Accessibility

The `IN` / `OUT` visual labels are marked `aria-hidden` so screen readers do not
announce them as repeated content. The command input has a visible focus ring.
