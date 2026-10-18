# Root coding sessions

`POST /api/sessions` is the supported API for coordinators to create ordinary
root coding sessions. It creates an idle session; assignment and execution are
separate steps. This page records the existing API and the coordinator procedure
approved by Greg on 2026-10-13. It does not change repository authority rules.

Related: [Agent delegation sessions](agent-delegation-sessions.md),
[Architecture](../architecture.md) and
[Supervised Pilot Onboarding](supervised-pilot-onboarding.md), whose supervisor
steps create a colleague's root session in a project.

## Choosing a route

| Route | Use |
| --- | --- |
| TermAl UI, new-session dialog | Operator creation: select the agent and project, then create and open the session. The UI sends the same POST, including an explicitly configured model; the default-preference sentinel omits the model and uses the backend default. Ordinary Codex creation leaves prompt settings to the backend defaults. See `ui/src/app-workspace-actions.ts:382`, `ui/src/app-session-actions.ts:783`, `ui/src/app-session-model-requests.ts:40` and `ui/src/api.ts:954`. |
| `POST /api/sessions` | Coordinator creation for a ready independent coding package, with an explicit model and a recorded receipt. The procedure below covers local project roots. |
| `termal_spawn_session` | A parent creates a bounded child delegation: reviewer, explorer or coding worker. The parent integrates coding subwork; the child does not land independently. Use the delegation contract for write policies, isolated worktrees, result packets and waits. |

The coordination CLI currently supports `sessions list` and mailbox operations;
there is **no `sessions create` subcommand** (`src/coordination_cli.rs:19`). An
HTTP client such as PowerShell can call the supported API from a shell. A REPL
(`termal repl` / `termal cli`) is not a root-session creation client for the host.

**Never run the TermAl binary without a known subcommand.** With no arguments it
selects server mode (`src/main.rs:703`), which can start another host rather than
show help. Use the host-provided `TERMAL_CLI`, `TERMAL_SESSION_ID` and
`TERMAL_BASE_URL` for coordination; keep the actual session identity.

## Coordinator ownership and record

The project coordinator creates or resumes roots for ready, disjoint packages,
within host capacity; this API's availability does not grant every agent
permission to allocate roots. Each coding root owns one package, claim and
worktree, and produces its own evidence and landing handoff. Keep bounded coding
delegations for subwork integrated by their parent. Do not create speculative or
unbounded idle capacity. Serial gate windows and landing ownership remain in
force.

Codex `gpt-6.1-sol` is the default for new coding assignments. Record an exception
when Sol is unavailable or moving already-started work would cost more than
continuing it. The recorded initial exception was the existing Claude
CSharpProof assignment; an existing Codex root already using Sol was not an
exception. This is a coordinator choice, not the API's omitted-model default.

Use the agreed [team strategy](#team-strategy) to choose roots, bounded
delegations and capacity. Ready package count alone is insufficient evidence of
capacity.

For **every root created**, the creator writes a tracker note on its package's
item containing:

- Creator name and session ID, timestamp and route.
- Project ID and resolved cwd, even when only one was supplied in the request.
- Explicit agent and model, plus any requested approval, sandbox and effort settings.
- Purpose and task item; record the assignment-to-session mapping.
- Creation receipt: HTTP status, `sessionId`, `revision`, `serverInstanceId` and
  the returned `session` (or its relevant identity/settings fields).

Purpose, task and creator are coordination records, **not fields of
`CreateSessionRequest`**. The API does not write this tracker note automatically.
Check the receipt against the intended project, cwd and model before sending the
assignment. The root then claims its item, creates its worktree and names its
source root before its first edit, following the repository's turn-boundary rule.

## Team strategy

The TermAl and Engram coordinators agreed a joint team strategy on 2026-10-13. Its single maintained text is Engram's [team strategy](https://github.com/grlap/Engram/blob/master/docs/team-strategy.md); this page does not restate it. For TermAl, a root's package lands by hand-off to the integration owner under this repository's CLAUDE.md, and the coordinator who creates a root records it as described in [Creating and recording a local root](#creating-and-recording-a-local-root).

## Request contract

Send JSON with `Content-Type: application/json` to the running host's
`POST /api/sessions` (`src/main.rs:408`, `src/api.rs:1395`). Field names are
camelCase. All fields are optional at the wire level; omission or JSON `null`
means no explicit value. Unknown fields are ignored by this request type, so
check spelling rather than treating a successful POST as field validation.
Source: [CreateSessionRequest](../../src/wire.rs#L1158), `src/wire.rs:1156–1176`.

| Field | Accepted value and behavior |
| --- | --- |
| `agent` | Case-sensitive `Codex`, `Claude`, `Cursor`, `Gemini`, `OpenCode`, `Kimi`; omitted defaults to `Codex`. The lowercase CLI spellings are not JSON enum values (`src/wire.rs:197`, `src/session_crud.rs:3749`). |
| `name` | String; trimmed, with blank treated as absent. Absence generates a name (`src/session_crud.rs:3795`, `src/state_inner.rs:113`). |
| `workdir` | String naming an existing directory; trimmed, resolved and normalized. Empty or non-directory paths fail. Use an absolute path in coordinator requests (`src/paths.rs:354`, `src/paths.rs:381`). |
| `projectId` | String matching an existing project. With no workdir, the project's root is used. With both, a local workdir must be inside that project. With workdir alone, the host infers a containing project; without either, it uses its default workdir (`src/session_crud.rs:3775–3835`). |
| `model` | String; trimmed, blank treated as absent. Absence uses the app's configured default for the agent. Creation does not validate model membership in a provider catalog. OpenCode additionally limits nonblank models to 200 characters and rejects control characters (`src/session_crud.rs:3781`, `src/state_inner.rs:115`, `src/opencode.rs:17`). |

Agent-specific fields, validated against `src/wire.rs:834–1043` and
`src/session_crud.rs:3750–3773`, `src/session_crud.rs:3841–3985`:

| Agent | Fields and JSON values |
| --- | --- |
| Codex | `approvalPolicy`: `untrusted`, `on-failure`, `on-request`, `auto-approve`, `never`; `sandboxMode`: `read-only`, `workspace-write`, `danger-full-access`; `reasoningEffort`: `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra`. Omitted settings use backend preferences. |
| Claude | `claudeApprovalMode`: `ask`, `auto-approve`, `plan`, `read-only-auto-approve`; `claudeEffort`: `default`, `low`, `medium`, `high`, `xhigh`, `max`. Omitted settings use backend preferences. |
| Cursor | `cursorMode`: `agent`, `plan`, `ask`; omitted mode uses the backend default. |
| Gemini | `geminiApprovalMode`: `default`, `auto_edit`, `yolo`, `plan`; omitted mode uses the backend default. |
| OpenCode | `opencodeApprovalMode`: `ask`, `auto-approve`; omitted policy uses backend preferences. Dynamic mode is selected after OpenCode reports its live options; it is not a creation field. |
| Kimi | `kimiApprovalMode`: `ask`, `auto-approve`; `kimiMode`: `default`, `plan`, `yolo`, `auto`. Omitted policy uses backend preferences; omitted mode is `default`. |

`model` is accepted for every agent. Supplying another agent's non-null settings
is rejected with HTTP 400. Allowed enum values describe the wire format, not a
promise that a particular provider/model supports every setting.

## Success, errors and admission

Success is **HTTP 201** with a JSON object containing `sessionId`, `session`,
`revision` and `serverInstanceId` (`src/api.rs:1395–1400`,
[CreateSessionResponse](../../src/wire.rs#L3000)). `session` is the full session
wire object with resolved identity, workdir, project and settings. A new local
root is idle, with no messages, runtime or `parentDelegationId`
(`src/state_inner.rs:157`, `src/state_inner.rs:181–253`). `revision` is the
client-state revision; `serverInstanceId` identifies the server lifetime. It is
not a task revision or an admission grant.

The creation path checks blocking agent readiness, creates the record and
publishes `SessionCreated` (`src/agent_readiness.rs:42`,
`src/session_crud.rs:3835`, `src/session_crud.rs:3941–4023`). Persistence normally
queues a background delta; the 201 receipt is not a synchronous SQLite commit
acknowledgment (`src/sse_broadcast.rs:159–181`). Creation has no prompt and does
not authorize a turn, prove provider/model availability or claim an Engram item.
Assignment delivery and Engram turn admission happen separately
(`src/api.rs:1817`, `src/api.rs:266`).

Errors have two body formats. This handler takes Axum's `Json` extractor directly;
it does **not** use `api_json_rejection` (`src/api.rs:1374`, `src/api.rs:1397`).
Extractor rejections are plain text; handler `ApiError` responses are JSON
`{"error":"..."}` (`src/wire.rs:186`).

| Condition | Status and body |
| --- | --- |
| Malformed JSON, such as an invalid backslash escape | 400, plain text beginning `Failed to parse the request body as JSON`, followed by parse detail. |
| Valid JSON with an unknown agent, invalid enum or wrong field type | 422, plain text beginning `Failed to deserialize the JSON body into the target type`, followed by field/variant detail. For example, `agent: "Unknown"` fails before the handler. |
| Missing or unsupported JSON content type | 415, plain text `Expected request with \`Content-Type: application/json\``. |
| JSON request body exceeds 16 MiB | 413, plain text beginning `Failed to buffer the request body`, with limit detail (`src/main.rs:67`, `src/main.rs:597`; `axum-core` 0.5.6 `src/extract/rejection.rs:40–47`). |
| Unknown model string | No catalog rejection at creation; normally 201 if the other checks pass. A later provider turn can fail. OpenCode's length/control-character validation is the exception described above. |
| Unknown explicit project | 400, JSON error `unknown project \`<id>\`` (`src/session_crud.rs:3803`, also rechecked at `src/session_crud.rs:3910`). |
| Empty workdir / non-directory | 400, JSON error `session workdir cannot be empty` / `\`<path>\` is not a directory` (`src/paths.rs:354–367`). |
| Workdir outside the explicitly selected local project | 400, JSON error `session workdir \`<cwd>\` must stay inside project \`<name>\`` (`src/session_crud.rs:3828`). |
| Existing workdir outside every project, with no projectId | Can succeed as a projectless root. The coordinator procedure still requires a project record; supply `projectId` to enforce containment (`src/session_crud.rs:3807–3818`, `src/session_crud.rs:3938`). |
| Agent readiness is blocking | 400, JSON error containing the readiness detail (`src/agent_readiness.rs:49`, `src/session_crud.rs:3835`). Nonblocking warnings do not reject creation. |
| Wrong agent-specific settings | 400, JSON error describing supported settings; for example `opencodeApprovalMode is only supported by OpenCode` (`src/session_crud.rs:3750`, `src/session_crud.rs:3841`). |
| Explicit project's root or remote ownership changes during preflight | 409, JSON error `project changed while creating the session` (`src/session_crud.rs:3923–3930`). An inferred attachment that disappears or changes is dropped, leaving a projectless root (`src/session_crud.rs:3916–3934`). |
| Fallback persistence or blocking task fails | 500, JSON error beginning `failed to persist session:` or `blocking task failed:` (`src/session_crud.rs:4001`, `src/api.rs:1371`). |

Extractor status/body sources are the pinned Axum 0.8.8 dependency
(`Cargo.lock:247`): `axum/src/extract/rejection.rs:11–39`; plain-text conversion
is in `axum-core` 0.5.6, `src/macros.rs:61–75,120–133`. These are dependency
source locations, not TermAl's JSON error wrapper. Parse-detail suffixes vary
with the bad input. An explicitly selected remote project takes the remote
proxy path (`src/session_crud.rs:3820`); this local-root procedure does not
establish a remote creation or capacity policy.

## Limitations

- Root creation does not validate an unknown model against the provider catalog;
  the receipt cannot establish that a later turn will run that model.
- With only an existing cwd outside all projects, the API can create a projectless
  root; use an explicit `projectId` and verify the receipt for coordinated work.

## Creating and recording a local root

1. Select the ready package, confirm its files are disjoint from other owners,
   inspect capacity and choose an explicit model. Reuse a suitable existing root
   when possible.
2. POST the request and preserve the complete status/body receipt. Choose
   `projectId` to use the project root, `workdir` to request a directory and infer
   its project, or both to enforce local containment. For coordinated work,
   verify the resolved project and cwd even when only one was sent.
3. Record the required provenance note before delivering the assignment.
4. Send the bounded package assignment to the returned root through the supported
   UI/message or durable mailbox route. A successful creation alone leaves it idle.
5. Follow the existing claim, worktree, test, review and integration workflow.

Example request, matching the recorded TermAl Sol root settings:

```json
{
  "agent": "Codex",
  "name": "Termal::Sol-docs",
  "projectId": "project-6",
  "workdir": "C:/github/Personal/TermAl",
  "model": "gpt-6.1-sol",
  "approvalPolicy": "never",
  "sandboxMode": "danger-full-access",
  "reasoningEffort": "high"
}
```

For a project-root request, omit `workdir` and keep `projectId`; for the workdir
form, omit `projectId` and verify the inferred project in the receipt. Project
IDs are host-local: discover the correct ID instead of copying this example to
another host. The approval/sandbox settings above describe the recorded example;
they do not grant additional repository authority.

PowerShell 7 can construct JSON without shell escape transformations and preserve
both success and HTTP error responses with `-SkipHttpErrorCheck`:

```powershell
$request = @{
    agent = 'Codex'
    name = 'Termal::Sol-docs'
    projectId = 'project-6'
    workdir = 'C:/github/Personal/TermAl'
    model = 'gpt-6.1-sol'
    approvalPolicy = 'never'
    sandboxMode = 'danger-full-access'
    reasoningEffort = 'high'
} | ConvertTo-Json
$receipt = Invoke-WebRequest -Method Post `
    -Uri "$env:TERMAL_BASE_URL/api/sessions" `
    -ContentType 'application/json' -Body $request -SkipHttpErrorCheck
$receipt.StatusCode
$receipt.Content
```

Inspect the status before using `sessionId`; a non-201 body is an error receipt.
This example requires PowerShell 7 (`pwsh`); Windows PowerShell 5.1 does not
support `-SkipHttpErrorCheck`. Transport failures still throw and need the
reconciliation described below. Use the running host's supplied URL.
Forward slashes also avoid the observed Git Bash JSON quoting failure: a Windows
backslash in JSON must be escaped as `\\`, and shell quoting must preserve it.
Three recorded TermAl creations returned sessions `session-12326`,
`session-12327` and `session-12328`, revisions `19899818` through `19899820`;
their creator, package mappings and receipts remain in their tracker notes.
Those identifiers are examples of creation receipts, not targets to recreate.

The POST has no creation idempotency key (`src/wire.rs:1158`). If transport fails
after sending, do not blindly retry: a root may already exist. Reconcile the
session inventory and provenance before deciding whether to create another.
