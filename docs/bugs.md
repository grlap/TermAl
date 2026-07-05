# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Codex thread discovery can re-import an orphaned thread from a failed thread/start

**Severity:** Low - a Codex `thread/start` whose response errors or times out can leave a thread on disk that startup discovery re-imports as a phantom, unlinked top-level session (no `parentDelegationId`). The common fast-reap paths are suppressed; only this narrow window remains.

Two narrow windows remain. On a `thread/start` timeout
(`wait_for_codex_json_rpc_response` returns `Err`), TermAl never learns the thread
id, so it cannot suppress rediscovery. On a persist failure
(`set_external_session_id_if_runtime_matches` returns `Err`), the thread id is
known, but suppression is skipped because it would need the same `commit_locked`
persist that just failed. In either case, if the Codex app-server already wrote
the thread to disk, `import_discovered_codex_threads` re-imports it as a visible
session on the next startup (the match key is `external_session_id == thread.id`,
which is `None` here), and parent-link repair cannot hide it: no delegation record
maps it and the imported row has no marker message.

**Current behavior:**
- A thread created just before a `thread/start` response error/timeout is not
  suppressed and can be re-imported on the next boot.
- Isolated-worktree delegations are unaffected — their threads live outside the
  discovery scopes.

**Proposal:**
- On the persist-failure path the thread id is already known, so a best-effort
  suppress attempt there is cheap incremental hardening (a transient persist
  failure may also fail the suppress persist, so it is not a full fix).
- Record the pending `thread/start` request so the kill/detach path can reconcile
  and ignore the created thread even when the response never yields a usable id.
- For a durable, timing-independent fix, give read-only/reviewer delegate children
  a Codex home outside the discovery scopes (as isolated-worktree children already
  have), so their threads are never rediscovered.

## Claude read-only reviewer delegations cannot run read-only git commands

**Severity:** Medium - `/review-local` Claude delegations can fail before review because the read-only policy denies the git commands required to collect the diff.

A read-only Claude reviewer delegation for `/review-local` was unable to run
`git status`, `git diff`, `git diff --cached`, or `git ls-files` because TermAl
denied all terminal/shell execution under the delegation's read-only policy. The
review command explicitly permits read-only git inspection, but the enforced
tool policy blocks the child before it can discover staged, unstaged, or
untracked changes.

**Current behavior:**
- Claude `/review-local` delegations with `writePolicy: readOnly` can receive
  `TermAl denied this tool request because this Claude reviewer delegation is
  read-only.` for read-only shell/git commands.
- The child cannot reconstruct the worktree diff reliably from file-read tools
  alone and returns a failed result packet.
- `/review-with-delegate` can lose the Claude reviewer while the Codex reviewer
  still completes.

**Proposal:**
- Allow a narrow read-only command set for reviewer delegations, including
  `git status`, `git diff`, `git diff --cached`, `git ls-files`, and `git show`,
  while continuing to block mutating commands.
- Or have the parent capture and pass the required diff/status/untracked outputs
  into read-only reviewer prompts so delegated children do not need shell access
  to start review.

## Running delegation status poll mutates the parent card revision

**Severity:** Medium - `cargo test --bin termal` is red because a no-op delegation status poll advances state revision unexpectedly.

`tests::delegations::delegation_status_and_unavailable_result_poll_preserve_revision_without_refresh`
fails in isolation: after creating a running read-only delegation, polling its
status changes the revision from `3` to `4` even though the delegation remains
running and no result is available. The mutation appears to come from parent
parallel-agent card detail refresh: the creation path writes a detailed
running row, while the status refresh path wants the generic
`Delegated session is running.` detail.

**Current behavior:**
- `cargo test --bin termal` fails on the exact delegation revision assertion.
- Polling a still-running delegation can persist an otherwise no-op parent-card
  detail refresh.
- Result polling for an unavailable result is intended to return `409` without
  changing revision after any real refresh work has already been accounted for.

**Proposal:**
- Align the created running parent-card detail with the no-op refresh matcher,
  or teach the refresh matcher to treat the initial running detail as already
  current while still replacing pending-interaction details after they resolve.
- Keep the unavailable-result path covered so a running delegation result poll
  does not advance revision when there is no lifecycle, detach, or wait change.

## Windows read-only Codex delegations run with full-access sandboxing

**Severity:** High - Windows Codex reviewer delegations marked read-only can still write through the Codex process sandbox and gain network egress.

`src/delegations.rs` maps read-only Codex delegations to `CodexSandboxMode::DangerFullAccess` on Windows to avoid the current `windows sandbox: spawn setup refresh` failure. That makes the delegation usable, but it also means the child agent's own shell/apply_patch tools are no longer sandbox-enforced as read-only. TermAl's mediated file API can still reject writes, but the child process sandbox is broader than the documented "enforced isolation" contract.

**Current behavior:**
- Windows Codex delegations created with `writePolicy: readOnly` use `danger-full-access`.
- The delegated child can use Codex-owned tools with full filesystem access and network access despite the read-only policy.
- Project docs still describe read-only delegation isolation as enforced rather than prompt-only.

**Proposal:**
- Prefer a narrower Windows fallback if it avoids the Codex sandbox setup failure, or reject Windows Codex read-only delegation with a clear platform error.
- If a temporary full-access fallback remains, surface the reduced enforcement in the UI and update the delegation architecture docs.
- Track restoring true read-only sandboxing once the upstream Windows Codex sandbox issue is fixed.

## Implementation Tasks

- [ ] P2: Extract delegation child-interaction detail helpers:
  move child pending-interaction detail synthesis and parent-card refresh
  helpers out of `src/delegations.rs` into a focused delegation status module.
- [ ] P2: Resolve `flow.md` placement:
  decide whether the untracked review-flow note belongs under `docs/`, should
  be added to an ignore list, or should remain local-only outside version
  control.
- [ ] P2: Keep Telegram prompt-target selectors in parity:
  add a shared predicate or parity coverage if `latest_project_prompt_target_session`
  and `find_latest_telegram_project_prompt_session` diverge beyond their current
  `SessionRecord` versus `/api/state` projection split.
- [ ] P2: Extract Telegram prompt-target forwarding tests:
  move the delegated/error/unknown prompt-target resolution cluster out of
  `src/tests/telegram_forwarding.rs` before that test file crosses the active
  size threshold.
- [ ] P2: Extract delegation result parsing and synthesis helpers:
  move the cohesive result-packet parsing, plain-output synthesis, findings
  parsing, and summary compaction cluster out of `src/delegations.rs` so future
  delegation result changes land in a focused module.
- [ ] P2: Extract workspace session-reference helpers:
  move session-reference collection, delegated-child reference detection, and
  adjacent reconciliation helpers out of `ui/src/workspace.ts` so workspace tree
  utilities stay below the active size threshold.
- [ ] P2: Cover orphan-thread suppression end-to-end:
  add a test that drives the shared-Codex setup waiter with the AppState record
  removed (and the detach-first ordering) so it proves `suppress_orphaned_codex_thread`
  actually fires on the waiter path. The current
  `suppress_orphaned_codex_thread_blocks_discovery_reimport` test only exercises
  the helper in isolation, not the `src/codex.rs` waiter branch it guards.
- [ ] P3: Use a lighter persist for ignore-set-only mutations:
  `suppress_orphaned_codex_thread` (and its sibling
  `clear_external_session_id_if_runtime_matches`) commit via `commit_locked`, which
  bumps the state revision and broadcasts SSE for a client-invisible
  `ignored_discovered_codex_thread_ids` change; a persist-without-revision path would
  avoid the needless client refresh.
