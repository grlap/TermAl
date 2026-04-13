# Feature Reference: Session Model Switching

This document describes the current session-scoped model controls in TermAl.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Implemented for `Claude`, `Codex`, `Cursor`, and `Gemini`.

Model selection is no longer a create-dialog setting for those agents. New
sessions start on the agent default, then TermAl loads the live model list from
the session itself and lets the user switch from the Prompt tab or the slash
palette.

## Core UX

- `Prompt` settings cards own model selection for Claude, Codex, Cursor, and
  Gemini.
- `/model` in the composer opens the same session-scoped model controls.
- Orchestrator template session cards use the same themed model combobox. A
  template node can keep "Assistant default" or pin a specific model for the
  sessions created from that template.
- New sessions automatically request their model list as soon as the session is
  created or opened.
- Every supported agent exposes a `Refresh models` action in the session card.
- Manual model-id entry is supported for all four agents.
- If the selected model is not in the current live list, TermAl warns before
  sending the next prompt and requires a second send to continue.

## Per-agent behavior

### Claude

- Model options come from the live Claude session.
- Model changes are applied to the running session with Claude's `set_model`
  control request.
- Session mode is also session-scoped: `ask`, `auto-approve`, or `plan`.
- Claude effort is session-scoped as well: `default`, `low`, `medium`, `high`,
  or `max` when the current model supports it.
- Effort changes apply on the next Claude prompt because the Claude runtime has
  to restart with the new `--effort` setting.

### Codex

- Model options come from Codex app-server `model/list`.
- Model, sandbox, approval policy, and reasoning effort are all session-scoped.
- Those settings apply on the next Codex prompt.
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
