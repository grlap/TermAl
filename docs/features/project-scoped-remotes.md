# Feature Reference: Project-Scoped Remotes

This document describes the project-scoped remote architecture that is currently
implemented in TermAl.

## Status

Implemented for SSH-backed remote execution through the local control plane.

The browser always talks to the local TermAl server. The local server owns the
browser-facing API, persisted preferences, project list, workspace layouts, and
event stream. Projects may be bound to the built-in local remote or to an SSH
remote. Sessions, orchestrator instances, terminal commands, file operations,
git operations, review documents, and instruction search route from that project
ownership.

## Core model

### Ownership

- The browser never connects to remote TermAl servers directly.
- The built-in `local` remote is always present, enabled, and non-removable.
- Remote configuration lives in app preferences inside `~/.termal/termal.sqlite`.
- Each project stores an optional `remoteId`; omitted or `local` means local.
- Sessions and orchestrator instances inherit routing from their project.
- Remote-backed browser-visible sessions are local proxy records with stored
  remote ids for proxying.

### Remote config

The shipped config shape is intentionally small:

```ts
type RemoteConfig = {
  id: string;
  name: string;
  transport: "local" | "ssh";
  enabled: boolean;
  host?: string | null;
  port?: number | null;
  user?: string | null;
};
```

No private keys or secret material are stored. TermAl relies on the user's
system `ssh` and `ssh-agent` setup.

## SSH connection model

For SSH remotes, the local server creates a forwarded local port from the
47000-56999 range:

```text
Browser -> local TermAl :8787 -> ssh -L local_port:127.0.0.1:8787 -> remote TermAl
```

Startup attempts:

1. **Managed server:** run `ssh ... remote termal server`, then probe the
   forwarded `/api/health`. This intentionally stays a direct remote command so
   Windows SSH hosts do not need a POSIX shell just to start TermAl.
2. **Tunnel only fallback:** run `ssh -N ...` and expect a TermAl server to
   already be running on the remote host.

Both modes use batch SSH, `ExitOnForwardFailure`, and keepalive options. Managed
mode starts a remote server only for the lifetime of the SSH process. Saving a
remote definition does not start, stop, install, or upgrade anything.

## Remote registration and upgrades

SSH remotes expose one-shot lifecycle actions from the Remotes preferences panel:

- **Register TermAl** verifies an existing TermAl checkout on the remote host,
  checks that `git` and `cargo` are available, creates the remote `.termal/bin`
  directory, and writes `remote-install.json` with the checkout path/platform.
- **Build / upgrade** reads `~/.termal/remote-install.json`, runs
  `git pull --ff-only`, runs `cargo build --release --bin termal`, and installs
  the resulting binary to `~/.termal/bin/termal` on POSIX remotes or
  `%USERPROFILE%\.termal\bin\termal.exe` on Windows remotes.

Registration is intentionally based on an existing remote enlistment/checkout.
TermAl does not clone the repository, install Rust, configure SSH keys, or manage
a system service. The local server invokes both actions as one-shot SSH
commands, using `sh -lc` for POSIX checkout paths and encoded PowerShell for
Windows checkout paths, then captures stdout/stderr and returns the sanitized
output to the browser.

Build / upgrade can run for several minutes on a slow remote checkout. The
preferences panel keeps the action pending during that request, but build output
is returned only after the remote command finishes; progress streaming is not
part of the current lifecycle action API.

Managed startup still runs `termal server` from the remote command environment.
If you want the managed server path to pick up the binary installed by
**Build / upgrade**, put `~/.termal/bin` on the remote user's `PATH`.

## Routing rules

Project-scoped routes resolve the project first:

- file read/write
- directory listing
- git status/diff/file actions/commit/push/sync
- terminal run and terminal stream
- review document load/save
- instruction search
- orchestrator creation

Session-scoped routes resolve the session's remote mapping:

- send message
- cancel queued prompt
- stop/kill
- approval/user-input/MCP/Codex request replies
- model list refresh and session settings
- Codex thread actions
- agent-command discovery

Planned coordination routing (mailboxes and boards) will remain
control-plane-owned instead of following project or session routing. It is not
implemented yet: current mailbox eligibility is local-only and rejects
remote-backed proxies. See
[Durable agent mailboxes — Planned remote access](agent-mailboxes.md#planned-remote-access)
for the future placement rule, relay transport, and recovery semantics.

The UI passes canonical local project/session ids. Remote-native ids stay inside
the backend mapping layer.

### Routing authority and endpoint replacement

This contract is part of the broader [TermAl architecture](../architecture.md#system-overview).

App preferences are the authority for a remote id. `RemoteRegistry` publishes
that complete settings-owned configuration before reconciling cached
connections. Each request receives an immutable lease pinned to the routing
fields it observed: id, transport, enabled state, host, port, and user. The
display name is intentionally excluded from routing identity. Persisting any
remote edit still retires the cached connection and resets stream continuity,
including a display-name-only edit. A rename therefore aborts overlapping work
with the same retryable `409 Conflict`, even though a fresh request can use the
unchanged endpoint immediately.

Replacing or removing a remote retires the cached connection instead of
retargeting it in place. Request dispatch, response localization, event-bridge
frames, and terminal-stream consumption all revalidate their pinned authority.
Work that crosses a replacement is rejected and must not be folded into local
state or forwarded to the browser. A later A -> B -> A edit is accepted once A
is authoritative again because the routing bytes match, but the B publication
still retires the original A connection so an overlapping request cannot be
silently retargeted.

Concurrent authority changes use `409 Conflict` with stable messages:

- `remote connection changed before request dispatch` for ordinary remote
  actions and stream/fallback consumption
- `remote connection changed during remote creation` for project, session,
  orchestrator, lazy-binding, and fork round trips
- `project remote binding changed during remote creation`

These are retryable concurrency conflicts, distinct from transport failures.
Disabling a remote also retires overlapping leases with the ordinary `409`
connection-change response; retry cannot succeed until the operator re-enables
that remote, so clients should not automatically loop on this status.
Removal is the deliberate compatibility exception: work whose remote id is
deleted while in flight retains the existing `400 Bad Request` `unknown remote`
response, while replacement under the same id returns the stable `409`
connection-change response.
An unknown project retains the existing endpoint-specific API contract:
session creation reports `400 Bad Request`, while orchestrator creation reports
`404 Not Found`. Remote create responses that lose authority are rejected
without reviving deleted projects or persisting proxies owned by the old
endpoint.

Remote routing settings are published to live in-memory authority before the
old connection is torn down. The normal persistence path queues the SQLite
write and returns before that background write completes; background failures
are logged and retried. If the persistence queue is unavailable, the commit
falls back to a synchronous write. A synchronous fallback failure is returned
by the API and logged while the new in-memory route remains active until
restart, so a restart can restore the last successfully persisted route.

## State and events

The local server subscribes to each active remote's `/api/events` stream and
merges remote snapshots/deltas into local browser-facing state.

Important invariants:

- The browser receives one `/api/state` shape and one `/api/events` stream.
- Remote project/session/orchestrator ids are rewritten to local ids before the
  browser sees them.
- Remote revisions are not forwarded as the browser's main revision counter.
- Remote fallback state payloads trigger local resync behavior instead of being
  applied as ordinary local deltas.

## Project deletion

`DELETE /api/projects/{id}` removes the local project reference only.

For local and remote-backed projects:

- existing sessions remain visible and are detached from the project
- orchestrator instances are detached from the project
- remote project data is not deleted from the remote backend
- workspace tabs clear stale `originProjectId` references

This is deliberate. Project deletion in TermAl is a local organization action,
not a remote filesystem or remote database deletion.

## Terminal behavior

Terminal commands use the same project/session routing rules as file and git
operations.

- Local and remote destinations have independent in-flight command budgets.
- Streamed terminal commands use SSE `output`, `complete`, and `error` events.
- Local streamed commands kill the process tree when the SSE client disconnects.
- Remote streams are proxied when supported and fall back to JSON only for
  404/405 older-remotes compatibility.
- Commands have no production timeout by design; long-running foreground
  workflows are expected.

## Current limitations

- Remote readiness is still coarse. The UI reports SSH/connectivity problems,
  but does not model every remote agent binary independently.
- Remote registration assumes the checkout, Rust toolchain, `git`, and SSH auth
  already exist. TermAl verifies and uses them, but does not provision them.
- Windows remote lifecycle support is selected from Windows-style checkout
  paths such as `C:\src\TermAl`; POSIX `~/src/TermAl` paths keep using POSIX
  shell scripts.
- A documented terminal 429 peek/resolve race can transiently put a request
  against the wrong local-vs-remote concurrency counter if project routing
  changes between the cheap in-memory peek and the later full scope resolution.

## Non-goals

- Browser connections directly to remote servers
- Multi-user collaboration semantics (planned single-user cross-machine mailbox
  coordination remains in scope — see
  [Durable agent mailboxes — Planned remote access](agent-mailboxes.md#planned-remote-access))
- Relay-hosted auth and machine registration
- Cross-remote session migration
- A global merged filesystem or git workspace across remotes
