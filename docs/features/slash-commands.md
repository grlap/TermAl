# Feature Brief: Slash Commands

This brief tracks parity work for Claude-style slash commands in the TermAl
composer.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Problem

Claude Code exposes slash commands such as `/review`, `/release-notes`,
`/security-review`, `/simplify`, and `/batch`. TermAl has no equivalent
command discovery or picker UI today.

## Protocol behavior

Claude's initialize response already carries command metadata in the `system`
event with `subtype: "init"`.

Relevant fields:
- `commands`
- `models`
- `pid`

TermAl currently only extracts `session_id` from that event and drops the rest.

## Dispatch model

Slash commands do not need a special transport path. They are sent as normal
user messages. The missing work is client-side discovery and selection.

That means the feature breaks down into:

1. Discovery: parse available commands from the init response.
2. UI: show a picker when the user types `/`.
3. Dispatch: send the chosen command text as a normal user message.

## Backend tasks

- Parse `commands` in `handle_claude_event` alongside `session_id`.
- Store commands per session or in a shared app cache.
- Expose commands through the state API or a dedicated `GET /api/commands`
  endpoint.
- Persist the discovered command list so it survives restart.

## Frontend tasks

- Add a `SlashCommand` type.
- Detect `/` at the start of composer input.
- Show a filtered command picker with keyboard navigation.
- Support fuzzy filtering such as `/re` -> `/review`.
- On selection, insert the command text into the composer or send it
  immediately.

## Open question

Codex may or may not have an equivalent slash command model. If it does, the
metadata would likely come from the app-server initialize flow or from cached
Codex metadata. That should be verified before building a cross-agent UI.
