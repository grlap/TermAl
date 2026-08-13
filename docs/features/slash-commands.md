# Feature Reference: Session Slash Commands

This document describes the slash-command behavior that TermAl currently ships
in the composer.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Implemented for session controls and Claude-native slash commands.

## What TermAl supports today

Typing `/` in the composer opens a session-control palette. The palette is
scoped to the active session and uses the same settings APIs as the Prompt tab.

Supported commands:

- `/model` for `Claude`, `Codex`, `Cursor`, `Gemini`, and `OpenCode`
- `/mode` for `Claude`, `Cursor`, `Gemini`, and `OpenCode`
- `/sandbox` for `Codex`
- `/approvals` for `Codex`
- `/effort` for `Claude` and `Codex`
- `/fast` for `Codex`
- `/mcp` and `/mcp verbose` for `Codex`

## Behavior

- Keyboard navigation works inside the palette.
- `Enter` applies the highlighted choice and closes the palette.
- `Space` applies the highlighted choice and keeps the palette open.
- The active choice stays aligned with the real selected session setting after
  live refreshes and setting changes.
- `/model` supports manual model-id entry for Claude, Codex, Cursor, and
  Gemini. OpenCode accepts only its live ACP model options plus Auto; see
  [OpenCode ACP Integration](./opencode-acp-integration.md).
- For live model lists, labels resolve to canonical ids before TermAl stores
  the selection.
- `/mcp` is a local status command, not a model prompt. It reads the owning
  Codex app-server's configured MCP servers, authentication state, and tool
  counts. `/mcp verbose` also shows individual tool names and descriptions.
  Remote sessions proxy the read to the TermAl instance that owns the Codex
  runtime.

## Live model integration

For Claude, Codex, Cursor, Gemini, and OpenCode, the slash palette can:

- trigger live model refresh when the model list is missing
- show inline loading state
- show inline error guidance
- offer retry without leaving the composer

## What is not implemented yet

This is not full cross-agent slash-command parity. TermAl now discovers
Claude's native slash commands from the live runtime when available and falls
back to `.claude/commands` prompt templates, but it still does not have
equivalent native-command discovery for Codex, Cursor, Gemini, or OpenCode.
The dedicated Codex `/mcp` status surface is implemented directly against the
documented app-server API and does not imply general Codex native-command
discovery.

Project command templates and future Claude skills are owned by the backend
agent-command resolver, not by this session-control layer. See
[`agent-slash-commands.md`](agent-slash-commands.md) for `$ARGUMENTS`, optional
`Additional User Note` handling, and the shared regular-send/delegation
resolution contract.
