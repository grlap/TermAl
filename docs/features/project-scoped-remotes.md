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
- Remote configuration lives in app preferences inside `~/.termal/sessions.json`.
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
   forwarded `/api/health`.
2. **Tunnel only fallback:** run `ssh -N ...` and expect a TermAl server to
   already be running on the remote host.

Both modes use batch SSH, `ExitOnForwardFailure`, and keepalive options. Managed
mode does not currently update a remote checkout, build binaries, or install
TermAl. Those concerns are future packaging work, not part of the current remote
config.

## Routing rules

Project-scoped routes resolve the project first:

- file read/write
- directory listing
- git status/diff/file actions/commit/push/sync
- terminal run and terminal stream
- review document load/save
- project digest/actions
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

The UI passes canonical local project/session ids. Remote-native ids stay inside
the backend mapping layer.

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
- Managed SSH startup assumes `termal server` is available on the remote PATH.
- Remote source checkout updates and binary installation are future packaging
  work.
- A documented terminal 429 peek/resolve race can transiently put a request
  against the wrong local-vs-remote concurrency counter if project routing
  changes between the cheap in-memory peek and the later full scope resolution.

## Non-goals

- Browser connections directly to remote servers
- Multi-user collaboration semantics
- Relay-hosted auth and machine registration
- Cross-remote session migration
- A global merged filesystem or git workspace across remotes
