# Feature Brief: Code Navigation MCP

## Status

Proposed.

This brief defines an MCP server that gives agent sessions a fast, semantic way
to navigate large source trees. The first target is very large C# workspaces
with a few thousand projects, but the design should leave room for language
adapters beyond C#.

Related:
- [Agent Delegation Sessions](./agent-delegation-sessions.md)
- [Instruction Debugger](./instruction-debugger.md)
- [File Change Awareness](./file-change-awareness.md)
- [Source Renderers](./source-renderers.md)

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

For a C# workspace with a few thousand projects, the missing capability is a
compact, structured code intelligence layer that can answer source-navigation
questions without dumping large files into the transcript.

## Goals

- Provide exact symbol navigation through MCP tools.
- Replace broad `rg`/`grep` as the default source-navigation reflex for Claude,
  Codex, and delegated explorer/reviewer agents in indexed workspaces.
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
- Share one warm workspace index across parent sessions and delegated child
  sessions so each agent does not pay a cold-start penalty.
- Keep common warm navigation calls fast enough that agents do not fall back to
  shell search.

## Non-goals for v1

- No full IDE replacement.
- No autonomous code modification through the navigation MCP.
- No automatic commits, pushes, or branch creation.
- No attempt to index every language equally in the first version.
- No cloud dependency for local workspaces.
- No unrestricted transcript-sized output; every tool should enforce paging and
  result budgets.

## Core Idea

Add a **Code Navigation MCP** server for each active local workspace:

```text
workspace files
  -> lightweight workspace discovery
  -> persisted path/text/project/symbol index
  -> bounded language-service cache
  -> search and ranking layer
  -> MCP tools consumed by agents
```

The MCP server should be read-only by default. Agents still use their normal
file-editing and terminal tools to make changes, while the navigation MCP gives
them the shortest reliable path to the right files, symbols, tests, and
dependency edges.

TermAl should supervise one shared navigation server per workspace root, not one
per agent process. Parent sessions and delegated Claude/Codex/Cursor/Gemini
sessions should attach to that shared server and reuse its persisted index and
warm caches. Remote workspace support is separate because index placement and
path mapping are different problems.

The primary design rule is:

**Return the smallest precise context that lets an agent take the next step.**

## Navigation Layers

The MCP should expose three distinct layers of source understanding. Agents
should use the cheapest layer that can answer the question, while preferring
compiler-backed answers for code facts.

- **Indexed text lookup**: `find_file` and `search_text` answer path and text
  questions with grep-like matching, stable line/byte offsets, workspace-aware
  ranking, and response budgets. This is the right layer for config keys, route
  strings, error messages, log fragments, comments, and literals: questions
  that are genuinely about where exact text appears.
- **Syntax / AST structure**: `outline` and syntax-derived spans answer
  structural questions inside a file without requiring a whole-file read or
  cross-file symbol resolution. This is the right layer for finding containing
  types, members, local functions, top-level declarations, and source spans
  before reading or editing a large file.
- **Semantic model**: `symbol_at`, `search_symbol`, `definition`,
  `references`, `project_graph`, `projects_containing`, and later tools such as
  `implementations`, `callers`, `callees`, `type_hierarchy`, and
  `related_tests` answer program-meaning questions through Roslyn/MSBuild or
  the relevant language adapter. This is the right layer for definitions,
  exact references, overloads, partial declarations, generated code ownership,
  project membership, dependency direction, and impact analysis.

Text finds bytes. Syntax finds source shape. The semantic model finds program
meaning. `search_symbol` may use name matching to locate candidates, but its
results should still carry semantic identity: symbol id, kind, project, target
framework, and declaration spans.

The default preference should be:

```text
semantic model for code facts
syntax / AST for file structure and spans
indexed text lookup for literal text questions
shell rg/grep only for unavailable, unindexed, or non-source scopes
```

Agents should still use shell `rg` or `grep` when the MCP server is
unavailable, the scope is outside the workspace index, the query targets
binary-adjacent logs or transient build/generated output, or the question is
outside source navigation. In an indexed workspace, shell text search should
not be the first step for code facts unless `server_capabilities()` or index
metadata shows that the relevant semantic layer is unavailable, partial, or
stale.

## Agent Navigation Workflow

The MCP should make the semantic layer the agent's default reflex for code
facts, not a fallback after broad text search.

When the task already names a symbol, stack frame, build error, route, or file,
the agent should follow a surgical flow:

```text
repo_overview()
  -> search_symbol(...) or symbol_at(path, line, column?)
  -> definition(target)
  -> outline(path) for large files before any whole-file read
  -> references(target, group_by = "project")
  -> related_tests(target) before behavior changes
  -> source_context(path, spans, contextLines, maxBytes)
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

For agent instructions, a "large file" should mean any source file above roughly
400 lines or 24 KB, unless the server reports a lower language-specific
threshold. Agents may still use shell `rg` for unindexed generated output,
binary-adjacent logs, or when the MCP server is unavailable, but the default
path for source, project, and symbol navigation should be the MCP.

## Tool Surface

The surface should be divided into phase-gated capability levels so agents know
which workflows are safe. The server should expose its effective capabilities
through normal MCP tool discovery and, optionally, a small
`server_capabilities()` tool that reports supported languages, index status,
available tools, response budgets, and disabled features.

For implementation planning, core tools are required for the grep-replacement
MVP, higher-level tools should follow once the core signals are reliable, and
expanded tools are optional or later-phase capabilities.

### Core tools

These tools should ship together because they cover the minimum useful
large-repository navigation loop and are the minimum agent-usable grep
replacement.

```text
server_capabilities()
```

Returns supported languages, enabled tools, index status, response budgets,
latency/deadline defaults, and feature flags for partial phase availability.
Agents can use normal MCP tool discovery too, but a compact capability summary
helps them branch without probing broad queries.

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

Fast text search with ranking and filters. This is the indexed text lookup
layer: it should behave like `rg`, but with structured output, stable line/byte
offsets, result budgets, and workspace-aware ranking. For code facts, agents
should use the semantic layer instead: `search_symbol`, `definition`, and
`references`.

Useful filters:

- fixed string vs regex
- case-sensitive vs case-insensitive
- whole-word matching
- multiline matching, when the underlying index supports it
- path globs
- project name
- language
- production vs test
- generated vs handwritten
- file extension
- hidden files
- maximum line length and snippet bytes

Ranking should prefer exact-token matches, handwritten source, project-local
matches, recently changed files when relevant, and production/test scope
matching the query. It should demote build output, vendored dependencies,
generated files, designer files, and broad package/cache directories by default.

```text
outline(path, depth)
```

Returns a syntactic file map with names and spans, but without method bodies or
cross-file resolution. This is the primary token-saver for large C# files and
should be cheap enough to call before any whole-file read.

`depth` should be bounded and explicit:

- `1`: namespaces and top-level types
- `2`: type members, excluding method bodies
- `3`: local functions, top-level statements, and lambda anchors where the
  language adapter can report them cheaply

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
source_context(path, spans, contextLines, maxBytes)
```

Reads bounded live source around one or more returned spans. This is the bridge
between navigation and editing: `outline`, `definition`, `references`, and
`symbol_at` identify spans; `source_context` returns only the selected live
source with line numbers, byte limits, truncation metadata, and freshness
metadata. Batch span reads should be supported for common fan-out workflows.

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
- record
- record struct
- enum
- delegate
- method
- constructor
- property
- field
- event
- extension method
- local function
- lambda anchor
- top-level statement
- attribute usage
- controller
- handler
- test

`attribute usage` is not a Roslyn symbol kind; it should be treated as an
adapter-level search category over attributes attached to symbols.

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

Because this is the most explosive query in a workspace with a few thousand
projects, it should default to grouped summaries and bounded locations rather
than a global flat dump. Filters should include project glob, production/test,
generated/handwritten, reference kind, caller type, target framework, and
whether to include interface or override expansion. Reflection, string-based
lookup, DI container wiring, and expression-tree-only edges should be reported
as `heuristic` or excluded by default.

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

```text
projects_containing(path)
```

Returns all projects that compile or link a file. This matters for shared files
and linked `<Compile Include="...">` entries and is needed before interpreting
diagnostics or symbol identity for files included by multiple projects.

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
callees(target, depth, filters, limit, cursor)
```

Walks the call graph around a method, constructor, property accessor, or
delegate invocation. These tools should be on-demand, scope-limited, and
confidence-labeled; precomputing whole-repo call graphs is not required for the
core grep-replacement workflow.

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
batch_source_context(requests)
```

## Result Shape

Every source result should be stable, line-addressable, and small. All list
responses should include a short summary, cursor metadata, and index freshness
metadata so the agent can decide whether to tighten filters, page, or trust the
answer.

All tool responses should enforce both item and byte/token budgets. Defaults
should be tuned for agent turns rather than human IDE panes:

- default response target: roughly 8 KB of JSON/snippet payload
- hard response cap: roughly 32 KB unless the caller explicitly opts into a
  larger budget
- snippets should include line numbers and a small surrounding context by
  default
- `truncated: true` means the result set was larger than the response budget
- `partial: true` means the server hit a deadline, stale dependency, failed
  project load, or partial index and is returning an incomplete answer
- list tools should return `nextCursor` when more exact results are available
- all results should include enough path, line, column, and byte-offset metadata
  for a follow-up `source_context` call

Targets should be accepted in any of these forms:

```json
{ "symbolId": "roslyn:Billing.Application:net8.0:M:Billing.Application.InvoiceService.CreateInvoiceAsync(...)" }
{ "path": "src/Billing/Application/InvoiceService.cs", "line": 42, "column": 17 }
{ "qualifiedName": "Billing.Application.InvoiceService.CreateInvoiceAsync" }
{
  "projectPath": "src/Billing/Application/Billing.Application.csproj",
  "targetFramework": "net8.0",
  "documentationCommentId": "M:Billing.Application.InvoiceService.CreateInvoiceAsync(...)"
}
```

Symbol ids are stable within an `indexVersion`. If an index refresh invalidates
an id, tools should return a clear stale-id error and, where possible, a
replacement candidate. For C#, the stable identity should be based on Roslyn's
documentation comment id or metadata name plus project identity and target
framework, not a display name alone.

```json
{
  "path": "src/Billing/Application/InvoiceService.cs",
  "line": 42,
  "endLine": 88,
  "snippetLines": [38, 54],
  "project": "Billing.Application",
  "projectPath": "src/Billing/Application/Billing.Application.csproj",
  "assemblyName": "Billing.Application",
  "targetFramework": "net8.0",
  "configuration": "Debug",
  "symbolId": "roslyn:Billing.Application:net8.0:M:Billing.Application.InvoiceService.CreateInvoiceAsync(...)",
  "documentationCommentId": "M:Billing.Application.InvoiceService.CreateInvoiceAsync(...)",
  "kind": "method",
  "accessibility": "public",
  "containingNamespace": "Billing.Application",
  "containingType": "InvoiceService",
  "signature": "Task<Invoice> CreateInvoiceAsync(CreateInvoiceCommand command)",
  "isGenerated": false,
  "isTest": false,
  "score": 0.93,
  "confidence": "exact",
  "partial": false,
  "truncated": false,
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
  "partial": false,
  "nextCursor": "...",
  "indexVersion": "workspace-sha-or-index-id",
  "indexStatus": "fresh",
  "coverage": {
    "indexedProjects": 1510,
    "totalProjects": 2400
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

Confidence is separate from the navigation layer. For example, indexed text
lookup can return fresh or stale results, and semantic tools can return exact,
partial, or stale results depending on project load and index state. If exposed
as a field, `navigationLayer` should describe the abstraction used:
`text`, `syntax`, or `semantic`.

Every tool response should carry index metadata when freshness could affect the
answer:

- `indexVersion`
- `indexStatus`: `fresh`, `building`, `partial`, or `stale`
- `coverage`: indexed projects/files compared with discovered total
- missing or stale project summaries for partial indexes
- `partialReason`, when `partial: true`
- `deadlineMs` and `elapsedMs` for calls that may be slow

## C# First Index

The C# adapter should use Roslyn and MSBuild semantics instead of regex parsing.

Recommended index inputs:

- `.sln`, `.slnx`, `.slnf`, and discovered `.csproj` files
- `Directory.Build.props`
- `Directory.Build.targets`
- `global.json`
- `NuGet.config`
- package references
- project references
- linked compile items
- generated-code markers
- test framework packages and attributes

Workspace discovery must not assume one canonical solution. Large enterprise
repos often contain many overlapping solutions, generated solution filters, or
no useful solution file at all. The indexer should support:

- solution-less project discovery from canonical workspace roots
- `.slnf` solution filters and generated solution files
- overlapping solution membership without duplicating project identities
- lazy project loading when the current task only needs a subset
- explicit user/workspace preference for primary solutions or project clusters
- project load failures recorded as first-class index status, not hidden logs

Recommended symbol data:

- fully qualified metadata name
- display signature
- source span
- declaring project
- project path and stable project id
- assembly name
- target framework, configuration, and platform
- accessibility
- arity and parameter signature for overload identity
- attributes
- XML documentation summary, if available
- partial declaration spans
- generated vs handwritten flag
- test vs production classification
- nullable, preprocessor, and conditional-compilation context where relevant
- source-generator origin metadata where detectable

### Symbol Identity

C# symbol identity must handle overloads, partials, linked files, generated
source, and multi-targeted projects. A symbol id should include:

- adapter prefix and index version compatibility
- canonical project path or project id
- assembly name
- target framework and configuration/platform
- Roslyn documentation comment id or metadata name
- member arity and parameter signature for overloads
- generated-origin marker when the symbol has no handwritten source span

For multi-targeted projects, the default policy should choose a canonical target
framework for broad search results while preserving the exact TFM in symbol ids.
The canonical TFM should be configurable per workspace. When behavior differs
by TFM, tools should return separate results rather than collapsing them.

Partial types and members should share one logical symbol id with multiple
declaration spans. Extension methods should be anchored to their declaring
static type and method symbol, while search/ranking may expose the extended type
as a convenience field.

### Storage Model

The first implementation should use a persisted local index rather than
rebuilding navigation state inside each agent session. SQLite with FTS5 is the
default pragmatic storage choice unless benchmarks show it is insufficient.
The schema should include at least:

- files with path, language, hash, generated/test flags, and freshness state
- projects with path, name, assembly, target frameworks, solution membership,
  load status, and package/project references
- symbols with stable ids, kind, name facets, declaration spans, project ids,
  target frameworks, and generated/test flags
- references with symbol id, source location, reference kind, caller symbol,
  project id, and confidence
- text-search FTS rows with path, line/byte offsets, language, generated/test
  flags, and ranking features
- dependency graph edges
- index metadata, coverage, stale scopes, and failure summaries

The index should support single-writer/multi-reader access so multiple agents
can query while a background refresh updates stale scopes. The spec should track
a rough size budget per million lines of code once benchmark data exists.

### Memory And Process Model

A few-thousand-project C# repo cannot assume a retained whole-repo Roslyn
compilation. The indexer should separate:

- a lightweight persisted index for path/text/project/symbol metadata
- an ephemeral Roslyn workspace/project cache for semantic rebinding
- bounded compilation objects with LRU eviction
- background indexing workers with CPU and memory caps
- cancellation for indexing and for individual expensive tool calls

`MSBuildWorkspace.OpenSolutionAsync` over the entire repo should not be the
required startup path. The server should be useful during partial readiness,
load project clusters lazily, and never block ordinary TermAl session startup on
full semantic indexing.

### Source Generators

Generated source must be explicit in both coverage and confidence. The indexer
should record whether generated outputs are indexed, which generator or MSBuild
target produced them when detectable, and whether a symbol came from generated
or handwritten syntax. Generator execution should be cached per project snapshot
and should not run per query. When generated output is unavailable, tools should
return `partial` or `heuristic` instead of silently missing generated symbols.

Recommended semantic edges:

- project references
- package references
- type inheritance
- interface implementation
- method override
- method calls, when resolved on demand or from a bounded cached graph
- property/event references
- attribute usage
- dependency injection registrations, where detectable and confidence-labeled
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

The budget algorithm should be deterministic. A hard `maxTokens` or `maxBytes`
must never be exceeded. If the full pack does not fit, include content in this
order and report omitted categories:

- primary target definitions and requested `must_include_paths`
- direct tests with exact references
- direct callers or entrypoints closest to the target project
- relevant project graph edges
- nearby sibling declarations from outlines
- heuristic matches and recent-change hints

The response should distinguish `omittedBecauseBudget` from `partial` so agents
know whether the pack is complete within its requested scope.

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
- support delta refresh for changed files and affected project clusters
- keep stale project summaries queryable so agents can see what coverage is
  missing

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
read-only Code Navigation MCP before reading files. Prefer MCP search and
symbol tools over shell `rg`/`grep` for source navigation.

Default flow:
1. Call `repo_overview()` before code work and check `indexStatus`.
2. Use the semantic layer for code facts: `search_symbol`, `definition`,
   `references`, and `symbol_at`.
3. Use `search_text` for config keys, route strings, error messages, log
   fragments, comments, and literals.
4. If starting from a stack trace, build error, diagnostic, grep hit, or diff
   hunk, call `symbol_at(path, line, column?)`.
5. Before reading a large file, call `outline(path)`, then `source_context` for
   only the needed spans.
6. Before changing behavior, call `references(target, group_by="project")` and
   `related_tests(target)`.
7. Trust `confidence: exact`; treat `heuristic`, `partial`, and `stale` as
   leads.
8. Keep limits small. Tighten filters before paging broad result sets.
9. Use shell `rg` only when the MCP is unavailable, the relevant layer is
   unavailable or stale, the scope is unindexed, or the query is outside source
   navigation.
```

Codex and Claude prompts can share the same rules, but TermAl should inject
agent-specific wording where needed. In particular, delegated explorer and
reviewer sessions should be told that MCP navigation is mandatory before broad
file reads, because their value comes from finding the right source slice
without spending the parent transcript on exploration.

## TermAl Integration

TermAl should supervise the initial implementation as a workspace-scoped MCP
server process. A concrete command shape could be:

```text
termal code-nav-mcp --workspace-root <path> --index-root <path> --workspace-id <id>
```

The backend should own workspace path mapping, process lifecycle, index root
selection, and server restart behavior. Agent processes should receive only the
MCP descriptor and compact instructions.

Recommended user-facing integration points:

- workspace-level MCP server registration
- visible index status in the project/session chrome
- "Open in source" links for every source result
- optional Code Navigation tab for humans to inspect symbol search, references,
  project graph, and impact results
- delegation prompts that automatically tell explorer/reviewer agents to use
  the navigation MCP before reading broad file sets

Recommended agent integration:

- attach the MCP server to Codex through `config.mcp_servers` when the workspace
  is local and indexable
- attach the MCP server to Claude through generated `--mcp-config`
- include a compact instruction in session setup explaining which tools exist
  and when to prefer them
- expose the same MCP tool names across agents where possible
- keep the backend authoritative for workspace path mapping and server lifecycle
- make delegated child sessions inherit the parent workspace navigation server
  instead of launching their own cold indexer
- surface a clear unavailable/degraded state when the server is missing, still
  indexing, or has failed to load relevant projects

The integration should fail soft for ordinary chat but fail visible for
navigation-heavy code work. If a large C# workspace is detected and the MCP is
not attached, the session instructions should say that source navigation is
degraded and shell search may be noisy.

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
- every potentially expensive tool call should have a deadline, defaulting to
  roughly 5 seconds and configurable up to a bounded maximum
- broad calls that exceed their deadline should return `partial: true` with
  exact results found so far rather than blocking other agents

Warm-query latency targets should be explicit so replacing `rg` is measurable:

- `repo_overview`: p95 under 100 ms from the persisted index
- `find_file`: p95 under 100 ms
- `search_text`: p95 under 250 ms for scoped queries
- `outline`: p95 under 100 ms from syntax/index data
- `search_symbol`: p95 under 250 ms from the symbol index
- `definition`: p95 under 250 ms when the target project is already loaded or
  the definition span is in the persisted index
- `references`: p95 under 1 second for scoped, grouped queries; broader queries
  should return grouped summaries plus cursors or partial results

Default exclusions should be concrete and overridable:

- `**/.git/**`
- `**/{bin,obj,packages,node_modules,TestResults}/**`
- `**/.vs/**`
- `**/.idea/**`
- `**/*.{g,designer,generated}.cs`
- generated source folders discovered through MSBuild or source-generator
  conventions, unless the caller includes generated output explicitly

The server should avoid hidden writes. Future write-capable tools, if any, must
be separate from navigation tools and go through TermAl's normal approval and
file-change protection paths.

## Evaluation And Testing

The feature should be validated as an agent workflow, not only as an indexer.
Acceptance tests and benchmarks should cover:

- synthetic and real few-thousand-project C# workspaces
- multiple solutions, `.slnf`, overlapping solutions, and solution-less project
  discovery
- linked files compiled by multiple projects
- multi-targeted projects such as `net48;net8.0`
- generated source and missing-generator partial coverage
- stale index behavior after local edits
- grouped reference queries with large result sets
- Claude and Codex MCP attachment, including delegated child sessions
- response byte/token caps, cursors, deadlines, and partial results

Success metrics should include p95 tool latency, index size, memory ceiling,
percentage of agent file reads preceded by MCP span navigation, percentage of
shell `rg` calls avoided in large C# sessions, and click/open rates for returned
source locations in the human UI.

## Phased Plan

### Phase 0: Index substrate and workspace discovery

- implement the workspace-scoped MCP server process
- create the persisted index store and text-search tables
- discover `.sln`, `.slnx`, `.slnf`, loose `.csproj`, source roots, test roots,
  generated/build-output conventions, and default exclusions
- expose `server_capabilities`, `repo_overview`, `find_file`, and `search_text`
  for internal testing
- do not advertise this as a grep replacement yet

### Phase 1: Agent grep-replacement MVP

This is the first release that should be attached to Claude/Codex as the
preferred source-navigation path. It should ship as a coherent slice:

- expose `repo_overview`
- expose `find_file`
- expose `search_text` with grep-parity flags and workspace ranking
- expose `project_graph`
- expose `projects_containing`
- expose `outline`
- expose `source_context`
- expose `search_symbol`
- expose `symbol_at`
- expose `definition`
- expose scoped, grouped `references`
- load Roslyn/MSBuild semantics lazily for the project clusters needed by those
  symbol and reference calls
- attach the server to Codex and Claude sessions with agent instructions
- share the same warm server with delegated child sessions
- enforce response budgets, deadlines, partial results, and freshness metadata
- meet the initial warm-query latency targets for scoped queries

### Phase 2: C# scale hardening

- implement lazy Roslyn/MSBuild project loading by project cluster
- implement bounded compilation caches with LRU eviction
- finalize symbol id schema for overloads, partials, linked files,
  multi-targeting, and generated source
- implement delta invalidation from file-change awareness and standalone file
  watching
- record project load failures, stale scopes, and source-generator coverage
- benchmark on synthetic and real few-thousand-project repos

### Phase 3: Expanded semantic navigation

- expose `implementations`
- expose `callers` and `callees` as on-demand, scoped graph queries
- expose `type_hierarchy`
- expose `related_tests`
- expose `dependency_path`, `namespace_layout`, `projects_under`, and
  `config_lookup`
- tune ranking with real agent transcripts from large repos

### Phase 4: Context packs and impact summaries

- expose `context_pack`
- expose first-pass `impact`
- add optional `xml_doc`
- decide whether `diagnostics` reads last build output, live Roslyn analyzers, or
  both as separate tools
- keep all higher-level tools explainable as summaries over lower-level results

### Phase 5: Human inspection surface

- add a TermAl Code Navigation tab
- show project graph, symbol search, references, and context-pack previews
- wire source-result clicks into existing source panels

## Open Questions

- How should remote workspaces expose indexes without copying huge repositories
  back to the local machine?
- Should context-pack ranking remain deterministic in the MCP, with optional
  model-assisted reranking left to the calling agent?
- What is the right cache invalidation boundary for generated source, source
  generators, and conditional compilation?
- How should symbol ids resolve across index rebuilds when source has moved but
  the logical symbol still exists?
- Should SQLite + FTS5 remain the storage backend after large-repo benchmarks, or
  should symbol/reference search move to Tantivy/LMDB/another store?
- Which reciprocal links should be added from related feature briefs once this
  proposal is accepted?
