# Feature Brief: Shared Codex App-Server

This document describes how TermAl talks to Codex, and the identity model that
everything in this area depends on. It is the hardest subsystem in the codebase to
reason about; most of the bugs here have been subtle races or wrong invariants, so
this brief leads with the mental model and the pitfalls rather than a call graph.

Primary implementation:

- `src/codex.rs` — spawn/attach, the writer loop, thread-setup + parking, waiters
- `src/codex_events.rs` — inbound event routing and per-session state mutation
- `src/runtime.rs` — `SharedCodexSessionState`, `CodexRuntimeCommand`, the type defs
- `src/session_runtime.rs` — `SharedCodexRuntime`, `RuntimeToken`, detach
- `src/state_boot.rs` — `import_discovered_codex_threads` (boot-time discovery)
- `src/session_lifecycle.rs` — `kill_session` (removal + rediscovery suppression)

Related briefs: [agent-delegation-sessions.md](./agent-delegation-sessions.md),
[sqlite-session-storage.md](./sqlite-session-storage.md). Architecture overview:
[../architecture.md](../architecture.md).

## The one fact everything follows from

**Codex does not run one process per chat. One long-lived Codex process hosts every
*local* Codex session in a TermAl backend at once** (the "shared app-server";
remote-proxied sessions run on their own remote backend's app-server, and the handle
lives on that backend's `AppState`). TermAl attaches many logical sessions to it and
multiplexes JSON-RPC over a single stdio pipe:
`thread/start` / `thread/resume` open a conversation thread, `turn/start` runs a turn.

Because the process is shared and long-lived, **its identity cannot answer
per-session questions**. That single fact is the source of nearly every bug in this
area.

## Two identities: process vs attachment

There are two different "who is this?" questions, and conflating them is the classic
mistake:

- **Process identity** — `runtime_id`, the shared app-server. Held by
  `SharedCodexRuntime`, wrapped as `RuntimeToken::Codex(runtime_id)`. Every session
  shares it, and it **survives a detach + re-attach** — the same process is still
  there. Correct for exactly one thing: runtime *exit* fan-out ("this process died,
  fail every session on it").

- **Attachment identity** — one session's *current* attachment to that process. A
  session that stops and re-attaches gets a **new** attachment but the **same**
  `runtime_id`. Nothing keyed on `runtime_id` alone can tell the new attachment from
  the old one.

### Why it matters

A response or event that belongs to a torn-down attachment can arrive late, after
the session has re-attached. Guarded only by the process id, it still "matches" and
acts on the live session — e.g. a stale thread-setup waiter overwrites the live
attachment's persisted thread id. The record then claims thread A while the runtime
runs thread B; on restart TermAl resumes the wrong thread and rediscovers B as a
duplicate. That is the session leak reappearing through a side door.

### The attachment epoch (parked)

The fix is a generation stamp: `CodexAttachment { runtime_id, generation }`, minted
per attachment in `spawn_codex_runtime` and threaded through every per-session guard
so the weak process token is unreachable on those paths. This work is **parked** (git
stash; spec in beads `tm-d22`) because the reproducible leak is already fixed and the
remaining failures need a detach at an exact instant. Do not restart it casually — it
took many rounds and repeatedly re-introduced same-class bugs. See `tm-d22`, `tm-c7l`,
`tm-nqc`.

## Thread setup and parking (the fixed leak)

The reproducible leak — one prompt minting many Codex threads — is **fixed**
(commit `4203b31`). The mechanism and its fix are worth understanding because the
whole parking model exists for it.

- Writes are fire-and-forget on a **single serialized writer thread**; a `thread/start`
  request returns immediately and the session's `thread_id` is populated
  asynchronously — by the setup *response*, or by an earlier `thread/started`
  notification (`codex_events.rs`), whichever lands first. (Both can occur, which is why
  `thread_id`-bound and setup-in-flight are not mutually exclusive — see below.)
- The old fast path keyed on `thread_id`, so every prompt arriving in that window saw
  "no thread yet" and fired **another** `thread/start`. The app-server dutifully
  created a thread for each.
- Fix: `PendingCodexThreadSetup` holds the in-flight setup **and** the prompt.
  `handle_shared_codex_prompt_command` decides in one critical section, and the order
  is load-bearing: it tests **setup-in-flight before thread-bound**, because
  `thread/started` can bind `thread_id` while the setup response is still pending — so
  both states can be true at once, and checking `thread_id` first would start a turn on
  a half-bound thread and leave the parked prompt to run a second one.
  - `{setup in flight}` → **park** this prompt on the setup (its waiter runs whatever
    is parked; newest wins) — checked first
  - `{thread bound, no setup}` → start the turn immediately
  - `{no thread, no setup}` → claim the slot and fire exactly one setup

**Trap:** the "prompt handling and turn start are serialized on the writer thread"
invariant does **not** extend to waiters. The `StartTurnAfterSetup` hand-off is
enqueued by a *waiter* thread, so a setup can be in flight when the hand-off runs.
Reasoning that ignores this produced multiple wrong "this cannot happen" comments and
one `debug_assert!` that actually fired.

## Orphan-thread discovery and suppression

Codex persists its threads to its **own** state DB. At boot,
`import_discovered_codex_threads` scans that DB and imports threads TermAl does not
recognize as top-level "ghost" sessions, so history from Codex runs outside TermAl is
not lost. This feature is also how orphans become visible clutter:

- A `thread/start` that times out, or whose binding is lost, leaves an **orphan
  thread** on disk. Discovery re-imports it as a **phantom top-level, zero-message
  session**. (`tm-91c`, `tm-y22`.)
- Suppression is `ignored_discovered_codex_thread_ids` (the "ignore set").
  `kill_session` adds a killed Codex session's thread to it
  (`session_lifecycle.rs`, `ignore_discovered_codex_thread`), so a removed phantom is
  not re-imported on the next scan. This is not an unconditional forever guarantee:
  boot-time import prunes the ignore set to threads still present in the current scan
  (`retain`), and discovery is capped per home — so a suppressed thread that later
  falls outside a capped scan can drop out of the set and be re-imported afterward
  (`tm-91c`).
- Discovery **un-ignores** a thread that a live record still claims (its
  `external_session_id` matches) before consulting the ignore set — so a thread a
  session genuinely owns can never be stranded as suppressed. This is load-bearing:
  suppression on the waiter side is only safe *because* discovery reclaims a
  still-owned thread first.

Cleanup pattern (used when phantoms accumulate): cross-check each candidate is truly
empty against the SQLite blob, then `POST /api/sessions/{id}/kill` — which both
removes the record and suppresses the thread. Verify the killed thread ids landed in
the persisted ignore set afterward.

## Pitfalls learned the hard way

- **`/api/state` does not expose the ignore set.** Reading
  `ignoredDiscoveredCodexThreadIds` off the HTTP response yields `undefined` (easy to
  misread as `0` and conclude suppression is broken). The real set lives in the
  persisted `app_state` blob in `~/.termal/termal.sqlite` (`app_state` table,
  single row, `key` / `value_json`).
- **`/api/state` returns summary-only sessions** — `messages: []` for *every*
  session regardless of size. The real signal is `messageCount`. Never infer that a
  session is empty from the messages array, and cross-check the SQLite blob before any
  destructive action.
- **Process-scoped guards are the default trap.** Any `_if_runtime_matches` call on a
  per-session path is keyed to the shared process and can act on behalf of a dead
  attachment. Grepping for the *constructor* `RuntimeToken::Codex(` misses the ones
  fed as a variable — audit call sites, not just constructions.
- **A green suite can prove less than it looks.** Env-mutating edits that silently
  fail on CRLF, tests that exercise the safe branch of the bug they claim to cover,
  and `act()` flushing deferred values are all ways a passing test proves nothing.
  Assert the edit landed; negative-check the fix by neutering it and watching the
  test fail for the right reason.

## Status summary

- Reproducible one-prompt-many-threads leak: **fixed** (`4203b31`).
- Attachment-epoch hardening for the rare detach races: **parked** (`tm-d22`).
- Orphan-thread re-import: **known**, mitigated by kill+suppress cleanup (`tm-91c`,
  `tm-y22`).
