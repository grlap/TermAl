# Feature Brief: Kimi Code CLI Integration

TermAl exposes **Kimi** as an ACP-backed agent alongside Cursor, Gemini and
OpenCode. This is a CLI integration, not a direct Moonshot chat API client.
See the [architecture](../architecture.md),
[agent comparison](agent-integration-comparison.md), and
[current contracts](current-agent-contracts.md).

## Installed contract and setup

Checked 2026-09-20 against native Windows Kimi Code CLI **2.0.2**. The installed
binary completed `initialize` with protocolVersion 1 and advertised
`loadSession: true`, `sessionCapabilities.resume: {}`, and terminal auth method
`login`. A subsequent disposable session accepted existing authentication,
returned live model choices (K3 selected), and closed successfully. These probes
sent no model prompt and changed no login settings; they are not evidence of a
successful provider turn or end-to-end resume.

A second no-prompt disposable probe on the same build checked mode changes,
`session/resume`, and `session/load`. The mode setter's `config_option_update`
notification, its ACK, and both continuation responses each included the full
`configOptions` array (`model`, `thinking`, `mode`), including four model choices.
The empty conversation was closed after each check. This is payload-contract
evidence, not acceptance of transcript continuity or cancellation during a turn.
Partial config notifications without a model list leave the cached list intact.
An explicit model still requires advertised support and an exact setter ACK;
a nonconforming continuation missing that evidence refuses safely on every retry.

Official references:

- [Installation, including Windows](https://www.kimi.com/code/docs/en/kimi-code-cli/guides/getting-started)
- [ACP methods and capabilities](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/kimi-acp)

Install Kimi Code CLI and run `kimi login` in a terminal. TermAl looks for
`kimi.exe` on Windows or executable `kimi` on Unix, first on PATH, then under
`~/.kimi-code/bin`. The latter supports an installer-updated PATH that the
running host has not inherited yet. Paths are canonicalized before changing
the child working directory. Windows `.cmd`, `.bat`, and `.ps1` shims are not
used by this adapter. Readiness checks executable availability only, not tokens
or a paid provider request.

Launch is `kimi acp` (not the old `kimi --acp`). When `login` is advertised,
TermAl calls ACP `authenticate` to validate existing authentication. It never
executes the advertised terminal login command or writes credentials; failures
point the operator to `kimi login`.

## Session behavior

- Create-session and orchestrator agent selectors include Kimi.
  Creation rejects other agents' settings rather than silently ignoring them.
  Orchestrator nodes require manual approvals: Auto is disabled in the editor
  and rejected at template save and instance creation, including saved templates.
- Settings / Kimi stores the default model. Default/Auto leaves the CLI's
  configured model authoritative at initial setup; no hard-coded model alias.
- Session model choices come from ACP `configOptions`; selecting an advertised
  model between turns schedules an ACP restart, preserving the conversation ID.
  Setup uses `session/set_config_option` with `configId: model` and requires the
  requested model in its acknowledgment. Unknown models refuse rather than
  silently using a different model.
  The session Prompt view exposes model and reasoning-effort controls. Refresh reconnects an
  idle runtime and resumes the stored conversation to fetch a fresh catalog;
  repeated refreshes do not silently reuse the old list. Model changes and
  refresh are disabled while active, awaiting approval, or stopping.
  Refresh publishes the model catalog before reconciling a valid saved selection
  for thinking discovery, so a misspelled or removed model cannot block model
  discovery. It does not admit that
  runtime for prompts: the next prompt resumes and validates the latest choice.
  The saved model is requested intent (including Auto), never overwritten by a
  delayed refresh or runtime notification. Auto therefore remains displayed as
  Auto instead of being rewritten to the CLI's current concrete model.
  The Prompt picker is intentionally catalog-only, with no manual-entry fallback.
  If setup fails or the CLI supplies no catalog, its setup/compatibility error
  must be resolved first; refresh can recover from an invalid saved model but
  cannot invent support for an unadvertised model.
- Reasoning effort uses the live ACP `thinking` catalog in Prompt settings and
  `/effort`. The observed 2.0.2 choices are Low, High and Max (not Codex's effort
  scale).

  - Discovery: refresh requires an exact model ACK for a valid explicit model
    before publishing that model's thinking choices. An invalid saved model still
    gets a model catalog for recovery, but its observed thinking catalog is
    cleared. Concurrent model changes discard old discovery results. Replaced
    runtimes cannot publish observations; combined notifications use one revision.
    For an explicit requested model, asynchronous thinking-only notifications
    cannot establish model identity and preserve the existing catalog. A
    model-bearing notification or verified refresh/setter result is required to
    update it, including after invalid-model recovery or a model switch. Auto
    continues to follow the current runtime's observed choices.
  - Selection: idle-only PATCH validates a canonical advertised value. Requested
    `kimiEffort` persists independently of observed `kimiCurrentEffort` and
    `kimiEffortOptions`, including across model changes. An unsupported saved
    effort is displayed as unavailable. Choose a replacement or **CLI current**
    (`kimiEffort: "auto"`) to clear the request, even without a catalog. This
    leaves the CLI's current effort alone, not a factory reset. Only the literal
    `auto` is reserved; thinking values remain case-sensitive producer IDs.
  - Admission: every prompt revalidates against the fresh post-model/manual-mode
    catalog and requires an exact thinking setter ACK with manual mode retained
    when a setter is needed. Installed 2.0.2 setters return full config options
    (verified without a prompt). Partial asynchronous notifications preserve
    cached choices but are not authoritative setter ACKs. A partial mode ACK or
    contradictory response refuses explicit effort; clear it or use a compatible
    CLI to recover.
  - Cloning: one guarded refresh precedes copying explicit effort. Sends and
    settings remain fenced until configuration finishes; failure reports that
    the clone exists but its effort could not be preserved.
  - Budgets: Kimi refresh allows 95 seconds for initialization, authentication,
    session setup, model-dependent discovery and response delivery. Its remote
    forwarding budget is 100 seconds; other agents retain their 30-second remote
    refresh transport budget.
  - Engram attribution: explicit effort is included in actor context. Changing
    or clearing it rotates an installed Engram process identity on the next turn;
    without an installed descriptor, effort alone needs no runtime restart.
    Observed CLI effort is not promoted into requested actor identity. This is
    host-side attribution coverage, not live Engram acceptance certification.
- Shared ACP streaming text, thinking, tool cards, permission cards and
  cancellation are reused.
- **Mode before every prompt.** TermAl sets Kimi's mode and requires its
  acknowledgment, including for resumed sessions. The mode is the session's
  `kimiMode`, or `default` for a read-only delegation child. A mode Kimi kept
  from an earlier session is never inherited.
- **Who answers permission requests.** This depends on the session's TermAl
  policy, `kimiApprovalMode`, and on the delegation. See Approvals and mode,
  and Read-only delegation children.
- **Kimi-specific notifications.** The singular `config_option_update`,
  `current_mode_update` and partial-catalog preservation are Kimi-specific.
  `current_mode_update` is recorded for display as `kimiCurrentMode`, and the
  read-only gate also acts on it. Cursor, Gemini and OpenCode keep their
  existing notification contracts.
  User Stop sends ACP cancellation, waits within the shared graceful-stop bound,
  then terminates the local process while preserving the external conversation ID.
- Existing conversation IDs use advertised resume/load support. A failed
  continuation retains the ID and never silently starts a new conversation.
- Kimi is a structured reviewer adapter: `mode: reviewer`, and `mode:
  explorer` with `writePolicy: readOnly`, are admitted and run behind the host's
  read-only gate. Kimi is not an acceptance evaluator; evaluators stay Claude or
  Codex. A Kimi parent's composer delegations default to `mode: reviewer`
  with `writePolicy: readOnly`, like Claude and Codex: the child runs in the
  shared workspace behind the host gate, with the network limit described
  under Known limits.

This slice does not expose image attachments or native form elicitation. The
ACP permission-based question fallback remains available: AskUserQuestion
arrives as a permission request. Client filesystem, terminal and elicitation
capabilities are not advertised. It does not certify Engram live-control or
provider acceptance for Kimi.

## Approvals and mode

This replaces the earlier "tool approvals remain manual, no override"
contract for ordinary Kimi sessions. Read-only delegation children keep the
host gate below, which always takes precedence. There are two separate
settings, plus an app default for effort.

### TermAl's policy: `kimiApprovalMode`

- **Values.** `ask` or `auto-approve`, the same values and meaning as
  OpenCode's policy. A session without it reads as `ask`.
- **App default.** `defaultKimiApprovalMode` in Settings → Kimi.
- **Where it can be set.**
  - the session's Prompt settings;
  - `/approvals`;
  - the session-creation dialog, which edits a draft of the app default for
    that one session.
- **Orchestrator templates.** `autoApprove` maps to `auto-approve`.
- **Rules for changing it.**
  - A settings request that changes it must not also change `model`,
    `kimiEffort` or `kimiMode`; that is a 400, before any mutation.
  - Changing it while the session is busy is a 409. Resending the unchanged
    value is accepted.
- **What `auto-approve` answers.** TermAl answers a permission request itself
  only if all of these hold:
  - the session is Active, so a pending manual card suspends auto-approve;
  - no Stop is in progress;
  - the session is not a read-only delegation child;
  - the tool is on the Kimi Code 2.0.2 allowlist: Bash, Write, Edit,
    CronCreate, or an MCP tool named `mcp__<server>__<tool>`;
  - the request offers exactly one `allow_once` option. That option is
    selected; `allow_always` never is.
- **What always stays a manual card.**
  - AskUserQuestion: its answers arrive as `allow_once` options, so picking
    one would answer the user's question.
  - ExitPlanMode, a plan approval.
  - Any request offering more than one `allow_once` option.
  - Any other tool.
- **A stale or stopping runtime** of an auto-approve session is answered
  `cancelled`, never approved.

### Kimi's own mode: `kimiMode`

- **Values.** A session without it reads as `default`. Labels quote Kimi's
  help (`kimi --help`):

  | Value | Meaning |
  | --- | --- |
  | `default` | asks before commands and edits |
  | `plan` | "Start in plan mode." |
  | `yolo` | "Ask When Needed mode: routine edits and commands run automatically; risky actions, questions, and plans still ask." |
  | `auto` | "Never Ask mode: never interrupts you; everything runs and is decided automatically." |

  In captures of both `yolo` and `auto`, `echo`, writing a file, deleting it
  and `git init` all ran without a permission request. So TermAl's policy
  rarely or never applies in those modes, and the UI says so.
- **Where it can be set:** the Prompt settings and `/mode`. There is no app
  default.
- **Applying it.** It is applied before every prompt and must be
  acknowledged exactly, or the prompt is refused. The thinking setter's
  acknowledgment must retain the same mode.
- **While the session is busy,** any request carrying it is a 409, even one
  with the unchanged value, as for `model` and `kimiEffort`.
- **Observed mode.** `kimiCurrentMode` shows the mode Kimi last acknowledged
  or reported. Each prompt's mode ACK refreshes it, and a model change clears
  it until the fresh runtime's next prompt. It is display only.
- **Orchestrators** never set it.

### Default effort: `defaultKimiEffort`

- **Values.** `auto`, which leaves the CLI's choice, or one effort token.
- **Applied at creation.** It is copied onto new Kimi sessions as
  `kimiEffort` when they are created. A later Settings change never alters
  an existing session.
- **Checked when prompting,** as today: a value the model does not advertise
  shows the "unavailable" state.
- **Validated before any change.** An invalid value rejects the whole
  Settings request (400), and no other field of that request is applied.

### Delegation children

A Kimi delegation child never inherits a policy or mode. It gets `ask` and
`default`, whatever the app default or its parent says. A read-only child is
answered by the gate in `default` mode.

## Read-only delegation children

A Kimi child of a running read-only delegation stays in Kimi's `default` mode.
TermAl answers every `session/request_permission` it sends, and shows no manual
card (`kimi_read_only.rs`). The rules rest on raw ACP captures of Kimi Code
2.0.2 (model `kimi-code/k3`) and are pinned to that contract.

### What Kimi sends

- **The request.** A permission request carries only `toolCall.toolCallId`,
  the exact tool name as `toolCall.title`, and a summary truncated to about 50
  characters, for example "Requesting approval to Running: git st…". It
  carries no `rawInput`.
- **The arguments.** Kimi reports `rawInput` only after the request is
  answered. It never arrives while the request is pending: in a capture that
  held every request for 3 s, none arrived. Before the request, Kimi streams
  the call's cumulative argument JSON as the content text of its
  `tool_call_update`s. The last text before the request is the complete
  object.
- **What asks permission.** Bash, Write and Edit, CronCreate, ExitPlanMode,
  MCP tools, and a subagent's writes. MCP tools are named
  `mcp__<server>__<tool>`, the same form Claude uses.
- **What runs without asking.** Read, Grep, Glob and ReadMediaFile; FetchURL
  and WebSearch; TodoList, Skill and EnterPlanMode; Agent and AgentSwarm (a
  subagent's writes still ask); and the plan file under
  `~/.kimi-code/sessions/`.
- **Modes.** `default`, `plan`, `auto` and `yolo`. No model tool switches to
  `auto` or `yolo`.
- **Bash is bash.** Kimi's own tool description says "Execute a `bash`
  command". A probe printed `shell=/usr/bin/bash bash=5.2.37`, both with
  Git's directories on `PATH` and without them.

### How each request is decided

The decision is made while the reader holds the state lock that also guards
Stop and runtime replacement. A stale or stopping runtime is answered
`cancelled`. Nothing is held back: the answer is sent before the handler
returns, and `allow_always` is never chosen.

- **Identity.** The tool name is the request's own title. It must equal the
  title of that call's first `tool_call`.
- **Arguments.** The arguments are the last streamed content text received
  before the request, in the exact content shape Kimi streams.
  - It must parse as one JSON object with no repeated key at any depth.
  - It must be at most 256 KiB.
  - If no stream was observed for that call, the request is rejected. This
    covers a subagent's requests.
- **Bash** is approved once if and only if all of these hold:
  - the request summary shows the start of the command;
  - the arguments contain only the keys `command`, `description`, `timeout`,
    `cwd`, `run_in_background` and `disable_timeout`;
  - `run_in_background` and `disable_timeout` are absent or false;
  - `cwd`, if present, is the child's working directory. On Windows,
    separators, case and a trailing separator are ignored. Elsewhere a
    backslash is part of a file name, so `/work\repo` is not `/work/repo`;
  - the command passes the bash read-only checker that also gates Claude
    reviewers.
- **TermAl result tools.**
  `mcp__termal-delegation__termal_submit_review_result` and
  `mcp__termal-delegation__termal_review_freeze_check` are approved once if
  and only if:
  - the summary names the tool;
  - the arguments are a valid request for it;
  - the delegation holds that capability.

  Any other name is rejected, including a foreign alias and the
  acceptance-evaluation tool.
- **ExitPlanMode** is answered with `plan_reject_and_exit`. Without that
  option it is rejected.
- **Everything else that asks** is rejected once. Kimi's failed tool card
  records the refusal, and TermAl logs the reason, which names the tool only.
- **After the answer.** When `rawInput` arrives for an approved call, it must
  equal the approved arguments as a JSON value, so whitespace and key order do
  not matter. Otherwise TermAl cancels the prompt and fails the turn. This
  check only detects a mismatch: by then the approved call has already run
  with whatever arguments Kimi used. Only the check made before the decision
  prevents a bad call.
- **Mode.** Once Kimi has acknowledged `default` for a prompt, and until that
  prompt settles, a mode update to anything other than `default` or `plan`
  also cancels the prompt and fails the turn. A mode Kimi reports during
  session setup, before that acknowledgement, is not a violation: TermAl is
  about to replace it, and the prompt is refused if the acknowledgement says
  otherwise.
- **Bookkeeping.** A finished call is forgotten, including when its last
  update also carries `rawInput`. When more calls are in flight than the gate
  tracks, an approved call whose `rawInput` check is still owed is kept in
  preference to the others.
- **The child's prompt** says what the gate allows, including that Agent and
  AgentSwarm are unavailable.

### Known limits

- **The network.** Tools that run without asking cannot be gated. FetchURL and
  WebSearch are an exfiltration channel: a reviewer could put reviewed-tree
  content into a URL, which is why Claude reviewers are denied WebFetch.
  `kimi acp` has no per-session way to disable tools, so a Kimi reviewer
  carries that risk.
- **Workspace writes** go through the gate: every Kimi write asks
  permission, including a subagent's, and every such request is refused.
- **Isolated-worktree Kimi children** keep manual approvals.
  Auto-approval bounded to owned paths is a separate follow-up (tm-9xo6).

## Verification

Regression fixtures cover executable discovery, launch arguments, identity,
default-model persistence, auth selection, live model configuration, manual
permissions, continuation refusal, delegation admission, and the read-only
gate (`src/tests/kimi_read_only.rs`, replaying Kimi's real message order).
UI tests cover selection, settings, model choices and delegation defaults.
Live authenticated prompt/tool/cancel/resume acceptance remains a separate,
explicit check; the adapter is not deployed merely by editing this repository.
