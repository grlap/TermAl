# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## `SAFE_DELEGATION_STATUS_FETCH_MESSAGES` allow-list over-redacts the `formatUnavailableApiMessage` family

**Severity:** Medium - a benign, useful `kind: "backend-unavailable"` diagnostic is silently collapsed to the generic packet message, losing the "restart TermAl" signal that distinguishes a stale backend from a transient 5xx.

`ui/src/delegation-commands.ts:849-858`. The new allow-list inversion is a sound direction (closes the round-45 deny-list gap), but it's too narrow: `api.ts:1649-1653 formatUnavailableApiMessage()` builds messages like `"The running backend does not expose /api/sessions/.../delegations/... (HTTP 404). Restart TermAl so the latest API routes are loaded."` via `createBackendUnavailableError` (the same constructor as the allowed `"The TermAl backend is unavailable."`). These messages contain the request path including the parent session id and delegation id — both of which are caller-supplied, already present elsewhere in the packet (`delegationId`, mismatched-id packets), AND carry the load-bearing `restartRequired: true` signal. The current redaction strips the human-readable diagnostic to the generic `"Delegation status fetch failed."` even though the structured `apiErrorKind` and `restartRequired` fields are still preserved on the packet — the wrapper UI loses the message and only knows "something failed at status N".

**Current behavior:**
- Both `"The TermAl backend is unavailable."` and the longer `"The running backend does not expose ..."` are produced by `kind: "backend-unavailable"` ApiRequestErrors.
- Only the short message is allow-listed; the long message gets the generic redaction.
- Wrappers see the generic packet and have to reconstruct the restart-required diagnostic from `apiErrorKind`/`status` alone.

**Proposal:**
- Either extend the allow-list with a pattern keyed off `"The running backend does not expose"` (or a stricter shape match including the HTTP status component).
- Or redact by `apiErrorKind`: pass through verbatim when `apiErrorKind === "backend-unavailable"` (which only emits two well-known message families, neither of which contains user-supplied secrets), and apply allow-list redaction only to `apiErrorKind === "request-failed"` where backend payloads can be arbitrary.

## `WaitDelegationErrorPacket` field shape varies by branch but the doc shape doesn't reflect

**Severity:** Low - MCP wrappers writing typed projections cannot tell from the doc whether `apiErrorKind`/`status`/`restartRequired` are always present.

`ui/src/delegation-commands.ts:828-846` and `docs/features/agent-delegation-sessions.md`. `apiErrorKind`, `status`, `restartRequired` are populated only when the underlying error is `ApiRequestError` (line 828-840 branch). The `else` branch (line 841-846) sets `name = error.name` for arbitrary thrown values without those fields. Wrapper authors who key off `name === "ApiRequestError"` to predict field presence will be surprised by the unprefixed branch shape.

**Current behavior:**
- `apiErrorKind`/`status`/`restartRequired` only present in `ApiRequestError` branch.
- `else` branch ships `kind: "status-fetch-failed"` + `name` only.
- Doc shape doesn't distinguish the two cases.

**Proposal:**
- Either narrow the union in the doc shape (`WaitDelegationErrorPacket = ApiErrorPacket | UnknownErrorPacket`).
- Or always populate `apiErrorKind: null, status: null, restartRequired: null` for non-`ApiRequestError` branches so the field set is uniform.

## `agent` and `status` typed as bare `string` in `agent-delegation-sessions.md` but typed enums in source

**Severity:** Low - MCP wrapper authors generating a TS shim from the brief land on a strictly-broader type than the runtime contract.

`docs/features/agent-delegation-sessions.md:373-386`. The brief documents `agent: string` and `status: string`. `ui/src/delegation-commands.ts:84-92` declares them as `Session["agent"]` and `Session["status"]` (typed enums). Consumers that downstream `switch` on the value will not get exhaustiveness narrowing, and a future enum addition will not surface in wrapper schemas.

**Current behavior:**
- Doc types `agent`/`status` as bare `string`.
- TS source uses typed enum unions.
- Wrappers built from the doc are strictly broader than runtime.

**Proposal:**
- Inline the agent/status enum unions in the doc (e.g., `agent: "Codex" | "Claude" | "Gemini" | ...`).
- Or add a one-line note pointing readers at the canonical TS source for the precise enum.

## `delegationSummary` always-present `result: null` is a wire-shape change vs prior optional omission

**Severity:** Low - same nominal type, slightly different wire JSON; downstream `result === undefined` checks would silently flip.

`ui/src/delegation-commands.ts:558-577`. Round-46 changed `delegationSummary` to set `result: null` unconditionally before the `if (record.result)` block (then conditionally overwriting), in support of the new `expectOwnNullableStringProperty` shape pinning. The doc shape `result?: DelegationResultSummary | null` allows both `undefined` (key omitted) and `null` (key present), so this is not a contract break — but downstream `result === undefined` checks (none currently in-tree, but plausible in a future MCP wrapper) would silently break.

**Current behavior:**
- `summary.result` is always own-property-present, either `null` or `DelegationResultSummary`.
- Previous shape could omit the key entirely.
- `JSON.stringify(summary)` now emits `"result": null` instead of omitting.

**Proposal:**
- Either keep the prior shape (omit when absent) and tighten the test instead via `toHaveProperty`.
- Or update the doc to commit to the always-present-as-null shape so wrappers don't have to test both.

## `conversationMarkerColorsMatchForState` is reconciler policy in a colors module

**Severity:** Low - state-comparison helper lives in a module whose name suggests render-time concerns.

`ui/src/conversation-marker-colors.ts:23-33`. Both call sites of `conversationMarkerColorsMatchForState` (`app-session-actions.ts:345`, `session-reconcile.ts:263`) are state-equality decisions for marker reconciliation. Render-path consumers (`ConversationOverviewRail.tsx`, `panels/conversation-markers.tsx`) only use `normalizeConversationMarkerColor`. The colors module now mixes "what color to paint" and "are these two markers logically the same for state-reuse decisions" — the canonical-equality + reference-fallback rule is reconciliation policy, not color policy.

**Current behavior:**
- Module exposes 1 constant + 3 functions (`canonical…`, `normalize…`, `…MatchForState`).
- Two callers use the state-matcher; two use the normalizer.
- Module-name implies presentation-only concerns.

**Proposal:**
- Either keep the helper here and document the colocation rationale in the file header (canonical equality is owned here so callers don't reimplement the three-level model).
- Or move `conversationMarkerColorsMatchForState` to a small `marker-state-equality.ts` (or co-locate with one of the two consumers) and import only the canonical primitive.

## `agent-delegation-sessions.md` lacks an implementation cross-link to `delegation-commands.ts`

**Severity:** Low - the brief embeds source-level type definitions without naming the source.

`docs/features/agent-delegation-sessions.md:222, 375-393`. The doc now matches the implementation accurately for `SpawnDelegationCommandResult` and `DelegationChildSessionSummary`, but never references `ui/src/delegation-commands.ts`. The "Related" block at the top references peer feature docs but never points to the actual TypeScript module that owns the command surface. CLAUDE.md says "Cross-link them both ways when a new doc references an existing one"; embedding type definitions without naming the source is exactly the drift vector that one-line cross-references prevent.

**Current behavior:**
- The brief embeds `SpawnDelegationCommandResult`/`DelegationChildSessionSummary` definitions inline.
- No reference to `ui/src/delegation-commands.ts`.
- Only the directional `// Keep in sync with src/delegations.rs` constant comment provides any source pointer.

**Proposal:**
- Add an "Implementation: `ui/src/delegation-commands.ts`" line to the §Internal Commands or §Data Model section.
- Or add a one-line note next to the `SpawnDelegationCommandResult` definition citing where it lives in source.

## `agent-delegation-sessions.md` internal-command return types still use pre-packet shapes

**Severity:** Low - MCP wrapper authors following the feature brief miss the revision, server-instance, and redacted-summary packet fields that the implemented command surface now returns.

`docs/features/agent-delegation-sessions.md:221-226`. The internal-command block still documents `get_delegation_status(...) -> DelegationStatus`, `get_delegation_result(...) -> DelegationResult`, and `cancel_delegation(...) -> DelegationStatus`. The implementation returns packet wrappers: `DelegationStatusCommandResult` for status/cancel and `DelegationResultPacket` for result. Those wrappers carry `revision`, `serverInstanceId`, and the redacted summary/result projection used by the current UI and wrapper contract.

**Current behavior:**
- Spawn is documented as returning `SpawnDelegationCommandResult`.
- Status/cancel/result are documented as returning raw/simple delegation objects.
- Wrapper authors reading the doc miss packet fields and may build a narrower contract than runtime.

**Proposal:**
- Update the command block to list `DelegationStatusCommandResult` and `DelegationResultPacket`.
- Add the missing packet shapes alongside `SpawnDelegationCommandResult`, including `revision`, `serverInstanceId`, and summary redaction details.

## `conversation-overview-map.ts` cap-vs-fast-path ordering change lacks rationale comment

**Severity:** Low - a 10-line refactor that flips homogeneous-run merging from "unbounded" to "capped at `maxItemsPerSegment`" without an inline note.

`ui/src/panels/conversation-overview-map.ts:421-430`. The change moves the `itemCount > maxItemsPerSegment` check above the `sameVisualClass` fast-path return. Semantically: previously same-class items merged unboundedly; now cap is dominant. The new test pins 45 same-class assistant messages → [20, 20, 5]. A reader sees the new ordering with no comment explaining "we deliberately let the cap dominate the same-class fast path so dense homogeneous regions get visual chunking." If a future caller passes `maxItemsPerSegment: Infinity` to recover the prior unbounded behavior, the escape hatch should be visible from source.

**Current behavior:**
- Cap check now dominates the same-visual-class fast path.
- Behavior change framed as "minor refactor" in the ledger churn.
- No inline comment explains the dominance.

**Proposal:**
- Add a 2-3 line comment above `canMergeConversationOverviewSegmentItem` (or above the relocated cap check) explaining the cap-dominates-fast-path policy and citing the test as the contract pin.

## `SpawnDelegationCommandResult` declared before `DelegationChildSessionSummary` in `delegation-commands.ts`

**Severity:** Low - readers scanning top-to-bottom encounter `childSession: DelegationChildSessionSummary` before learning what fields it carries.

`ui/src/delegation-commands.ts:85-102`. `SpawnDelegationCommandResult` references `DelegationChildSessionSummary` on line 89 but the type is declared on line 94, after the consumer. TypeScript hoists types so this compiles, but other public types in the file (`DelegationStatusCommandResult`, `DelegationResultPacket`) follow declaration-then-use ordering. Pure ordering hygiene fix — readers most likely to read this section are MCP wrapper authors.

**Current behavior:**
- `SpawnDelegationCommandResult` references `DelegationChildSessionSummary` before declaration.
- TS hoisting makes this compile.
- Inconsistent with the file's other public-type ordering.

**Proposal:**
- Hoist `DelegationChildSessionSummary` above `SpawnDelegationCommandResult`. Pure ordering move, no semantic change.

## Explicit non-default delegation `model` preservation branch is not pinned by any positive test

**Severity:** Low - the third meaningful branch of `(agent, model)` selection is not exercised by any positive assertion.

`src/tests/delegations.rs`. The new `delegation_omitted_agent_and_model_use_parent_agent_default_model` (round 46) and the round-45 `delegation_omitted_model_uses_selected_agent_default_not_parent_model` together pin `agent: None, model: None` (parent default) and `agent: Some(Codex), model: None` (selected default). The third branch — `agent: Some(Agent), model: Some("custom-model-string")` flowing verbatim into `child_session.model` and `delegation.model` — is exercised only obliquely by `delegation_empty_model_uses_agent_default` (which tests the whitespace branch, not the verbatim-string branch) and a cancellation test that happens to pass the agent's own default. A refactor that accidentally always defaulted (e.g., `Some(agent.default_model())` substituting the request unconditionally) would not regress current tests.

**Current behavior:**
- Two of three meaningful `(agent, model)` branches pinned.
- The verbatim non-default model preservation branch is not pinned.
- A regression that always defaulted would silently pass.

**Proposal:**
- Add a positive test `delegation_explicit_model_is_preserved_verbatim` passing `agent: Some(Codex), model: Some("custom-model-string")` and asserting both `created.child_session.model == "custom-model-string"` and `created.delegation.model.as_deref() == Some("custom-model-string")`.

## `MAX_DELEGATION_PROMPT_BYTES` cross-reference comment lacks parity-test mention

**Severity:** Low - the round-46 partial fix mentions only the file name, not the parity-pin test that closes the verification loop.

`src/delegations.rs:8-9` and `ui/src/delegation-commands.ts:35`. The new `// Keep in sync with` comments name the mirror file but do not reference the parity-pin test (`delegation-commands.test.ts`'s `expect(MAX_DELEGATION_PROMPT_BYTES).toBe(64 * 1024)`). A future contributor changing one side must (a) discover the mirror, then (b) discover and update the parity test. The round-45 finding "no mechanism enforces backend parity" is half-closed.

**Current behavior:**
- Both sides have one-line cross-reference comments.
- Neither comment names the parity test.
- A one-sided edit that updates the value but forgets the test would silently pass on each side independently.

**Proposal:**
- Expand the comment on both sides to include "and update the parity-pin test in `delegation-commands.test.ts`".
- Or add a backend-side test that re-asserts the literal value (mirroring the UI-side assertion) so a one-sided edit fails CI immediately.

## `conversation-marker-colors.test.ts` missing `url(a)` vs `url(b)` arm

**Severity:** Low - the fall-through `left === right` path with two distinct invalid strings is the most likely refactor target and is not pinned.

`ui/src/conversation-marker-colors.test.ts:38-54`. The new test arms cover canonical-canonical match, invalid+default mismatch, and identical-invalid match (`url(...) === url(...)`), but do not cover the most fragile case: two distinct invalid strings (e.g., `url(a)` vs `url(b)`). A future refactor that flipped the fall-through to "always treat invalid pair as match" or "always treat invalid pair as mismatch" would pass the existing three arms.

**Current behavior:**
- Three arms cover the canonical paths and the identical-invalid path.
- Distinct-invalid path is not pinned.

**Proposal:**
- Add `expect(matchForState("url(a)", "url(b)")).toBe(false)` arm.
- Consider an arm for non-string + invalid-string (e.g., `null` vs `"url(...)"`).

## `caps homogeneous visual segments` test missing exact-cap and `cap=1` boundary cases

**Severity:** Low - 45 / 20 = 2.25 segments masks `>` vs `>=` discrimination.

`ui/src/panels/conversation-overview-map.test.ts:512-569`. The new test exercises 45 messages with `maxItemsPerSegment: 20`, expecting [20, 20, 5]. A regression that set `itemCount >= maxItemsPerSegment` (off-by-one) would still produce a viable split because 45 isn't a multiple of 19. Two notable boundaries are not pinned: (a) `itemCount === maxItemsPerSegment` exactly (e.g., 20 messages, expect `[20]` not `[19, 1]`); (b) `maxItemsPerSegment: 1` (each item as its own segment).

**Current behavior:**
- Single test arm with non-boundary values.
- Off-by-one regressions in the comparator pass.

**Proposal:**
- Add an arm with exactly `maxItemsPerSegment` items expecting a single segment.
- Add an arm with `maxItemsPerSegment: 1` expecting one segment per item.

## `expectOwnNullableStringProperty` doesn't pin per-field shape

**Severity:** Low - accepts both `null` and any `string` for any field; a numeric model id would still pass.

`ui/src/delegation-commands.test.ts:125-133`. The helper closes the round-45 "presence" gap (good) but leaves a "shape" gap: `typeof "x" === "string"` checks the runtime type of whatever the spread produced, not the intended schema. A future refactor that swapped `model: record.model ?? null` for a numeric model id (e.g., `model: record.modelId`) would still pass.

**Current behavior:**
- Helper checks own-property presence + `null | string` shape, but is field-agnostic.
- Per-field shape constraints (e.g., `model: string`, `startedAt: ISO timestamp`) are not enforced.

**Proposal:**
- Per-field helpers with explicit `expect.toBeOneOf([expect.any(String), null])` matchers.
- Or accept a type validator parameter (`expectOwnPropertyOfShape(record, "model", v => v === null || typeof v === "string")`).

## Sensitive-`ApiRequestError` `it.each` missing JWT and GitHub PAT shapes

**Severity:** Low - the allow-list is now load-bearing; a regression to a regex-based blocklist would silently regress these.

`ui/src/delegation-commands.test.ts:1131-1171`. Seven sensitive shapes covered: token assignment, bearer token, env var, raw token prefix (`sk-proj-`), UNC path, home path, URL. Missing: JWT-style tokens (`eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.signature`) and GitHub PATs (`X-GitHub-Token: ghp_...`). These are different in shape from the listed `sk-proj-` prefix and would not match a future regex-based blocklist's existing `sk-` prefix arm.

**Current behavior:**
- Seven sensitive shapes pinned.
- JWT and GitHub PAT shapes implicitly redacted by allow-list-default-redact, but not pinned.

**Proposal:**
- Add `["JWT token", "Authorization: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.signature"]` and `["GitHub PAT", "X-GitHub-Token: ghp_someTokenValue123"]` arms.

## Batch wait failure test invokes captured callbacks through optional chaining

**Severity:** Low - a request-ordering regression would fail late or misleadingly instead of proving the expected dispatch sequence.

`ui/src/delegation-commands.test.ts:984-993`. The batch failure test captures `resolveFirst` and `rejectSecond`, then invokes them with `resolveFirst?.(...)` / `rejectSecond?.(...)`. If the transport stops dispatching requests in the expected order, the optional calls become no-ops and the test failure points at the eventual wait outcome rather than the missing callback capture. The same test casts `thirdSignal` at the assertion site instead of using the local abort-signal assertion helper.

**Current behavior:**
- Missing first/second callback captures are tolerated until later assertions time out or mismatch.
- The third request abort signal is asserted through a local cast rather than the shared helper.
- The test is less diagnostic than the contract it is trying to pin.

**Proposal:**
- Assert `resolveFirst` and `rejectSecond` are defined before invoking them.
- Reuse `expectCapturedAbortSignal(thirdSignal)` for the third request signal.

## `app-session-actions.test.ts` casing-only marker test missing genuine-mismatch sibling

**Severity:** Low - a regression to `matchForState(current.color, current.color)` would still pass casing-only inputs.

`ui/src/app-session-actions.test.ts:817-867`. The "casing-only differs" test is good, but the symmetric inverse — `local: "#3b82f6"`, response: `"#3B82F6"` — and the genuine mismatch case — different valid hex like `#EF4444` vs `#3B82F6` — are not covered. The change at `app-session-actions.ts:345-346` is `conversationMarkerColorsMatchForState(current.color, response.color)`. A regression to `matchForState(current.color, current.color)` would silently pass the casing-only inputs.

**Current behavior:**
- One test arm covers `#EF4444` vs `#ef4444` (case-only).
- No arm covers different valid hex (genuine mismatch).
- Argument-order regressions silently pass.

**Proposal:**
- Add a sibling test where local and response pin different valid-but-not-matching hex (e.g., `#EF4444` vs `#3B82F6`), asserting the same-instance fast-path is NOT taken.

## Delegation child session `name` set verbatim from caller-supplied `title` with no length cap

**Severity:** Note - the round-46 redaction trusts that `name` is metadata; a sloppy MCP wrapper that sets `title = prompt.slice(0, N)` would round-trip prompt content through the redacted summary.

`src/delegations.rs:292-298` (and `src/state_inner.rs:130`). `request.title` flows verbatim into `child_session.name`, which is now exposed via `DelegationChildSessionSummary` to MCP/tool callers. Today the only producer is the caller's `title`, never auto-derived from the prompt server-side, and no codepath under `src/` rewrites `session.name` from prompt content. So a benign caller cannot accidentally leak prompt text. But there is no server-side maximum length on `title`, while the prompt is capped at 64 KiB — a wrapper could push thousands of bytes of prompt text through `title` past the redaction. Functionally this matches the round-45 contract (callers chose what to put in `title`), so it is not a regression — but the redaction's safety assumption ("`name` is metadata") now becomes a wrapper-side responsibility rather than a system invariant.

**Current behavior:**
- `request.title` flows verbatim into `child_session.name` with no length cap.
- No server-side validation that `name` excludes prompt content.
- Prompt is capped at 64 KiB; `title` is unbounded.

**Proposal:**
- Add a server-side `MAX_DELEGATION_TITLE_CHARS` cap mirroring the existing `MAX_DELEGATION_PUBLIC_SUMMARY_CHARS` pattern.
- Reject overlong titles at the API layer with parity comment to the UI side.

## Wait-loop limits still independently pinned without parity comments

**Severity:** Note - `MAX_DELEGATION_WAIT_IDS`, `MIN_DELEGATION_WAIT_INTERVAL_MS`, `MAX_DELEGATION_WAIT_TIMEOUT_MS` lack the parity comments that `MAX_DELEGATION_PROMPT_BYTES` got in this round.

`ui/src/delegation-commands.ts:32-34`. Round-46 closed half of the prior parity-drift concern by adding `// Keep in sync with src/delegations.rs` for the prompt-byte cap, but the wait-loop limits are still independently pinned on each side and could drift. Not a confidentiality/integrity issue (these are availability limits) but the partial fix leaves a mixed signal about which constants are parity-critical.

**Current behavior:**
- `MAX_DELEGATION_PROMPT_BYTES` has bidirectional parity comments.
- `MAX_DELEGATION_WAIT_IDS`/`MIN_INTERVAL_MS`/`MAX_TIMEOUT_MS` do not.
- A reader sees mixed convention.

**Proposal:**
- Either extend parity comments to the wait-loop constants too.
- Or note explicitly that they intentionally do not need parity (the backend enforces the truth and the UI cap is a defensive client-side floor).

## `SAFE_DELEGATION_STATUS_FETCH_MESSAGES` allow-list audit boundary undocumented

**Severity:** Note - the set is now load-bearing; adding a new entry should be a deliberate audit step.

`ui/src/delegation-commands.ts:38-44`. The allow-list approach is the right policy for security-by-default, but adding a new entry needs a security review of every call site that constructs an `ApiRequestError` with that message. No comment captures this contract.

**Current behavior:**
- Allow-list has 1 literal + 1 pattern.
- Adding a new entry has no documented audit checklist.
- Future contributors might casually expand the set.

**Proposal:**
- One-line comment above `SAFE_DELEGATION_STATUS_FETCH_MESSAGES` explaining that the set is the audit boundary for status-fetch messages forwarded to wrapper callers, and that adding a new entry requires a security review of every callsite that constructs an `ApiRequestError` with that message.

## `conversationMarkerColorsMatchForState` three-mode semantics pinned only by test, not docstring

**Severity:** Note - the three deliberately-distinct cases (both canonical / mixed / both invalid) are not documented in source.

`ui/src/conversation-marker-colors.ts:23-33`. The helper has three meaningful behaviors: (1) both canonical → canonical equality, (2) one canonical and one not → false (forces resync to authoritative), (3) neither canonical → raw `===` equality (treat identical garbage as same state). These are pinned by tests in `conversation-marker-colors.test.ts` and `session-reconcile.test.ts`. Future readers / refactorers won't see the rationale unless they read both test files.

**Current behavior:**
- Three-mode behavior is intentional but not documented at source.
- A future "fix" that always returned false for invalid sides would break the reconciler-policy contract without obvious source-level pushback.

**Proposal:**
- Add a JSDoc to `conversationMarkerColorsMatchForState` describing the three cases.

## `DeltaApplyResult.kind === "applied"` no longer guarantees a fresh sessions array

**Severity:** Medium - the type contract that callers historically relied on ("applied implies new array") was silently weakened by the textReplace no-op short-circuit; only one of three production callers has been retrofitted to know about the new semantics.

`ui/src/live-updates.ts:568-589, 109-122`. The new no-op path inside the `textReplace` arm of `applyDeltaToSessions` returns the original `sessions` array reference (preserving identity) when the delta's `text`/`preview`/`sessionMutationStamp`/`messageCount` all match the session. The single caller at `ui/src/app-live-state.ts:2629-2634` was updated to detect this via `result.sessions !== sessionsRef.current`, but the type definition still describes `kind: "applied"` as the "mutation happened" signal. Two other production callers (`app-live-state.ts:2736`, `:2838`) still treat any `"applied"` as material and unconditionally re-render / mark transport activity. A future caller (or a future refactor that adds an identity-preserving fast path to other delta arms) will silently reintroduce the watchdog-masking bug because the contract change lives only in the one consumer that knows about it, not in the producer's type.

**Current behavior:**
- `applyDeltaToSessions` in the `textReplace` arm can return `{ kind: "applied", sessions: input }` (identity-preserved) when nothing actually changed.
- `DeltaApplyResult` (line 109-122) declares `applied`/`appliedNeedsResync`/`needsResync` without distinguishing material apply from no-op.
- The same-revision-replayable branch in `app-live-state.ts:2629` reads `result.sessions !== sessionsRef.current` to gate transport-activity / watchdog marking; other callers don't.

**Proposal:**
- Encode the no-op signal at the type level: introduce a `kind: "appliedNoOp"` variant (or `materialApply: boolean` flag) so the contract is explicit, and update every consumer's exhaustive switch / branch.
- Alternatively (lower-friction): add a JSDoc note on `DeltaApplyResult` documenting the identity-preservation invariant, plus paired comments at every "applied" return site so a future maintainer who adds a similar short-circuit knows to keep the consumers' identity-equality check in sync.

## Same-revision-replayable no-op short-circuit covers only `textReplace`, leaving `messageUpdated` / `commandUpdate` / `parallelAgentsUpdate` watchdog-maskable

**Severity:** Medium - the watchdog-mask fix landed for textReplace but not for the other replayable session-delta types; a server re-emitting an identical-content `commandUpdate` (or any non-textReplace replayable delta) at the same revision still resets the watchdog.

`ui/src/live-updates.ts:568-589` (textReplace short-circuit) versus `messageUpdated` (line 432), `commandUpdate` (line 613), `parallelAgentsUpdate` (line 672), `conversationMarkerCreated/Updated/Deleted` (lines 726, 750). `isSameRevisionReplayableSessionDelta` (line 211) excludes only `textDelta`, so all of the above flow through the same `revisionAction === "ignore"` + replayable branch in `app-live-state.ts:2629`. They unconditionally call `replaceSession`, producing a fresh `sessions` array reference even when content matches. The call site's `result.sessions !== sessionsRef.current` check classifies them as material applies, marks `markLiveTransportActivity` + `markLiveSessionResumeWatchdogBaseline`, and resets the watchdog. The bug fixed for `textReplace` is still present for the other delta types.

**Current behavior:**
- The textReplace path correctly preserves array identity on no-op.
- All other replayable session-delta arms produce a fresh `sessions` array regardless of content equality.
- The watchdog still gets masked when the server re-emits an identical-content `commandUpdate` / `messageUpdated` / `parallelAgentsUpdate` at the same revision.

**Proposal:**
- Extract a shared helper (e.g., `isContentIdenticalSessionDelta(session, delta)`) that all replayable arms can short-circuit on, OR move the equality detection into the call site (deep-compare relevant fields of the affected session pre/post). The shared helper option is cleaner; the call-site option avoids touching every reducer arm but is more expensive at runtime.
- Add parallel watchdog-mask tests in `App.live-state.deltas.test.tsx` for `commandUpdate`, `messageUpdated`, `parallelAgentsUpdate` mirroring the existing textReplace test at line 2976.

## Same-revision unknown-session resync removed without an explicit protocol-contract reference

**Severity:** Medium - the new fall-through trades a bounded `/api/state` resync for a "next state event will reconcile" assumption; the implicit backend contract that justifies the change isn't documented anywhere, and no test pins the backstop reconciliation path.

`ui/src/app-live-state.ts:2666-2687`. The previous behavior fired `requestStateResync({forceAdoptEqualOrNewerRevision})` + `startSessionHydration(delta.sessionId)` when a same-revision delta arrived for a session the client didn't know about. The new fall-through skips both. The justifying comment claims "the next authoritative state event will reconcile any real divergence" — but this rests on an implicit backend contract that "session creation always advances the revision counter" (currently true: see `src/sse_broadcast.rs` and `src/remote_create_proxies.rs:198-207` which suppress no-change `SessionCreated` deltas). Neither `docs/architecture.md` nor any test pins this contract. A future backend change that emits a same-revision delta for a genuinely new session (e.g., a delegated child session, a remote bridge re-emission, a backfill flow) would silently leave the client out-of-sync until something else triggers reconciliation.

**Current behavior:**
- Same-revision delta with `kind: "needsResync"` (unknown sessionId) falls through to the generic ignored-delta confirmation block.
- No `/api/state` resync is fired.
- The backstop reconciliation path (the next genuine state event or watchdog tick) is not covered by any test for the genuine-divergence case.

**Proposal:**
- Add a one-line protocol-contract note to `docs/architecture.md` §"Real-time Updates" — "session creation always advances the revision counter; same-revision deltas only target sessions the client already knows about" — and cross-link from the comment block at `app-live-state.ts:2666-2687`.
- Add a coverage test that simulates a genuinely-new same-revision session (rare but worth pinning) and asserts the next state event reconciles the divergence within a bounded window.

## Remote marker ingestion bypasses the hex-only color validator

**Severity:** Medium - remote marker snapshots and deltas can still persist and rebroadcast invalid CSS-like color values even though local marker routes reject them.

`src/remote_routes.rs:2008`. Local create/patch paths normalize marker colors through the backend validator, but remote marker localization assigns the incoming `marker.color` directly after only rewriting the `session_id`. The same concern applies to remote marker created/updated delta ingestion: a remote payload can carry `url(...)`, `var(...)`, or other unsupported strings into local state and SSE broadcasts.

**Current behavior:**
- Local marker create/update rejects unsupported color values.
- Remote full-session localization and marker deltas store remote marker colors unchanged.
- Invalid remote marker colors can enter persisted/client-visible state despite the new local-route tests.

**Proposal:**
- Run remote marker colors through the same validator/normalizer used by local marker routes.
- Reject bad remote payloads as bad gateway, or skip/log invalid remote markers consistently.
- Add remote snapshot and remote delta tests with invalid marker colors.

## `conversation-marker-colors.ts` lacks header comment and module-placement rationale

**Severity:** Low - the new module is consumed by five callers but ships without the CLAUDE.md split-file convention header.

`ui/src/conversation-marker-colors.ts:1-15`. CLAUDE.md requires "(1) what it owns, (2) what it deliberately does not own, (3) the file it was split out of (or new-module rationale)". Both touched companions in this round (`panels/conversation-markers.tsx`, `delegation-commands.ts`) follow that pattern; this new module does not. Five call sites (`ConversationOverviewRail.tsx`, `panels/conversation-markers.tsx`, `app-session-actions.ts`, `session-reconcile.ts`, plus the test) depend on the contract: hex-only allow-list, `#3b82f6` fallback, mirrors backend validator. None of that is documented at the source. Module placement (`ui/src/` top-level vs `ui/src/panels/`) is correct (callers span panels/ and non-panels) but the rationale is implicit.

**Current behavior:**
- The new module has no file-level header.
- Top-level placement vs `panels/` is undocumented.
- Callers depend on the implicit "hex-only, with default fallback" contract.

**Proposal:**
- Add a 4-6 line header explaining ownership ("hex-color allow-list + default for marker chip / overview-rail CSS custom properties; mirrors the server-side validator in `src/session_markers.rs:560-565`"), what it does not own (server persistence rejection, cross-marker color comparison policy), and the new-module rationale + top-level placement reason.

## `delegation-commands.ts` size growth is approaching an extraction boundary

**Severity:** Low - the file is at 966 lines (below the §9 ~1500-line threshold) but has gained several distinct command-surface concerns in a short span.

`ui/src/delegation-commands.ts`. Started at ~360 lines at round 41, grew to ~700 at round 43, ~810 at round 44, and now sits at ~966 lines. Newer concern clusters include error-packet sanitization (`safeDelegationStatusFetchMessage` and its allowlist), child/delegation summary redaction, and partial-state preservation (`applyCurrentInstanceStatusBatchResponses`, `singleServerInstanceId`, `newestStatusMetadata`). Two natural extraction boundaries are visible: `delegation-error-packets.ts` (sanitizer + packet builders) and `delegation-wait-loop.ts` (scheduling + batch deadline algebra).

**Current behavior:**
- File hosts spawn validation, transport-id safety, wait-loop scheduling, batched fetch deadline, partial-state preservation, error-message redaction.
- Each new round adds ~150-200 net lines.
- Extraction is still deferred because the file remains below the project split threshold.

**Proposal:**
- Defer extraction until either the §9 threshold is breached or the next material cluster lands.
- If the next delegation batch adds another cluster, split error-packet construction or the wait loop first.

## `useLayoutEffect` calling `ensureMessageSlotCacheForCurrentSession()` is now redundant in `AgentSessionPanel.tsx`

**Severity:** Low - dead code that confuses future readers; functional correctness is unchanged.

`ui/src/panels/AgentSessionPanel.tsx:683-685`. Now that every read site (`jumpToMarker` line 728, `handleConversationItemMount`) routes through `ensureMessageSlotCacheForCurrentSession()` and the helper lazily resets the cache when `messageSlotNodesSessionIdRef.current !== session.id`, the layout effect's eager `ensureMessageSlotCacheForCurrentSession()` call has no observable effect. It only mattered before this round when `jumpToMarker` read `messageSlotNodesRef.current` directly — that path was closed by this round's fix.

**Current behavior:**
- The `useLayoutEffect` calls the helper for its side effect.
- Every read site now self-corrects via the helper.
- The layout effect provides no additional safety.

**Proposal:**
- Remove the `useLayoutEffect` (every read site self-corrects).
- Or add a one-line comment explaining it serves as eager initialization for the next-render baseline (debatable value).

## Backend marker color rejection tests cover only 2 dangerous inputs and use brittle substring assertions

**Severity:** Low - the tests pin the contract for `url(...)` and `var(...)` but don't cover the broader attack surface the validator handles.

`src/tests/conversation_markers.rs:1252-1331`. The validator at `src/session_markers.rs:560-565` is a strict allow-list (`#` prefix + 3/4/6/8 hex digits), so structurally `expression(...)`, `linear-gradient(...)`, `<script>`, named colors (`red`, `transparent`), CSS escapes (`\75 rl(...)`), JS injection attempts, malformed hex with non-hex digits, odd-length hex, empty / whitespace-only are all rejected. Only `url(...)` and `var(...)` are pinned. Assertions check `error.contains("conversation marker color")` (substring) — brittle to wording changes in either direction.

**Current behavior:**
- Two dangerous inputs pinned (POST: `url(...)`, PATCH: `var(...)`).
- Substring assertion: `error.contains("conversation marker color")`.
- A future validator relaxation (e.g., accepting named colors) would not regress the tests because none of those inputs are tested.
- A future error wording change ("marker color must be a hex value") would silently pass even though the contract changed.

**Proposal:**
- Convert to a parameterized loop covering broader attack surface: `expression(...)`, `<script>`, `linear-gradient(...)`, `red`, `transparent`, `#xyz`, `#12345`, `#1234567`, empty.
- Pin the full literal error message (`assert_eq!`) so wording drift fails loudly.
- Or extract the message as a `pub(crate) const` and reference both in handler and test.

## `conversation-marker-colors.test.ts` skips boundary-length and dirty-input cases

**Severity:** Low - 2 `it()` blocks across 8 inputs; a future regex relaxation to `{3,8}` would still pass.

`ui/src/conversation-marker-colors.test.ts:8-30`. The regex enforces 3, 4, 6, or 8 hex digits. The test pins one too-short case (`#12`) and the canonical happy paths but never asserts that 5 (`#12345`) and 7 (`#1234567`) are rejected. Only one whitespace case (leading + trailing) on lowercase 3-digit input. Embedded-newline case absent.

**Current behavior:**
- Boundary lengths 5 and 7 hex digits not exercised.
- Uppercase-with-leading-whitespace not exercised.
- Embedded-whitespace not exercised.

**Proposal:**
- Convert to `it.each(...)` covering: `#12345` rejected, `#1234567` rejected, `"  #ABCDEF  "` normalized, `"\n#abcdef\n"` normalized.
- Add explicit `#FFFFFFFF` test alongside `#fff` to pin both the 8-digit alpha path and the 3-digit short form.

## Marker color sanitization tests use single dangerous-input case in two pin locations

**Severity:** Low - both `ConversationOverviewRail.test.tsx` and `panels/conversation-markers.test.ts` test only `url(...)`.

A regression where the normalizer was inadvertently bypassed for some dangerous values (e.g., a refactor that only handled `var(...)` but not `url(...)`) would only be caught for one path. Mirror the backend test concern: broader input coverage would surface bypasses for any single dangerous shape.

**Current behavior:**
- Both pin tests use `url(https://example.test/marker)`.
- `var(--signal-blue)` and other dangerous shapes not exercised at the render boundary.

**Proposal:**
- Convert both pin/chip tests to `it.each([["url", "url(https://example.test/x)"], ["var", "var(--signal-blue)"], ["named", "red"], ["empty", ""]])`.

## `delta.messageCount === session.messageCount` asymmetric when `session.messageCount` is `null`/`undefined`

**Severity:** Low - a missed-optimization corner case in the textReplace no-op short-circuit; the conservative fall-through is safe but the asymmetry will mislead a future polarity-flip refactor.

`ui/src/live-updates.ts:586`. The strict-equality check `delta.messageCount === session.messageCount` against a `number | null | undefined` field means a session whose `messageCount` is `undefined` (legacy session, mid-load, or a metadata-only summary that hasn't been fully populated) never satisfies the predicate even when `delta.messageCount` reflects the same effective state. Net effect today: the short-circuit is conservatively skipped (a missed optimization, not a correctness bug). The risk is that a future polarity-flip refactor — e.g., "if either side is nullish, treat as match" — would break the no-op detection asymmetrically across session ages.

**Current behavior:**
- `delta.messageCount` is always a `number` (per the wire shape).
- `session.messageCount` may be `null` / `undefined` for legacy or partially-loaded sessions.
- The strict-equality check fails for these sessions even when content is unchanged, so the short-circuit doesn't fire.

**Proposal:**
- Add an inline comment noting that the check is intentionally conservative when `session.messageCount` is nullish, OR
- Normalize both sides to a non-nullish baseline (`(session.messageCount ?? null) === (delta.messageCount ?? null)`) to make the symmetry explicit.

## Telegram relay loses per-message forwarding progress on mid-batch send error

**Severity:** High - a chunk send failure mid-batch silently discards in-memory state mutations, producing duplicate messages on the next poll plus unrecoverable state until a successful poll iteration commits.

`src/telegram.rs:929-947` (`forward_new_assistant_message_if_any`). The loop sets `state.last_forwarded_assistant_message_id` and `state.last_forwarded_assistant_message_text_chars` AFTER each successful `telegram.send_message(...)`, then continues with the next chunk/message via `?`-propagation. If chunk N+1 fails, the function returns `Err(...)`. Both call sites — `sync_telegram_digest` (line 779) and `forward_telegram_text_to_project` (line 732) — match `Err(err) => eprintln!(...)` and DO NOT flip the local `dirty` flag. The run loop's `if dirty { persist_telegram_bot_state(...) }` therefore does not save. The first message's id-mutation lives only in memory; on the next poll iteration the relay re-forwards message 1 (visible Telegram duplicate) AND retries message 2. The doc-comment "Record progress per-message so a mid-batch send failure still preserves the messages that DID make it" describes the intent but the surrounding error-propagation contract silently undoes it.

**Current behavior:**
- `forward_new_assistant_message_if_any` mutates `state.last_forwarded_assistant_message_id`/`_text_chars` per-message.
- Mid-batch `telegram.send_message` failure `?`-propagates an `Err(...)`.
- Both callers swallow the `Err` via `eprintln!` and leave `dirty = false`.
- Run loop skips `persist_telegram_bot_state(...)`; in-memory progress is lost.
- Next poll re-forwards already-shipped messages, duplicating them in the chat.

**Proposal:**
- Persist `state` immediately after each successful `send_message` (or have the helper return `Ok(true)` + the partial state on `Err`, so the run loop persists before logging).
- Or have callers set `dirty = true` even on `Err` (the helper documents that it may have mutated state before failing).
- Add coverage that fails the second `send_message` and asserts `state.last_forwarded_assistant_message_id` reflects the first message's id on the next poll cycle.

## `position_of_last` lookup ignores `author` while `needs_resend_truncated` gates on it

**Severity:** Medium - a stale state file or future id-format change with `last_forwarded_assistant_message_id` matching a non-assistant text message produces silent skipping or replay.

`src/telegram.rs:846-856`. The position lookup uses `matches!(message, TelegramSessionFetchMessage::Text { id, .. } if id == tracked)` and does NOT restrict to `author == "assistant"`. The companion `needs_resend_truncated` check at lines 864-876 DOES gate on `author == "assistant"`, so the asymmetry is real: if the tracked id maps to a user text message, position_of_last returns `Some(pos)`, needs_resend_truncated returns `None`, needs_baseline is `false`, and `start_index = pos + 1` skips messages that should have been forwarded.

**Current behavior:**
- `position_of_last` matches by `id` alone, accepting any `Text` author.
- `needs_resend_truncated` matches `id` AND `author == "assistant"`.
- A non-assistant id collision (today rare, but defensively undefined) silently skews `start_index`.

**Proposal:**
- Add `author == "assistant"` to the `position_of_last` `matches!` filter so the lookup pins to the same family the rest of the function considers.
- Add coverage with a stale state id that maps to a user message; assert the relay re-baselines instead of skipping.

## `telegram_error_is_message_not_modified` substring match against an unstable error format

**Severity:** Medium - Telegram's localized or reworded `description` either over-matches a different 400 (silent edit-failure swallow) or under-matches (sends duplicate digests). Structured `error_code` is discarded by `TelegramApiEnvelope`.

`src/telegram.rs:685-687, 344-353`. The check is `err.to_string().contains("message is not modified")`. `request_json` formats errors using only `envelope.description` — `error_code: 400` is parsed away. A future Telegram change like "Bad Request: message previously not modified, retry" or a localization tweak either over-matches (a different 400 silently treated as no-op) or under-matches (canonical English text changes → duplicate digest spam). The fix's pre-existing baseline behavior is the under-match path so the over-match risk is the real concern.

**Current behavior:**
- `TelegramApiEnvelope` parses `description` only; `error_code` is dropped.
- `bail!` formats the description as part of the error chain.
- Substring match `contains("message is not modified")` over-matches any future 400 with that substring.

**Proposal:**
- Surface `error_code: Option<i64>` from `TelegramApiEnvelope` and bubble it through a typed Telegram error (e.g., `TelegramApiError { code, description }`).
- Match on `error_code == 400 && description.contains("message is not modified")` or anchor with `starts_with`.
- Add a unit test that pins the canonical error string and another that pins the `error_code == 400` path.

## `forward_telegram_text_to_project` has asymmetric error handling between digest API and inner forward

**Severity:** Medium - same network-class failure produces different lifecycle outcomes depending on which call hit it first.

`src/telegram.rs:705-717`. `?` propagates errors from `termal.send_session_message`, the immediate digest fetch, and `send_fresh_telegram_digest_from_response`, but the inner `forward_new_assistant_message_if_any` failure is swallowed via `eprintln!`. Both error categories represent "transient HTTPS/transport hiccup". The outer call site (`handle_telegram_update`) already logs and continues on any returned `Err`, so the asymmetry is a policy choice without a documented rationale.

**Current behavior:**
- Digest-fetch and digest-send errors abort the user's text-forward acknowledgment path.
- Session-fetch / session-forward errors get logged to stderr and the function continues.
- No comment captures why the boundary lives here.

**Proposal:**
- When the planned "category-only logging" Step 2 of `telegram-ui-integration.md` lands, document the policy at the top of the function: "Inner forward errors are non-fatal because the digest-with-buttons already shipped; outer errors abort because we have nothing to acknowledge yet."
- Or hoist the inner forward outside the chain.
- Either way, capture the rationale in source.

## `docs/features/telegram-ui-integration.md` REST surface and security-model gaps

**Severity:** Medium - the design brief proposes endpoints that submit free-form prompts to the agent runtime without origin/loopback enforcement, and the link-code linking flow has spec inconsistencies.

`docs/features/telegram-ui-integration.md:213-242`. (a) Brief proposes `POST /api/telegram/start-link` (returns `{ link_code, expires_at }`) but no `POST /api/telegram/link/cancel` or `GET /api/telegram/link/status` — lifecycle uses polling on `GET /api/telegram/status`. (b) No `POST /api/telegram/relay/start|stop`; single `POST /api/telegram/config` with `enabled` does dual duty (token + lifecycle). A user wanting to update the token AND turn the relay off in two steps repeats the token in the second request body. (c) No CSRF / origin / loopback-only enforcement notes — Phase-1 single-user, but the localhost endpoint is reachable from any process on the machine, including a malicious browser tab targeting `127.0.0.1:8787`. The relay endpoints submit free-form prompts to the agent runtime, so they need at minimum origin enforcement. (d) Link code: brief example `tA9k-7Zq2e` is 9 chars, doc says "8 chars base32"; doc never specifies single-use vs TTL-only-expiry semantics; an attacker probing for codes gets a "right format, wrong value" oracle from the rejection response.

**Current behavior:**
- REST surface in the brief does not match the build-sequence's lifecycle model.
- Token storage and runtime lifecycle share one endpoint.
- No origin/loopback security-model section.
- Link-code length spec inconsistent between text and example; single-use vs TTL semantics undocumented.

**Proposal:**
- Add `POST /api/telegram/link/cancel` + `GET /api/telegram/link/status` + `POST /api/telegram/relay/start|stop`; split token vs runtime endpoints.
- Add a "Security model" subsection covering Origin-header enforcement, loopback-only listener confirmation, and a token-rotation note.
- Pin link-code spec (length, alphabet, single-use semantics, idempotent already-bound replay, per-chat rate-limit on invalid attempts).

## Bare-Mermaid-error visualization detection has no direct test coverage

**Severity:** Low - a future Mermaid version that changes the syntax-error SVG markers silently re-introduces the red bomb visualization in the iframe.

`ui/src/mermaid-render.ts:380-385, 395-435` (`isMermaidErrorVisualizationSvg`). The new path detects Mermaid 11.x's "syntax error" SVG and rethrows so `MermaidDiagram`'s clean error fallback can take over. Detection uses substring match on three patterns (`aria-roledescription="error"`, `Syntax error in text`, `class="error-icon"`). The third pattern is the weakest signal and could false-positive on a legitimate user diagram styling its own `error-icon` class. No test in `MarkdownContent.test.tsx`, `MessageCard.test.tsx`, `MarkdownContent.mermaid-fallback.test.tsx`, or a (nonexistent) `mermaid-render.test.ts` mocks `mermaidRenderMock.mockResolvedValueOnce` with a syntax-error SVG and asserts the fallback path engages.

**Current behavior:**
- `isMermaidErrorVisualizationSvg` is exported but never directly exercised.
- No assertion that mocked syntax-error SVG output flows to the diagram-error fallback (one-line message + source) instead of the iframe.
- Three substring checks; the `class="error-icon"` pattern is the weakest.

**Proposal:**
- Add a test mocking `mermaid.render` to return a syntax-error SVG; assert `MermaidDiagram` shows the source-as-code fallback.
- Tighten the `class="error-icon"` check to require it alongside `aria-roledescription="error"` (or scope to `<g class="error-icon">` inside `<svg aria-roledescription="error">`).

## Mermaid temporary render container cleanup has no regression coverage

**Severity:** Low - Mermaid can leave temporary `#d${diagramId}` or `#${diagramId}` nodes in `document.body`, and the cleanup path is not pinned.

`ui/src/mermaid-render.ts:402`. The render wrapper now removes Mermaid's temporary DOM container after render failure/error visualization handling. Existing tests cover fallback rendering but do not simulate Mermaid appending those body nodes and then failing, so a future refactor could drop the cleanup while the visible fallback still passes.

**Current behavior:**
- Mermaid temp-wrapper cleanup is implemented in the render path.
- No test appends the temp wrapper/SVG and asserts cleanup after rejection or error-SVG conversion.
- Orphaned body nodes could accumulate without a failing regression test.

**Proposal:**
- Add a focused `renderTermalMermaidDiagram` test that mocks `mermaid.render` to append temp nodes, then throws or returns an error visualization.
- Assert the temp wrapper/SVG is removed from `document.body` afterward.

## Marker color fallback normalization can hide corrupt local marker state during reconciliation

**Severity:** Low - authoritative marker snapshots can be treated as equal to locally corrupt raw marker colors.

`ui/src/session-reconcile.ts:263-264`. Reconciliation compares marker colors with `normalizeConversationMarkerColor`, which falls back unsupported values to a display-safe default. That is correct at render boundaries, but using display fallback equality for state reconciliation can preserve a raw local `url(...)`/invalid color when the authoritative snapshot carries a canonical safe color that normalizes to the same fallback.

**Current behavior:**
- Marker color equality in reconciliation uses display normalization.
- Invalid local raw color can compare equal to a canonical authoritative fallback color.
- The local corrupt value can remain in client state instead of being replaced by authoritative wire data.

**Proposal:**
- Keep raw/canonical marker equality in reconciliation, or normalize marker colors once at ingestion before storing them in session state.
- Reserve fallback normalization for render/display-only boundaries.

## Marker stale-response recovery lacks normalized-color branch coverage

**Severity:** Low - the action recovery guard changed behavior for color-normalized marker equality, but the exact branch is not directly tested.

`ui/src/app-session-actions.ts:345-346`. `conversationMarkerExactlyMatches` now normalizes marker colors before comparing stale same-instance responses. Utility and component tests cover marker color normalization, but there is no `app-session-actions` test proving a stale response whose only difference is color case/normalization is accepted without scheduling recovery resync.

**Current behavior:**
- Stale marker response matching treats normalized color-equivalent markers as exact matches.
- The action recovery branch is not covered with casing/normalization-only marker color differences.

**Proposal:**
- Add a stale same-instance marker success/update test where only color casing or normalization differs.
- Assert no recovery resync is requested.

## App.live-state.reconnect "fallback-only-hidden" test phases 1-3 are tautological

**Severity:** Low - the "keeps fallback-only assistant text hidden until a fresh SSE state confirms it" test does not put the assistant message in the fallback or resync payload it claims to gate on; only Phase 4 has teeth.

`ui/src/App.live-state.reconnect.test.tsx:1270-1411`. The new test dispatches an `_sseFallback: true` event with the user-only message, then resolves the resync deferred with revision 2 also containing only the user message. Both "hidden" assertions in Phases 1-3 are tautological — a snapshot without the assistant message obviously doesn't render it. The interesting regression — a fallback or resync payload that DOES contain unconfirmed assistant text being incorrectly exposed — is not exercised. A future regression where the fallback adoption path leaks unconfirmed assistant text would still pass this test.

**Current behavior:**
- Phase 1: dispatches user-only opened state; asserts assistant text not visible.
- Phase 2: dispatches `_sseFallback: true` with user-only state; asserts assistant text not visible.
- Phase 3: resolves resync with user-only state; asserts assistant text not visible.
- Phase 4: dispatches fresh SSE state at revision 3 with assistant text; asserts visible.
- Only Phase 4 actually pins behavior; phases 1-3 cannot fail because no test payload ever contains the assistant text those assertions guard.

**Proposal:**
- Add an unconfirmed assistant message to the `_sseFallback` payload in Phase 2 (and/or to the resolved resync state in Phase 3); assert it remains hidden.
- Phase 4 still confirms visibility once a fresh non-fallback SSE state arrives.

## Telegram relay first-touch chat binding gives effective shell access if the bot token leaks

**Severity:** High - any Telegram chat that can reach the bot will be silently linked on the first `/start`/`/help` and can then drive the local agent (full FS/shell access) through `send_session_message`.

`src/telegram.rs:605-647, 696-714`. `effective_telegram_chat_id(...).is_none()` is enough for any reachable chat to claim the binding (`state.chat_id = Some(chat_id)` at line 615), and once linked, free text from that chat is forwarded directly into the local TermAl session via `termal.send_session_message`, which dispatches to the agent runtime. `TERMAL_TELEGRAM_CHAT_ID` is documented as optional and the README presents the link-on-first-start flow as the expected setup. Bot tokens have realistic leak vectors (logs, shell history, shared screen, accidental commit) — anyone who recovers a leaked token plus an unset chat id has effective shell access to the local machine.

**Current behavior:**
- The first message into an unlinked relay binds that chat permanently to `~/.termal/telegram-bot.json`.
- After binding, free text from the linked chat is forwarded into the active project session as a prompt.
- Prompts dispatched through this path inherit the agent runtime's full filesystem and shell capabilities.
- `TERMAL_TELEGRAM_CHAT_ID` exists but is optional.

**Proposal:**
- Default-deny first-touch linking unless a binding has been pre-declared via `TERMAL_TELEGRAM_CHAT_ID` or a relay-side `/api/telegram/link` admin call.
- Or: require a shared secret / passphrase as the first argument to `/start`, validated against an env-supplied value before persisting `chat_id`.
- Document the threat model in `README.md` so operators understand a leaked token plus an empty chat-id is shell-equivalent.

## Fresh Telegram assistant replies can be consumed as a baseline instead of forwarded

**Severity:** High - the first assistant reply after a Telegram-originated prompt can be silently dropped for fresh or switched relay sessions.

`src/telegram.rs:714, 887-902`. A Telegram text prompt is forwarded through `send_session_message`, then the immediate assistant-forward attempt usually sees the session as active and returns without forwarding. When the turn later settles, `last_forwarded_assistant_message_id` can still be `None`; the baseline branch records the newest assistant message id and returns `Ok(changed)` without sending the text to Telegram. The same shape can happen after switching the primary session or after a relay state reset.

**Current behavior:**
- Fresh relay/session state has no assistant forwarding cursor.
- The first settled assistant reply is treated as old history to baseline rather than a new Telegram reply.
- Telegram users can see their prompt accepted but never receive the first assistant response.

**Proposal:**
- Establish an explicit assistant baseline before dispatching Telegram-originated text, then forward the first post-dispatch assistant message.
- Or track Telegram-initiated turns/sessions separately from cold-start history suppression so the baseline path cannot swallow new replies.
- Add coverage for a session with no prior assistant baseline.

## Telegram bot token and backend error details can leak through stderr

**Severity:** High - Telegram request URLs include `/bot{token}` and error chains are printed with expanded `{err:#}` logging.

`src/telegram.rs:230, 340-346, 444-453, 51, 67, 74, 733, 783`. `TelegramApiClient` stores the bot token inside `api_base_url`, and reqwest error chains can include the failing URL. The same relay also lifts Telegram descriptions and TermAl non-JSON error bodies into errors that are later formatted with `{err:#}`. On shared dev machines, stderr is often piped to logs, terminal recording, or CI output; a network/proxy/TLS failure can therefore expose the Telegram bot token, and backend error bodies can expose paths or prompt fragments.

**Current behavior:**
- Telegram API URLs embed the bot token.
- Telegram and TermAl error responses are forwarded into stderr lines without truncation or sanitization.
- Expanded error formatting can include URLs, paths, prompt fragments, or token-bearing diagnostics.

**Proposal:**
- Redact bot tokens and URLs before logging Telegram/reqwest errors.
- Truncate non-JSON backend error payloads to a small fixed length and prefer structured `error.error` fields.
- Replace `{err:#}` with category-only log lines unless a local debug flag is enabled.

## Telegram-forwarded text has no length cap before reaching the local backend

**Severity:** Medium - any linked chat (or any chat that captured the binding via the first-touch hazard above) can submit prompts up to the 10 MB Axum body limit straight into the agent.

`src/telegram.rs:407-416, 605, 714`. `handle_telegram_message` only checks `is_empty()` before calling `forward_telegram_text_to_project`, which forwards verbatim through `termal.send_session_message`. The new MCP-style delegation surface enforces `MAX_DELEGATION_PROMPT_BYTES = 64 * 1024` and rejects oversize prompts by UTF-8 byte length; the Telegram path is the equivalent escalation channel and ships without the same guard.

**Current behavior:**
- Telegram payloads of any size below the Axum 10 MB body limit are forwarded unchanged.
- Prompt-injection content lands directly in the agent's turn dispatch with full tool access.
- No per-minute prompt-rate cap.

**Proposal:**
- Apply a per-message UTF-8 byte limit (consistent with `MAX_DELEGATION_PROMPT_BYTES`) on the Telegram side and reply with a Telegram error message when exceeded.
- Add a per-minute / per-chat prompt-rate cap so an attacker that captured the chat binding cannot fan out N HTTP calls per second.

## Telegram relay forwards full assistant text to Telegram by default

**Severity:** Medium - assistant replies can include code, local file paths, file contents, or secrets and are sent to a third-party service without an explicit opt-in.

`src/telegram.rs:936-938`. The relay chunks and forwards the full settled assistant message body to Telegram once the session is no longer active. This goes beyond the compact project digest and sends arbitrary model output off-machine by default.

**Current behavior:**
- The Telegram digest path is compact, but settled assistant messages are forwarded in full.
- Assistant text may contain local workspace details or user-provided secrets.
- Users enabling the relay do not get a separate opt-in for full-content forwarding.

**Proposal:**
- Make full assistant text forwarding an explicit opt-in setting.
- Keep digest-only forwarding as the default for Telegram integrations.
- Document the third-party content exposure and add any practical redaction/truncation before full forwarding.

## Telegram `getUpdates` batch processing is unbounded and re-runs on poll-iteration panic

**Severity:** Medium - a Telegram update burst (real attack, retry storm, or accidental flood) becomes a multiplicative wave of HTTP calls into the local backend, and a panic mid-batch re-runs the same updates on the next poll.

`src/telegram.rs:48, 65-69, 78`. The relay accepts the entire `Vec<TelegramUpdate>` Telegram returns and walks each update through `handle_telegram_update`, which can issue multiple outbound HTTP calls per update (digest fetch, send_message, action dispatch, session fetch). State is persisted once at the end of the iteration; a panic mid-batch leaves `next_update_id` un-advanced and Telegram resends the same batch on the next poll, amplifying the effect.

**Current behavior:**
- `getUpdates` does not pass an explicit `limit`, so Telegram returns up to its server-side default (100).
- A 100-update batch can fan out to several hundred backend HTTP calls.
- Mid-batch panic loses all per-update state (including advanced `next_update_id`), and Telegram replays.

**Proposal:**
- Cap `getUpdates` `limit` (e.g., 25) on the request side.
- Persist `next_update_id` per update inside the batch loop rather than once at the end.
- Add a per-iteration backoff after errors so a sustained failure does not tight-loop.

## Telegram forwarder is gated inside the digest-hash-changed branch

**Severity:** Medium - new assistant text whose digest preview hashes identically to the previous one is silently never forwarded to Telegram.

`src/telegram.rs:752-786`. `sync_telegram_digest` wraps both the digest re-render AND the assistant-text forward inside one `if state.last_digest_hash != ...` guard. The digest's `done_summary` is built from a truncated preview (~80 chars); a long assistant message whose first ~80 chars match the previous preview hashes the same as the previous digest, so the guard short-circuits and `forward_new_assistant_message_if_any` is never called. The forwarder already self-dedupes via `last_forwarded_assistant_message_id` + `last_forwarded_assistant_message_text_chars`, so coupling it to the digest-hash check buys nothing and creates a real silent-drop hazard.

**Current behavior:**
- The assistant-text forward only runs when the digest hash changed.
- A new assistant message that does not move the truncated preview hash never reaches Telegram.

**Proposal:**
- Decouple the forward from the digest-hash check — invoke `forward_new_assistant_message_if_any` unconditionally each poll iteration; rely on its existing per-id + per-char-count dedupe.
- Or: hash the latest assistant message id+chars separately from the digest text+actions and gate on either changing.

## `last_forwarded_assistant_message_text_chars` doc comment promises self-healing the code does not deliver

**Severity:** Medium - the legacy-state contract documented on the field claims a one-time re-forward when a state file lacks the new field, but the actual code maps `None => None` and never triggers re-forwarding.

`src/telegram.rs:165-170, 869-871`. The field's doc comment promises "older state files (which lack `last_forwarded_assistant_message_text_chars`, deserialized as `None`) trigger a one-time re-forward when the latest message is next observed (acceptable cost for self-healing)". The `needs_resend_truncated` computation maps `last_chars: None` to `Some(pos) if current > prev | _ => None`, which always falls through to `None`. Pre-upgrade users whose previous build truncated a forward mid-stream will not see the unforwarded tail.

**Current behavior:**
- Old state file (`last_forwarded_assistant_message_id` set, `last_forwarded_assistant_message_text_chars` absent → `None`).
- `needs_resend_truncated = None`, `needs_baseline = false`, `start_index = pos + 1`.
- The truncated message is skipped and only strictly newer messages are forwarded.

**Proposal:**
- Either change the inner match arm to `None => Some(pos)` so unknown char count triggers a one-time re-forward (matches the comment).
- Or update the doc comment to describe the actual "trust the old id; only forward strictly newer messages" behavior so future readers do not assume self-healing.

## `TelegramSessionFetchSession.status` is stringly-typed against a closed backend enum

**Severity:** Medium - the active-stream gate that prevents mid-stream forwards is the only safeguard against shipping truncated assistant text; it is currently a `==` against a `String`.

`src/telegram.rs:540-544, 840`. `TelegramSessionFetchSession::status: String` defaults to `""` and is compared via `response.session.status == "active"`. The backend `SessionStatus` enum (`src/wire.rs:712`) is a closed set of `Active | Idle | Approval | Error`. A future variant addition (e.g., `Pending`, `Stopping`) would silently bypass the gate (treated as not-active → forward proceeds despite a still-streaming agent). The dedupe-by-id-and-char-count fallback might paper over the mistake but only after a truncated forward has already shipped to the user's chat.

**Current behavior:**
- Status is parsed as a free-form `String`.
- `== "active"` is the only gate against forwarding mid-stream content.
- A new active-equivalent backend variant would be silently misclassified as settled.

**Proposal:**
- Model the field as a typed enum with `#[serde(rename_all = "lowercase")]` and `#[serde(other)] Unknown`.
- Decide the policy explicitly for the `Unknown` arm — treat it as "active" (safe but pessimistic) or trigger a refresh of the wire-projection assumptions.

## `preferStreamingPlainTextRender` prop name and "Three render paths" comment are stale after the unified-render refactor

**Severity:** Medium - public-shaped prop semantics drift silently from a removed implementation; future readers see a name describing code that no longer exists.

`ui/src/message-cards.tsx:311, 327, 359-388`. The `StreamingAssistantTextShell` render path was removed; the prop now functionally means "this is the live-streaming assistant message". The leading comment block ("Three possible render paths...") describes the old three-path implementation, contradicted by the next paragraph saying "always renders through `<DeferredMarkdownContent>`". Test names and the prop signature carry the old name.

**Current behavior:**
- `preferStreamingPlainTextRender` is the only render-path discriminator but its name suggests a removed plain-text shell.
- The leading comment describes three paths; only one survives.

**Proposal:**
- Rename to `isStreamingAssistantTextMessage` (matching the local `isStreamingAssistantMessage` variable already used inside `MessageCardImpl`).
- Replace the "Three possible render paths" sentence with the current single-path explanation.
- Pure refactor, no behavior change.

## CSS bubble `width: fit-content` transition causes horizontal layout reflow at turn end

**Severity:** Medium - the "stable component subtree across stream → settle" goal is partially undermined by a CSS-driven layout jump.

`ui/src/styles.css:4448-4452`. `:has(.markdown-table-scroll)` applies `width: fit-content; max-width: min(96rem, 96%)` to the bubble. With the new `deferAllBlocks: true` policy a streaming bubble has NO `.markdown-table-scroll` until the turn settles (the table sits in `.markdown-streaming-fragment` instead). When streaming ends, the bubble's effective `max-width` jumps from default `42rem` to `96rem` AND its `width` switches to `fit-content` — the bubble grows wider, producing a visible horizontal reflow at the same moment the React subtree was supposed to be stable.

**Current behavior:**
- During streaming the bubble follows the prose-default sizing (`42rem` cap).
- On settle the `:has(.markdown-table-scroll)` selector engages and the bubble jumps to `fit-content` / 96rem cap.

**Proposal:**
- Anticipate `width: fit-content` while streaming if a `|`-line has been seen (e.g., a class on the streaming-fragment placeholder that triggers the same selector).
- Or: document the layout shift as accepted and add a regression that asserts bubble width remains stable across the stream→settle transition so any future drift is visible in tests.

## Mermaid aspect-ratio sizing can clip constrained diagrams

**Severity:** Medium - constrained Mermaid iframes can hide diagram content instead of only removing blank frame space.

`ui/src/mermaid-render.ts:134-152` sizes Mermaid iframes with `aspectRatio: ${frameWidth} / ${frameHeight}` + `height: auto`. The change fixes the wide-blank-frame regression, but the iframe document still keeps the SVG at intrinsic width while hiding vertical overflow. When `max-width: 100%` constrains the iframe, the frame height can shrink without the inner SVG scaling with it, so wide or tall diagrams can lose bottom content instead of simply removing unused whitespace.

**Current behavior:**
- The outer iframe height is driven by CSS `aspect-ratio` and constrained width.
- The inner Mermaid SVG can remain at intrinsic dimensions.
- The iframe hides vertical overflow, so constrained diagrams can be clipped at the bottom.

**Proposal:**
- Keep explicit intrinsic height for horizontally-scrollable diagrams, or make the inner SVG scale with the iframe width before using aspect-ratio sizing.
- Add visual/regression coverage for constrained wide and tall diagrams.

## Mermaid error-SVG detection can treat valid diagram text as a render failure

**Severity:** Low - a valid diagram containing the phrase "Syntax error in text" can be routed to the fallback instead of rendered.

`ui/src/mermaid-render.ts:424-430` returns true if the SVG contains the literal phrase "Syntax error in text". Mermaid's canonical error SVG contains that phrase, but valid diagrams can also include it as a label, note, or text node. That makes the detection too broad when the stronger root-level error markers are absent.

**Current behavior:**
- `aria-roledescription="error"`, `class="error-icon"`, or the plain phrase all independently classify the SVG as an error visualization.
- The plain phrase alone can appear in legitimate diagram content.
- A false positive causes `renderTermalMermaidDiagram` to throw and display the clean fallback instead of the diagram.

**Proposal:**
- Require the stronger root error signal, or require multiple Mermaid-error markers before throwing.
- Add negative coverage with a valid SVG/diagram label containing "Syntax error in text".

## Telegram REST integration brief uses snake_case payload keys

**Severity:** Low - proposed API shapes in the Telegram UI brief do not follow TermAl's camelCase JSON convention.

`docs/features/telegram-ui-integration.md:278` proposes `{ link_code, expires_at }` for `POST /api/telegram/start-link`. The API protocol convention is camelCase for JSON keys, and adopting the brief as written would create a new endpoint family that drifts from existing frontend/backend API shape expectations.

**Current behavior:**
- The brief proposes snake_case response keys for a REST endpoint.
- Nearby proposed names also use shapes like `project_id`.
- Future implementation could copy the brief into code and ship inconsistent API payloads.

**Proposal:**
- Use `linkCode`, `expiresAt`, `projectId`, and other camelCase keys for REST request/response payloads.
- Keep snake_case only for internal persisted files if needed.

## Asymmetric `orchestrator_auto_dispatch_blocked` between two persist-failure rollback sites

**Severity:** Medium - an Error session can remain auto-dispatch-eligible after a runtime-exit commit failure while disk and memory disagree.

`src/session_lifecycle.rs:449` defensively sets `record.orchestrator_auto_dispatch_blocked = true` on persist-failure rollback in the stop-session path. `src/turn_lifecycle.rs:455` does NOT mirror that defensive set in the runtime-exit rollback path, and the inner block at `turn_lifecycle.rs:413` has already explicitly cleared the flag to `false` before the failed `commit_locked`. Net effect: if the runtime-exit commit fails, the session in-memory state is `SessionStatus::Error` with the "Turn failed: …" message, but the orchestrator can still observe it as eligible for auto-dispatch.

**Current behavior:**
- `stop_session` rollback sets `orchestrator_auto_dispatch_blocked = true` defensively.
- `handle_runtime_exit_if_matches` rollback leaves the flag at whatever the inner block last wrote (`false`).
- An Error session with a failed persist commit can still be re-dispatched.
- The new tests do not pin `orchestrator_auto_dispatch_blocked` in either rollback path.

**Proposal:**
- Either mirror the `session_lifecycle.rs` defensive set (`true`) in `turn_lifecycle.rs`, or document why the asymmetry is intentional.
- Tighten the persist-failure tests to also pin `orchestrator_auto_dispatch_blocked`, `runtime`, `runtime_stop_in_progress`, and the "stopped/failed" message presence.

## Conversation overview segment cap does not bound homogeneous runs

**Severity:** Low - `maxItemsPerSegment` reads as a hard cap but same-kind message runs can bypass it.

`ui/src/panels/conversation-overview-map.ts:422` merges same-visual-class messages before enforcing the item cap. Homogeneous transcript runs can therefore collapse into a single unbounded segment, which weakens keyboard and accessibility navigation granularity and makes the option contract misleading.

**Current behavior:**
- Mixed-kind runs are split according to the configured segment cap.
- Same-kind runs can exceed `maxItemsPerSegment`.

**Proposal:**
- Apply the item cap before the same-visual-class fast path, or rename/comment the option as a mixed-run cap.
- Add focused coverage for a long homogeneous run.

## Mermaid fallback loader lives in the message card renderer

**Severity:** Low - Mermaid fallback loading and cache ownership are mixed into an already large rendering component.

`ui/src/message-cards.tsx:182` now owns dynamic import failure classification and global fallback script loading, while `mermaid-render.ts` already owns Mermaid rendering configuration and queueing.

**Current behavior:**
- Mermaid fallback loader/cache logic lives in `message-cards.tsx`.
- Rendering, fallback loading, and message-card composition are coupled in the same large module.

**Proposal:**
- Move fallback loading into `mermaid-render.ts` or a small `mermaid-loader.ts`.
- Keep message cards calling a single render helper.

## Markdown diff change-block grouping rules duplicated between renderer and index builder

**Severity:** Medium - the change-navigation index walker copies the renderer's grouping rules; future drift between the two will silently desynchronize navigation stops from rendered blocks.

`ui/src/panels/markdown-diff-view.tsx:508-526` and `ui/src/panels/markdown-diff-change-index.ts:60-87`. Both walks have identical logic: skip `normal`, gather consecutive non-`normal` segments, break at the same `current.kind === "added" && next.kind === "removed"` boundary, and produce identical id strings (`segments.map(s => s.id).join(":")`). The renderer then re-derives the same id and looks it up in a `Map<id, index>` the navigation code built from the index walker's output. The header comment in `markdown-diff-change-index.ts:46-54` explicitly acknowledges "the rule is duplicated here so the navigation index does not drift from what the user sees" — i.e., the only thing keeping the two walks in sync is the test suite.

**Current behavior:**
- Renderer (`renderMarkdownDiffSegments`) and index builder (`computeMarkdownDiffChangeBlocks`) walk the same segment array twice with identical grouping rules.
- The navigation index is recovered by id-lookup against a Map built from the index walker's output.
- Any future change to the grouping rules (e.g., a third break-rule for a new segment kind) must be made in both places.

**Proposal:**
- Have `renderMarkdownDiffSegments` consume the precomputed `changeBlocks` directly. Iterate (`normal` segment OR `changeBlocks[changeBlockCursor]`); the renderer emits the editable section for normals and the `<section>` wrapper for the next change-block, advancing `changeBlockCursor` after each.
- Single source of truth for grouping rules in `computeMarkdownDiffChangeBlocks`; the navigation index becomes the literal cursor position, no Map lookup needed, and the renderer's per-render Map allocation goes away.

## Concurrent shutdown callers can flip `persist_worker_alive` before the join owner finishes

**Severity:** Medium - the documented "flag flips only after worker join" contract is not true when two `AppState` clones call `shutdown_persist_blocking()` concurrently.

`shutdown_persist_blocking()` takes the worker handle out of `persist_thread_handle`, releases that mutex, and then blocks in `handle.join()`. A second concurrent caller can enter while the first caller is still joining, see `None`, and run the idempotent branch that stores `persist_worker_alive = false`. That lets a concurrent `commit_delta_locked()` take the synchronous fallback while the worker may still be doing its final drain/write, reopening the dual-writer persistence race the round-13 ordering was meant to close.

**Current behavior:**
- The first shutdown caller owns the join handle but does not hold the handle mutex while joining.
- A second shutdown caller treats `None` as "already stopped" even if the first caller is still waiting for the worker to stop.
- The second caller can publish `alive == false` before the worker has actually exited.

**Proposal:**
- Serialize the full shutdown transition so no caller can observe the stopped state until the join owner has returned from `handle.join()` and stored `persist_worker_alive = false`.
- Alternatively replace the `Option<JoinHandle>` state with an explicit `Running` / `Stopping` / `Stopped` state so only the join owner can transition from stopping to stopped.

## Rendered diff regions reset document-level Mermaid/math budgets

**Severity:** Medium - splitting rendered diff preview into one `MarkdownContent` per region weakens existing browser-side render-budget guards.

The rendered diff view now maps every renderable region to its own `MarkdownContent`. `MarkdownContent` counts Mermaid fences and math expressions per rendered document, so this split resets `MAX_MERMAID_DIAGRAMS_PER_DOCUMENT` and `MAX_MATH_EXPRESSIONS_PER_DOCUMENT` for each region instead of for the full diff preview. A crafted or simply large diff with many Mermaid/math regions can render far more expensive diagrams/equations than the previous single synthetic-document path allowed.

**Current behavior:**
- Each rendered diff region gets an independent Mermaid/math budget.
- The whole rendered diff preview no longer has one aggregate render cap.

**Proposal:**
- Compute aggregate Mermaid/math counts before mapping regions and apply a document-level fallback when the aggregate exceeds the cap.
- Or pass a shared render-budget context/override into each region-level `MarkdownContent`.

## Post-shutdown persistence writes still leave a post-collection-pre-join window

**Severity:** Medium - round-13 closed the dual-writer file race, but a narrow gap remains between the worker's final `collect_persist_delta` and `handle.join()` returning.

Round 13 moved the `persist_worker_alive` flip from BEFORE the Shutdown signal to AFTER `handle.join()` returns. That closed the dual-writer hazard (concurrent fallback writes racing the worker's still-in-progress final drain on the same persistence path). However, after the worker captures its final delta but before `handle.join()` returns, a concurrent `commit_delta_locked` will observe `alive == true`, bump `inner.mutation_stamp`, and return without persisting. That mutation is not picked up by the worker (already past collection) nor by the sync fallback (flag still true). `commit_locked` and `commit_persisted_delta_locked` are unaffected because they call `persist_internal_locked` which itself errors and falls back when the channel becomes disconnected.

**Current behavior:**
- The dual-writer file race is closed by round 13.
- A narrow window between the worker's final `collect_persist_delta` and `handle.join()` returning still exists; `commit_delta_locked` calls in that window observe `alive == true` and return without persisting.
- `commit_locked` / `commit_persisted_delta_locked` infer fallback from `persist_tx.send` failure, but `persist_tx` only disconnects when the LAST `AppState` clone drops its sender; with multiple clones (which is the production shape), `send` succeeds silently into a worker that has exited.

**Proposal:**
- Either (a) serialize the worker's final drain with sync fallback by holding `inner` for the worker's collect-and-write final iteration, or (b) require callers to quiesce non-HTTP producers before invoking `shutdown_persist_blocking`.
- Add a regression that races a late `commit_delta_locked` with the worker's final collection and proves the final persisted state is the latest `StateInner`.
- Add an explicit `persist_worker_alive` Acquire check to `persist_internal_locked` so all four commit variants share one shutdown contract.

## Duplicate remote delta hydrations fall through to unloaded-transcript delta application

**Severity:** Medium - duplicate in-flight hydration callers receive `Ok(false)`, which every delta handler treats as "no repair happened; continue applying the delta".

The in-flight map suppresses duplicate `/api/sessions/{id}` fetches, but it does not coordinate the waiting delta handlers. For a summary-only remote proxy, a concurrent text delta or replacement can still run against missing messages and trigger a broad `/api/state` resync; a message-created delta can partially mutate an unloaded transcript before the first full hydration finishes.

**Current behavior:**
- The first delta for an unloaded remote session starts full-session hydration.
- A duplicate delta for the same remote/session sees the in-flight key and returns `Ok(false)`.
- Callers continue into the narrow delta path as if no hydration was needed.

**Proposal:**
- Return a distinct outcome such as `HydrationInFlight`, or have duplicates wait/queue behind the first hydration.
- After the first hydration completes, re-check the session transcript watermark before applying queued or retried deltas.
- Add burst/concurrent same-session delta coverage proving only one remote fetch occurs and duplicate deltas do not mutate unloaded transcripts.

## Text-repair hydration lacks live rendering regression coverage

**Severity:** Medium - the lower-revision text-repair adoption path is covered only by a classifier unit test.

The new adoption rule is intended to fix the user-visible bug where the latest assistant message stays hidden until an unrelated focus, scroll, or prompt rerender. The current coverage proves the pure classifier returns `adopted`, but it does not prove the live hook requests the flagged hydration, adopts the lower-revision session response after an unrelated newer live revision, flushes the session slice, and renders the repaired text immediately.

**Current behavior:**
- `classifyFetchedSessionAdoption` has a unit test for divergent text repair after a newer revision.
- No hook or app-level regression drives `/api/sessions/{id}` through the live-state path and asserts immediate transcript rendering.

**Proposal:**
- Add a `useAppLiveState` or `App.live-state.reconnect` regression where text-repair hydration is requested, a newer unrelated live event advances `latestStateRevisionRef`, the session response resolves at the original request revision, and the active transcript updates without any extra user action.

## Timer-driven reconnect fallback can stop after `/api/state` progress before SSE proves recovery

**Severity:** Medium - a fallback snapshot can refresh visible UI while the live EventSource transport is still unhealthy.

`ui/src/app-live-state.ts:2068` disables `rearmUntilLiveEventOnSuccess` when a same-instance `/api/state` response makes forward revision progress, unless the recovery path is the manual-retry variant. A successful `/api/state` fetch proves that polling can reach the backend and can repair visible state, but it does not prove the SSE stream has reopened or can deliver later assistant deltas. If the transport remains broken, a later live message can stay hidden until another reconnect/error/user action restarts recovery.

**Current behavior:**
- Timer-driven reconnect fallback asks to keep polling until live-event proof.
- Same-instance `/api/state` forward progress disables that live-proof rearm path for non-manual recovery.
- UI state can look refreshed while the EventSource transport is still unconfirmed.

**Proposal:**
- Split "snapshot refreshed UI" from "transport recovered" in the reconnect state machine.
- Keep reconnect polling armed until `confirmReconnectRecoveryFromLiveEvent()` runs from a data-bearing SSE event, unless a cause-specific recovery path intentionally documents a different contract.
- Add a regression that adopts same-instance `/api/state` progress through the timer-driven reconnect path, keeps SSE unopened/unconfirmed, advances timers, and asserts another fallback poll is scheduled.

## Remote hydration in-flight cleanup can race with the RAII guard

**Severity:** Low - clearing `remote_delta_hydrations_in_flight` by key can remove or later invalidate a newer in-flight hydration for the same remote/session.

The remote hydration guard removes its `(remote_id, session_id)` key on drop. `clear_remote_applied_revision` can also remove keys for a remote while an older hydration guard is still alive. If a later hydration inserts the same key after that cleanup, the older guard can drop afterward and remove the newer marker, allowing duplicate hydrations despite the guard.

**Current behavior:**
- In-flight hydration entries are keyed only by `(remote_id, session_id)`.
- Remote continuity cleanup can remove a live key while the guard that owns it is still alive.
- A stale guard drop cannot distinguish its own entry from a newer entry with the same key.

**Proposal:**
- Store a unique token or generation per in-flight entry and remove only when the token still matches.
- Or avoid clearing live in-flight markers during remote continuity reset; let the owning guard retire its own marker.
- Add cleanup tests covering overlapping guards and per-remote cleanup.

## Lagged force-adopt marker clearing on EventSource reconnect lacks coverage

**Severity:** Low - the frontend now clears an armed lagged recovery marker on EventSource error/reconnect, but no test pins that boundary.

The new baseline guard covers same-stream stale recovery after a newer delta, but a separate hazard is an old `lagged` marker surviving across a closed EventSource into a new stream. The implementation clears the marker on reconnect/error cleanup, yet no regression proves a stale lower/same-instance state on the new stream cannot be force-adopted.

**Current behavior:**
- `clearForceAdoptNextStateEvent` runs during EventSource error/reconnect cleanup.
- Existing lagged tests do not cross an EventSource boundary.

**Proposal:**
- Add a reconnect test that dispatches `lagged`, triggers `error`, opens a new EventSource, and sends a lower/same-instance state that must not be force-adopted.

## Remote hydration dedupe coverage bypasses the production burst path

**Severity:** Low - the current duplicate-hydration test manually seeds the in-flight map instead of driving real bursty remote deltas.

The test pins the duplicate branch, but it would not catch a regression where the first real hydration leaks the guard, where a successful hydration does not clear the marker, or where multiple actual same-session delta handlers still issue duplicate remote session fetches.

**Current behavior:**
- The test inserts an in-flight key directly.
- It does not prove the first production hydration inserts and clears the guard.
- It does not prove bursty same-session deltas issue only one remote session fetch.

**Proposal:**
- Add coverage for a successful hydration path that asserts the guard is removed afterward.
- Add a burst/concurrent same-session delta case that asserts only one remote session fetch is issued.

## `apply_remote_state_if_newer_locked` `force: bool` parameter is unnamed at call sites

**Severity:** Low - seven call sites pass `false` and one passes `true`; readers cannot tell what `force` means without consulting the function signature.

`apply_remote_state_if_newer_locked` was extended with a `force: bool` parameter so that `apply_remote_lagged_recovery_state_snapshot` can bypass the same-revision replay gate. The parameter is correct, but the convention scales poorly: a future caller that copies a neighbouring `false` from any of the seven existing sites will inherit the gated behaviour without realising the parameter exists, and a future maintainer who needs the bypass at a different site will have to re-derive what the boolean means.

**Current behavior:**
- `apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None, false)` appears at seven call sites.
- One new call site passes `true` for lagged-recovery force-apply.
- The doc-comment on the function explains the parameter, but the call sites do not self-document.

**Proposal:**
- Replace `force: bool` with a typed `enum SnapshotApplyMode { GateBySnapshotRevision, ForceApplyAfterLagged }` (or similar). All existing call sites become `SnapshotApplyMode::GateBySnapshotRevision`; the lagged-recovery site reads `SnapshotApplyMode::ForceApplyAfterLagged` and self-documents.
- Optional: also push the bypass-gate into a tiny inline comment at the lagged-recovery site naming the upstream invariant (`api_sse.rs::state_events` yields `state` immediately after `lagged` within one `tokio::select!` arm).

## SSE recreation control plane is split between `sseEpoch` state and `pendingSseRecreateOnInstanceChangeRef`

**Severity:** Medium - two coordination mechanisms for one concern, increases regression risk and reduces debuggability.

`forceSseReconnect()` sets `pendingSseRecreateOnInstanceChangeRef.current = true` synchronously and the consume happens inside `adoptState` only when `fullStateServerInstanceChanged` is true. This adds a second control plane for SSE reconnection alongside the existing `sseEpoch` state, with the ref-vs-state ordering being load-bearing (synchronous `setSseEpoch` would tear down the in-flight probe). The pattern is documented inline, but ref state is not visible in React DevTools or in any state diff, so a subsequent maintainer reading the SSE transport effect cannot see the gate that determines whether the effect re-runs. The Round 8 comment in the doc-block notes this exact split-plane pattern was reverted before for the same reason. There is also no clear-on-no-instance-change reset path: if `forceSseReconnect()` fires but the recovery probe response comes back as same-instance (false alarm), the flag stays armed and could fire on a much later legitimate restart.

**Current behavior:**
- `forceSseReconnect()` mutates a ref invisible to DevTools.
- The flag is consumed only inside the `fullStateServerInstanceChanged` branch of `adoptState`.
- A successful recovery probe with no instance change leaves the flag armed indefinitely.
- The flag-on-adopt ordering relative to `setSseEpoch` is not pinned by a load-bearing test (current tests assert the recreate happens but not the ordering).

**Proposal:**
- Lift the gate into a state-driven shape (e.g., a single `sseReconnectReason` state with `instanceChangeAfterAdopt` as one of its values), so the SSE reconnection trigger is visible in React DevTools.
- Or add a load-bearing test that fails if the consume-on-adopt ordering is reversed.
- Either way, clear `pendingSseRecreateOnInstanceChangeRef` on any `adoptState` success that does not change the instance, so a false-alarm `forceSseReconnect()` cannot fire on a later legitimate restart.

## Sticky shutdown tests bypass `/api/events` stream wiring

**Severity:** Medium - helper-level tests can pass while the production SSE handler still hangs during shutdown.

The new tests validate the sticky `watch` shutdown helper directly, but they do not exercise `state_events` or the `/api/events` route using that signal. A future regression in the stream's pre-loop checks or select wiring could keep long-lived SSE connections open and block graceful shutdown while the helper tests still pass.

**Current behavior:**
- Tests cover the shutdown signal helper before/after registration.
- They do not hold or open `/api/events` streams and assert termination through the route handler.

**Proposal:**
- Add route-level SSE shutdown tests for shutdown-before-connect and shutdown-after-initial-state.
- Wrap both in timeouts so missed shutdown delivery fails loudly.

## Shutdown signal registration errors can look like real shutdown

**Severity:** Medium - `src/main.rs:147-166`. The new `shutdown_signal()` helper ignores `tokio::signal::ctrl_c().await` errors, and on Unix the SIGTERM branch completes immediately if `tokio::signal::unix::signal(...)` returns `Err`.

Those error paths should be diagnostics or startup failures, not successful shutdown triggers. If signal registration fails, the server can exit immediately after startup with little context.

**Current behavior:**
- Ctrl+C signal errors are discarded with `let _ = ...`.
- Unix SIGTERM registration failure makes the `terminate` future complete.
- The `tokio::select!` cannot distinguish a real shutdown signal from a signal-listener setup failure.

**Proposal:**
- Make signal setup fallible during startup and return an error if registration fails.
- Or log the registration/await error and park that branch with `std::future::pending::<()>().await` so it cannot trigger shutdown.

## Final shutdown persist failure exits without retry

**Severity:** Medium - `src/app_boot.rs:270-275`. The normal persist worker records failures and retries with backoff, but a shutdown tick sets `should_exit_after_tick` and breaks after the first final attempt even if that attempt failed.

A transient SQLite lock, disk hiccup, or I/O error during graceful shutdown can still drop pending mutations. The new drain logs the failure, but the process continues toward exit as though the final state reached disk.

**Current behavior:**
- `retry_state.record_result(&result)` records the final failure.
- `should_exit_after_tick` still breaks the loop immediately.
- Pending changed sessions can remain only in memory when the process exits.

**Proposal:**
- On shutdown, exit only after a successful final persist.
- Or use a bounded retry/timeout policy and return/log a shutdown failure outcome that clearly says durability was not confirmed.
- Add a test covering `Err` followed by `Ok` after `PersistRequest::Shutdown`.

## Triplicate `requestStateResync + startSessionHydration` recovery pattern in delta handler

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. Three near-identical recovery sites within ~110 lines of the same handler perform the same `requestStateResync({ rearmOnFailure: true }) + startSessionHydration(delta.sessionId)` pair. The `appliedNeedsResync` branch knows `delta.sessionId` is statically a string; the other two branches add a runtime guard (`"sessionId" in delta && typeof delta.sessionId === "string"`) — the type narrowing is subtly different at each site.

A future fourth recovery branch would need to update three sites; collapsing into a helper subsumes the gate and centralizes the contract comment.

**Proposal:**
- Extract `function triggerRecoveryForDelta(delta: DeltaEvent)` that performs the resync and conditional hydration.
- Replace the three call sites with the helper. Centralize the contract comment.

## Two backend Lagged branches duplicate the lagged-marker emission

**Severity:** Low - `src/api_sse.rs:182-200, 204-215`. The state-receiver and delta-receiver Lagged branches now both yield `lagged` followed by a recovery state snapshot built via `state_snapshot_payload_for_sse(state.clone()).await`. The branches are byte-identical apart from comments. The third Lagged branch (`file_receiver` at line 221) deliberately doesn't recover — so a 2-of-3 helper is still warranted for the asymmetric maintenance risk: a future change that grows one branch (e.g., a tracing log, structured `data` body, or `revision` hint on the marker) needs to be mirrored manually on the other.

**Proposal:**
- Extract a helper that yields the marker + recovery snapshot. The `async_stream::stream!` macro doesn't compose cleanly with helpers that themselves yield, so consider a named local closure or document the invariant explicitly.
- Or, accept the duplication and add cross-referencing comments naming both branches.

## Per-session hydration burst has no cooldown beyond in-flight deduplication

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. The new `startSessionHydration(delta.sessionId)` calls trigger `GET /api/sessions/{id}` (full transcript fetch) on every problematic delta. `hydratingSessionIdsRef` deduplicates concurrent fetches per session, but it does not rate-limit successive fetches: once a hydration completes, the next problematic delta on the same session immediately schedules another full transcript fetch. On a flaky network with bursty deltas, a hydration→delta→hydration loop is possible, each iteration shipping the entire transcript over the wire.

**Current behavior:**
- In-flight dedup via `hydratingSessionIdsRef` collapses simultaneous calls to one round-trip.
- After completion, the next problematic delta immediately schedules another fetch with no cooldown.
- Phase-1 local-only deployment makes this practically free; future remote-host or flaky-network use exposes the storm risk.

**Proposal:**
- Add a per-session cooldown timestamp ("don't re-hydrate the same session within Nms of the last completed hydration unless the new delta carries a revision strictly greater than the one that started the previous hydration").
- Or document the burst as intentional given the local-only deployment cost; add a comment naming the trade-off so future reviewers don't keep flagging it.

## Watchdog-inversion tests don't assert the "Waiting for the next chunk of output…" affordance state

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx:3439` and `ui/src/App.live-state.watchdog.test.tsx:625`. The two recent inverted tests assert that the recovered text becomes visible, but say nothing about the "Waiting for the next chunk of output…" affordance. After the recovery snapshot adopts (the deltas test's snapshot has `status: "idle"`, the watchdog test's stays `status: "active"`), the affordance state is the most user-visible signal of whether recovery actually replaced the wedged UI vs just rendered the recovered text somewhere on the page.

**Proposal:**
- In `deltas.test.tsx`: add `expect(screen.queryByText("Waiting for the next chunk of output...")).not.toBeInTheDocument();` after the assertion that the recovered chunk is visible (recovery snapshot is idle, affordance should disappear).
- In `watchdog.test.tsx`: add an assertion clarifying expected affordance state for the still-active recovery (the assistant chunk now sits at the boundary, so the affordance should NOT be present).

## Rendered Markdown diff navigation does not scroll when there is exactly one change

**Severity:** Low - prev/next buttons can appear to do nothing for the common "one changed block" case.

`MarkdownDiffView` and `RenderedDiffView` intentionally skip the initial scroll so restored parent scroll position is preserved. Navigation scrolls from a `useEffect` keyed on the current index. When there is exactly one change/region, pressing next or previous computes the same index, React bails out of the state update, and the scroll effect does not run. The controls remain enabled but cannot bring the lone target into view.

**Current behavior:**
- Initial mount skips scrolling by design.
- One-change/one-region navigation resolves to the same index.
- No separate "navigation requested" signal exists to force a scroll to the same target.

**Proposal:**
- Drive the scroll side effect from a navigation request counter, or call an explicit scroll helper from the prev/next handlers.
- Cover both `MarkdownDiffView` and `RenderedDiffView` one-target cases.

## Rendered diff region navigation has no explicit scroll-container layout contract

**Severity:** Low - the new region-navigation ref may target a wrapper that is not the actual scroll container.

`RenderedDiffView` introduces `.diff-rendered-view-scroll` and queries it for `data-rendered-diff-region-index` targets, but the changed CSS does not give that wrapper an explicit flex/overflow contract, and the component does not adopt the existing `source-editor-shell source-editor-shell-with-statusbar` layout used by the Monaco and Markdown diff modes. If the parent remains the real scroller, `scrollIntoView()` may work inconsistently and the statusbar can diverge from the rest of the diff editor surface.

**Current behavior:**
- `RenderedDiffView` owns a new internal scroll ref.
- `.diff-rendered-view-scroll` has no explicit overflow/flex sizing.
- The rendered diff footer is not wrapped in the established editor shell/statusbar structure.

**Proposal:**
- Either adopt the existing editor-shell/statusbar layout contract or add explicit CSS that makes `.diff-rendered-view-scroll` the intended scroll container.
- Add a focused layout/navigation regression for rendered-region scrolling.

## Post-commit hardening helpers have no automated production-path coverage

**Severity:** Low - `src/persist.rs:213-227`. `verify_persist_commit_integrity` is `#[cfg(not(test))]`-only because it depends on production SQLite path hardening. The post-commit contract - redirection remains fatal, owner-only chmod/mode verification remains fatal unless `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS` is set - has no direct automated coverage.

**Proposal:**
- Expose a testable seam (e.g., inject the hardening function via a closure or trait), OR
- Add a Linux-only integration test that creates a real chmod-failing scenario.

## Watchdog wake-gap stops-after-progress invariant is not pinned

**Severity:** Low - `ui/src/backend-connection.test.tsx`. No direct negative-case test pins that watchdog wake-gap reconnect probes (which do NOT set `pendingBadLiveEventRecovery`) STOP after same-instance snapshot progress without a data-bearing SSE event. The cause-specific flag's whole premise is that wake-gap probes can stop while parse/reducer-error probes keep polling, but only the polling-continues side is pinned.

**Proposal:**
- Add a regression that triggers a watchdog wake-gap reconnect (no parse/reducer error), receives a same-instance progressed `/api/state` snapshot, advances `RECONNECT_STATE_RESYNC_MAX_DELAY_MS`, and asserts `countStateFetches()` did not increment.

## `app-live-state.ts` reconnect state machine continues to grow

**Severity:** Low - `ui/src/app-live-state.ts:2504 lines`. TS utility threshold (1500) exceeded; new `pendingBadLiveEventRecovery` adds another flag-shaped piece of reconnect bookkeeping. The reconnect/resync state machine inside `useEffect` now coordinates 6+ pieces of cross-cutting state.

**Proposal:**
- Extract a `ReconnectStateMachine` (or similar) module that owns the flag set + transitions and exposes named events (`onSseError`, `onSseReopen`, `onBadLiveEvent`, `onSnapshotAdopted`, `onLiveEventConfirmed`).
- Defer to a pure code-move commit per CLAUDE.md.

## `select_visible_session_hydration_fallback_error` lacks integration coverage

**Severity:** Low - `src/state_accessors.rs:351-369`. Unit tests pin the helper and typed local-miss fallback in isolation, but no integration-style test asserts the public `get_session` path returns 404 to the caller when a recoverable remote error is followed by a `not_found` fallback. A future refactor that drops the selector call from the `or_else` chain would not be caught.

**Current behavior:**
- Selector is unit-tested but the wiring that makes the new behavior reach a caller is not pinned.

**Proposal:**
- Add an integration-style test that drives the public `AppState::get_session` path through a recoverable remote hydration miss followed by a vanished cached summary, and asserts the response is `404 session not found` / `LocalSessionMissing` rather than the original recoverable remote error.

## `useAppSessionActions` ref cluster has grown from 1 to 4 to feed the rejected-action classifier

**Severity:** Medium - `ui/src/app-session-actions.ts:316-356`. `useAppSessionActions` now requires `latestStateRevisionRef`, `lastSeenServerInstanceIdRef`, `projectsRef`, and `sessionsRef` because of the inline `classifyRejectedActionState` call site. The ref count grew from 1 → 4 in a few rounds, all to feed one classifier function.

**Current behavior:**
- Every new evidence dimension for stale action snapshots pushes another ref into this hook.
- App.tsx, the test harness, and the hook signature all need editing whenever a new dimension is added.
- Same anti-pattern the resync-options ref cluster had before extraction.

**Proposal:**
- Pass a single `actionStateClassifierContextRef: MutableRefObject<{ revision, serverInstanceId, projects, sessions }>` (or a memoized snapshot getter) so adding a new evidence dimension does not require touching the hook signature, the caller, and the test harness.
- Defer to a dedicated commit per CLAUDE.md.

## `connectionRetryDisplayStateByMessageId` two-stage memoization is correct but threaded through ~4 stability hops

**Severity:** Medium - `ui/src/SessionPaneView.tsx:858-895`. The retry-display memoization now uses `signature → ref-cached map → useCallback wrapper → useSessionRenderCallbacks deps → MessageCard renderer identity`. The map identity stability invariant is load-bearing for `SessionBody` memoization but only documented sparsely. A future change to retry-display semantics needs to be threaded through ~4 separate stability hops.

**Current behavior:**
- Hand-rolled signature-stable memo bridges to a renderer that already had its own deps tax.
- Reviewers have flagged this as "complex invariant without nearby comments" several rounds in a row.

**Proposal:**
- Extract the signature-stable memo into a small `useStableMapBySignature` hook in a sibling utility module so the pattern is reusable and named.
- Or memoize directly on `(messages, status)` and accept one rebuild per message-list change — `MessageCard` is already memoized below the `SessionBody` memo gate.

## Directory-level state hardening retains a TOCTOU window after symlink check

**Severity:** Low - `src/persist.rs:146-149`. Round-15 carryover. `harden_local_state_directory_permissions` calls `reject_existing_state_directory_redirection_unix` (which uses `fs::symlink_metadata`), then `harden_local_state_permissions(path, 0o700)` — which uses path-based `fs::set_permissions` and `fs::metadata`, both of which follow symlinks. An attacker able to replace the directory between the two calls would get the chmod redirected through the symlink. The matching file path now uses `O_NOFOLLOW + fchmod`, but the directory path has not been migrated.

**Current behavior:**
- File-level chmod is symlink-safe (O_NOFOLLOW + fchmod).
- Directory-level chmod is not.
- Mitigated by Phase-1 single-user threat model (only the user controlling `~/` could plant the symlink).

**Proposal:**
- Open the directory with `O_DIRECTORY | O_NOFOLLOW`, then `fchmod` on the resulting fd; or use `fchmodat(AT_FDCWD, path, mode, AT_SYMLINK_NOFOLLOW)`.

## Non-optimistic user-prompt display causes 100-300ms felt lag on every Send

**Severity:** Medium - `ui/src/app-session-actions.ts:851-895` and `ui/src/app-live-state.ts:1283-1385`. The composer is non-optimistic: clicking Send clears the textarea, fires `await sendMessage(...)`, and then runs `adoptState(state)` against the full `StateResponse` returned by the POST. The "you said X" card only appears after the round-trip plus the heavy `adoptState` walk completes.

`adoptState` re-derives codex, agentReadiness, projects, orchestrators, workspaces, and walks transcripts on the main thread. On a focused active session this lands in the 100-300ms range every send (longer when an active turn is mid-stream). The codebase has already self-diagnosed the path in `docs/prompt-responsiveness-refactor-plan.md` but no optimistic-insert fix has landed.

The lag compounds with two existing tracked bugs ("Focused live sessions monopolize the main thread during state adoption", "Composer drafts have three authoritative stores") but is itself a separable contributor.

**Current behavior:**
- User clicks Send -> textarea clears -> POST fires -> response returns -> `adoptState` walks -> card paints.
- Total delay: round-trip (typically 30-100ms locally) + adoptState (50-200ms on focused live sessions) = visible 100-300ms gap.
- During the gap the session shows neither the user prompt nor the composer text.

**Proposal:**
- Insert an optimistic user-message card in `handleSend` before `await sendMessage(...)`, keyed by a temp id.
- When the POST response arrives or the SSE `messageCreated` delta lands (whichever is first), reconcile by id (swap temp id for server-assigned `messageId`).
- This collapses the round-trip and the adoptState walk out of the felt-lag path simultaneously.
- Cross-link to `docs/prompt-responsiveness-refactor-plan.md` and decide whether this is a standalone fix or folds into the larger refactor.

## `applyDeltaToSessions` duplicates the "lookup first, metadata-only fallback when missing" pattern across five non-created delta types

**Severity:** Low - `ui/src/live-updates.ts:329-599`. The reordered `messagesLoaded === false` branch (apply to in-memory message when present, fall back to metadata-only only when `messageIndex === -1`) is now repeated five times across `messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, and `parallelAgentsUpdate`. The previous code had the same shape duplicated five times in the wrong order; the new code is now the right order duplicated five times. A future sixth retained-non-created delta type will need to re-derive the same flow.

**Current behavior:**
- Each branch independently re-implements `findMessageIndex` -> `if (-1 && !messagesLoaded) metadata-only` -> `if (-1) needsResync` -> type-narrow -> apply.
- The existing duplication is what let the fallback land in the wrong order originally; the next protocol addition has the same cliff.

**Proposal:**
- Extract a `tryApplyMetadataOnlyFallbackForMissingTarget(session, sessionIndex, sessions, delta)` helper (or similar) that centralizes the missing-target/unhydrated decision so each delta type calls a single helper instead of inlining the same branch.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

## Production SQLite persistence is bypassed in the test build

**Severity:** Medium - `src/app_boot.rs:229`. The runtime persistence changes now depend on SQLite schema setup, startup load, metadata writes, per-session row updates, tombstone cleanup, and cached delta persistence, but `#[cfg(test)]` still routes the background persist worker through the old full-state JSON fallback.

Many production SQLite helpers in `src/persist.rs` are `#[cfg(not(test))]`, so existing persistence tests can pass while the real runtime SQLite write/load/delete behavior remains unexercised. The newest post-commit hardening policy (`verify_persist_commit_integrity`, fatal owner-only permission verification, cache invalidation reset, and fatal pre-transaction redirection checks) is part of that production-only surface.

**Current behavior:**
- Test builds bypass `persist_delta_via_cache` and related SQLite write paths.
- Production SQLite load/save helpers are mostly compiled out under `cargo test`.
- Current tests cover retry bookkeeping and legacy JSON fixtures, but not the runtime SQLite persistence contract or the post-commit hardening decisions.

**Proposal:**
- Make the SQLite persistence path testable under `cargo test` with temp database files.
- Add coverage for full snapshot save/load, delta upsert, metadata-only update, hidden/deleted session row removal, and startup load from SQLite.
- Add coverage for post-commit permission failures, cache invalidation reset, and fatal redirection/reparse checks.
- Keep legacy JSON fixture tests separate from production runtime persistence tests.

## `SessionPaneView.tsx` and `app-session-actions.ts` past architecture file-size thresholds

**Severity:** Low - `ui/src/SessionPaneView.tsx` is now 3,160 lines and `ui/src/app-session-actions.ts` is 1,968 lines, both past the architecture rubric §9 thresholds (~2,000 for TSX components, ~1,500 for utility modules). The round-11 extractions of `connection-retry.ts`, `app-live-state-resync-options.ts`, `session-hydration-adoption.ts`, and `SessionPaneView.render-callbacks.tsx`, plus the later `action-state-adoption.ts` split, reduced these files but left them over their respective thresholds.

The companion `app-live-state.ts` entry already exists; this captures the two related Phase-2 candidates that emerged after the round-11 splits.

**Current behavior:**
- `SessionPaneView.tsx` mixes pane orchestration with reconnect-card / waiting-indicator / retry-display orchestration.
- `app-session-actions.ts` still mixes action handlers with optimistic-update and adoption-outcome side-effect wiring.
- Both files now have natural extraction boundaries with their own existing direct unit-test coverage.

**Proposal:**
- Pure code move per CLAUDE.md, in dedicated split commits (one per file).
- For `SessionPaneView.tsx`: candidate is the reconnect-card / waiting-indicator computation cluster.
- For `app-session-actions.ts`: candidate is the optimistic-update + adoption-outcome side-effect cluster now that pure stale target evidence has moved out.

## `App.live-state.deltas.test.tsx` past 2,000-line review threshold

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx`. File is now 3,435 lines and 18 `it` blocks after this round's cross-instance regression coverage, well past the architecture rubric §9 ~2,000-line threshold for TSX files. The header already lists three sibling files split out (`reconnect`, `visibility`, `watchdog`), establishing the per-cluster split pattern.

The newest tests still cluster around hydration/restart races and cross-instance recovery, which is a coherent split boundary. Pure code move per CLAUDE.md.

**Current behavior:**
- Single test file mixes hydration races, watchdog resync, ignored deltas, orchestrator-only deltas, scroll/render coalescing, and resync-after-mismatch flows.
- 18 `it` blocks; the newest coverage adds another cross-instance state-adoption scenario.
- Per-cluster grep tax growing.

**Proposal:**
- Pure code move: extract the 4–5 hydration-focused tests into `ui/src/App.live-state.hydration.test.tsx`, mirroring the sibling-split pattern.
- Defer to a dedicated split commit; do not couple with feature changes.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is now 2,435 lines after this round. The architecture rubric §9 sets a pragmatic ~1,500-line threshold for TypeScript utility modules. The hydration adoption helpers have moved out, but the module still mixes retry scheduling, profiling, JSON peek helpers, and the main state machine.

**Current behavior:**
- Single module mixes hydration matching, retry scheduling, profiling, JSON peek helpers, and the main state machine.
- Per-cluster grep tax growing with each round.

**Proposal:**
- Defer to a dedicated pure-code-move commit per CLAUDE.md.
- Extract `hydration-retention.ts` (or `session-hydration.ts`) containing `hydrationRetainedMessagesMatch`, `SESSION_HYDRATION_RETRY_DELAYS_MS`, `SessionHydrationTarget`, `SessionHydrationRequestContext`, and the matching unit tests.

## `AgentSessionPanel.test.tsx` past 5,000-line review threshold

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx`. File is now 5,659 lines (+511 this round), past the project's review threshold for test files. The added blocks cluster naturally by concern — composer memo coverage, scroll-following coverage, ResizeObserver fixtures — and would extract cleanly into siblings without behavioral change.

The adjacent `App.live-state.*.test.tsx` split (April 20) is the precedent for per-cluster `.test.tsx` files. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `AgentSessionPanel.test.tsx` mixes composer, scroll, resize, and lifecycle clusters.
- Per-cluster grep tax growing with each replay-cache-adjacent feature round.

**Proposal:**
- Pure code move: extract into `AgentSessionPanel.composer.test.tsx`, `AgentSessionPanel.scroll.test.tsx`, `AgentSessionPanel.resize.test.tsx` (matching the App.live-state cluster shape).
- Defer to a dedicated split commit; do not couple with feature changes.

## `src/tests/remote.rs` past the 5,000-line review threshold

**Severity:** Low - `src/tests/remote.rs` is now 9,202 lines after this round's +471-line addition, well past the project's review-threshold for test files. The new replay-cache work clusters cohesively between lines ~2,810 and ~4,040 (the `RemoteDeltaReplayCache` shape helper, the `local_replay_test_remote` / `seed_loaded_remote_proxy_session` / `assert_delta_publishes_once_then_replay_skips` / `assert_remote_delta_replay_cache_shape` / `test_remote_delta_replay_key` helpers, and the `remote_delta_replay_*` tests).

The growth is incremental across many rounds of replay-cache hardening, not a single landing — but extracting the cluster keeps the rest of the file's per-test density manageable. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `src/tests/remote.rs` mixes hydration tests, orchestrator-sync tests, replay-cache tests, and protocol-shape tests.
- Per-cluster grep is harder than necessary; future replay-cache work continues to grow the file.

**Proposal:**
- Extract the replay-cache cluster (lines ~2,810–4,040) into `src/tests/remote_delta_replay.rs` as a pure code move — including the helpers and all `remote_delta_replay_*` tests.
- Defer to a dedicated split commit; do not couple with feature changes.

## `SourcePanel.tsx` is growing along a separable axis

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` grew from ~803 to 1119 lines in this round (+316). It is approaching but has not crossed the ~2,000-line scrutiny threshold. The new responsibility (rendered-Markdown commit pipeline orchestration: collect — resolve ranges — check overlap — reduce edits — re-emit with EOL style) is meaningfully separable from the existing source-buffer/save/rebase/compare orchestration. It has its own state (`hasRenderedMarkdownDraftActive`, `renderedMarkdownCommittersRef`), pure helpers already split into `markdown-commit-ranges`/`markdown-diff-segments`, and a clean parent-callback interface.

**Current behavior:**
- SourcePanel owns two distinct orchestration responsibilities in one component.

**Proposal:**
- No action this commit. Consider extracting a `useRenderedMarkdownDrafts(fileStateRef, editorValueRef, setEditorValueState, ...)` hook in a follow-up, owning `renderedMarkdownCommittersRef`, `hasRenderedMarkdownDraftActive`, `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleRenderedMarkdownSectionDraftChange`.
- The hook would expose a small surface for SourcePanel to consume and keep the file under the scrutiny threshold.

## Metadata-first summaries make transcript search incomplete

**Severity:** Medium - search can silently miss transcript matches for sessions that have only metadata summaries loaded.

`/api/state` now returns session summaries with `messages: []` and
`messagesLoaded: false`. The session search index still walks
`session.messages` directly, so non-visible sessions can be treated as having
no searchable transcript even though the transcript simply has not been
hydrated in this browser view.

**Current behavior:**
- `ui/src/session-find.ts` builds transcript search items from
  `session.messages`.
- Metadata-first session summaries clear `messages` before reaching the
  frontend.
- Search has no "transcript not loaded" state and no on-demand hydration path
  before concluding that there are no message matches.

**Proposal:**
- Gate transcript search to hydrated sessions and surface incomplete results
  when a session summary is not loaded.
- Or hydrate/index target sessions on demand when search needs transcript
  content.
- Add coverage proving metadata-only summaries do not silently produce false
  "no transcript match" results.

## Metadata-first state summaries still broadcast full pending prompts

**Severity:** Low - transcript payloads were removed from global state, but queued prompt text can still ride along with every session summary.

Metadata-first state summaries clear `messages`, but the session summary still
includes full pending-prompt data. Queued prompts can contain user-authored
instructions or expanded prompt content, so this remains a smaller but real
data-minimization leak in `/api/state` and SSE `state` broadcasts.

**Current behavior:**
- `src/state_accessors.rs` builds transcript-free summaries but keeps the full
  `pending_prompts` projection.
- Every listening tab can receive pending prompt content for sessions it is not
  actively hydrating.

**Proposal:**
- Project pending prompts to a bounded metadata-only summary in `StateResponse`.
- Keep full queued-prompt content on targeted full-session responses where the
  active pane actually needs it.

## Hydration retry loop can spam persistent failures

**Severity:** Low - visible-session hydration retries clamp to the last retry delay and can continue indefinitely for persistent non-404 failures.

The new retry loop correctly recovers from stale hydration rejection and transient `fetchSession` failures, but it has no ceiling. A visible metadata-only session whose targeted hydration keeps failing will retry every 3 seconds and repeatedly call the normal request-error reporting path.

**Current behavior:**
- `ui/src/app-live-state.ts` schedules retry delays of 50 ms, 250 ms, 1000 ms, then 3000 ms, and clamps all later retries to 3000 ms.
- Non-404 `fetchSession` failures report the request error and schedule another retry.
- The transient non-404 failure branch is not covered by a regression test.

**Proposal:**
- Cap repeated user-facing error reporting or retry attempts for the same visible session while keeping event-driven or manual recovery possible.
- Add a test where the first `/api/sessions/{id}` request fails with a non-404 error, the retry succeeds, and the transcript appears without a tab switch or unrelated state event.

## Remote test module size slows review and triage

**Severity:** Note - `src/tests/remote.rs` is large enough that focused remote
review now has to scan many unrelated scenarios.

The file contains hydration, delta, orchestrator, proxy, and sync-gap coverage
in one module. New hydration/replay tests are coherent, but keeping every remote
scenario in the same file makes future review targeting and regression triage
harder, especially as the metadata-first remote work continues adding focused
cases.

**Current behavior:**
- Remote tests for several boundaries live in one oversized module.
- New review findings repeatedly point into the same large file, making
  ownership and intended fixture reuse harder to see.

**Proposal:**
- Split remote tests by boundary, for example `remote_hydration.rs`,
  `remote_deltas.rs`, and `remote_orchestrators.rs`.
- Move shared fake-server and remote-session helpers into a small support
  module used by those test files.

## Session store publication can race ahead of React session state

**Severity:** Medium - the new `session-store` publishes some session slices before the corresponding React `sessions` state commits, so the UI can mix newer store-backed session data with older prop-derived session state in one render.

The staged refactor publishes `session-store` updates directly from
`ui/src/app-live-state.ts` and `ui/src/app-session-actions.ts`, while other
parts of the active pane still derive session data from React state in
`ui/src/SessionPaneView.tsx`. That leaves two live sources of truth on slightly
different timelines: `AgentSessionPanel` / `PaneTabs` can read the new store
snapshot immediately, while sibling props such as `commandMessages`,
`diffMessages`, waiting-indicator state, and other session-derived metadata are
still coming from the previous React `sessions` commit.

**Current behavior:**
- `session-store` is synced directly from live-state/action paths before some
  `setSessions(...)` commits land.
- `AgentSessionPanel` and `PaneTabs` read session data from the store.
- `SessionPaneView` still derives other active-session slices from React state,
  so the same active pane can render mixed-version session data within one
  update.

**Proposal:**
- Keep store publication aligned with committed React state, or finish moving
  the remaining active-session derivations in `SessionPaneView` onto the same
  store boundary.
- Document which layer is authoritative during the transition so later changes
  do not deepen the split-brain state model.
- Add an integration test that forces a store-backed session update plus a
  lagging React-state-derived sibling prop and asserts the active pane never
  renders a torn combination.

## Deferred heavy-content activation is coupled into the message-card renderer

**Severity:** Low - `ui/src/message-cards.tsx` now owns deferred heavy-content
activation policy in addition to Markdown, code, Mermaid, KaTeX, diff, and
message-card composition concerns.

The new provider/hook is useful, but keeping the virtualization activation
contract embedded in the same large renderer increases coupling between scroll
policy and message rendering. Future performance fixes will have to reason
through a broad module instead of a small boundary with a clear contract.

**Current behavior:**
- Deferred activation context, heavy Markdown/code rendering, and message-card
  composition live in one large module.
- Virtualization policy reaches into message rendering through exported
  activation context.
- The ownership boundary is not documented near the exported provider.

**Proposal:**
- Extract the deferred activation provider/hook into a focused module with a
  short contract comment.
- Consider extracting the heavy Markdown/code rendering path separately so
  virtualization policy and content rendering can evolve independently.

## `CodexUpdated` delta carries a full subsystem snapshot despite the "delta" name

**Severity:** Medium - `src/wire.rs::DeltaEvent::CodexUpdated { revision, codex: CodexState }` publishes the entire `CodexState` on every rate-limit tick and every notice addition. The architectural contract the codebase otherwise respects is "state events for full snapshots, delta events for scoped changes". `CodexUpdated` is small today (rate_limits + notices capped at 5), but the naming invites future bulky additions to `CodexState` (login state, model-availability maps, per-provider metadata) to be broadcast in full on every tiny change.

**Current behavior:**
- The variant ships a full `CodexState` payload.
- Two publish sites in `src/session_sync.rs` send the complete snapshot even when only the rate limits changed.
- Wire name and shape set a precedent for "delta = tiny changes" that this variant violates.

**Proposal:**
- Split into narrower variants: `CodexRateLimitsUpdated { revision, rate_limits }` and `CodexNoticesUpdated { revision, notices }`. The two call sites in `session_sync.rs` already pick their publish trigger, so split dispatch is straightforward.
- Alternatively, add a source-level comment on the `CodexUpdated` variant stating that `codex` is intentionally the full subsystem snapshot and any future field addition to `CodexState` must reconsider whether a narrower event is needed.

## `DeferredHeavyContent` near-viewport activation now deferred by one paint

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder — content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ~1 paint even when the card is already near viewport on mount.
- The deferral was added as part of the `allowDeferredActivation` cooldown gate (to avoid layout thrash during active scrolls).
- Near-viewport mount activation now produces a one-frame placeholder flicker in place of the previous zero-frame activation.

**Proposal:**
- Use `useLayoutEffect` when `allowDeferredActivation === true` (or for the near-viewport branch generally). Keep the `requestAnimationFrame` in the IntersectionObserver entry path for rapid-entry de-dupe.
- Alternatively, add a targeted comment explaining the deliberate trade-off if the new behavior is intended.

## `"sessionId" in delta` poll-cancel branches are not extensible

**Severity:** Low - `ui/src/app-live-state.ts:1613, 1633` handle delta-event poll cancellations by structurally checking `"sessionId" in delta`. The two `revisionAction === "ignore"` / `"resync"` branches each hard-code the knowledge that only `SessionDeltaEvent` variants carry `sessionId`. Adding a third non-session delta type requires remembering to update both branches, and a new session-scoped delta that uses a different key (e.g. `sessionIds: string[]`) would silently miss both gates.

**Current behavior:**
- Two branches each run `"sessionId" in delta && typeof delta.sessionId === "string"`.
- The `SessionDeltaEvent` exclude type in `ui/src/live-updates.ts:76` exists but is not used here.

**Proposal:**
- Extract a `cancelPollsForDelta(delta: DeltaEvent)` helper that switches on `delta.type` (or uses the same `SessionDeltaEvent` narrowing). Call it from both branches.
- That also centralizes the "which deltas cancel which polls" contract in one place.

## `prevIsActive`-in-render replaced with post-commit effect delays the first-activation measurement pass

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:426-432` converted the `prevIsActive !== isActive` render-time derived-state update into a post-commit `useEffect`. Under the previous pattern, a session switching from `isActive: false — true` flipped `setIsMeasuringPostActivation(true)` during render, so the first frame rendered the measuring shell with the correct `preferImmediateHeavyRender` value. The new effect defers that flip to after commit — the first paint of the newly-active session briefly shows `isMeasuringPostActivation: false`, flipping to the measurement shell only on the next render.

Usually invisible (the effect runs the same tick). Under slow devices this may cause a one-frame flicker on session activation.

**Current behavior:**
- Post-commit effect fires after the first frame of the reactivated session.
- First paint uses `isMeasuringPostActivation: false` regardless of the actual transition.

**Proposal:**
- Restore the render-time pattern: `if (prevIsActive !== isActive) { setPrevIsActive(isActive); ... }` (the established React "derived state" form).
- Or upgrade the effect to `useLayoutEffect` so it runs before paint.
- The P2 task for `key={sessionId}` on the virtualizer supersedes this if that fix lands first.

## Focused live sessions monopolize the main thread during state adoption

**Severity:** Medium - a visible, focused TermAl tab with an active Codex session can spend multiple seconds of an 8 s sample on main-thread work even when no requests fail and no exceptions fire.

A live Chrome profile against the current dev tab showed no runtime exceptions, no failed network requests, and no framework error overlay, but the page still burned about `6.6 s` of `TaskDuration`, `0.97 s` of `ScriptDuration`, `372` style recalculations, and several long tasks above `2 s` while Codex was active. The hottest app frames were `handleStateEvent(...)` in `ui/src/app-live-state.ts`, `request(...)` / `looksLikeHtmlResponse(...)` in `ui/src/api.ts`, `reconcileSessions(...)` / `reconcileMessages(...)` in `ui/src/session-reconcile.ts`, `estimateConversationMessageHeight(...)` in `ui/src/panels/conversation-virtualization.ts`, and repeated `getBoundingClientRect()` reads in `ui/src/panels/VirtualizedConversationMessageList.tsx`. A second targeted typing profile pointed the same way: 16 simulated keystrokes averaged only about `1.0 ms` of synchronous input work and about `11 ms` to the next frame, while `handleStateEvent(...)` alone still consumed about `199 ms` of self time. Narrower composer-rerender and adoption-fan-out regressions have been fixed separately, but the remaining profile still points at broader whole-tab churn.

**Current behavior:**
- A visible, focused active session still produces repeated long main-thread tasks while Codex is working or waiting for output.
- Per-chunk session deltas now coalesce their full-session store publication and broad `sessions` render update to one animation frame, but full state snapshots and transcript measurement still need separate cuts.
- `codexUpdated` deltas and same-value backend connection-state updates are now coalesced or ignored, but snapshot adoption remains the dominant unresolved path.
- Slow `state` events now log per-phase timings in development, so the next profiling round should use the `[TermAl perf] slow state event ...` line to pick the next cut.
- Stale same-instance snapshots now avoid full JSON parse, so the remaining problematic lines should be adopted snapshots or server-restart/fallback snapshots.
- `handleStateEvent(...)` still drives broad adoption work through `adoptState(...)` / `adoptSessions(...)`, transcript reconciliation, and follow-on measurement/render work even after the narrower cleanup fan-out cut.
- `/api/state` resync currently reads full response bodies as text and runs `looksLikeHtmlResponse(...)` before JSON parsing, adding avoidable CPU on large successful snapshots.
- Transcript virtualization still spends measurable time on regex-heavy height estimation and synchronous layout reads, so live session churn compounds with scroll/measure work instead of staying isolated to the active status surface.

**Proposal:**
- Make the live state path more metadata-first so transcript arrays, workspace layout, and per-session maps are not reconciled or pruned when the incoming snapshot did not materially change those slices.
- Split the `/api/state` response handling into a cheap JSON-first path and keep HTML sniffing on a narrow error/prefix check instead of scanning whole successful payloads.
- Cache height-estimation inputs by message identity/revision and reduce repeated `getBoundingClientRect()` passes in the virtualized transcript.
- Re-profile the focused active-session path after each cut and keep this issue open until long-task bursts drop back below user-visible jank thresholds.

**Plan:**
- Start at the root of the profile: cut `handleStateEvent(...)` / `adoptState(...)` work first, because that is where both the passive and targeted rounds spend the most app CPU.
- Break the work into independently measurable slices: state adoption fan-out, `/api/state` parsing path, and transcript virtualization measurement/estimation.
- After each slice lands, rerun the live active-session profile and the focused typing round so reductions in `handleStateEvent(...)` self time, `TaskDuration`, and next-frame latency are verified instead of assumed.

## Composer drafts have three authoritative stores

**Severity:** Medium - committed composer drafts are tracked in React state (`draftsBySessionId`), a mutable ref (`draftsBySessionIdRef`), and the new `useSyncExternalStore`-backed `session-store`, with a post-commit effect mirroring state → ref and imperative paths writing the ref before React commits. Under concurrent draft updates the deferred effect can overwrite a newer ref value with a stale committed one, which then propagates to the composer snapshot via `syncComposerDraftForSession`.

`ui/src/session-store.ts` added a third source of truth for per-session drafts. Imperative handlers in `ui/src/app-session-actions.ts` (`handleDraftChange`, `sendPromptForSession`, queue-prompt flows) and `ui/src/app-workspace-actions.ts` write `draftsBySessionIdRef.current` synchronously before calling `setDraftsBySessionId`, so the store sync reads the fresh value. A separate effect in `ui/src/App.tsx` copies `draftsBySessionId` back into the ref after each commit. When two draft updates land in the same tick, the later-committed effect can briefly regress the ref to an older snapshot, and the store's composer-snapshot slice (`syncComposerDraftForSession`) can publish that stale draft to subscribers.

**Current behavior:**
- Three stores own the same data: React state, the ref, and the `session-store` slice.
- Imperative paths write ref → store before React commits; the effect writes state → ref after commit.
- Under concurrent updates the effect can stomp a newer imperative write with a stale React-committed value.

**Proposal:**
- Pick one owner for the ref: either drop the post-commit effect and rely entirely on imperative writes, or remove the imperative ref mutations and let the store read through a ref that mirrors state exactly once per commit.
- Document the invariant in the `session-store.ts` header so future changes do not reintroduce a third writer.
- Add a regression test that drives two overlapping `handleDraftChange` calls in the same tick and asserts the store snapshot matches the last-written value.

## Composer sizing double-resets on session switch

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:918-931` runs `resizeComposerInput(true)` synchronously inside a `useLayoutEffect` keyed on `[activeSessionId]`, and a following `useEffect` keyed on `[composerDraft]` schedules another resize via `requestAnimationFrame` on the same first render. The rAF resize is redundant because the synchronous one already measured the new metrics.

**Current behavior:**
- Layout effect resets cached sizing state and calls `resizeComposerInput(true)` synchronously.
- Draft effect schedules a second `requestAnimationFrame` resize on the same first render.
- First render of any newly-activated session does two resize passes instead of one.

**Proposal:**
- Track a "just-resized-synchronously" flag set in the layout effect and checked at the top of `scheduleComposerResize`, or gate the draft effect with a prev-draft ref so the "initial draft equals committed" case is a no-op.

## Duplicated `Session` projection types in `session-store.ts` and `session-slash-palette.ts`

**Severity:** Low - `ComposerSessionSnapshot` (`ui/src/session-store.ts:36-83`) and `SlashPaletteSession` (`ui/src/panels/session-slash-palette.ts:51-65`) each re-pick overlapping-but-non-identical field sets from `Session`. Three `Session`-like shapes now exist (`Session`, `ComposerSessionSnapshot`, `SlashPaletteSession`) with no compile-time check that additions to `Session` reach both projections — a new agent setting added to `Session` could silently default to `undefined` in consumers that read through either projection.

**Current behavior:**
- Both projection types declare field lists by hand.
- No `Pick<Session, ...>` derivation; nothing fails to compile when `Session` grows a new field.

**Proposal:**
- Derive both types via `Pick<Session, ...>`, or express `SlashPaletteSession` as `Omit<ComposerSessionSnapshot, ...>` where their field sets differ.
- Colocate the derivations in `session-store.ts` so the projection contract is visible in one place.

## `resolvedWaitingIndicatorPrompt` duplicates `findLastUserPrompt` derivation across `SessionBody` and `SessionPaneView`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:399-404` computes `resolvedWaitingIndicatorPrompt` by calling `findLastUserPrompt(activeSession)` inside `SessionBody` whenever the live turn indicator is showing, overriding the `waitingIndicatorPrompt` prop that `ui/src/SessionPaneView.tsx:795-805` already computed via the same helper and `useMemo`. The override was added to pick up store-subscriber updates between parent renders (correct intent), but it leaves two parallel code paths that must be kept in sync.

Two smaller concerns ride along:
- The override's condition includes an `"approval"` status arm (`status === "active" || status === "approval"`) that is presently unreachable: `SessionPaneView` only sets `showWaitingIndicator=true` when `status === "active"` or (`!isSessionBusy && isSending`), and `isSessionBusy` is true for `"approval"`, so `showWaitingIndicator && status === "approval"` never holds. Harmless defensive check but misleading for readers inferring the truth table.
- The resolution is not wrapped in `useMemo`, so it re-runs on every `SessionBody` re-render — once per streaming chunk. `findLastUserPrompt` scans from the tail, so it usually stops early, but sessions dominated by trailing tool/assistant output could scan deep.

**Current behavior:**
- `SessionBody` (`AgentSessionPanel.tsx:399-404`) and `SessionPaneView` (`SessionPaneView.tsx:795-805`) both derive the waiting-indicator prompt by calling `findLastUserPrompt(activeSession)` on the same store record.
- The override runs on every `SessionBody` render, uncached.
- The `status === "approval"` arm of the override's condition is unreachable under current upstream gating.

**Proposal:**
- Collapse to one computation at the store-subscriber boundary. Either `SessionBody` becomes the sole resolver (drop the `useMemo` and prop passthrough in `SessionPaneView`), or add a one-line cross-reference comment on both sites so future readers know the two are paired.
- Narrow the override's condition to `status === "active"` to match the upstream truth table.
- Wrap the override in `useMemo(() => findLastUserPrompt(activeSession), [activeSession.messages])` to avoid re-scanning on every streaming chunk.

## Conversation cards overlap for one frame during scroll through long messages

**Severity:** Medium - `estimateConversationMessageHeight` in `ui/src/panels/conversation-virtualization.ts` produces an initial height for unmeasured cards using a per-line pixel heuristic with line-count caps (`Math.min(outputLineCount, 14)` for `command`, `Math.min(diffLineCount, 20)` for `diff`) and overall ceilings of 1400/1500/1600/1800/900 px. For heavy messages — review-tool output, build logs, large patches — the estimate is 20–40% under the rendered height, so `layout.tops[index]` for cards below an under-priced neighbour places them inside the neighbour's rendered area. The user sees the cards painted on top of each other for one frame, until the `ResizeObserver` measurement lands and `setLayoutVersion` rebuilds the layout.

An initial attempt to fix this by raising estimates to a single 40k px cap (and adding `visibility: hidden` per-card until measured) was reverted after it introduced two worse regressions: (1) per-card `visibility: hidden` combined with the wrapper's `is-measuring-post-activation` hide left the whole transcript empty for a frame whenever the virtualization window shifted before measurements landed; (2) raising the cap made the `getAdjustedVirtualizedScrollTopForHeightChange` shrink-adjustment huge (40k estimate − 8k actual = −32k scrollTop jump), so slow wheel-scrolling through heavy transcripts caused visible scroll jumps of tens of thousands of pixels. The revert restores the one-frame overlap as the known limitation.

**Current behavior:**
- Initial layout uses estimates that badly under-price long commands / diffs.
- First paint places subsequent cards overlapping the under-priced one for one frame.
- Next frame, `ResizeObserver` fires, `setLayoutVersion` rebuilds, positions correct.
- Visible to the user as a brief "jumble" during scroll.

**Proposal:**
- Proper fix likely needs off-screen pre-measurement (render the card in a hidden measure-only tree, read `getBoundingClientRect` height, then place in the layout) rather than a formula-based estimate. This is a bigger change than a single pure-function tweak.
- Alternative: batch-measurement pass when the virtualization window shifts — hide the wrapper briefly, mount the newly-entering cards, wait for all their measurements, then reveal.
- Not: raise the estimator cap. Large overshoots trade one visible artifact for a worse one.

## Hard kill (SIGKILL, power loss) can still lose the last un-drained persist write

**Severity:** Low - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window — the persist channel carried a full-state clone — so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

The message is not hidden; it is genuinely gone from SQLite. No amount of frontend re-rendering will bring it back.

**Current behavior:**
- Active-turn deltas (e.g., streaming assistant text, `MessageCreated` at the end of a turn) commit through `commit_persisted_delta_locked`, which only signals the persist thread.
- The persist thread acquires `inner` briefly, collects the delta, and writes to SQLite.
- Between "signal sent" and "row written" there is a small time window (usually sub-millisecond, but can stretch under contention) during which a hard kill of the backend loses the mutation.
- On backend restart + SSE reconnect, the browser's `allowRevisionDowngrade: true` adoption path applies the persisted state. The persisted state is missing the un-drained mutation, so the in-memory latest message is overwritten and disappears.

**Proposal:**
- The user-initiated restart path (Ctrl+C / SIGTERM) is now covered by the graceful-shutdown drain — see the preamble.
- For the residual hard-kill case (SIGKILL, power loss): consider opt-in synchronous persistence for the last message of a turn — the turn-completion commit (`finish_turn_ok_if_runtime_matches`'s `commit_locked`) could flush synchronously before returning, trading a few ms of latency on turn completion for zero-loss durability of the final message.
- Or accept and document this as a known Phase-1 limitation in `docs/architecture.md` (background-persist durability contract: at most one un-drained mutation may be lost on hard kill).

## SSE state broadcaster can reorder state events against deltas

**Severity:** Medium - under a burst of mutations, a delta event can arrive at the client before the state event for the same revision, triggering avoidable `/api/state` resync fetches.

Before the broadcaster thread, `commit_locked` published state synchronously (`state_events.send(payload)` under the state mutex), so state N always hit the SSE stream before any follow-up delta N+1. Now `publish_snapshot` enqueues the owned `StateResponse` to an mpsc channel and returns; the broadcaster thread drains and serializes on its own schedule. `publish_delta` remains synchronous. A caller that does `commit_locked(...)?` + `publish_delta(...)` can therefore race: the delta hits `delta_events` before the broadcaster drains state N. The frontend's `decideDeltaRevisionAction` requires `delta.revision === current + 1`; if state N hasn't advanced `latestStateRevisionRef` yet, the delta is treated as a gap and the client fires a full `fetchState`.

**Current behavior:**
- `publish_snapshot` is async (channel + broadcaster thread).
- `publish_delta` is sync.
- Client can observe delta N+1 before state N.
- Extra `/api/state` resync fetches fire under sustained mutation bursts.
- Correctness preserved (resync fixes the view), but behavior is chatty and pushes load onto `/api/state` — which is exactly the path we just made cheaper.

**Proposal:**
- Route deltas through the same broadcaster thread so state and delta events for the same revision stream in order. Coalescing is fine because deltas are idempotent after a state snapshot.
- Or: have `publish_snapshot` synchronously send a revision-only "marker" into `state_events` immediately and let the broadcaster thread serialize and send the full payload; the client's `latestStateRevisionRef` advances on the marker.
- Or: document the tradeoff and rely on the existing `/api/state` resync fallback; track the extra traffic.

## SSE state broadcaster queue can grow before coalescing

**Severity:** Low - bursty commits can enqueue multiple full `StateResponse` snapshots before the broadcaster gets a chance to drop superseded ones.

The broadcaster thread coalesces snapshots only after receiving from its unbounded `mpsc::channel`. During a burst of commits, the sender side can enqueue several large snapshots first, so the "newest only" behavior does not actually bound queued memory or provide backpressure.

**Current behavior:**
- `publish_snapshot` sends owned `StateResponse` values to an unbounded channel.
- The broadcaster drains and coalesces only after snapshots have already queued.
- Full-state snapshots can accumulate during bursts even though older snapshots will be superseded.

- Replace the unbounded queue with a single-slot latest mailbox or bounded channel.
- Drop or overwrite superseded snapshots before they can accumulate in memory.
- Add a burst test that publishes multiple large snapshots while the broadcaster is delayed and asserts only the latest snapshot is retained.

## Implementation Tasks

- [ ] P2: Add reconnect-specific gapped session-delta recovery coverage:
  arm reconnect fallback polling, reopen SSE, dispatch an advancing stamped `textDelta`/`textReplace` across a revision gap, and assert live text renders before snapshot repair while recovery remains pending until authoritative repair succeeds.
- [ ] P2: Add equal-revision gap repair snapshot adoption coverage:
  skip a non-session revision, optimistically apply a later session delta, then return `/api/state` at the same revision and assert the skipped global state is adopted instead of rejected as stale.
- [ ] P2: Add production SQLite persistence coverage:
  make the SQLite runtime persistence path available under `cargo test`, then cover temp-database full snapshot save/load, delta upsert, metadata-only update, hidden/deleted row removal, and startup load.
- [ ] P2: Add Windows state-path redirection coverage:
  cover SQLite main-file symlinks, sidecar symlinks, and `.termal` directory junction/symlink cases behind Windows-gated tests.
- [ ] P2: Add post-shutdown persistence ordering coverage:
  race a late background commit against `shutdown_persist_blocking()` and prove the final persisted state reflects the latest `StateInner`, not an older worker-drained delta.
- [ ] P2: Add concurrent shutdown idempotency race coverage:
  call `shutdown_persist_blocking()` concurrently from two `AppState` clones and assert `persist_worker_alive` cannot flip false until the join owner has returned.
- [ ] P2: Add graceful-shutdown open-SSE coverage:
  cover both shutdown-before-connect and shutdown-after-initial-state through `/api/events`, and assert the stream exits within a timeout so the persist drain is reached.
- [ ] P2: Add shutdown persist failure retry coverage:
  force the final shutdown persist attempt to fail once and then succeed, and assert the worker does not exit before the successful write.
- [ ] P2: Add non-send action restart live-stream delta-on-recreated-stream coverage:
  the round-13 fix proves `forceSseReconnect()` is called on cross-instance `adoptActionState` recovery, but does not dispatch live deltas through the recreated EventSource. Submit an approval/input-style action after backend restart, then dispatch assistant deltas on the new `EventSourceMock` and assert they render in the active transcript bubble.
- [ ] P2: Add live text-repair hydration rendering regression:
  drive the live-state hook or app through text-repair hydration after an unrelated newer live revision and assert the active transcript renders the repaired assistant text without scroll, focus, or another prompt.
- [ ] P2: Add AgentSessionPanel deferred-tail component regressions:
  cover switching from a non-empty deferred transcript to an empty current session, and same-id updated assistant text through the rendered component path (`useDeferredValue`, pending-prompt filtering, and the virtualized list), not only the exported helper.
- [ ] P2: Add lagged-marker EventSource reconnect-boundary regression:
  dispatch `lagged`, trigger EventSource error/reconnect, then send a lower/same-instance state on the new stream and assert the old marker cannot force-adopt it.
- [ ] P2: Add remote hydration dedupe production-path coverage:
  drive bursty same-session remote deltas through the production hydration path, assert only one remote session fetch is issued, and assert the in-flight guard is cleared after successful hydration.
- [ ] P2: Add failed manual retry reconnect-rearm regression:
  cover manual retry hitting a transient failure, then the next scheduled attempt adopting a newer same-instance snapshot while polling still continues until SSE confirms.
- [ ] P2: Add timer-driven reconnect same-instance-progress live-proof regression:
  trigger the non-manual reconnect fallback path, adopt a same-instance `/api/state` snapshot with forward progress while SSE remains unopened/unconfirmed, advance timers, and assert fallback polling continues until a data-bearing live event confirms recovery.
- [ ] P2 watchdog wake-gap stop-after-progress regression:
  trigger watchdog wake-gap recovery, adopt same-instance `/api/state` progress, and assert no additional reconnect polling occurs before a later live event.
- [ ] P2: Cover the index clamp-on-shrink branch in `MarkdownDiffView` and `RenderedDiffView`:
  re-render the parent with a smaller `regions`/`segments` array while `currentChangeIndex`/`currentRegionIndex` points past the new end and assert the counter snaps to "Change/Region 1 of N" while prev/next still wrap correctly. Today the existing prev/next tests only exercise wrap-around at full length; the `current >= changeCount/regionCount` clamp branch in the `useEffect` is unexercised.
- [ ] P2: Add rendered diff render-budget coverage:
  create many Mermaid/math rendered regions and assert the preview applies the same document-level caps as a single `MarkdownContent` document.
- [ ] P2: Add single-target rendered diff navigation coverage:
  assert prev/next scrolls the only Markdown diff change and the only rendered diff region even though the selected index does not change.
- [ ] P2: Route the new lagged-recovery reconnect test through the textDelta fast-path it documents:
  the new `App.live-state.reconnect.test.tsx` test exercises the revision-gap branch (the `messageCreated` delta omits `sessionMutationStamp` so it falls into the resync fallback). Add `sessionMutationStamp` so the delta routes through the matched-stamp fast-path that the surrounding `handleDeltaEvent` comment is most concerned about, OR rename the test to clarify it covers the revision-gap branch specifically and add a sibling test for the textDelta fast-path.
- [ ] P2: Split the bad-live-event + workspaceFilesChanged test into isolated arrange-act-assert phases:
  `ui/src/backend-connection.test.tsx:1225-1261` co-fires the stale `delta` and the `workspaceFilesChanged` event in one `act()`. The assertion `countStateFetches() === hydratedStateFetchCount` is satisfied if either side skips confirmation, so the test cannot pinpoint which side regressed. Dispatch `workspaceFilesChanged` alone first and assert no fetch fired; then add the stale delta separately and re-assert.
- [ ] P2: Add frontend stop/failure delta-before-snapshot terminal-message coverage:
  dispatch cancellation/update deltas before the same-revision snapshot and assert appended stop/failure terminal messages remain rendered without relying on a later unrelated refresh.
- [ ] P2: Add homogeneous conversation-overview segment cap coverage:
  build a long same-kind message run with `maxItemsPerSegment` set and assert the segment policy is either capped or explicitly documented as a mixed-run-only cap.
- [ ] P2: Replace `function scrollIntoView() { ... = this; }` capture pattern in `AgentSessionPanel.test.tsx`:
  `AgentSessionPanel.test.tsx:1515, 1568, 1680` rely on `this`-binding inside a method-form function. Future swc/esbuild config that arrow-rewrites methods or any "use strict" tightening would silently break the capture. Use `vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(function (this: HTMLElement) { scrolledNode = this; })` and rely on `mockRestore()` to clean up.
- [ ] P2: Tighten `expectRequestErrorDeferredUpdatesOnly` to assert deferred-update payloads:
  `app-session-actions.test.ts:158-172` checks shape (every call is a function, never null) but never invokes the deferred functions. A regression where the deferred function returns a stale or `null` next state would still pass. Invoke each captured updater with a fixed `prev` and assert the resulting next state.
- [ ] P2: Add doc-comment to `clear_active_turn_file_change_tracking` enumerating callers and intent:
  the helper is now shared across normal-completion sites (`src/session_interaction.rs`, `src/state_accessors.rs`, etc.) and the two new persist-failure rollback sites (`src/session_lifecycle.rs:450`, `src/turn_lifecycle.rs:455`). A future "preserve grace deadline" tweak motivated by one purpose would silently change the other. Document the unconditional-wipe contract so readers see both intents.
- [ ] P2: Add Rust persist-failure rollback negative-coverage tests:
  `src/tests/session_stop.rs:560`, `src/tests/session_stop_runtime.rs:897` only cover the failure path. Add a sibling test that runs the same setup with succeeding persistence and asserts the post-stop record retains its expected (non-cleared) fields, proving the cleanup is gated on the failure branch and that the helper is not unconditionally clearing on every commit.
- [ ] P1: Add Telegram-relay unit tests for the pure helpers introduced in `src/telegram.rs`:
  cover `chunk_telegram_message_text` (empty, exact-3500-char, under-limit, no-newline-in-window hard-split, newline-in-window soft-split, multi-byte / emoji char-vs-UTF16-unit, trailing-newline preservation), `telegram_turn_settled_footer` for `idle` / `approval` / `error` / unknown-status arms, `telegram_error_is_message_not_modified` against the Telegram error wording, and a serde-decode round-trip for `TelegramUpdate` / `TelegramChatMessage` against a real-shape `getUpdates` JSON snapshot to pin the snake_case contract.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P1: Add direct splitter tests for `splitStreamingMarkdownForRendering(text, { deferAllBlocks: true })`:
  `ui/src/markdown-streaming-split.test.ts` currently has zero direct coverage for `deferAllBlocks`. Cover closed pipe-table followed by prose, closed fence followed by prose, closed math followed by prose, multiple closed blocks (cut at the earliest), nested constructs, and identity behavior on inputs containing no block constructs.
- [ ] P2: Add Mermaid error-SVG detector coverage:
  cover `isMermaidErrorVisualizationSvg` positive cases for `aria-roledescription="error"` and `class="error-icon"`, the "Syntax error in text" behavior, a valid-SVG negative case, and `renderTermalMermaidDiagram` throwing while resetting Mermaid config.
- [ ] P2: Add Mermaid temporary DOM cleanup coverage:
  mock `mermaid.render` so it appends `#d${diagramId}` / `#${diagramId}` nodes to `document.body`, then rejects or returns an error visualization; assert `renderTermalMermaidDiagram` removes the temporary nodes.
- [ ] P2: Add remote marker color validation coverage:
  cover remote full-session localization and `ConversationMarkerCreated` / `ConversationMarkerUpdated` remote deltas with invalid colors, asserting they are rejected, skipped, or normalized consistently with local marker routes.
- [ ] P2: Expand backend marker color rejection coverage:
  parameterize create/patch rejection cases across malformed hex lengths, named colors, gradients, empty/whitespace values, and CSS-like inputs; pin the exact error string or a shared validator error constant.
- [ ] P2: Add marker reconciliation color-integrity coverage:
  reconcile a local marker with an invalid raw color against an authoritative safe marker and assert client state adopts the authoritative value rather than preserving the corrupt local raw value through display fallback equality.
- [ ] P2: Add stale marker response normalized-color coverage:
  in `app-session-actions.test.ts`, return a stale same-instance marker response whose only difference is color casing/normalization and assert the guarded recovery path accepts it without requesting a resync.
- [ ] P1: Pin the "no remount across streaming → settled transition" claim with DOM-identity assertions:
  `ui/src/MarkdownContent.test.tsx:1456` and `ui/src/MessageCard.test.tsx:245-285` assert the rendered shape but do not assert that React preserved the same DOM nodes across the rerender. Capture a stable child DOM node (e.g., a paragraph from the settled prefix) before flipping `isStreaming` to false, then assert `container.contains(savedNode)` and reference equality after the rerender.
- [ ] P2: Restore discrimination in `expectRenderedMarkdownTableContains`:
  `ui/src/App.live-state.deltas.test.tsx:296-315` was relaxed to accept either `.markdown-table-scroll table` or `.markdown-streaming-fragment` after `deferAllBlocks: true` shipped. The relaxation silently passes if a streaming table never settles. Split into two helpers (`expectStreamingTableFragmentContains` for active-streaming phase, `expectSettledTableContains` for after-turn-end) called at the right phases, or advance the test through whatever signal flips `isStreaming` to false before the assertion.
- [ ] P2: Move the Mermaid bundle-URL assertion out of the `appendChild` spy:
  `ui/src/MarkdownContent.mermaid-fallback.test.tsx:54` asserts inside the mock implementation; failure traces point at the spy rather than the test body. The post-render assertions at lines 77-78 (`expect(appendedScripts).toHaveLength(1); expect(appendedScripts[0]?.src).toBe(expectedBundleSrc);`) already pin the contract; delete the inline `expect()`.
- [ ] P2: Pin the heavy-content gate is bypassed during streaming:
  `ui/src/MessageCard.test.tsx:245-284` confirms shape but does not assert `.deferred-markdown-placeholder` is absent during streaming. Add an assertion for an `isStreaming` assistant message regardless of size, and pair with a long-enough streaming message (over the heavy threshold) confirming the gate stays bypassed.
- [ ] P2: Add a wire-projection round-trip test for `TelegramSessionFetchMessage`:
  the parallel narrow projection of `Message` in `src/telegram.rs:481-515` will silently desync if `wire_messages.rs` renames the discriminator or the `Text` variant fields — serde will deserialize into `Other` and the relay will go silent on text messages. Round-trip a representative `Message::Text` payload through `TelegramSessionFetchMessage` and assert the `Text` arm matched.
- [ ] P2: Pin the textReplace no-op short-circuit's identity-preservation contract directly:
  `ui/src/live-updates.test.ts` (no test today). Add a `describe("textReplace no-op short-circuit")` block asserting `expect(result.sessions).toBe(sessions)` (toBe identity, not toEqual) when the delta's `text`/`preview`/`sessionMutationStamp`/`messageCount` all match the session, plus a negative case (text differs by one character) asserting `result.sessions !== sessions`. The 3 fixed integration tests pin only end-state behavior; without a direct unit test, a future refactor of `textReplace` (e.g., always producing a fresh array for hygienic immutability) would silently break the call-site's `result.sessions !== sessionsRef.current` identity check and re-introduce the watchdog-masking regression with no test failure.
- [ ] P2: Add boundary tests for the textReplace no-op 4-field equality predicate:
  `ui/src/live-updates.ts:568-589` checks four equalities (`text`, derived `previewIfApplied`, derived `stampIfApplied`, `messageCount`). Add an `it.each(...)` block in `live-updates.test.ts` covering each equality being violated independently — `delta.preview` undefined vs. defined-and-different, `delta.sessionMutationStamp` undefined / older / newer, `delta.messageCount` differing while text matches — asserting `result.sessions !== sessions` for each violation, plus an all-equal case asserting identity preservation.
- [ ] P2: Add watchdog-mask coverage for non-`textReplace` replayable session-delta types:
  `App.live-state.deltas.test.tsx:2976` covers the textReplace flavor of "watchdog-resyncs when repeated ignored deltas arrive for an active session". Add three sibling tests using `commandUpdate`, `messageUpdated`, and `parallelAgentsUpdate` with the same active-session/no-progress setup. Each should assert the watchdog fires within `LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS + 3000` ms despite repeated identical-content replays. Today the no-op short-circuit only exists for `textReplace`; these tests will fail until the short-circuit is generalized (see "Same-revision-replayable no-op short-circuit covers only `textReplace`" entry above).
- [ ] P2: Add genuine-divergence reconciliation coverage for same-revision unknown-session deltas:
  `ui/src/app-live-state.ts:2666-2687` removed the `requestStateResync` + `startSessionHydration` for `kind: "needsResync"` at the same revision. The justifying comment relies on "the next authoritative state event will reconcile any real divergence" but no test pins this. Add a coverage test that (a) sets the client up with a session list missing `session-X`, (b) dispatches a same-revision session delta for `session-X` (where `latestStateRevisionRef.current === delta.revision`) — asserting NO immediate `/api/state` fetch, then (c) dispatches the next authoritative `state` event including `session-X` and asserts it adopts cleanly. If no backstop exists today, the test will document the gap and force a decision (re-add the resync, or add a deferred reconciliation path).
- [ ] P2: Tighten the delegation batch failure test's captured callback assertions:
  in `ui/src/delegation-commands.test.ts`, assert `resolveFirst` and `rejectSecond` are defined before invoking them and use the shared abort-signal helper for `thirdSignal`, so request scheduling regressions fail at the capture point.
