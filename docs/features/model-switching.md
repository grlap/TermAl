# Feature Reference: Session Model Switching

This document describes the current session-scoped model controls in TermAl.

Protocol baseline: [Current Agent Integration Contracts](current-agent-contracts.md).

## Status

Implemented for `Claude`, `Codex`, `Cursor`, `Gemini`, and `OpenCode`.

Model selection is no longer a create-dialog setting for those agents. New
sessions start on the agent default, then TermAl loads the live model list from
the session itself and lets the user switch from the Prompt tab or the slash
palette.

## Core UX

- `Prompt` settings cards own model selection for Claude, Codex, Cursor,
  Gemini, and OpenCode.
- `/model` in the composer opens the same session-scoped model controls.
- Orchestrator template session cards use the same themed model combobox. A
  template node can keep "Assistant default" or pin a specific model for the
  sessions created from that template.
- New sessions automatically request their model list as soon as the session is
  created or opened.
- Every supported agent exposes a `Refresh models` action in the session card.
- Manual model-id entry is supported for Claude, Codex, Cursor, and Gemini.
  OpenCode model ids come from its dynamic ACP config list, with Auto as the
  agent-authoritative selection.
- If the selected model is not in the current live list, TermAl warns before
  sending the next prompt and requires a second send to continue.

## Per-agent behavior

### Claude

- Model options come from the live Claude session.
- Model changes are applied to the running session with Claude's `set_model`
  control request.
- Session mode is also session-scoped: `ask`, `auto-approve`, or `plan`.
- Claude effort is session-scoped as well: `default`, `low`, `medium`, `high`,
  `xhigh`, or `max` when the current model supports it.
- Effort changes apply on the next Claude prompt because the Claude runtime has
  to restart with the new `--effort` setting.

### Codex

- Model options come from Codex app-server `model/list`.
- Model, sandbox, approval policy, reasoning effort, and Fast mode are all
  session-scoped.
- Those settings apply on the next Codex prompt.
- `auto-approve` is a TermAl-managed approval policy, distinct from Codex's
  native `never`. TermAl starts the Codex turn with native `on-request` and
  immediately accepts command-execution, file-change, and permissions approval
  requests. Structured user input, MCP elicitation, and generic app-server
  requests remain interactive. An active read-only delegation always wins over
  AutoApprove and receives the normal decline response instead of an approval.
- Fast mode appears only when the active model's `model/list` entry advertises
  the Fast service tier. TermAl sends the exact tier id advertised by the
  catalog; Standard sends an explicit null so a previously sticky tier is
  cleared. If a persisted Fast choice cannot be resolved, dispatch requests the
  live catalog first. An unavailable catalog or missing tier fails visibly,
  preserves Fast, and offers retry or Standard through `/fast` or settings.
- `/fast` opens the same Standard/Fast choice in the composer. Switching to a
  model that does not advertise Fast safely resets the session to Standard.
- Reasoning-effort options are filtered by the selected model's supported
  capabilities.
- If a model change forces reasoning effort to normalize, TermAl updates the
  session and shows an inline notice explaining the reset.

### Cursor

- Model options come from Cursor ACP session config.
- Model changes are pushed to the live session with
  `session/set_config_option`.
- Session mode is session-scoped: `agent`, `plan`, or `ask`.

### Gemini

- Model options come from the Gemini ACP session.
- Model selection is session-scoped and uses the live session model list.
- Gemini approval mode is also session-scoped: `default`, `auto_edit`, `yolo`,
  or `plan`.
- Approval-mode changes apply on the next Gemini prompt and may require the ACP
  runtime to restart cleanly.

### OpenCode

- Model, reasoning-variant (`effort`), and mode options come from OpenCode ACP
  session config. Variants remain dynamic because they are model-specific.
- Auto is OpenCode-authoritative and follows its current effective value.
- Explicit TermAl choices are re-applied in model-then-variant-then-mode order
  and acknowledged after session new/resume/load before the next prompt.
- A live model change waits for model-specific variant and mode options. If a
  carried dependent choice is no longer offered, is rejected, or cannot be
  validated because refreshed options do not arrive, only that choice resets
  to Auto; the accepted model change remains in place and TermAl shows a
  recovery notice.
- If an explicit saved choice disappears, TermAl persists Auto, adopts the
  current OpenCode value, and shows a visible recovery notice.
- See [OpenCode ACP Integration](./opencode-acp-integration.md) for the
  continuity, acknowledgement, permission, and delegation contracts.

## Validation and recovery

- Known model labels are normalized to the live model id before they are stored.
- Manual model ids that are not in the current list are still allowed, but the
  UI calls that out explicitly.
- Refresh failures are rewritten into agent-specific guidance instead of raw
  transport errors.
- Orchestrator template model choices are design-time defaults. Runtime session
  model refresh still happens on the actual created session after launch.

## Remaining gaps

- Richer visual treatment for recommended/default models and capability hints.
- Deeper end-to-end coverage for create -> refresh -> manual model -> first
  prompt flows.
- More agent-specific recovery actions when model refresh fails because of
  install, auth, or runtime state problems.
