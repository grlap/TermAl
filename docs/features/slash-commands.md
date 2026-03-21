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

- `/model` for `Claude`, `Codex`, `Cursor`, and `Gemini`
- `/mode` for `Claude`, `Cursor`, and `Gemini`
- `/sandbox` for `Codex`
- `/approvals` for `Codex`
- `/effort` for `Claude` and `Codex`

## Behavior

- Keyboard navigation works inside the palette.
- `Enter` applies the highlighted choice and closes the palette.
- `Space` applies the highlighted choice and keeps the palette open.
- The active choice stays aligned with the real selected session setting after
  live refreshes and setting changes.
- `/model` supports manual model-id entry as well as choosing from the live
  list.
- For live model lists, labels resolve to canonical ids before TermAl stores
  the selection.

## Live model integration

For Claude, Codex, Cursor, and Gemini, the slash palette can:

- trigger live model refresh when the model list is missing
- show inline loading state
- show inline error guidance
- offer retry without leaving the composer

## What is not implemented yet

This is not full cross-agent slash-command parity. TermAl now discovers
Claude's native slash commands from the live runtime when available and falls
back to `.claude/commands` prompt templates, but it still does not have
equivalent native-command discovery for Codex, Cursor, or Gemini.
