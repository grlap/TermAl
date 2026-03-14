# Feature Reference: Session Slash Commands

This document describes the slash-command behavior that TermAl currently ships
in the composer.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Implemented for session controls. Native agent slash-command discovery is still
future work.

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

This is not full Claude Code or Cursor command parity. TermAl does not yet
discover and dispatch the agents' native slash commands such as Claude's
`/review`, `/release-notes`, or `/security-review`.

The remaining work is native command discovery and metadata plumbing, not basic
session-control slash behavior.
