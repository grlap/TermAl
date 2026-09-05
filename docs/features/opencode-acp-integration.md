# Feature Reference: OpenCode ACP Integration

This document describes OpenCode as a first-class TermAl agent through
OpenCode's Agent Client Protocol server.

Supported baseline and shared capability/error rules:
[Current Agent Integration Contracts](current-agent-contracts.md#acp-advertised-capabilities-and-typed-permissions).

## Status

Implemented through the shared ACP runtime. OpenCode sessions participate in
normal session creation, persistence, resume, model selection, mode selection,
approval cards, cancellation, delegation, and UI state projection.

## Runtime and setup

TermAl launches one process per OpenCode session:

```bash
opencode acp
```

The executable must be available on `PATH`. Readiness resolves the command
without launching a diagnostic subprocess; a missing executable blocks session
creation with install guidance. Provider readiness cannot be proven safely from static local files,
so an installed CLI is reported as ready and the first real prompt verifies
provider access. If that prompt reports an authentication failure, run:

```bash
opencode auth login
```

or configure the selected provider's credentials.

## ACP lifecycle

TermAl initializes ACP v1 over JSON-RPC stdio and then uses:

- `session/new` for a new OpenCode conversation;
- `session/resume` when OpenCode advertises resume support and TermAl has a
  persisted external session id;
- the shared ACP `session/load` compatibility path only for agents that do not
  advertise resume but support the older load capability;
- `session/prompt` for turns;
- the `session/cancel` notification for user cancellation.

The inline delegation MCP descriptor follows the ACP `McpServer` schema:
`env` is an array of `{name, value}` entries. Claude and Codex use environment
maps in their native MCP configuration formats, so TermAl converts that map at
the shared ACP boundary used by Cursor, Gemini, and OpenCode. Protocol-shape
changes must include a live smoke against the real external binary; fixtures
owned by TermAl are not sufficient evidence because both sides can encode the
same incorrect assumption.

OpenCode 1.18.8 exposes no typed invalid-session discriminator safe enough to
authorize discarding continuity. A live wire probe on 2026-07-29 observed:

- `session/load` for an unknown id: `-32603`, message
  `Internal error: OpenCode service failure`, data `{"service":"session"}`;
- `session/prompt` for an unknown id: `-32602`, prose
  `Invalid params: session not found: <id>`, with only the echoed
  `{"sessionId":"<id>"}` in data;
- after `session/new` then `session/close`, both `session/load` and
  `session/resume` still succeed and return the session config options.

Neither generic service metadata nor prose is a stable typed contract. TermAl
therefore surfaces every OpenCode load/resume failure and preserves the exact
stored id. It never silently starts a replacement conversation under the same
transcript. The failed transcript remains readable as an archive, and the
error explicitly directs the user to create a new OpenCode session to start
fresh.

Cancellation is a fire-and-forget ACP notification. TermAl then gives the active
prompt up to two seconds to settle; that observed settlement, rather than a
protocol acknowledgement, completes the graceful stop. If the process cannot
settle within the bounded grace, TermAl terminates it but retains the external
session id: local process termination does not prove the agent-side session is
invalid.

## Model, reasoning-variant, and mode authority

OpenCode advertises dynamic `model`, reasoning-variant (`effort`), and `mode`
config options in its ACP session response and config-update notifications.
TermAl stores both the selected authority and the effective value. Reasoning
variants are never hard-coded because their available values depend on the
selected model:

- `auto` is OpenCode-authoritative. TermAl adopts the current value and does not
  emit `session/set_config_option`.
- An explicit TermAl selection is TermAl-authoritative. After every new,
  resume, or load handshake, TermAl re-applies the model first, waits for its
  acknowledgement, then re-applies the reasoning variant and mode in order,
  waiting for each acknowledgement before allowing the prompt.
- If OpenCode rejects an explicit model, reasoning variant, or mode, TermAl
  adopts the agent's current value, emits a visible session notice, and keeps
  the session running. Transport failure remains runtime-fatal.
- Every handshake or live config-options payload reconciles only the option
  lists it actually includes. OpenCode-side drift therefore cannot silently
  replace an explicit TermAl choice, while a model-only payload preserves the
  absent reasoning-variant and mode authority and option lists (and vice
  versa).
- `Refresh models` performs a controlled local ACP runtime restart and resumes
  the same persisted OpenCode session. ACP has no standalone config-query
  method, so the fresh resume/load handshake is the authoritative way to fetch
  updated option lists; refresh is disabled during active interactions.
- A user-initiated live change uses the same serialized writer, waits for the
  tracked `session/set_config_option` response, and commits the selected
  authority only after OpenCode accepts it. A standalone option rejection
  leaves the prior authority intact. After a model has already been accepted,
  however, a dependent reasoning variant or mode that disappears, is rejected,
  or cannot be validated because refreshed options do not arrive is reset
  individually to `auto`; TermAl keeps the model change, adopts the latest
  reported effective value when available, and emits a visible session notice.
  If the post-model option refresh times out, the unavailable option list is
  cleared rather than presented as authoritative and the notice directs the
  user to `Refresh models`, whose controlled resume handshake reloads the
  model-specific choices.
  Transport failure tears down the runtime so the next prompt must reconcile
  before dispatch. Scheduling admission and protocol acknowledgement share one
  55-second request deadline, with scheduling bounded to the first five seconds
  and four seconds reserved for post-model option discovery. If scheduling
  expires while a prompt still owns the writer, the queued change is discarded
  so it cannot land after the API has told the caller to retry.
- If an explicit saved value disappears from the live option list, TermAl
  switches that selection to `auto`, adopts OpenCode's current value, persists
  the recovery, and adds a visible assistant notice.

The Prompt tab and slash palette expose the live model, reasoning-variant, and
mode options, including explicit Auto entries and the effective values reported
by OpenCode. The app-level OpenCode model default applies to new sessions;
`default` leaves new sessions on OpenCode Auto.

## Permissions

OpenCode `session/request_permission` calls use the shared ACP approval path.
TermAl preserves the exact OpenCode option ids for allow-once, allow-always, and
reject decisions. Only exact ACP v1 option kinds select permission outcomes;
names and opaque option ids never grant permission authority.
Multiple requests are kept in arrival order: a later card cannot overtake the
queue head, and the session remains in Approval until every pending request is
resolved.

OpenCode orchestrator nodes do not expose Auto-approve. OpenCode permission
requests always remain visible approval cards.

## Delegation boundary

OpenCode supports normal and `isolatedWorktree` delegation sessions. It does
not support `readOnly` delegation: OpenCode does not currently give TermAl a
hard shared-worktree read-only enforcement boundary. The backend rejects that
agent/policy combination before creating a child session or runtime, through
both the REST route and the TermAl delegation MCP tool.

Callers that need OpenCode review should use an isolated worktree. Callers that
require the exact read-only reviewer contract should select an agent whose
runtime supports that policy.

## Repository instructions

OpenCode owns discovery of repository instruction files such as `AGENTS.md` and
`CLAUDE.md`. TermAl sends only the resolved user/task prompt through ACP and
does not duplicate instruction-file contents into that prompt.

## Deliberate v1 exclusions

- No hidden OpenCode spare or warm-process pool.
- No static provider-credential inspection in readiness.
- No silent replacement session after an ambiguous resume failure.
- No destructive in-place continuity reset. Start a new OpenCode session when
  the archived session cannot resume.

## Related references

- [Session Model Switching](./model-switching.md)
- [Slash Commands](./slash-commands.md)
- [Agent Delegation Sessions](./agent-delegation-sessions.md)
- [Architecture](../architecture.md)
