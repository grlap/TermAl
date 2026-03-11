# Feature Brief: Session Model Switching

This brief tracks the work needed to let users change models on existing
Claude and Codex sessions.

Backlog source: [`docs/bugs.md`](../bugs.md)

## Problem

`Session.model` exists in both the Rust backend and the TypeScript frontend,
but it is effectively a static label today. It is hardcoded at session
creation and is neither surfaced in the UI nor changeable once a session is
running.

The missing capability is changing the active model after session creation,
similar to `/model` inside Claude Code.

## What already works

- Claude already supports a `set_model` control request mid-session.
- Claude returns available `models` in its initialize `control_response`.
- Codex already syncs a session-scoped `config.toml` in `CODEX_HOME`.
- The runtime command plumbing already has a close template in
  `SetPermissionMode`.

## Model discovery

Claude:
- Parse the `models` field from the initialize `control_response`.
- Cache the list in `AppState`.
- Persist it so the list is available after restart.
- Before any Claude session initializes, show `Default` only.

Codex:
- Read available models from `models_cache.json` inside `CODEX_HOME`.
- Cache and persist the list alongside other app state.

## Backend tasks

- Add `model: Option<String>` to `UpdateSessionSettingsRequest`.
- Add `SetModel(String)` to `ClaudeRuntimeCommand`.
- Handle `SetModel` in the Claude writer thread.
- Add `write_claude_set_model` using the same pattern as
  `write_claude_set_permission_mode`.
- In `update_session_settings`, send `SetModel` to a live Claude runtime and
  update `config.toml` for Codex.
- Update `session.model` in persisted state for both agents.
- Parse and persist Claude model lists from initialize responses.
- Read Codex `models_cache.json` on startup or on first Codex session init.
- Add `GET /api/models` returning `{ claude: [...], codex: [...] }`.

## Frontend tasks

- Add `model?: string` to the session settings update payload.
- Add a `fetchModels` API call for `GET /api/models`.
- Extend `SessionSettingsField` with `"model"`.
- Wire model changes through the existing session settings flow.
- Add model selectors to both Claude and Codex session settings views.

## UX note

Claude can switch models mid-session.

Codex does not expose a mid-turn model switch path through the current TermAl
integration. For Codex, updating the model should modify `config.toml` and
take effect on the next turn. The UI should say that explicitly.
