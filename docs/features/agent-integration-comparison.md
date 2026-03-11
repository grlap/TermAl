# Agent Integration Comparison

Cross-agent reference for TermAl adapter design.

## Protocol and transport

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Wire protocol | NDJSON (custom) | JSON-RPC 2.0 | `stream-json` stdout or ACP (experimental) | ACP (JSON-RPC 2.0) |
| Transport | stdin/stdout only | stdin/stdout or WebSocket | Spawned CLI stdout or stdio (ACP) | stdin/stdout only |
| Bidirectional | Yes | Yes | No (stream-json) / Yes (ACP) | Yes |
| Server mode | No | `codex app-server` | `--experimental-acp` | `cursor agent acp` |
| Best TermAl integration | Spawn child and speak NDJSON over stdio | Spawn app-server and speak JSON-RPC over stdio | Spawn CLI with `--output-format stream-json` (or ACP when stable) | Spawn `agent acp` and speak JSON-RPC over stdio |

## Session persistence

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Storage format | Append-only `.jsonl` | JSONL rollout plus SQLite cache | Single `.json` per session | Managed by Cursor |
| Location | `.claude/sessions/{id}.jsonl` | `~/.codex/sessions/YYYY/MM/DD/rollout-{id}.jsonl` | `~/.gemini/tmp/<project>/chats/session-{ts}-{id}.json` | Managed internally |
| Resume mechanism | `--resume <session-id>` | `thread/resume` | `--resume <index\|latest>` (CLI) or `session/load` (ACP) | `session/load` |
| Session discovery | Scan `.claude/sessions/` | Query SQLite `threads` table | Scan `chats/` directory | Via ACP session management |
| Cross-session memory | File-based `CLAUDE.md` | SQLite-backed memory pipeline | `save_memory` writes to `GEMINI.md` | `.cursor/rules` |

## Context and instructions

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Instructions file | `CLAUDE.md` | `AGENTS.md` | `GEMINI.md` | `.cursor/rules` |
| Hierarchy | Global plus Project | Global plus Project plus Subdirectory | Global plus Extension plus Project | Global plus Project |
| Memory file | `~/.claude/memory.md` | N/A | `~/.gemini/memory.md` | N/A |
| Ignore file | `.claudeignore` | N/A | `.geminiignore` | `.cursorignore` |
| JIT subdirectory discovery | No | No | Yes | No |

## Approval and permissions

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Mechanism | `control_request` and `control_response` | JSON-RPC server requests | Policy engine and confirmation bus | `session/request_permission` |
| TermAl can intercept | Yes | Yes | No (stream-json) / Yes (ACP) | Yes |
| Persist decisions | Settings file | N/A | `auto-saved.toml` | Via `allow-always` option |
| Safety checkers | Built-in | Sandbox model | Pluggable | Built-in |

## Advanced features

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Thread forking | No | `thread/fork` | No | No |
| Compaction | Auto summarization | `thread/compact/start` | Two-pass summarization | Managed internally |
| Rollback | No | `thread/rollback` | `rewindTo(messageId)` plus shadow git restore | No |
| Archival | No | `thread/archive` and `thread/unarchive` | No | No |
| Loop detection | Repetition detection | No | Multi-stage detection | Built-in |
| Hooks | Pre-commit style | No | 11 lifecycle events | Session start/end, prompt, stop |
| Sub-agents | Via tools | Single agent | Local plus remote via A2A | `cursor/task` sub-agent |
| Checkpointing | Git optional | Git | Shadow git repo | Managed internally |
| Mid-turn interrupt | `control_request` interrupt | Yes via JSON-RPC | No IPC path (stream-json) | `session/cancel` |
| Agent modes | N/A | N/A | N/A | `agent`, `plan`, `ask` |
| Cloud handoff | No | No | No | Yes (push to cloud via `&` prefix) |

## Pricing

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Model | Subscription plan | Per-token API key | Per-token API key or OAuth | Cursor subscription |

## VS Code extension

| | Claude Code | Codex | Gemini CLI | Cursor CLI |
|---|---|---|---|---|
| Extension | `anthropic.claude-code` | No official extension | `vscode-ide-companion` | Native (Cursor is the editor) |
| Connects via | Spawns CLI and speaks NDJSON | N/A | HTTP plus MCP | Built-in |
| Context sync | File context via tools | N/A | Active file, cursor, selection, open files | Full editor context |
| Auth | Same process | N/A | Bearer token via env var | Same process |

## ACP protocol convergence

Both Cursor CLI and Gemini CLI (experimental) implement the Agent Client Protocol
(ACP), a JSON-RPC 2.0 over stdio standard originally developed between Zed and
Google. This means a **shared ACP adapter** in TermAl could serve both agents,
with differences isolated to:

| Concern | Cursor CLI | Gemini CLI (ACP) |
|---------|-----------|-----------------|
| Executable | `cursor` | `gemini` |
| Subcommand | `agent acp` | `--experimental-acp` |
| Auth methods | `cursor agent login`, `--api-key`, `--auth-token` | OAuth, `GEMINI_API_KEY`, Vertex AI |
| Extended methods | `cursor/ask_question`, `cursor/create_plan`, `cursor/update_todos` | TBD |
| Stability | Stable | Experimental |

See individual feature briefs for full protocol details:
- [`cursor-cli-integration.md`](./cursor-cli-integration.md)
- [`gemini-cli-integration.md`](./gemini-cli-integration.md)
