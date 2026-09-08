# Shared live events

See [architecture](../architecture.md) for application recovery boundaries and
[multi-browser workspaces](./multi-browser-workspaces.md) for layout persistence.
This brief is listed in the [feature index](./README.md).

Opening several TermAl browser tabs must leave HTTP capacity for navigation,
session hydration, workspace saves and prompts. HTTP/1 browsers commonly permit
six connections per origin, shared between tabs. One long-lived SSE connection
per tab can occupy all six while the backend continues to answer other clients.

`live-events.worker.ts` owns a `shared-live-events.ts` hub. Every tab on the same
origin and worker version subscribes through `live-event-source.ts`, leaving
one upstream EventSource for all subscribers. The hub forwards `state`, `delta`,
`lagged` and `workspaceFilesChanged` in order. It stores no transcript or cached
state snapshot.

A tab joining an OPEN stream receives `open` followed by `snapshotRequired`.
The app fetches `/api/state` immediately using its existing server-instance and
revision guards, then hydrates transcripts separately. A joining tab during
CONNECTING receives that status and waits for the initial SSE snapshot, with
the existing HTTP fallback timer still available.

Recovery belongs to the hub: a joining tab cannot interrupt a healthy OPEN stream
or a fresh CONNECTING replacement still owned by another port. A recovery
subscription can replace a CLOSED source or a CONNECTING attempt at least
10 seconds old. Old source callbacks
are fenced after replacement. Each client retains the app's reconnect watchdog.

Port heartbeats run every 15 seconds. A port without an acknowledgment for 90
seconds is reclaimed, even if the browser never sent `pagehide`. Closing the
last port closes the upstream and its timer. Persisted `pagehide` releases the
port; `pageshow` rejoins and obtains current state. A client that was frozen
beyond its lease rejoins through a fresh worker port before attempting fallback.

Ordinary visibility return probes the existing port instead of resubscribing.
The hub replies with the request identifier on the same ordered MessagePort
channel as live events, so queued events precede its acknowledgment. This relies
on the [MessagePort queue contract](https://html.spec.whatwg.org/multipage/web-messaging.html#message-ports):
delaying a live port does not selectively discard earlier data but deliver a
later acknowledgment. Channel deserialization errors trigger the existing
failure path, and closed or expired ports cannot answer a new probe. A stale
reply cannot satisfy a later probe. An unanswered probe uses the same eight-second
budget as worker startup, then rejoins and follows normal snapshot recovery.
Hiding, closing or suspending the page cancels the pending probe. This proves
channel attachment, not backend availability; upstream errors, lag notifications
and the app's reconnect watchdog retain their existing recovery paths.

For freeze/resume specifically, the [Page Lifecycle draft's task rule](https://wicg.github.io/page-lifecycle/#html-html-event-loop-definitions)
and [Chrome's frozen-state guidance](https://developer.chrome.com/docs/web-platform/page-lifecycle-api#developer-recommendations-for-each-state)
describe suspending tasks until resume, not clearing a surviving page's queue.
We infer that earlier port events therefore remain ahead of a post-resume reply.
This is a documented-model assumption, not a measured browser-freeze result or
proof of every browser implementation. A discarded page reloads with a fresh
subscription; bfcache uses the explicit detach/rejoin path above. Selective
message loss on a surviving port would require sequence/gap detection instead.

If SharedWorker is unsupported or denied, the factory returns native
EventSource. Worker startup without an acknowledgment for eight seconds,
asynchronous worker failure and client channel failure switch the existing
facade to native SSE, preserving event listeners and cleanup. This fallback
retains the browser's per-tab connection limit. All tabs must load the new
transport before the old per-tab streams stop occupying connection slots.

Tests cover eight subscribers sharing one source and receiving deltas, late
snapshot hydration and revision races, concurrent recovery, source callback
fencing, worker failure fallback, silent peer death, bfcache lifecycle,
worker entry wiring and server/client event-name parity. Production builds must
emit the worker asset as well as the app bundle; source edits alone do not
update the backend's prebuilt `ui/dist`.
