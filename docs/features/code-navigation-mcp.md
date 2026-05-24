# Feature Brief: Code Navigation MCP

## Status

Proposed.

This brief defines an MCP server that gives agent sessions a fast, semantic way
to navigate large source trees. The first target is very large C# workspaces
with hundreds or thousands of projects, but the design should leave room for
language adapters beyond C#.

Related:
- [Agent Delegation Sessions](./agent-delegation-sessions.md)
- [Instruction Debugger](./instruction-debugger.md)
- [File Change Awareness](./file-change-awareness.md)
- [Source Renderers](./source-renderers.md)

Follow-up: add reciprocal links from those referenced feature briefs if this
brief remains in the tree.

## Problem

Agents can already read files, run shell commands, and use text search. That is
enough for small repositories, but it becomes inefficient in a large enterprise
codebase:

- raw search returns too many weak matches
- project ownership and dependency direction are hard to infer
- exact references are confused with comments, strings, generated code, and
  similarly named symbols
- call graph and implementation lookups require expensive manual exploration
- agents spend too much context on irrelevant files before finding the right
  edit surface
- repeated sessions rebuild the same mental map from scratch

For a C# workspace with 1000+ projects, the missing capability is a compact,
structured code intelligence layer that can answer source-navigation questions
without dumping large files into the transcript.

## Goals

- Provide exact symbol navigation through MCP tools.
- Return small, ranked, line-addressable results instead of whole-file dumps.
- Build a persisted project and symbol index per workspace.
- Support C# first using Roslyn/MSBuild semantics.
- Let agents request context packs around a file, symbol, bug, build error, or
  planned change.
- Distinguish semantic facts from heuristic guesses.
- Support dirty workspaces by mixing persisted index data with live file reads
  when needed.
- Make large-repo exploration faster for both direct agent work and delegated
  explorer/reviewer sessions.

## Non-goals for v1

- No full IDE replacement.
- No autonomous code modification through the navigation MCP.
- No automatic commits, pushes, or branch creation.
- No attempt to index every language equally in the first version.
- No cloud dependency for local workspaces.
- No unrestricted transcript-sized output; every tool should enforce paging and
  result budgets.

## Core Idea

Add a **Code Navigation MCP** server for each active workspace:

```text
workspace files
  -> project graph index
  -> language-specific symbol index
  -> search and ranking layer
  -> MCP tools consumed by agents
```

The MCP server should be read-only by default. Agents still use their normal
file-editing and terminal tools to make changes, while the navigation MCP gives
them the shortest reliable path to the right files, symbols, tests, and
dependency edges.

The primary design rule is:

**Return the smallest precise context that lets an agent take the next step.**

## Tool Surface

### MVP tools

These are the first tools to implement because they cover the highest-value
navigation loop.

```text
repo_overview()
```

Returns a compact workspace map:

- detected languages
- solution and project counts
- major source/test roots
- target frameworks
- generated-code conventions
- recently changed files
- index status and timestamp

```text
search_text(query, filters, limit, cursor)
```

Fast text search with ranking and filters. This should behave like `rg`, but
with structured output and workspace-aware ranking.

Useful filters:

- path globs
- project name
- language
- production vs test
- generated vs handwritten
- file extension

```text
search_symbol(query, kinds, filters, limit, cursor)
```

Finds symbols by name, partial name, fully qualified name, or fuzzy query.

Useful symbol kinds:

- class
- interface
- struct
- enum
- method
- constructor
- property
- field
- event
- attribute
- extension method
- controller
- handler
- test

```text
definition(target)
```

Returns the exact definition for a symbol or source location, including path,
line span, signature, containing type, containing project, and a small snippet.

```text
references(target, filters, group_by, limit, cursor)
```

Returns exact references for a symbol or source location. For C#, this should be
Roslyn-backed rather than raw text search.

Useful grouping:

- project
- production vs test
- reference kind
- caller
- file

```text
project_graph(project, depth, direction)
```

Returns project references, package references, target frameworks, solution
membership, and dependency direction.

```text
context_pack(target, budget)
```

Builds a compact bundle of relevant source context for an agent turn. This is
the most important high-level tool. The server chooses a small set of files,
symbols, references, callers, tests, and dependency notes that fit within the
requested budget.

### Expanded tools

```text
implementations(target, filters, limit, cursor)
```

Finds implementations of interfaces, abstract members, virtual members, partial
types, and overridden methods.

```text
callers(target, depth, filters, limit, cursor)
callee(target, depth, filters, limit, cursor)
```

Walks the call graph around a method, constructor, property accessor, or
delegate invocation.

```text
type_hierarchy(target, direction, depth)
```

Returns base types, derived types, implemented interfaces, and implementing
types.

```text
related_tests(target, limit)
```

Finds likely tests through exact references, project relationships, naming
patterns, test framework metadata, and recent git history.

```text
diagnostics(scope)
```

Returns current build, analyzer, nullable, or test diagnostics for a file,
project, solution, or changed-file set.

```text
impact(target, change_kind, budget)
```

Summarizes likely blast radius before a change:

- direct references
- public API exposure
- call graph boundary
- project dependents
- related tests
- config or serialization concerns
- high-risk consumers

## Result Shape

Every source result should be stable, line-addressable, and small.

```json
{
  "path": "src/Billing/Application/InvoiceService.cs",
  "line": 42,
  "endLine": 88,
  "project": "Billing.Application",
  "symbolId": "roslyn:Billing.Application:...",
  "kind": "method",
  "signature": "Task<Invoice> CreateInvoiceAsync(CreateInvoiceCommand command)",
  "score": 0.93,
  "confidence": "exact",
  "snippet": "public async Task<Invoice> CreateInvoiceAsync(...)",
  "indexVersion": "workspace-sha-or-index-id"
}
```

For graph results, return edges rather than prose:

```json
{
  "nodes": [
    {
      "id": "project:Billing.Application",
      "kind": "project",
      "name": "Billing.Application"
    }
  ],
  "edges": [
    {
      "from": "project:Billing.Api",
      "to": "project:Billing.Application",
      "kind": "projectReference"
    }
  ]
}
```

## Confidence Model

The MCP server should label each result with how it was produced:

- `exact` - produced by the compiler or language service
- `indexed` - produced by a persisted index that may need freshness checks
- `heuristic` - inferred from naming, directory layout, git history, or text
  search
- `stale` - produced from an index older than the current file state
- `partial` - produced while the workspace index is still building

This matters because agents should treat compiler-backed references differently
from best-effort guesses.

## C# First Index

The C# adapter should use Roslyn and MSBuild semantics instead of regex parsing.

Recommended index inputs:

- `.sln`, `.slnx`, and discovered `.csproj` files
- `Directory.Build.props`
- `Directory.Build.targets`
- `global.json`
- `NuGet.config`
- package references
- project references
- generated-code markers
- test framework packages and attributes

Recommended symbol data:

- fully qualified metadata name
- display signature
- source span
- declaring project
- assembly name
- accessibility
- attributes
- XML documentation summary, if available
- partial declaration spans
- generated vs handwritten flag

Recommended semantic edges:

- project references
- package references
- type inheritance
- interface implementation
- method override
- method calls
- property/event references
- attribute usage
- dependency injection registrations, where detectable
- test-to-production references

## Context Pack Strategy

`context_pack` is the tool that should feel agent-native. It should synthesize a
small navigation result, not merely forward raw search hits.

Example request:

```json
{
  "target": {
    "kind": "symbol",
    "name": "InvoiceService.CreateInvoiceAsync"
  },
  "budget": {
    "maxFiles": 8,
    "maxSnippets": 20,
    "maxTokens": 12000
  }
}
```

Example response:

```json
{
  "summary": "Invoice creation is owned by Billing.Application and exposed via Billing.Api.",
  "primary": [
    {
      "path": "src/Billing/Application/InvoiceService.cs",
      "line": 42,
      "reason": "Target method definition"
    }
  ],
  "supporting": [
    {
      "path": "src/Billing/Api/InvoicesController.cs",
      "line": 31,
      "reason": "Primary API caller"
    },
    {
      "path": "tests/Billing.Application.Tests/InvoiceServiceTests.cs",
      "line": 18,
      "reason": "Closest direct tests"
    }
  ],
  "risks": [
    "Method is part of API request flow.",
    "Five tests assert current validation behavior."
  ]
}
```

Ranking should prefer:

- exact definitions over textual matches
- direct callers over distant callers
- handwritten source over generated source
- production entrypoints over internal plumbing when the task is behavioral
- tests that reference the exact symbol over tests that only match by name
- recently changed files when the question concerns current work

## Freshness And Dirty Workspaces

Large indexes cannot be rebuilt on every keystroke. The MCP server needs clear
freshness semantics:

- maintain a persisted index for the last stable workspace snapshot
- watch active workspace roots for changes
- mark affected files, projects, and symbols stale
- answer from the index when safe, but label stale results
- read live file content for snippets before returning them
- allow explicit `refresh(scope)` in later phases

TermAl's file-change awareness can feed the same invalidation model, but the
navigation server should still work as a standalone MCP process.

## TermAl Integration

Recommended user-facing integration points:

- workspace-level MCP server registration
- visible index status in the project/session chrome
- "Open in source" links for every source result
- optional Code Navigation tab for humans to inspect symbol search, references,
  project graph, and impact results
- delegation prompts that automatically tell explorer/reviewer agents to use
  the navigation MCP before reading broad file sets

Recommended agent integration:

- attach the MCP server to Codex, Claude, Gemini, and Cursor sessions when the
  workspace is local and indexable
- include a compact instruction in session setup explaining which tools exist
  and when to prefer them
- expose the same MCP tool names across agents where possible
- keep the backend authoritative for workspace path mapping and server lifecycle

## Safety And Performance

The server should be optimized for fast, bounded answers:

- constrain all file reads and indexes to canonical workspace roots
- reject or explicitly handle symlink and reparse-point escapes
- ignore secret, dependency, and build-output paths by default
- enforce hard file-read and snippet byte-size limits
- all list-style tools must support `limit` and `cursor`
- snippets should have a default line budget
- huge result sets should return summaries and paging metadata
- generated folders and build artifacts should be excluded by default
- index state should be cancellable and restartable
- indexing should not block ordinary TermAl session startup
- every response should include index status when freshness could matter

The server should avoid hidden writes. Future write-capable tools, if any, must
be separate from navigation tools and go through TermAl's normal approval and
file-change protection paths.

## Phased Plan

### Phase 1: Raw workspace map plus text search

- discover solutions, projects, source roots, and test roots
- expose `repo_overview`
- expose `search_text`
- return structured file/line snippets
- ignore semantic symbol indexing until the workspace map is reliable

### Phase 2: C# project graph

- parse `.sln` / `.slnx` / `.csproj`
- resolve project references and package references
- detect production/test projects
- expose `project_graph`
- add project filters to text search

### Phase 3: Roslyn symbol index

- load projects through MSBuild/Roslyn
- index definitions and symbol metadata
- expose `search_symbol` and `definition`
- include stable symbol ids and index versions

### Phase 4: References and implementations

- expose `references`
- expose `implementations`
- group references by project, file, and production/test classification
- mark generated and stale results clearly

### Phase 5: Context packs and impact summaries

- expose `context_pack`
- expose `related_tests`
- expose first-pass `impact`
- tune ranking with real agent transcripts from large repos

### Phase 6: Human inspection surface

- add a TermAl Code Navigation tab
- show project graph, symbol search, references, and context-pack previews
- wire source-result clicks into existing source panels

## Open Questions

- Should the MCP server live inside TermAl, or should TermAl supervise an
  external per-workspace process?
- How should remote workspaces expose indexes without copying huge repositories
  back to the local machine?
- How much call graph data should be precomputed versus resolved on demand?
- Should context-pack ranking be deterministic only, or should it allow an
  optional model-assisted reranker?
- What is the right cache invalidation boundary for generated source, source
  generators, and conditional compilation?
- How should multi-targeted projects report symbol identity when a symbol is
  compiled under several target frameworks?
