# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

No active repo bugs are currently tracked.

## Resolved

The agent readiness mutex contention bug (full-state snapshots recomputing readiness
under the app-state mutex) was fixed by introducing `AgentReadinessCache` with a 5-second
TTL outside `state.inner`, a double-checked locking refresh pattern, explicit cache
invalidation on session creation and settings changes, and moving `GET /api/state` to
`run_blocking_api`.

The revision-race where `create_session` and `update_app_settings` emitted two different
`StateResponse`s at the same revision (stale readiness in the SSE event, fresh readiness
in the API response) was fixed by refreshing the agent readiness cache *before* the
critical section and then using `commit_locked` atomically (bump + persist + publish SSE)
with the API response snapshot built under the same `inner` lock hold.  This also
eliminates the deferred-publish interleaving window (a concurrent mutation could no longer
sneak a revision bump between the SSE publish and the API response).

The `update_app_settings` drop-reacquire TOCTOU window (settings mutations visible to
concurrent threads before commit; "cannot remove remote" validation running under one
lock hold with commit under a different one) was fixed by hoisting
`normalize_remote_configs` and the cache refresh above the `inner` lock, so the lock is
held continuously from validation through mutation through commit — matching the
`create_session` pattern.

`publish_snapshot` now logs serialization errors instead of silently swallowing them.
`publish_state_locked` returns `()` instead of `Result<()>`, consistent with the existing
`publish_delta` fire-and-forget pattern.

The unnecessary `.clone()` in `agent_readiness_snapshot` (cloning the owned snapshot
before moving it into the cache) was replaced with a move-then-clone-from-stored pattern.

The `snapshot()` non-atomicity (readiness captured before `inner` lock, revision read
under it — allowing a `/api/state` call to return stale readiness at a revision whose SSE
carried fresh readiness) was fixed by having `snapshot()` ensure the cache is fresh, then
delegate to `snapshot_from_inner` which reads `cached_agent_readiness()` under the `inner`
lock.  All snapshot-building paths now use the same cache read, so any snapshot at revision
N carries the same readiness that was published in the SSE event for revision N.

A doc comment was added to `snapshot_from_inner` explaining the intentional staleness
tradeoff (it runs under the `inner` mutex where filesystem I/O is not safe).

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
