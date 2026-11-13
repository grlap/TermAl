# Gemini harness: Antigravity CLI

TermAl's Gemini harness runs **Antigravity CLI** as `agy acp` through the
shared ACP runtime. The saved agent type and Gemini approval settings remain
compatible with existing TermAl sessions.

Google's [transition announcement](https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/)
ended consumer Gemini CLI service on June 18, 2026. Gemini Code Assist
Standard/Enterprise and paid Gemini or Gemini Enterprise Agent Platform API
access remain supported by Google. Those are upstream legacy Gemini CLI
environments; TermAl's Gemini harness launches Antigravity, without silently
falling back to the old `gemini --acp` executable.

See [Google's ACP guide](https://antigravity.google/docs/ide/extensions/acp),
[Antigravity's ACP reference](https://antigravity.google/docs/cli/acp/), and
[Current Agent Integration Contracts](current-agent-contracts.md#acp-advertised-capabilities-and-typed-permissions).
The shared design is described in [Architecture](../architecture.md).

## Executable and readiness

Install native `agy` and make it available on the host's PATH. On Windows,
TermAl also checks `%LOCALAPPDATA%/agy/bin/agy.exe`, because the installer's
PATH update might postdate the host process. Discovery pins the absolute native
executable before changing the child working directory. Windows shell shims
are not launched.

Readiness confirms executable availability. Authentication is checked by the
ACP runtime using Antigravity's existing cached sign-in. TermAl does not read
or rewrite Antigravity credentials, settings or configuration, and does not
run `agy --login` or `agy acp --login`. Authentication failures require
operator action and are never presented as a successful readiness/auth probe.

The old Gemini dotenv-file discovery, Windows interactive-shell settings
override and legacy credential-file heuristics do not apply to Antigravity.
Operator-provided process/project environment continues to reach the child
through the ordinary runtime environment path.

## Approval modes

| TermAl setting | Launch |
| --- | --- |
| Default (or unspecified) | `agy acp` |
| Yolo | `agy acp --dangerously-skip-permissions` |
| AutoEdit | `agy acp --mode accept-edits` |
| Plan | `agy acp --mode plan` |

The runtime continues handling permission requests through advertised ACP
options. It does not invent option ids or automatically execute a login
command. Cached sign-in normally needs no `authenticate` request. If an
explicit process environment provides a Gemini API key, the runtime may select
`gemini-api-key`; a Google Cloud project and location may select
`agent-platform`. Either method must be present in the server's advertised
`authMethods`. Browser and terminal login methods remain operator-owned.

## Protocol and continuation

Transport remains ACP v1 JSON-RPC over stdio: `initialize`,
`session/new`, `session/prompt`, `session/update`,
`session/request_permission` and `session/cancel`.

Saved conversations use `session/resume` or `session/load` only when the
server's initialize capabilities explicitly advertise support. An omitted
capability is unsupported. A failed continuation preserves the saved id
instead of creating a replacement conversation.

## Verification

The Rust ACP tests cover executable discovery, missing/native/shim cases,
each approval-mode launch, noninteractive advertised auth selection, and
legacy-file isolation. Existing shared ACP tests continue covering permission,
cancel and capability-driven continuation behavior.

The live contract probe is **opt-in**, outside the default full gate. Run it
only in an assigned test slot on a host whose operator already signed in.
Set `TERMAL_AGY_CONTRACT_SMOKE=1` and `TERMAL_AGY_SMOKE_BINARY` to the
absolute native executable, then run from the candidate worktree:

```text
node scripts/test-launcher.mjs focused --input commit -- node --test scripts/agy-acp-contract.test.mjs
```

The probe launches one ACP server in an empty repository-owned scratch
workspace, negotiates initialize, records the binary hash, server version,
protocol version, advertised auth methods and load/resume capabilities, then
creates one session and sends **one harmless prompt** requesting permission
for `echo TERMAL_ACP_SMOKE`. It rejects the tool request (or cancels the
permission if no reject-once option is offered), sends `session/cancel`,
waits for the prompt to settle and reaps the child. It does not grant tool
execution. Byte and time limits bound the probe; no retry spends more quota.

No login or credential/configuration inspection is performed. An authentication
requirement is reported as **blocked-on-auth** and fails the test. If account
permission rules prevent a permission request from appearing, the test reports
**permission response not observed**; do not claim coverage or repeat the
paid prompt without approval. Load/resume is reported from actual advertised
capabilities, without a second prompt or an extra session.

The deterministic probe-controller test uses a fake stdio peer and consumes
no provider quota. With the opt-in environment absent, the live test is skipped.
