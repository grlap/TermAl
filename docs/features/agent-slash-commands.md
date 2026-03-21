# Feature Brief: Agent Slash Commands

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Implemented. `GET /api/sessions/{id}/agent-commands` now serves:

- live Claude native slash commands discovered from Claude's initialize metadata
- `.claude/commands/*.md` prompt templates from the session workdir

The slash palette shows those commands alongside the existing session-control
commands. Native Claude commands are sent as slash prompts such as `/review`,
while markdown templates still expand `$ARGUMENTS` locally before send.

See [`slash-commands.md`](slash-commands.md) for the existing session-control implementation.

## Problem

Claude exposes two useful command surfaces:

- custom prompt templates from `.claude/commands/*.md`
- native slash commands advertised by the live runtime, such as `/review`,
  `/release-notes`, or `/security-review`

TermAl now supports both for Claude sessions. This brief remains useful as the
design record for that work.

## Goals

- Let the user type `/` in the composer and see agent commands alongside session controls.
- Let the user select an agent command and have its `.md` content sent as the prompt.
- Support command arguments (e.g., `/fix-bug 3`).
- Keep the existing session-control commands working unchanged.

## Non-goals for v1

- No command editing from within TermAl (users edit `.md` files directly).
- No cross-agent command discovery (Codex, Gemini commands — future work).
- No command output formatting beyond what the agent naturally produces.
- No command-specific UI (progress bars, step indicators).

## Implemented architecture

- The slash palette in `AgentSessionPanel.tsx` uses a static `SLASH_COMMANDS` array
  with hardcoded `SlashCommandId` values (`"model" | "mode" | "sandbox" | ...`).
- Palette items are either `"command"` (expands text) or `"choice"` (applies a setting).
  Agent commands use the third kind: `"agent-command"`.
- The backend command-discovery endpoint merges two sources for Claude sessions:
  live native-command metadata from the initialized runtime and filesystem
  templates from `.claude/commands`.
- Claude Code's command-template format remains simple: the filename (minus `.md`)
  is the command name, the first line is the description, the full content is
  the prompt, and `$ARGUMENTS` is replaced with user-provided arguments.

### 1. Backend: command discovery endpoint

```
GET /api/sessions/{id}/agent-commands
```

Response:

```json
{
  "commands": [
    {
      "name": "review-local",
      "description": "Review staged and unstaged changes using multiple specialized reviewers.",
      "content": "Review staged and unstaged changes using...\n\n## Step 1: ...",
      "source": ".claude/commands/review-local.md"
    },
    {
      "name": "fix-bug",
      "description": "Fix a bug from docs/bugs.md by number.",
      "content": "Fix a bug from `docs/bugs.md`...\n\n$ARGUMENTS\n...",
      "source": ".claude/commands/fix-bug.md"
    }
  ]
}
```

Implementation:
- Look up the session's `workdir` from `SessionRecord`.
- Read `{workdir}/.claude/commands/*.md` on each request.
- For Claude sessions, merge those templates with any native commands cached from
  the live Claude initialize response.
- Prefer native Claude commands over same-name filesystem templates so `/review`
  and runtime-backed project commands dispatch natively when available.
- Return empty array if neither source is available.
- Frontend caches results per session and invalidates on workdir change or explicit refresh.

### 2. Frontend: agent command type

Extend the slash palette to support agent commands.

```typescript
// New palette item kind
type SlashPaletteItem =
  | { kind: "command"; ... }      // existing: session control (expands text)
  | { kind: "choice"; ... }       // existing: setting value (applies immediately)
  | { kind: "agent-command";      // new: agent slash command
      key: string;
      command: string;            // "/review-local"
      label: string;             // "/review-local"
      detail: string;            // first line of .md file
      content: string;           // full .md content (the prompt to send)
      hasArguments: boolean;     // true if content contains $ARGUMENTS
    };
```

### 3. Frontend: command fetching

```typescript
// api.ts
export function fetchAgentCommands(sessionId: string): Promise<AgentCommandsResponse> {
  return request<AgentCommandsResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/agent-commands`
  );
}
```

Fetch agent commands:
- When the composer opens the slash palette for a session and commands are not loaded yet.
- On explicit refresh from the slash palette.
- Cache in app state per session ID.

### 4. Frontend: palette integration

Modify `buildSlashPaletteState` to include agent commands:

```
User types "/"
  → Show two sections:
    ┌─────────────────────────────────────┐
    │ Agent Commands                      │
    │   /review-local  Review staged...   │
    │   /fix-bug       Fix a bug from...  │
    │ Session Controls                    │
    │   /model         Change the model   │
    │   /mode          Change the mode    │
    │   /effort        Change effort      │
    └─────────────────────────────────────┘

User types "/rev"
  → Filter to matching commands:
    ┌─────────────────────────────────────┐
    │ Agent Commands                      │
    │   /review-local  Review staged...   │
    └─────────────────────────────────────┘
```

### 5. Frontend: command execution

When an agent command is selected:

**Without arguments** (`hasArguments: false`):
- Send the full `.md` content as the prompt via `sendMessage()`.
- Clear the composer.

**With arguments** (`hasArguments: true`):
- Expand the command in the composer: `/fix-bug ` (with trailing space).
- User types the argument (e.g., `3`).
- On Enter: replace `$ARGUMENTS` in the content with the user's input, send as prompt.

### 6. Argument substitution

Claude Code's convention:
- `$ARGUMENTS` in the `.md` content is replaced with whatever follows the command name.
- Example: user types `/fix-bug 3` → content has `$ARGUMENTS` replaced with `3`.
- If no arguments provided and `$ARGUMENTS` is present, send with `$ARGUMENTS` replaced
  by empty string (matches Claude Code behavior).

## UI plan

### Composer slash menu changes

- Add a section header `Agent Commands` above agent commands in the palette.
- Add a section header `Session Controls` above the existing session-control commands.
- Agent commands use the same keyboard navigation (Arrow Up/Down, Enter).
- Enter on an agent command without arguments → sends immediately.
- Enter on an agent command with arguments → expands to `/command-name ` for arg input.
- Show a subtle icon or badge to distinguish agent commands from session controls.

### Loading and error states

- While agent commands are loading, show a spinner in the palette header.
- If the commands directory doesn't exist, don't show the Agent Commands section at all.
- If the fetch fails, show an inline error with a retry button (same pattern as model refresh).

### Refresh

- Add a small refresh button in the Agent Commands header.
- Clicking it re-fetches from the backend (picks up newly added `.md` files).

## API plan

One new endpoint:

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/sessions/{id}/agent-commands` | Discover agent commands for session's project |

The response includes full command content so the frontend can send it as a prompt without
a second round-trip. Command files are small (typically <5KB), so embedding content is fine.

## Implementation phases

### Phase 1: backend discovery

- Add `AgentCommand` struct to `main.rs`.
- Implement `list_agent_commands` handler.
- Register `GET /api/sessions/{id}/agent-commands` route.
- Read `.claude/commands/*.md` from session workdir.
- Return command list with name, description, content, source path.

### Phase 2: frontend palette integration

- Add `AgentCommand` type to `types.ts`.
- Add `fetchAgentCommands()` to `api.ts`.
- Extend `SlashPaletteItem` with `"agent-command"` kind.
- Extend `buildSlashPaletteState` to merge agent commands into the palette.
- Add section headers to the palette UI.
- Fetch commands on session creation, cache per session.

### Phase 3: command execution

- Implement `applySlashPaletteItem` for `"agent-command"` kind.
- Handle `$ARGUMENTS` substitution.
- Commands without arguments: send immediately on Enter.
- Commands with arguments: expand in composer, send on second Enter.

### Phase 4: polish

- Add refresh button for agent commands.
- Add loading/error states.
- Add visual distinction between agent commands and session controls.
- Handle edge cases: empty commands dir, unreadable files, very large files.

## Testing plan

Backend:
- Commands directory exists with `.md` files → returns correct list.
- Commands directory doesn't exist → returns empty array.
- Session not found → returns 404.
- File with no content → returns empty description and content.
- Non-`.md` files in the directory → ignored.
- Subdirectories in commands/ → ignored (flat list only).

Frontend:
- Slash palette shows agent commands when available.
- Slash palette hides Agent Commands section when none exist.
- Filtering works across both sections (agent commands + session controls).
- Selecting a no-argument command sends the content as a prompt.
- Selecting an argument command expands in composer, sends on Enter with substitution.
- `$ARGUMENTS` replaced correctly in content.
- Refresh button re-fetches commands.
- Loading spinner shown during fetch.
- Error state shown with retry on fetch failure.

## Acceptance criteria

- Typing `/` in the composer shows agent commands from `.claude/commands/` alongside
  session controls.
- Selecting `/review-local` sends the full `review-local.md` content as the prompt to
  the active Claude session.
- Selecting `/fix-bug` expands to `/fix-bug ` in the composer; typing `3` and pressing
  Enter sends the content with `$ARGUMENTS` replaced by `3`.
- Commands are scoped to the session's workdir (different projects show different commands).
- Adding a new `.md` file to `.claude/commands/` and clicking refresh shows the new command.
- The existing session-control commands (`/model`, `/mode`, `/effort`) continue to work
  unchanged.
