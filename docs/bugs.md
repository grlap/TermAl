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

## Claude turns finalize while background Task subagents are still running

**Severity:** Medium - a Claude session reports idle, and a delegated reviewer returns a truncated result packet, while its own work is still in flight.

`src/claude_spawn.rs` treats a `{"type":"result"}` stream message as end-of-turn.
Claude Code now runs `Task` subagents in the background, so `result` can arrive
while a subagent (or a command it is waiting on) is still running, and TermAl
finalizes the turn early.

**Current behavior:**
- The session shows idle while the agent is still working.
- Reviewer delegations can return a `## Result` packet whose own summary admits the
  work is unfinished. Live repro: child `session-3301` returned `Findings: - None`
  under the summary "The Rust suite is still compiling in the background. I'll
  consolidate and deliver the final review packet as soon as it completes." A
  `Findings: None` from a truncated reviewer must not be read as a clean review.

**Proposal:**
- Track outstanding `Task`/tool-use ids for the turn and finalize only when a
  `result` arrives with none in flight.
- Until fixed, treat a delegated reviewer packet whose summary admits pending work
  as failed rather than clean.

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

## Windows `cargo test` reads and overwrites the developer's real `~/.termal`

**Severity:** Medium - on Windows dev machines with `HOME` set (e.g. Git Bash / MSYS), a normal `cargo test --bin termal` run escapes its temp sandbox, turns the telegram suite red, and clobbers the user's real `~/.termal/*` files.

`resolve_home_dir()` (`src/codex_home.rs:44`) resolves `HOME` first and only falls
back to `USERPROFILE`. The test harness overrides home via `TEST_HOME_ENV_KEY`,
which is `USERPROFILE` on Windows (`src/tests/mod.rs:691`). When `HOME` is also set
(the default in Git Bash), the override is ineffective, so home-relative paths such
as `telegram_bot_file_path()` resolve to the real `~/.termal`.

**Current behavior:**
- `telegram_config_update_keyring_write_failure_does_not_persist_plaintext_token`
  (`src/tests/telegram.rs:2547`) fails `!path.exists()` because a real
  `~/.termal/telegram-bot.json` exists; it panics while holding
  `TEST_HOME_ENV_MUTEX`, poisoning it and cascading ~30 telegram tests into
  `PoisonError` failures.
- Successful telegram tests write to the real path: the real
  `~/.termal/telegram-bot.json` was observed holding the test fixture
  (`chatId: 123`, `configMigratedToAppState: true`) — i.e. a plain `cargo test`
  overwrote the developer's real Telegram config. No secret leaks (the token
  lives in the OS credential store, not the file).

**Proposal:**
- Make `resolve_home_dir()` honor the harness override deterministically in test
  builds (consult `TEST_HOME_ENV_KEY` before `HOME`), or have the harness override
  `HOME` in addition to `USERPROFILE` on Windows.
- Point the harness at a per-test temp home so one fixture assertion cannot poison
  `TEST_HOME_ENV_MUTEX` and cascade the suite.

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

- [ ] P2: Re-enable read-only reviewer `git -C` / `--git-dir` behind env hardening:
  the repository-retargeting global options were removed because only the literal
  `.git` name is un-committable, so a reviewed change can track a bare-repo fixture
  and `git --git-dir=fixture.git --work-tree=. status` executes that config's
  `core.fsmonitor`. To restore them, have TermAl neutralize the exec sinks in the
  reviewer child's git environment (e.g. `GIT_CONFIG_COUNT` with `diff.external=`,
  `core.fsmonitor=`, `core.pager=cat`), since the checker only approves the agent's
  verbatim command and cannot rewrite it. `GIT_CONFIG_NOSYSTEM=1` does not help: it
  disables only the system config.
- [ ] P2: Collapse the four Codex reasoning-effort string parsers:
  `parse_codex_reasoning_effort` (`src/runtime.rs`),
  `parse_discovered_codex_reasoning_effort` (`src/codex_discovery.rs`),
  `codex_reasoning_effort_from_json_value` (`src/state.rs`), and the
  `TERMAL_CODEX_REASONING_EFFORT` match (`src/turns.rs`) each hand-maintain the same
  string mapping and all end in a silent drop. A new Codex level is dropped rather
  than rejected, which is how `max`/`ultra` went missing. Route them through a single
  `CodexReasoningEffort::from_api_value`. The runtime/model-list path now has a
  string-parse test (`codex_model_options_parses_max_and_ultra_reasoning_levels`);
  the discovery/state/env parsers still lack direct coverage, so unify them and add a
  shared parser test as part of the collapse.
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
