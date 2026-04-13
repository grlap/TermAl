# Feature Brief: Instruction Debugger

## Status

Partially implemented. TermAl ships an instruction search/debugger workspace
tab backed by `GET /api/instructions/search`. The full provenance graph and
effective-stack model below remains future design work.

This brief describes a provenance and debugging surface for agent instruction
documents such as `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, and related
Markdown-based prompt files.

## Problem

The current instruction-file story is opaque.

When a user opens an instruction document, they can read what it says, but they
cannot answer the harder debugging questions:

- Why is this instruction present?
- Which file introduced it?
- Which parent file or directory scope caused it to apply?
- Which condition matched?
- Did this instruction override another one?
- Is this file active for the current session, or is it dead weight?

This is a real debugging workflow, not just documentation browsing. The user is
effectively debugging a resolution system made of Markdown files, path scopes,
agent-specific hierarchy rules, and local conventions.

Today TermAl already has:

- a source view for opening files
- a workspace tab model for adding new inspection surfaces
- agent-command discovery for `.claude/commands/*.md`
- a planned territory view for cross-session file coordination

What is missing is an instruction-specific provenance model.

## Core idea

Add an **Instruction Debugger** that answers:

- how a document became active
- how a specific instruction block became effective
- what the final effective instruction stack is for a given session and path
- how a searched phrase is reachable from all relevant roots

The debugger should expose three complementary views:

1. **Trace**
   Shows the causal chain for one instruction or document.
2. **Effective Stack**
   Shows all active instructions for a session and optional file path, ordered
   by precedence.
3. **Graph**
   Shows the full document relationship graph for overview, dead-file
   discovery, and navigation.
4. **Search**
   Finds phrase matches and reverse-traces every path back to the full root set.

The graph is useful, but it is not the primary debugging surface. The primary
surface is the trace and reverse root search.

## Reverse provenance and roots

The debugger should support reverse provenance queries:

- start from a matched phrase or selected instruction span
- walk backward through all provenance edges
- enumerate the full set of roots that can reach that span
- show all paths, not just the nearest parent

This is closer to a `gcroot`-style query than a simple tree inspector.

The important output is not "who is the parent?" but:

- which roots can reach this instruction
- through which intermediate files and spans
- under which activation conditions
- which of those paths are active in the current session context

By default, the debugger should prefer completeness over brevity. The user can
collapse to shortest path or active-only path after the full root set is known.

## Two levels of provenance

The feature needs to distinguish between two related but different questions.

### Document provenance

How did this Markdown file become part of the active instruction set?

Examples:

- project root contains `AGENTS.md`
- current path falls under a subdirectory rule
- `.claude/commands/review-local.md` was loaded because the user invoked
  `/review-local`
- a reviewer file was discovered from `.claude/reviewers/`

### Instruction provenance

How did this specific paragraph, heading section, or instruction block survive
resolution and become effective?

Examples:

- inherited from `AGENTS.md`
- narrowed by a subdirectory-scoped file
- overridden by a later instruction with higher precedence
- inactive because its condition did not match

If TermAl only models document provenance, the user still cannot debug why a
specific instruction is active. The debugger needs both levels.

## Ground truth vs inference

TermAl does not own the instruction semantics of every agent runtime. Some
resolution steps are directly observable from local files; others are only
inferable from known agent conventions.

The debugger should make that explicit.

Each discovered relation should carry a provenance quality:

- `observed` - directly confirmed from local files or explicit runtime metadata
- `declared` - defined by a TermAl-managed convention such as command discovery
- `inferred` - best-effort explanation based on known agent rules

This prevents the UI from presenting guessed causality as hard truth.

## Goals

- Make instruction-file behavior explainable.
- Show the effective instruction stack for a session and optional target path.
- Let the user trace a specific instruction back to its source chain.
- Let the user search for a phrase and enumerate every root and path that can
  reach that phrase.
- Surface overrides, inactive rules, cycles, missing references, and dead files.
- Reuse the existing workspace tab model and source-view navigation.

## Non-goals for v1

- No attempt to perfectly emulate every agent's full private resolution logic.
- No live patching or editing of instruction files from the debugger.
- No semantic diffing of natural-language instructions.
- No line-level blame inside arbitrary prose beyond file spans and precedence.
- No remote or cloud-backed instruction resolution.

## User experience

### Entry points

Recommended entry points:

- A new `Instruction Debugger` workspace tab
- A `Why is this here?` action when an instruction file is open in source view
- A `Debug instructions` action from the session pane for the current session

### Trace view

The trace view is the highest-value workflow.

Example:

```text
AGENTS.md
  -> activated for project root
  -> subdirectory scope matched src/**
  -> docs/agents/backend.md became active
  -> instruction at docs/agents/backend.md:12 won precedence
  -> overrides AGENTS.md:44
```

For each step, show:

- source file
- line span
- relation type
- matching condition, if any
- precedence rank
- provenance quality (`observed`, `declared`, `inferred`)

The trace view should support:

- `full roots` mode: show every reachable root and path
- `active only` mode: show only paths active in the current context
- `shortest path` mode: collapse each root to its shortest explanation chain

### Effective stack view

For a selected session and optional file path, show:

- all active instruction blocks
- source file and line span
- precedence order
- whether each block overrides or is overridden
- inactive candidates that were considered but did not match

This is the equivalent of a compiler include list or CSS cascade inspector.

### Graph view

The graph view is the overview and navigation layer.

Nodes:

- instruction documents rendered as file nodes
- instruction spans rendered as line-anchored rows or ports inside each file node
- optional section nodes in a later phase

Edges:

- `discovers`
- `includes`
- `scopes`
- `activates`
- `overrides`
- `references`

Graph interactions:

- click node -> open inspector and source file
- click edge -> show the rule or condition behind that relation
- filter by session, agent, path, and active-only state

The graph should not flatten all relations to file-to-file edges. Where
possible, an edge should originate from a specific span or line range inside the
source file so the user can answer "which line pulled that file in?"

### Search view

The search view answers:

- where does this phrase appear?
- how did we get there?
- which roots can reach it?

Search flow:

1. user enters a phrase such as `dependency injection`
2. debugger finds matching instruction spans
3. for each match, debugger reverse-traces all reachable roots
4. UI groups results by match span, then by root

Each result should show:

- matched text
- source file and line span
- root count
- active path count
- `Show all roots`
- `Show graph`

Example:

```text
Match: .claude/reviewers/rust.md:9
"Prefer dependency injection where ownership boundaries are unstable."

Roots:
- CLAUDE.md
  -> reviewers.md:12
  -> .claude/reviewers/rust.md:9

- AGENTS.md
  -> docs/agents/backend.md:18
  -> .claude/reviewers/rust.md:9
```

### Inspector

Every view should feed the same right-side inspector with:

- file path
- line span
- raw Markdown excerpt
- rendered Markdown preview
- why this is active
- what it overrides
- what overrides it
- originating session and agent context

## Resolution context

Every debugger query should be evaluated against an explicit context:

```rust
struct InstructionResolutionContext {
    session_id: String,
    agent: Agent,
    workdir: String,
    target_path: Option<String>,
    command_name: Option<String>,
    project_id: Option<String>,
}
```

Without context, "is this active?" is not answerable.

The same file may be active for one session, inactive for another, and only
conditionally active for a given command or subdirectory.

## Root model

The debugger should distinguish between two classes of roots.

### Structural roots

These are top-level entry points in the instruction system, for example:

- `CLAUDE.md`
- `AGENTS.md`
- `GEMINI.md`
- `.cursor/rules`
- skill entry documents
- command entry documents

### Activation roots

These are context anchors that explain why a structural root mattered for this
resolution:

- current session agent is `Claude`
- current command is `/review-local`
- current target path is `src/api.rs`
- current workdir is the project root

Both root classes matter. A structural root without activation context does not
fully explain why the span is relevant, and activation context without the
structural root does not show where the instruction came from.

## Data model

### Instruction document

```rust
struct InstructionDocument {
    id: String,
    path: String,
    kind: InstructionDocumentKind,
    title: Option<String>,
    discovered_by: ProvenanceKind,
    applies_to_agents: Vec<Agent>,
}
```

Examples of `kind`:

- `RootInstruction`
- `SubdirectoryInstruction`
- `CommandInstruction`
- `ReviewerInstruction`
- `ReferencedInstruction`

### Instruction span

```rust
struct InstructionSpan {
    id: String,
    document_id: String,
    line_start: u32,
    line_end: u32,
    heading_path: Vec<String>,
    text: String,
}
```

This is the unit the user actually debugs. A span is usually a paragraph,
section, or command block, not a whole file.

### Provenance edge

```rust
struct ProvenanceEdge {
    id: String,
    from_id: String,
    to_id: String,
    relation: ProvenanceRelation,
    condition: Option<String>,
    matched: bool,
    precedence: Option<i32>,
    provenance_kind: ProvenanceKind,
    detail: Option<String>,
}
```

Examples of `relation`:

- `Discovers`
- `Scopes`
- `Activates`
- `Includes`
- `Overrides`
- `References`

### Effective instruction

```rust
struct EffectiveInstruction {
    span_id: String,
    active: bool,
    precedence_rank: i32,
    overridden_span_ids: Vec<String>,
    overridden_by_span_id: Option<String>,
    explanation: Vec<String>,
}
```

### Reverse path

```rust
struct InstructionRootPath {
    match_span_id: String,
    root_id: String,
    activation_root_ids: Vec<String>,
    edge_ids: Vec<String>,
    active: bool,
    shortest: bool,
}
```

This is the core result shape for phrase search and full-root trace queries.

## Backend architecture

### 1. Instruction adapters

Add agent-specific instruction adapters that know how to discover candidate
documents and explain known hierarchy rules.

Examples:

- `ClaudeInstructionAdapter`
- `CodexInstructionAdapter`
- `GeminiInstructionAdapter`
- `CursorInstructionAdapter`

Each adapter should:

- discover candidate files for the current workdir
- annotate known scope or hierarchy rules
- mark relations as `observed`, `declared`, or `inferred`
- produce a normalized document and edge set

This follows the same general adapter shape already used elsewhere in TermAl for
agent-specific runtime differences.

### 2. Shared instruction index

Build a backend index keyed by workdir plus agent type.

The index should cache:

- discovered documents
- extracted spans
- provenance edges
- last modified times for invalidation

Invalidate when:

- the session workdir changes
- an instruction file changes on disk
- the user explicitly refreshes

### 3. Effective resolution pass

Given a resolution context:

1. collect candidate documents from the adapter
2. extract spans from those documents
3. apply known scope and precedence rules
4. build the effective stack
5. retain the losing candidates so the debugger can explain them

The losing candidates matter. A debugger that only returns the winners cannot
explain why something disappeared.

### 4. Reverse reachability

Given a selected span or phrase match:

1. locate all matching spans
2. walk reverse provenance edges from each span
3. enumerate all reachable structural roots
4. attach activation roots for the current context
5. return all paths, with optional shortest-path summaries

The backend should not stop after finding the first parent or first root.

## API plan

### Graph snapshot

`GET /api/instructions/graph?sessionId={id}&targetPath={path?}`

Returns:

```json
{
  "documents": [],
  "spans": [],
  "edges": [],
  "context": {},
  "summary": {
    "activeDocuments": 0,
    "activeSpans": 0,
    "inactiveSpans": 0,
    "overrideEdges": 0
  }
}
```

Purpose:

- populate the graph view
- provide enough metadata for the inspector

### Effective stack

`GET /api/instructions/effective?sessionId={id}&targetPath={path?}&commandName={name?}`

Returns the ordered list of active and inactive candidate instruction spans for
the current context.

### Trace

`GET /api/instructions/trace?sessionId={id}&spanId={id}`

Returns the causal chain for one span, including:

- source span
- upstream documents and edges
- precedence decisions
- overridden and overriding spans
- all reachable roots for that span

### Phrase search

`GET /api/instructions/search?sessionId={id}&q={phrase}&targetPath={path?}&commandName={name?}`

Returns:

```json
{
  "matches": [
    {
      "spanId": "span-1",
      "path": ".claude/reviewers/rust.md",
      "lineStart": 9,
      "lineEnd": 9,
      "text": "Prefer dependency injection where ownership boundaries are unstable.",
      "active": true,
      "rootCount": 2,
      "activeRootCount": 1
    }
  ],
  "paths": [],
  "roots": [],
  "context": {}
}
```

Purpose:

- phrase search across instruction spans
- reverse root tracing for each match
- grouped `gcroot`-style explanation of how the phrase is reachable

### Source lookup

Recommended helper endpoint:

`GET /api/instructions/source?sessionId={id}&path={file}`

Purpose:

- return parsed span boundaries for an instruction file
- support click-to-line and span highlighting in the source panel

## Frontend plan

### Workspace integration

Add a new workspace tab kind:

- `instructionDebugger`

This should fit the existing generic workspace tab system rather than becoming a
special overlay. The user should be able to keep the debugger open alongside a
session, source editor, diff preview, filesystem, and git status.

### View layout

Recommended layout for the debugger tab:

- left: graph or span list
- center: trace or effective stack
- right: inspector

Recommended top-bar controls:

- session selector
- agent badge
- target path picker
- phrase search input
- active-only toggle
- refresh action
- `Trace | Effective | Graph | Search` mode switch

### Source navigation

Clicking any document or span should:

- open the source file in a source tab
- jump to the exact line
- preserve the debugger tab state

When the current source tab is an instruction file, TermAl should offer a
contextual action:

- `Why is this here?`
- `Find roots for selected phrase`

That action should open the debugger focused on the clicked line or nearest
instruction span. If the user has a text selection, the debugger should open in
Search mode seeded with that phrase.

## Diagnostics

The debugger should surface problems directly in the UI:

- missing referenced files
- cycles in include/reference relationships
- duplicate instruction files at the same scope
- files discovered but never active
- conflicting spans at equal precedence
- stale cache state after on-disk changes

These are not secondary details. They are often the actual bug.

## Relation to graph/canvas export

An Obsidian-style canvas is useful as a presentation layer, but it should be
built on top of the native provenance model, not instead of it.

Recommended order:

1. native document/span/edge model
2. native debugger tab and inspector
3. native reverse root search
4. optional export to `.canvas` / JSON Canvas

That keeps the feature grounded in debugging value instead of treating the graph
as the product.

## Implementation phases

### Phase 1: backend discovery and normalization

- add instruction document and span models
- add agent-specific discovery adapters
- discover candidate instruction files for current sessions
- expose `GET /api/instructions/graph`

### Phase 2: effective stack and trace

- implement precedence and activation resolution
- retain inactive candidates and override edges
- expose `GET /api/instructions/effective`
- expose `GET /api/instructions/trace`
- implement reverse reachability and root enumeration
- expose `GET /api/instructions/search`

### Phase 3: workspace tab and inspector

- add `instructionDebugger` tab support
- add trace, effective, and graph modes
- add search mode with grouped root paths
- add source jump-to-line integration
- add `Why is this here?` action from source view
- add `Find roots for selected phrase` from source view

### Phase 4: diagnostics and polish

- add missing-file and cycle diagnostics
- add cache invalidation and refresh behavior
- add filters for active-only and command-specific contexts
- add optional canvas export

## Testing plan

Backend:

- discovers known instruction files for each supported agent
- returns stable span boundaries for Markdown sections
- distinguishes active vs inactive candidates correctly
- explains override relationships
- marks inferred relations as inferred
- returns all reachable roots for a selected span, not just one parent chain
- returns grouped reverse paths for phrase search
- invalidates cached results when files change
- handles missing files and cycles gracefully

Frontend:

- debugger tab opens from session and source entry points
- trace view shows the full causal chain
- effective stack sorts by precedence
- graph view filters by active-only state
- search view groups phrase matches by root set
- clicking a span opens the source file at the correct line
- inspector shows override and provenance details
- refresh picks up file changes without losing current focus

## Acceptance criteria

- A user can open an instruction file and ask `Why is this here?`
- A user can search for a phrase such as `dependency injection` and see every
  reachable root and path that explains that phrase.
- TermAl shows the document and instruction provenance chain for the selected
  span
- TermAl shows the current effective instruction stack for a selected session
  and optional target path
- TermAl makes the difference between observed and inferred explanations clear
- The debugger integrates into the workspace as a first-class tab, not a modal
  dead end

## Why this matters

Instruction files are not passive notes. They are part of the execution
environment for the agent.

If the user cannot explain where an instruction came from, they cannot trust the
agent's behavior, and they cannot safely evolve a hierarchical Markdown-based
instruction system. The instruction debugger turns that opaque behavior into
something inspectable, navigable, and debuggable.
