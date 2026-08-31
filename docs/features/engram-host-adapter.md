# Engram Host Adapter

## Status

The local Engram integration is enforced and turn-gated for delegation-child
sessions. When a local project has Engram enabled, TermAl does not hand a
prompt to Claude, Codex, Cursor, Gemini, or OpenCode until Engram has returned a
grant and accepted `turn_begin` for that exact turn.

The adapter is deliberately local-only. Remote proxy sessions do not enter the
local Engram control plane. Project-scoped remote access remains a separate
design contract in [Project-scoped remotes](./project-scoped-remotes.md).

## Project configuration

Engram is configured per local TermAl project:

- `enabled`: enables the host-control adapter;
- `binaryPath`: absolute path to the Engram executable;
- `home`: absolute Engram home directory;
- `workAuthorityGrant`: the single project grant used by every TermAl agent;
- `deadlineMs`: bounded control-call deadline.

TermAl persists one grant per project, not one grant per session or agent. The
fixed Engram subject and control actor is `termal`. Individual activity remains
attributable through the distinct TermAl `session_id` carried on every Engram
CLI/control connection.

Grant material is secret. It is persisted server-side but removed from client
project snapshots, logs, errors, and durable operator notices. The PATCH API's
redacted placeholder preserves the current grant; explicit `null` clears it.

## Enablement validation

An enablement or connection-changing PATCH must pass both checks before the new
settings are committed.

### Doctor identity

TermAl runs:

```text
engram --project-file <project>/.engram-project --home <home> doctor --json
```

The command must exit successfully and return healthy JSON with:

- `control.required_assurance == "turn_gated"`;
- a non-empty `project_id`;
- an absolute `database` path.

The returned `project_id` and normalized database path are authoritative store
identity. TermAl persists that pair with the private project settings and uses
it for grant-ownership, rotation, revocation, and tombstone comparisons. It
does not infer store identity by hashing `.engram-project` or constructing a
database filename.

### Work-authority grant

When a grant is configured, TermAl runs:

```text
engram --project-file <project>/.engram-project --home <home> \
  authority show <grant-hash> --json
```

The grant is rejected unless the response proves all of the following:

- `installed` is true;
- `subject_actor_id` is exactly `termal`;
- `revoked_at` is null;
- `valid_from` is valid RFC 3339 and is not in the future;
- `valid_until` is valid RFC 3339 and is later than the current time.

Unknown grants, malformed output, store failures, expired grants, future grants,
revoked grants, and subject mismatches fail closed. Error text is redacted and
never includes the grant hash.

## Turn lifecycle

For every eligible delegation-child turn, including user prompts, mailbox
wakes, and orchestrator work, TermAl executes this sequence:

1. Resolve or refresh the session binding. `session_bind` always declares
   `assurance: "turn_gated"`, actor `termal`, and the exact TermAl session id.
2. Call `turn_evaluate` for the stable prompt fingerprint.
3. Only for a grant, call `turn_begin` with the returned grant id and delivery
   tokens.
4. Only after a matching begin receipt, deliver the prompt to the agent runtime.
5. On turn completion, Stop, error, runtime exit, project reset, or deletion,
   checkpoint a begun grant exactly once with the appropriate next intent.

Refuse, defer, transport degradation, protocol degradation, missing binding,
begin refusal, or dispatch-budget exhaustion all withhold the prompt. They
produce a durable Engram control card with `dispatch: "withheld"`; no agent
runtime command is sent. A stale callback is fenced by the runtime token,
active-turn generation, and Engram dispatch generation, so it cannot fail or
checkpoint a successor turn.

The former `sent_without_grant` card value remains deserializable only for
already-persisted historical cards. New enforced turns never emit it.

## Settings transitions

An Engram settings/reset transaction owns a project-generation fence while the
old connection drains. Prompts arriving during that interval remain in the
durable session queue; they are not sent through the old connection and do not
bypass Engram. When the exact owner releases the fence, TermAl drains the queue
against the committed configuration:

- if Engram remains enabled, the prompt receives a fresh bind/evaluate/begin;
- if Engram was explicitly disabled, ordinary non-Engram dispatch resumes.

Grant clear, rotation, home change, disable, and project deletion preserve the
existing revocation/tombstone rules. The authoritative doctor store identity is
carried into runtime descriptors and retirement records. A missing project
marker after successful validation therefore does not erase the known store
identity.

## Mailbox and Stop behavior

Mailbox wakes use the same gate as user prompts. If a mailbox turn is stopped
or fails after acceptance, TermAl restores the exact delivered-through sequence
before any successor work. The poisoned automatic queue is paused until an
explicit resume so the same wake cannot spin indefinitely. A wake refused by
Engram is never sent to the agent runtime.

Stop remains valid for an Active/Approval session whose local runtime has
already vanished. Terminalization, runtime cleanup, mailbox-boundary recovery,
and public status publication share the guarded lifecycle transaction.

See [Agent mailboxes](./agent-mailboxes.md) for the durable delivery and recovery
contract.

## Runtime composition

The project grant is also composed into eligible local agent MCP configuration.
The grant hash is a bearer secret: it reaches the Engram MCP child only through
the `ENGRAM_WORK_AUTHORITY_GRANT` environment variable in the child's MCP stdio
configuration and never on the command line, where every process listing could
read it. Engram reads that variable on `engram mcp` (unset means grant-less,
a malformed value fails startup, and the value is never logged), so the argv
form is not used anywhere in TermAl.
Every descriptor records the authoritative doctor store identity and fixed
actor `termal`. Runtime replacement and revocation remain generation-fenced so
a stale teardown cannot remove a newer descriptor.

## Failure and security policy

- Control and grant validation fail closed.
- Control calls are deadline-bounded and never run while holding the global
  state mutex.
- JSON is parsed into typed response structures; CLI stderr is diagnostic only.
- Grant material is never included in state snapshots, SSE deltas, transcript
  notices, or log messages.
- Store identity comparisons prefer the authoritative doctor tuple and use a
  normalized-home fallback only for settings created before that tuple exists.
- The adapter does not open or inspect Engram's SQLite database directly.

## Operator verification and restart

Before enabling a project, verify the same binary, project marker, and home that
TermAl will use:

```text
engram --project-file <project>/.engram-project --home <home> doctor --json
engram --project-file <project>/.engram-project --home <home> \
  authority show <grant-hash> --json
```

The grant must be active for subject `termal`. Configure the project through the
TermAl project settings UI/API, then restart TermAl so every already-running
agent runtime is rebuilt with the new host/MCP configuration. A valid smoke test
must observe one complete live-store sequence:

```text
session_bind(turn_gated) -> turn_evaluate(grant) -> turn_begin(begin) \
  -> agent delivery -> turn_checkpoint(checkpointed)
```

Do not treat a fixture-only test, direct SQLite inspection, or a control card
without runtime-delivery evidence as an end-to-end proof.
