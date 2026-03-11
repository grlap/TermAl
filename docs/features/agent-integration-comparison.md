# Agent Integration Comparison

Cross-agent reference for TermAl adapter design.

## Protocol and transport

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Wire protocol | NDJSON (custom) | JSON-RPC 2.0 | Direct SDK or `stream-json` stdout |
| Transport | stdin/stdout only | stdin/stdout or WebSocket | In-process SDK or spawned CLI |
| Bidirectional | Yes | Yes | No |
| Server mode | No | `codex app-server` | No |
| Best TermAl integration | Spawn child and speak NDJSON over stdio | Spawn app-server and speak JSON-RPC over stdio | Spawn CLI with `--output-format stream-json` |

## Session persistence

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Storage format | Append-only `.jsonl` | JSONL rollout plus SQLite cache | Single `.json` per session |
| Location | `.claude/sessions/{id}.jsonl` | `~/.codex/sessions/YYYY/MM/DD/rollout-{id}.jsonl` | `~/.gemini/tmp/<project>/chats/session-{ts}-{id}.json` |
| Resume mechanism | `--resume <session-id>` | `thread/resume` | Reload JSON and pass to `resumeChat()` |
| Session discovery | Scan `.claude/sessions/` | Query SQLite `threads` table | Scan `chats/` directory |
| Cross-session memory | File-based `CLAUDE.md` | SQLite-backed memory pipeline | `save_memory` writes to `GEMINI.md` |

## Context and instructions

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Instructions file | `CLAUDE.md` | `AGENTS.md` | `GEMINI.md` |
| Hierarchy | Global plus Project | Global plus Project plus Subdirectory | Global plus Extension plus Project |
| Memory file | `~/.claude/memory.md` | N/A | `~/.gemini/memory.md` |
| Ignore file | `.claudeignore` | N/A | `.geminiignore` |
| JIT subdirectory discovery | No | No | Yes |

## Approval and permissions

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Mechanism | `control_request` and `control_response` | JSON-RPC server requests | Policy engine and confirmation bus |
| TermAl can intercept | Yes | Yes | No |
| Persist decisions | Settings file | N/A | `auto-saved.toml` |
| Safety checkers | Built-in | Sandbox model | Pluggable |

## Advanced features

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Thread forking | No | `thread/fork` | No |
| Compaction | Auto summarization | `thread/compact/start` | Two-pass summarization |
| Rollback | No | `thread/rollback` | `rewindTo(messageId)` plus shadow git restore |
| Archival | No | `thread/archive` and `thread/unarchive` | No |
| Loop detection | Repetition detection | No | Multi-stage detection |
| Hooks | Pre-commit style | No | 11 lifecycle events |
| Sub-agents | Via tools | Single agent | Local plus remote via A2A |
| Checkpointing | Git optional | Git | Shadow git repo |
| Mid-turn interrupt | `control_request` interrupt | Yes via JSON-RPC | No IPC path |

## Pricing

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Model | Subscription plan | Per-token API key | Per-token API key or OAuth |

## VS Code extension

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| Extension | `anthropic.claude-code` | No official extension | `vscode-ide-companion` |
| Connects via | Spawns CLI and speaks NDJSON | N/A | HTTP plus MCP |
| Context sync | File context via tools | N/A | Active file, cursor, selection, open files |
| Auth | Same process | N/A | Bearer token via env var |
