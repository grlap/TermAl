# Feature Brief: Kimi CLI Integration

Status: future work.

Reference: [`agent-integration-comparison.md`](./agent-integration-comparison.md)

Last checked: 2026-05-02.

## Problem

Kimi is not currently a TermAl agent. TermAl supports Claude, Codex, Cursor, and
Gemini, with Cursor and Gemini sharing the ACP runtime path. Kimi now has a
technical-preview CLI with Agent Client Protocol support, which makes it a good
candidate for a future ACP-backed TermAl integration.

This brief tracks the likely integration path without committing the product to
supporting Kimi until the CLI and protocol behavior are stable enough.

## Current Kimi surface

Official references:

- Kimi CLI: https://platform.kimi.ai/docs/guide/kimi-cli-support
- Kimi API overview: https://platform.kimi.ai/docs/api/overview
- Kimi model list: https://platform.kimi.ai/docs/models
- Coding tool integrations: https://platform.kimi.ai/docs/guide/agent-support

Useful current facts:

- Kimi CLI is in technical preview.
- Kimi CLI supports ACP over stdio via `kimi --acp`.
- Kimi CLI is currently documented for macOS and Linux; Windows support is not
  available yet.
- Kimi API is OpenAI-compatible at `https://api.moonshot.ai/v1`.
- API authentication uses `Authorization: Bearer $MOONSHOT_API_KEY`.
- `kimi-k2.6` is the recommended current model for new Kimi work.
- The older `kimi-k2` series is documented as ending support on 2026-05-25.

## Recommended integration path

Use the shared ACP adapter, not a bespoke OpenAI Chat Completions loop.

```bash
kimi --acp
```

TermAl already has the right shape for this through `AcpAgent::Cursor` and
`AcpAgent::Gemini`: spawn a long-lived CLI process, speak JSON-RPC over stdio,
load or create an agent session, dispatch prompts, map `session/update` events
into TermAl message cards, and surface `session/request_permission` as approval
cards when the server supports it.

The OpenAI-compatible API remains useful for model metadata and documentation,
but it does not by itself provide the coding-agent runtime surface TermAl needs:
workspace tools, permissions, long-lived sessions, cancellation, and ACP-style
updates.

## Proposed scope

- Add `Kimi` to the backend `Agent` enum and frontend `AgentType` union.
- Add `AcpAgent::Kimi` with launch command `kimi --acp`.
- Add agent readiness probing for `kimi`.
- Add session creation UI, session tab labels, avatar, and static fallback
  model option.
- Default new Kimi sessions to `kimi-k2.6` unless the live ACP config reports a
  better default.
- Reuse the ACP session setup path for `initialize`, optional `authenticate`,
  `session/load` or `session/new`, `session/set_model`, `session/prompt`, and
  `session/cancel`.
- Reuse existing model refresh behavior if Kimi exposes a `model` session config
  option through ACP.
- Surface Kimi API key setup in readiness details if the CLI reports missing
  credentials.

## Non-goals

- Do not add a direct OpenAI-compatible Kimi chat runtime in the first pass.
- Do not support Windows until Kimi CLI supports Windows.
- Do not add Kimi-specific model parameter controls such as `thinking` until the
  session settings model can represent provider-specific knobs cleanly.
- Do not rely on deprecated Kimi model aliases such as `kimi-latest`.

## Backend tasks

- Extend `Agent::parse`, `Agent::from_str`, `Agent::name`, `Agent::avatar`, and
  `Agent::default_model`.
- Extend `Agent::acp_runtime` to return `Some(AcpAgent::Kimi)`.
- Extend `AcpAgent` with `Kimi`, including `agent()`, `command()`, and `label()`.
- Add readiness probing in `collect_agent_readiness` and `agent_readiness_for`.
- Add tests for Kimi readiness, agent parsing, and ACP launch command selection.
- Verify that model refresh, config updates, stop, kill, and runtime-exit paths
  remain generic over `AcpAgent`.

## Frontend tasks

- Extend `AgentType` with `"Kimi"`.
- Add Kimi to `NEW_SESSION_MODEL_OPTIONS`, `SESSION_SCOPED_MODEL_AGENTS`, and
  `createSessionModelHint`.
- Add an icon/avatar treatment in `agent-icon.tsx`.
- Add Kimi to create-session controls and readiness display.
- Verify slash `/model` works with either live ACP options or manual model IDs.
- Add targeted UI tests for create-session selection, readiness warnings, and
  model picker fallback.

## Open questions

1. Does Kimi ACP expose the same config shape for `model` as Cursor and Gemini?
2. What authentication methods does `initialize` advertise, and can TermAl give
   useful setup guidance without shelling out to an interactive flow?
3. Does Kimi ACP send structured file edits and command updates that map cleanly
   to TermAl command and diff cards?
4. Is `session/cancel` implemented and reliable enough for TermAl's stop button?
5. Does Kimi CLI persist session IDs in a way that makes `session/load` stable
   across process restarts?
6. Should Kimi-specific `thinking` controls become a general provider-extension
   settings mechanism instead of a one-off session field?
