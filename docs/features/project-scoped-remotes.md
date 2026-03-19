# Feature Brief: Project-Scoped Remotes

Backlog source: proposed feature brief; not yet linked from `docs/bugs.md`.

This brief describes a multi-machine architecture where the browser always talks
to the local TermAl server, and the local server routes project work to the
correct remote TermAl server.

## Goal

Support multiple remote backends without teaching the browser to manage
multiple backend connections directly.

The intended transport is:

`UI -> local TermAl server -> remote TermAl server`

The local server remains the control plane:

- stores preferences and remote configuration
- exposes the single browser-facing `/api` and SSE stream
- aggregates or proxies project and session state
- routes actions to the remote that owns the target project

Each project is assigned to one remote. Sessions inherit that routing from
their project.

## Core model

### Ownership

- The browser never connects to remote TermAl servers directly.
- The local server always exists and cannot be removed.
- Remote configuration lives in local preferences.
- Each project has a `remoteId`.
- Each session belongs to a project.
- Each session inherits the project's `remoteId`.

### Scope rules

- Project-scoped operations route by `project.remoteId`.
- Session-scoped operations route by the session's owning project.
- File, directory, git, and review actions should keep using project scope where
  possible.
- Session creation should require a project instead of relying on the current
  workspace or default workdir.

### Identity

- Browser-facing ids for projects and sessions must be globally unique across
  all remotes.
- The local server should expose stable composite ids such as
  `remoteId::projectId` and `remoteId::sessionId`, or an equivalent stable local
  alias.
- Remote-native ids stay an internal implementation detail of the local server.

## UX summary

- Add a `Remotes` section in Settings.
- The local remote is always present and marked as built-in.
- Creating a project requires selecting a remote.
- The project list shows which remote owns each project.
- Creating a session is project-first, not workspace-first.
- The browser still sees one application and one event stream, not a backend
  picker.

## Suggested remote config shape

The exact schema can evolve, but the first useful version is:

- `id`
- `name`
- `baseUrl`
- `enabled`
- `auth` or token placeholder for later expansion

The local remote should be represented explicitly, for example:

- `id: "local"`
- `name: "Local"`
- no removable toggle

## Task list

## 1. Preferences and persistence

- [ ] Extend backend `AppPreferences` to persist `remotes`.
- [ ] Add a backend `RemoteConfig` type and serde migration for old state files.
- [ ] Keep existing preference fields working unchanged.
- [ ] Reserve a built-in local remote entry and recreate it if missing.
- [ ] Extend frontend `AppPreferences` types to include remote configs.
- [ ] Extend settings API request and response payloads to read and write remotes.
- [ ] Add tests for preference persistence and migration.

## 2. Project model

- [ ] Add `remoteId` to the backend `Project` model.
- [ ] Add `remoteId` to the frontend `Project` type.
- [ ] Assign the startup default project to the built-in local remote.
- [ ] Validate that every created project references a known remote.
- [ ] Decide whether project creation is owned locally, mirrored remotely, or
      fully owned by the target remote.
- [ ] Update project serialization and migration tests.

## 3. Session routing rules

- [ ] Make session creation require a `projectId`.
- [ ] Remove or deprecate the create-session fallback that uses the current
      workspace or default workdir.
- [ ] Validate that a session workdir stays inside the selected project root.
- [ ] Resolve session routing from the owning project instead of ad hoc workdir
      inference.
- [ ] Update tests that currently create project-less sessions.

## 4. Local server as control plane

- [ ] Add a remote client layer in the local backend.
- [ ] Support a built-in in-process adapter for the local remote.
- [ ] Support HTTP clients for configured remote backends.
- [ ] Define a common interface for `get state`, `create project`, `create
      session`, `send message`, `stop`, `kill`, file access, git access, and
      review access.
- [ ] Normalize transport and remote API failures into local API errors.
- [ ] Include the owning remote name in user-facing error messages where useful.

## 5. State aggregation

- [ ] Decide the ownership model for project and session state:
- [ ] Option A: remotes own project and session state; the local server
      aggregates and rewrites ids.
- [ ] Option B: the local server owns the canonical project list and mirrors
      only the execution state to remotes.
- [ ] Pick one model and document it before implementation.
- [ ] Aggregate state from all enabled remotes into one browser-facing state
      response.
- [ ] Rewrite project ids and session ids so they are collision-safe.
- [ ] Preserve per-project and per-session remote metadata for display and
      routing.
- [ ] Keep cached state for temporarily offline remotes so the UI can show a
      degraded but legible view.

## 6. Event stream and revisions

- [ ] Subscribe the local server to each remote event stream, or poll if a
      remote does not support the needed streaming contract.
- [ ] Re-emit one merged browser-facing SSE stream from the local server.
- [ ] Do not forward raw remote revision numbers directly to the browser.
- [ ] Introduce a local aggregate revision counter for merged snapshots and
      deltas.
- [ ] Rewrite delta payload ids so they match the browser-facing project and
      session ids.
- [ ] Define reconnect behavior when one remote drops while others remain
      healthy.
- [ ] Add tests for delta routing, resync, and id rewriting.

## 7. Remote project creation flow

- [ ] Extend the create-project dialog to select a remote first.
- [ ] For local projects, keep using the existing local root picker.
- [ ] For remote projects, decide between manual path entry and a remote browse
      endpoint.
- [ ] Validate remote paths on the owning remote, not on the browser.
- [ ] Return project metadata that already includes the resolved `remoteId`.

## 8. UI settings and project surfaces

- [ ] Add a `Remotes` settings tab in the UI.
- [ ] Add create, edit, enable, disable, and remove actions for remote configs.
- [ ] Prevent removal of the built-in local remote.
- [ ] Show remote ownership in project rows, project selectors, and session
      context surfaces.
- [ ] Show remote-specific connection state where it matters, while keeping the
      global app connection pointed at the local server.

## 9. UI create-session flow

- [ ] Remove the `Current workspace` and `Default workspace` session creation
      path.
- [ ] Make project selection mandatory when creating a session.
- [ ] Derive session routing entirely from the selected project.
- [ ] Keep startup settings such as model and approval mode unchanged aside from
      the new required project selection.

## 10. File, git, review, and command routing

- [ ] Route file reads and writes by project ownership.
- [ ] Route directory listing by project ownership.
- [ ] Route git status, git diff, staging, and commit actions by project
      ownership.
- [ ] Route review document load and save by project ownership.
- [ ] Route command-discovery and instruction-search requests by the session's
      owning remote.
- [ ] Audit all frontend calls that still assume a single backend origin.

## 11. Readiness and capability reporting

- [ ] Stop treating agent readiness as one global local-machine value.
- [ ] Track readiness per remote.
- [ ] Decide how project creation should behave when the selected remote is
      reachable but missing an agent binary.
- [ ] Decide whether model lists and capability hints should be fetched lazily
      per session or prefetched per remote.

## 12. Security and trust boundaries

- [ ] Define how the local server authenticates to remotes.
- [ ] Decide whether auth is out of scope for the first pass and only trusted
      LAN or user-managed endpoints are supported.
- [ ] Validate that local preferences do not leak secret material back to the
      browser unnecessarily.
- [ ] Review path-handling assumptions for remote file access and ensure remote
      validation remains authoritative.

## 13. Dev and test tooling

- [ ] Decide how local development runs with one control-plane server and one or
      more remote servers.
- [ ] Add helpers or scripts for launching a local server plus mock remotes.
- [ ] Add integration coverage for:
- [ ] local project
- [ ] remote project
- [ ] project creation failure on remote
- [ ] remote event stream disconnect
- [ ] session id collision across remotes
- [ ] file and git routing by project ownership

## Recommended implementation order

1. Preferences model and remote config persistence.
2. Project `remoteId` and mandatory project-backed session creation.
3. Local backend remote client abstraction.
4. Aggregate state and merged SSE with rewritten ids.
5. Create-project remote picker and remote project path flow.
6. Proxy file, git, review, and session actions through the local control
   plane.
7. Per-remote readiness and failure UX polish.

## Open decisions

- Should the target remote own the canonical project record, or should the local
  server own project records and push only execution requests outward?
- Do remote projects need a remote file-picker endpoint, or is manual path entry
  acceptable for v1?
- Should the browser show remote-specific offline badges inside project and
  session cards, or is a project-level warning enough for the first pass?
- Is direct remote-to-remote session movement ever needed, or is creating a new
  project on another remote the only supported move?

## Non-goals for the first pass

- Browser connections directly to remote servers
- Multi-user collaboration semantics
- Relay-hosted auth and machine registration
- Cross-remote session migration
- A global merged filesystem or git workspace across remotes
