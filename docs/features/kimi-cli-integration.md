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
  cancellation are reused. Before every prompt, TermAl sets mode to `default`
  and requires its acknowledgment, including resumed sessions. Tool permission
  requests remain manual; saved Auto/YOLO modes are not inherited for prompts.
  The singular `config_option_update`, ignored `current_mode_update`, and
  partial-catalog preservation are Kimi-specific; Cursor, Gemini and OpenCode
  retain their existing notification contracts.
  User Stop sends ACP cancellation, waits within the shared graceful-stop bound,
  then terminates the local process while preserving the external conversation ID.
- Existing conversation IDs use advertised resume/load support. A failed
  continuation retains the ID and never silently starts a new conversation.
- Kimi is not a structured reviewer/evaluator adapter. Read-only delegations
  are refused before child creation; composer delegations default to explorer
  with an isolated worktree, which is not an OS security sandbox.

This slice does not expose Kimi approval-mode controls, image attachments,
or native form elicitation. ACP permission-based question fallback remains
available; client filesystem/terminal/elicitation capabilities are not advertised.
It does not certify Engram live-control or provider acceptance for Kimi.

## Verification

Regression fixtures cover executable discovery, launch arguments, identity,
default-model persistence, auth selection, live model configuration, manual
permissions, continuation refusal, and unsupported delegation admission.
UI tests cover selection, settings, model choices and delegation defaults.
Live authenticated prompt/tool/cancel/resume acceptance remains a separate,
explicit check; the adapter is not deployed merely by editing this repository.
