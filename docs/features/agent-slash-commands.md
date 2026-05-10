# Feature Brief: Agent Slash Commands

Backlog source: [`docs/bugs.md`](../bugs.md)

## Status

Implemented for discovery and backend-owned execution. `GET
/api/sessions/{id}/agent-commands` now serves:

- live Claude native slash commands discovered from Claude's initialize metadata
- `.claude/commands/*.md` prompt templates from the session workdir

The slash palette shows those commands alongside the existing session-control
commands. Native Claude commands are sent as slash prompts such as `/review`,
while markdown templates are resolved through `POST
/api/sessions/{id}/agent-commands/{name}/resolve`. The frontend passes
`arguments` and optional `note`; the backend applies `$ARGUMENTS`, appends any
note as a standard user-note block, and returns the resolved prompt plus any
trusted delegation defaults. Regular session sends and delegated sends use the
same resolver.

See [`slash-commands.md`](slash-commands.md) for the existing session-control implementation.

## Problem

Claude exposes two useful command surfaces:

- custom prompt templates from `.claude/commands/*.md`
- native slash commands advertised by the live runtime, such as `/review`,
  `/release-notes`, or `/security-review`

TermAl supports filesystem prompt templates for local sessions and merges live
native-command metadata for Claude sessions when available. This brief remains
useful as the design record for that work.

## Goals

- Let the user type `/` in the composer and see agent commands alongside session controls.
- Let the user select an agent command and have its `.md` content sent as the prompt.
- Support command arguments (e.g., `/fix-bug 3`).
- Support an optional user note that is appended to the resolved prompt without
  requiring every command template to define its own note placeholder.
- Use one backend resolver for regular sends and delegation sends so command
  expansion, notes, and write policy cannot diverge between paths.
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
- Claude Code command templates use the filename (minus `.md`) as the command
  name. When YAML frontmatter is present, `description:` and `argument-hint:`
  populate command metadata and TermAl strips the frontmatter before sending the
  prompt body; otherwise the first non-empty body line becomes the description.
- Command execution calls the backend resolver. React only decides whether a
  selected command first needs to expand in the composer for argument input.

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

### 1a. Backend: command/skill resolution endpoint

Target contract:

```http
POST /api/sessions/{id}/agent-commands/{name}/resolve
```

Request:

```json
{
  "arguments": "1024",
  "note": "Please add integration tests for Connectivity class.",
  "intent": "send"
}
```

`intent` is `"send"` for a normal session turn and `"delegate"` for child-session
delegation. The resolver must use the same template expansion rules for both
intents, but may return different execution defaults for delegation.

Response:

```json
{
  "name": "fix-bug",
  "source": ".claude/commands/fix-bug.md",
  "kind": "promptTemplate",
  "visiblePrompt": "/fix-bug 1024",
  "expandedPrompt": "Fix a bug from docs/bugs.md...\n\n1024\n\n## Additional User Note\n\nPlease add integration tests for Connectivity class.",
  "title": "Fix bug 1024"
}
```

Delegating a prompt-template command with trusted command-owned defaults returns
`delegation` only for `intent: "delegate"`. This is the trusted-source response
shape; project-local `.claude/commands/*.md` templates are not trusted today and
do not return delegation defaults:

```json
{
  "name": "review-local",
  "source": ".claude/commands/review-local.md",
  "kind": "promptTemplate",
  "visiblePrompt": "/review-local",
  "expandedPrompt": "Review staged and unstaged changes...",
  "title": "Review staged and unstaged changes using multiple specialized reviewers.",
  "delegation": {
    "title": "Review staged and unstaged changes using multiple specialized reviewers.",
    "mode": "reviewer",
    "writePolicy": { "kind": "isolatedWorktree", "ownedPaths": [] }
  }
}
```

Resolution rules:

- The frontend sends `arguments` and optional `note` as separate fields. The
  backend should not infer command-specific structure from a single free-form
  string unless command metadata declares that structure.
- `arguments` and `note` are trimmed and each capped at 65,536 bytes before
  template interpolation or note appending.
- `$ARGUMENTS` in prompt templates is replaced with `arguments` exactly after
  trimming only outer whitespace.
- `note` is never substituted into the template. If present after trimming outer
  whitespace, append it to the resolved prompt as:

  ```markdown
  ## Additional User Note

  <note text>
  ```

- If `note` is empty or omitted, the note block is omitted.
- Preserve internal note formatting verbatim. The resolver should not parse
  Markdown, redact content, or rewrite user intent.
- Native slash commands (`kind: "nativeSlash"`) resolve to a literal
  `visiblePrompt` such as `/review`. If a native runtime cannot accept appended
  notes, the resolver must either reject `note` with a validation error or
  convert the request to a prompt-template path that TermAl owns.
- Backend resolver metadata provides command-specific title generation,
  delegation mode, and write policy. Add new command behavior through trusted
  command/skill frontmatter metadata, not React component branches or Rust
  command-name lookup tables.

### 1b. Command/skill frontmatter metadata

Command templates and future `SKILL.md` files may declare TermAl execution
metadata under `metadata.termal`. This follows the Claude skill model: YAML
frontmatter is the always-loaded discovery layer, while the Markdown body remains
the prompt/instruction payload. TermAl strips recognized frontmatter before
sending the template to an agent and uses `description:` as the command palette
description when present.

TermAl parses prompt-template command frontmatter for resolver metadata today.
Project-local `.claude/commands/*.md` metadata may drive title generation after
passing the source/name gate, but delegation defaults that affect mode or write
policy are ignored. No production command source is marked trusted yet; future
TermAl-owned command or `SKILL.md` support should reuse the same
`metadata.termal` shape and set the trusted-source marker only for those
TermAl-owned files.

Metadata contract:

```yaml
---
name: review-local
description: Review staged and unstaged changes using multiple specialized reviewers.
metadata:
  termal:
    title:
      strategy: default
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: isolatedWorktree
---
```

Title strategies:

- `default`: use the command description or visible prompt.
- `prefixFirstArgument`: use `<prefix> <first argument>` when an argument is
  present, otherwise fall back to `default`.

Delegation metadata:

- In trusted metadata, `enabled: true` allows the resolver to return delegation
  defaults for `intent: "delegate"`.
- `mode` currently accepts `reviewer` or `explorer`; `worker` remains blocked
  until write-enabled worker delegations are implemented.
- `writePolicy.kind` currently accepts `readOnly` or `isolatedWorktree`.
  `sharedWorktree` remains unsupported for command metadata.

Trust rules:

- Only metadata from future TermAl-trusted filesystem command/skill files may
  grant delegation defaults. Native runtime-advertised commands or untrusted
  project entries named `review-local` must not inherit TermAl privileges by
  name.
- Invalid trusted `metadata.termal` must fail command resolution with a clear
  validation error. It must not silently broaden permissions or fall back to a
  more permissive policy. Untrusted delegation metadata is ignored rather than
  applied.
- Metadata is declarative; command names are not policy. `/review-local` and
  `/fix-bug` are examples, not special cases in Rust code.

Example user intent:

```text
/fix-bug 1024 -- Please add integration tests for Connectivity class.
```

The UI can parse this into `arguments: "1024"` and `note: "Please add
integration tests for Connectivity class."`, then call the resolver. Without an
unambiguous separator or metadata, the whole tail should be sent as
`arguments`, with `note` omitted.

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
      content: string;           // full .md template content for display/compatibility
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
- Ask the backend resolver for the final prompt.
- Send `visiblePrompt` and `expandedPrompt` through the normal session-send path.
- Clear the composer.

**With arguments** (`hasArguments: true`):
- Expand the command in the composer: `/fix-bug ` (with trailing space).
- User types the argument (e.g., `3`).
- On Enter: send the command name, parsed `arguments`, and optional `note` to
  the backend resolver, then send the resolved prompt.

**Delegation**:
- Use the same backend resolver with `intent: "delegate"`.
- Spawn the child session with the resolver's `expandedPrompt`.
- Apply resolver-provided delegation defaults such as `writePolicy`, `mode`,
  and title. React components must not hard-code command names such as
  `review-local` to choose write policy.

### 6. Argument substitution and notes

Claude Code's convention:
- `$ARGUMENTS` in the `.md` content is replaced with the resolver request's
  `arguments` field.
- Example: user types `/fix-bug 3`; the UI passes `arguments: "3"` and the
  backend replaces `$ARGUMENTS` with `3`.
- If no arguments provided and `$ARGUMENTS` is present, send with `$ARGUMENTS` replaced
  by empty string (matches Claude Code behavior).
- Optional user notes are appended after template expansion as an `Additional
  User Note` section. They are not substituted into `$ARGUMENTS`.

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

Discovery endpoint:

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/sessions/{id}/agent-commands` | Discover agent commands for session's project |

Resolution endpoint:

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/api/sessions/{id}/agent-commands/{name}/resolve` | Resolve a command template/native command into the prompt and execution defaults for a regular send or delegation |

The discovery response may continue to include command content for display and
compatibility, but frontend execution should use the resolver as the source of
truth. This keeps command/skill parsing, `$ARGUMENTS`, optional notes, and
delegation policy on the backend.

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
- Add backend command resolution for `$ARGUMENTS` substitution and optional
  notes.
- Commands without arguments: send immediately on Enter.
- Commands with arguments: expand in composer, send on second Enter.
- Regular session sends and delegation sends both call the resolver.
- Delegation uses resolver-provided `writePolicy`, not React hard-coding.

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

Backend resolver:
- Resolver replaces `$ARGUMENTS` with the request `arguments`.
- Resolver appends `## Additional User Note` only when `note` is non-empty.
- Resolver returns delegation defaults only for commands whose metadata comes
  from an explicitly trusted TermAl-owned source.

Frontend:
- Slash palette shows agent commands when available.
- Slash palette hides Agent Commands section when none exist.
- Filtering works across both sections (agent commands + session controls).
- Refresh button re-fetches commands.
- Loading spinner shown during fetch.
- Error state shown with retry on fetch failure.
- Selecting a no-argument command resolves it through the backend and sends the
  resolved prompt.
- Selecting an argument command expands in composer, resolves through the
  backend, and sends the resolved prompt.
- Delegating a command uses the same resolved prompt and resolver-provided
  delegation policy.

## Acceptance criteria

- Typing `/` in the composer shows agent commands from `.claude/commands/` alongside
  session controls.
- Selecting `/review-local` resolves through the backend and sends the resolved
  prompt to the active session.
- Selecting `/fix-bug` expands to `/fix-bug ` in the composer; typing `3` and pressing
  Enter resolves with `arguments: "3"` and sends the resolved prompt.
- Passing a note appends an `Additional User Note` block without changing
  `$ARGUMENTS`.
- Delegating a trusted command uses the same backend resolver as regular send
  and receives the resolver-selected delegation write policy.
- Commands are scoped to the session's workdir (different projects show different commands).
- Adding a new `.md` file to `.claude/commands/` and clicking refresh shows the new command.
- The existing session-control commands (`/model`, `/mode`, `/effort`) continue to work
  unchanged.
