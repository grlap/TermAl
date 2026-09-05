# Current Agent Integration Contracts

This is the supported protocol boundary for the adapters described in
[Agent Integration](../architecture.md#agent-integration). It is a capability
contract, not a promise to support every past or future build of an upstream
CLI. Unsupported methods or missing capabilities are not an invitation to
probe an older protocol or guess a request value.

## Baselines and evidence

| Integration | Baseline | Evidence and scope |
| --- | --- | --- |
| Shared ACP | Integer `protocolVersion: 1` | [ACP initialization](https://agentclientprotocol.com/protocol/v1/initialization), [session setup](https://agentclientprotocol.com/protocol/v1/session-setup), and [permission options](https://agentclientprotocol.com/protocol/v1/tool-calls#permission-options). |
| Cursor ACP | `2026.03.11-6dfa30c` | Local version check and initialize-only stdio probe on 2026-09-05: protocol version 1 and `agentCapabilities.loadSession: true`; exact capability response below. |
| Gemini ACP | `0.40.1` | Local `gemini --version` and installed CLI bundle inspection on 2026-09-05: `initialize` advertises `loadSession: true`; permission options carry typed kinds. |
| OpenCode ACP | `1.18.8` | Repository-recorded live wire probe in [OpenCode integration](opencode-acp-integration.md#acp-lifecycle), including its continuity-error limits. |
| Claude CLI | Numeric result-status contract documented since `2.1.220`; current inspected CLI `2.1.261` | `src/claude_spawn.rs` and the transient-result fixtures in `src/tests/claude.rs`; local `claude --version` on 2026-09-05. The number is not a substitute for the required fields below. |
| Codex app-server | `0.153.4`, v2 method/schema contract | Local `codex --version` and `codex app-server generate-json-schema --experimental --out <scratch-directory>` on 2026-09-05. The generated `v2/ModelListResponse.json`, `ThreadStartParams.json`, `ThreadResumeParams.json`, and `TurnStartParams.json` define the fields below. [Official app-server documentation](https://learn.chatgpt.com/docs/app-server) describes initialization, model discovery, and typed item events. |

Generated schemas are reproducible diagnostic artifacts, not a second checked-in
protocol implementation. The public Codex guide does not fully enumerate the
service-tier fields; the versioned CLI's generated schema supplies that evidence.
No provider prompt is needed to generate it. Version checks and schema inspection
are not claims of an end-to-end model turn on every platform.

The installed Cursor build's `acp` process returned this exact
`agentCapabilities` object to an ACP v1 `initialize` request in an empty
repository-local scratch directory on 2026-09-05:

```json
{
  "loadSession": true,
  "mcpCapabilities": { "http": true, "sse": true },
  "promptCapabilities": { "audio": false, "embeddedContext": false, "image": true }
}
```

The response's `protocolVersion` was `1`. The probe sent no session creation,
continuation, or model prompt, and its process was terminated and reaped after
capture. This proves the installed build advertises the load capability; it
does not substitute for an end-to-end continuation test. OpenCode's evidence
remains the dated repository-recorded 1.18.8 probe, not a new local invocation.

## ACP: advertised capabilities and typed permissions

TermAl sends ACP v1 `initialize` before session requests and reads capabilities
only from `agentCapabilities`. For an existing external session, an advertised
`sessionCapabilities.resume` object selects `session/resume`; otherwise explicit
`loadSession: true` selects `session/load`. Load is a current v1 method, not a
legacy fallback. Missing flags mean unsupported, never optimistic probing. A
saved conversation must not be replaced merely because the current process
cannot advertise or execute its continuation method.

`session/new` creates a new conversation when no external id is present. A
method-not-found response from an advertised method is a protocol failure, not
permission to downgrade capabilities and start another conversation. Load/resume
failures preserve the stored id. An invalid-id recovery requires a proven,
agent-specific current discriminator; generic error codes, arbitrary nested
reason strings, and human-readable error prose do not establish that authority.
The recorded OpenCode probe explicitly establishes no such discriminator.
Gemini 0.40.1's installed ACP handler converts ordinary loadSession errors into
generic internal-error `data.details`; its session selector's "Invalid session
identifier" text is prose, not a typed recovery contract. TermAl therefore keeps
the id and error rather than inferring authority from that wording.

`session/load` replays historical `session/update` notifications before its
response; TermAl suppresses that replay in its already-resident transcript.
`session/resume` does not replay. MCP descriptors use `env: [{name, value}]` in
both requests. Prompt and cancellation remain `session/prompt` and the
`session/cancel` notification.

Permission selection uses exact `kind` values: `allow_once`, `allow_always`,
`reject_once`, and `reject_always`. The response returns the exact advertised
`optionId`. Names and ids are display/opaque data, not fuzzy permission hints;
one-time approval must never select permanent approval because a label resembles
"allow". Missing usable typed options cannot authorize a tool. This contract is
for Cursor, Gemini and OpenCode; it does not change Claude's separate
`can_use_tool` question/permission handling.

Reject prefers `reject_once` but falls back to an advertised `reject_always`.
That fallback may persist the refusal according to the agent's policy; it is
deliberate on the deny side so Reject can still prevent the requested effect.
It never expands authorization. Approval has no equivalent escalation from
one-time to permanent permission.

See [Cursor](cursor-cli-integration.md), [Gemini](gemini-cli-integration.md),
[OpenCode](opencode-acp-integration.md), and the
[integration comparison](agent-integration-comparison.md).

## Claude: structural transient-error status

The retry classifier accepts only stream-json terminal objects with
`type: "result"`, `is_error: true`, and integer `api_error_status` equal to 429,
503, or 529. Missing, null, malformed, or different status values remain terminal,
even when `result` contains text such as `API Error: 529`. Result prose is not
parsed to recover a missing field.

The existing five-attempt backoff, pre-effect replay-safety latch, prompt/runtime
generation checks, and visible terminal errors remain authoritative. Numeric
status alone never makes a prompt safe to repeat after output or a tool boundary.
The [Claude architecture](../architecture.md#claude-code) separately defines
`can_use_tool`, `AskUserQuestion`, unattended behavior, and user Skip; this cleanup
does not introduce or modify a second question transport.

## Codex: typed events and catalog-only Fast dispatch

After `initialize` / `initialized`, TermAl uses `thread/start` or `thread/resume`
and `turn/start`. Transcript text comes from `item/agentMessage/delta` and the
authoritative `item/completed` agent-message payload, scoped by thread, turn and
item identity. Older `codex/event/agent_message*` mirrors are not another text
source. These mirrors follow the general notification-handler policy; no
dedicated legacy text branches remain.

The current `model/list` response is paginated using `data` and `nextCursor`.
Each model's `serviceTiers` contains `{id, name, description}` entries. The
generated schema marks `additionalSpeedTiers` deprecated in favor of those
entries: it is not authority to synthesize an unadvertised tier id.

| TermAl choice | Outgoing `serviceTier` |
| --- | --- |
| Standard | Explicit `null` on `thread/start`, `thread/resume`, and `turn/start`, clearing an inherited tier. |
| Fast with current advertised capability | The exact active model's advertised Fast tier id, preserving its case. |
| Fast without a loaded catalog | Request the live catalog before any thread/turn request, then resolve again. |
| Fast still unresolved | Fail before thread/turn submission with a visible retryable explanation and the `/fast` or settings Standard escape hatch. |

Omission, null, and a string are distinct request states; Fast must never become
omitted/null merely because its lookup failed. Dispatch-time discovery failure
or a missing model/tier does not clear the session's persisted Fast choice.
A refreshed catalog is published only if its runtime and admitted turn still
match; Stop, restart, or a newer turn discards the stale result. Standard and an
already resolved tier require no dispatch-time discovery. The blocking REPL
path obeys the same catalog requirement. No hard-coded `priority` guess remains.
Separate frontend settings and model-canonicalization policy is unchanged.

Unresolved Fast delivery runs on an independent worker, including when a
completion callback dispatches a queued or orchestrator turn. The shared stdout
reader must remain free to route the `model/list` response that worker needs.
Discovery failure records a guarded destination-session error, not a fatal
shared-reader error. Superseded delivery leaves mailbox recovery to the current
Stop/terminalization owner; a stale worker must not requeue a newer wake.

See [shared app-server ownership](shared-codex-app-server.md),
[model switching](model-switching.md), and the
[Codex architecture](../architecture.md#codex).
