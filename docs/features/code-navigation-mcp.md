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
- Provide file outlines so agents can inspect large files without reading full
  bodies.
- Support reverse navigation from stack traces, build errors, grep hits, and
  diff hunks back to the owning symbol.
- Build a persisted project and symbol index per workspace.
- Support C# first using Roslyn/MSBuild semantics.
- Let agents request context packs around a file, symbol, bug, build error, or
  planned change.
- Distinguish semantic facts from heuristic guesses.
- Support dirty workspaces by mixing persisted index data with live file reads
  when needed.
- Expose stable symbol handles and clear invalidation when indexes refresh.
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

## Agent Navigation Workflow

The MCP should make semantic navigation the agent's default reflex, not a
fallback after broad text search.

When the task already names a symbol, stack frame, build error, route, or file,
the agent should follow a surgical flow:

```text
repo_overview()
  -> search_symbol(...) or symbol_at(path, line, column?)
  -> definition(target)
  -> outline(path) for large files before any whole-file read
  -> references(target, group_by = "project")
  -> related_tests(target) before behavior changes
  -> read only the returned snippets or spans
```

When the task is exploratory, the agent should first find the project cluster:

```text
repo_overview()
  -> project_graph(project_or_prefix, depth = 2)
  -> search_symbol(...) for entrypoints such as controllers, handlers, services
  -> context_pack(target, budget)
  -> drill into specific symbols with the surgical flow
```

The rule to encode in agent instructions is:

**Never read a large file before `outline` or `definition` identifies the
relevant span.**

## Tool Surface

### Core tools

These tools should ship together because they cover the minimum useful
large-repository navigation loop.

```text
repo_overview()
```

Returns a compact workspace map:

- detected languages
- solution and project counts
- primary or recently active solutions, when detectable
- major source/test roots
- target frameworks
- generated-code conventions
- recently changed files
- index status, timestamp, coverage, and project-level missing/stale summaries

```text
find_file(name_or_glob, filters, limit, cursor)
```

Cheap path-only lookup for cases where symbol semantics are unnecessary or the
agent only has a filename from a log, build output, or human prompt.

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
outline(path, depth)
```

Returns a file-level symbol map without method bodies. This is the primary
token-saver for large C# files and should be cheap enough to call before any
whole-file read.

Useful outline fields:

- namespace
- type/member kind
- name
- signature
- accessibility
- source span
- containing type
- partial declaration flag
- generated/test classification

```text
search_symbol(query, kinds, filters, limit, cursor)
```

Finds symbols by name, partial name, fully qualified name, prefix, containing
type/namespace, or regex/fuzzy query. The match mode should be explicit so the
server does not have to guess whether a query is exact or fuzzy.

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
symbol_at(path, line, column)
```

Reverse lookup for stack traces, build errors, grep hits, diagnostics, and diff
hunks. Returns the smallest containing symbol plus any enclosing type,
namespace, project, and stable symbol id.

```text
definition(target)
```

Returns the exact definition for a symbol or source location, including all
declaration spans for partial types/members, path, line span, signature,
containing type, containing project, and a small snippet.

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

Useful options:

- direction: `upstream`, `downstream`, or `both`
- format: `edges` or a compact tree summary
- include packages
- include test projects

### Higher-level tools

```text
context_pack(target, budget)
```

Builds a compact bundle of relevant source context for an agent turn. This
should be built on top of reliable definitions, references, outlines, project
graph data, and related-test signals rather than forwarding raw top-N matches.
The request should allow a task hint such as `investigate_bug`, `rename`,
`add_feature`, `review`, or `explore`, plus `must_include_paths` and
`exclude_paths` for steering.

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
namespace_layout(prefix, filters, limit, cursor)
projects_under(prefix, filters, limit, cursor)
```

Lists namespaces, folders, and projects under a product/team prefix for
cold-start exploration.

```text
dependency_path(from_project, to_project, limit)
```

Returns the shortest project-reference or package-reference paths explaining
why one project depends on another.

```text
projects_containing(path)
```

Returns all projects that compile or link a file. This matters for shared files
and linked `<Compile Include="...">` entries.

```text
config_lookup(key, filters, limit, cursor)
```

First-class lookup across configuration files such as `appsettings*.json`,
`Directory.Build.props`, `Directory.Build.targets`, `global.json`, and
`NuGet.config`.

```text
generated_for(target)
```

Explains generated symbols or files, including the producing source, attribute,
source generator, or MSBuild target where detectable.

```text
recent_changes(scope, since, limit)
```

Git-aware navigation for current work, returning recent files, symbols, and
projects related to a scope.

```text
xml_doc(target)
```

Returns XML documentation summaries without requiring a source snippet read.

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

Batch forms should exist for common fan-out calls:

```text
batch_definition(targets)
batch_references(targets, filters, group_by, limit)
batch_outline(paths, depth)
```

## Result Shape

Every source result should be stable, line-addressable, and small. All list
responses should include a short summary, cursor metadata, and index freshness
metadata so the agent can decide whether to tighten filters, page, or trust the
answer.

Targets should be accepted in any of these forms:

```json
{ "symbolId": "roslyn:Billing.Application:..." }
{ "path": "src/Billing/Application/InvoiceService.cs", "line": 42, "column": 17 }
{ "qualifiedName": "Billing.Application.InvoiceService.CreateInvoiceAsync" }
```

Symbol ids are stable within an `indexVersion`. If an index refresh invalidates
an id, tools should return a clear stale-id error and, where possible, a
replacement candidate.

```json
{
  "path": "src/Billing/Application/InvoiceService.cs",
  "line": 42,
  "endLine": 88,
  "snippetLines": [38, 54],
  "project": "Billing.Application",
  "targetFramework": "net8.0",
  "symbolId": "roslyn:Billing.Application:...",
  "kind": "method",
  "accessibility": "public",
  "containingNamespace": "Billing.Application",
  "containingType": "InvoiceService",
  "signature": "Task<Invoice> CreateInvoiceAsync(CreateInvoiceCommand command)",
  "isGenerated": false,
  "isTest": false,
  "score": 0.93,
  "confidence": "exact",
  "snippet": "public async Task<Invoice> CreateInvoiceAsync(...)",
  "indexVersion": "workspace-sha-or-index-id",
  "indexStatus": "fresh"
}
```

References should default to grouped output, not a flat global dump:

```json
{
  "summary": "47 references across 12 projects (8 production, 4 test).",
  "totalReferences": 47,
  "groupBy": "project",
  "groups": [
    {
      "key": "Billing.Api",
      "count": 4,
      "summary": "4 callers in InvoicesController and BillingWebhook.",
      "locations": []
    }
  ],
  "truncated": true,
  "nextCursor": "...",
  "indexVersion": "workspace-sha-or-index-id",
  "indexStatus": "fresh",
  "coverage": {
    "indexedProjects": 1510,
    "totalProjects": 1600
  }
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

Every tool response should carry index metadata when freshness could affect the
answer:

- `indexVersion`
- `indexStatus`: `fresh`, `building`, `partial`, or `stale`
- `coverage`: indexed projects/files compared with discovered total
- missing or stale project summaries for partial indexes

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
- linked compile items
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
- test vs production classification
- target framework and conditional-compilation context

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
- source-generator inputs and generated outputs, where detectable

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
- small project-scoped result sets over broad global result sets
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
- re-bind symbols in modified files before returning cross-file references that
  could be affected by working-tree edits
- allow explicit `refresh(scope)` in later phases

TermAl's file-change awareness can feed the same invalidation model, but the
navigation server should still work as a standalone MCP process.

For large C# repositories, Roslyn cold-load can take minutes. Partial readiness
is expected. `repo_overview` should expose which projects are loaded, loading,
failed, stale, or not yet indexed so agents can state uncertainty instead of
treating partial answers as complete.

## Agent Instructions

Workspace setup should include compact instructions similar to:

```markdown
## Code Navigation MCP

This repository is too large for broad grep-based C# navigation. Use the
read-only Code Navigation MCP before reading files.

Default flow:
1. Call `repo_overview()` before code work and check `indexStatus`.
2. Use `search_symbol` for C# identifiers. Use `search_text` only for config
   keys, route strings, error messages, and literals.
3. If starting from a stack trace, build error, diagnostic, grep hit, or diff
   hunk, call `symbol_at(path, line, column?)`.
4. Before reading a large file, call `outline(path)` and read only the needed
   spans.
5. Before changing behavior, call `references(target, group_by="project")` and
   `related_tests(target)`.
6. Trust `confidence: exact`; treat `heuristic`, `partial`, and `stale` as
   leads.
7. Keep limits small. Tighten filters before paging broad result sets.
```

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
- default limits should stay small: roughly 10 symbol/reference results and 5
  file-grouped results
- snippets should have a default line budget
- huge result sets should return summaries and paging metadata
- generated folders and build artifacts should be excluded by default
- tests should be included separately from production by default rather than
  mixed into one ranked list
- project filters must be first-class and glob-capable
- index state should be cancellable and restartable
- indexing should not block ordinary TermAl session startup
- every response should include index status when freshness could matter

The server should avoid hidden writes. Future write-capable tools, if any, must
be separate from navigation tools and go through TermAl's normal approval and
file-change protection paths.

## Phased Plan

### Phase 1: Raw workspace map plus path/text search

- discover solutions, projects, source roots, and test roots
- expose `repo_overview`
- expose `find_file`
- expose `search_text`
- return structured file/line snippets
- ignore semantic symbol indexing until the workspace map is reliable

### Phase 2: C# project graph and syntactic outlines

- parse `.sln` / `.slnx` / `.csproj`
- resolve project references and package references
- detect linked compile items
- detect production/test projects
- expose `project_graph`
- expose `outline` using syntax trees where semantic loading is unavailable
- expose `projects_containing`
- add project filters to text search

### Phase 3: Roslyn symbol index

- load projects through MSBuild/Roslyn
- index definitions and symbol metadata
- expose `search_symbol`, `symbol_at`, and `definition`
- include stable symbol ids and index versions
- expose partial-index coverage and stale-id failure modes

### Phase 4: References and implementations

- expose `references`
- expose `implementations`
- group references by project, file, and production/test classification
- mark generated and stale results clearly

### Phase 5: Context packs and impact summaries

- expose `context_pack`
- expose `related_tests`
- expose first-pass `impact`
- expose `dependency_path`, `namespace_layout`, and `config_lookup`
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
- Should context-pack ranking remain deterministic in the MCP, with optional
  model-assisted reranking left to the calling agent?
- What is the right cache invalidation boundary for generated source, source
  generators, and conditional compilation?
- How should multi-targeted projects report symbol identity when a symbol is
  compiled under several target frameworks?
- How should symbol ids resolve across index rebuilds when source has moved but
  the logical symbol still exists?
