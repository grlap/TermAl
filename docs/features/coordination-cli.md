# Coordination CLI

`termal sessions ...` and `termal mailbox ...` expose TermAl's root-session
peer discovery and durable mailboxes as ordinary shell commands. They exist for
agent runtimes whose MCP configuration is administratively locked (for example
a Codex Enterprise `mcp_servers` allowlist that rejects the injected
`termal-delegation` server) but which may still run a CLI from the shell. No
MCP registration is required; a running TermAl server is the only dependency.

The CLI is a thin client of the loopback HTTP API. Every command runs through
the same bridge code as the `termal_*` delegation MCP tools
(`src/delegation_mcp.rs`), so id-or-name resolution, the root-only guard, the
self/child target rejections, idempotency keys, FIFO read bounds, typed
safe-replay retries and forward-only CAS acknowledgement are the MCP contracts,
not re-implementations. The CLI never opens `termal.sqlite` or
`coordination.sqlite` directly. Related docs: `docs/features/agent-mailboxes.md`
(mailbox semantics) and the Entry Points section of `docs/architecture.md`.

## Commands

```
termal sessions list [--as-session <id>] [--json] [--base-url <url>]
termal mailbox list --as-session <id> [--json] [--base-url <url>]
termal mailbox send --as-session <id> --to <session-id-or-name>
                    (--message <text> | --message-file <path or ->)
                    --idempotency-key <key> [--topic <text>]
                    [--state-stamp <text>] [--class <class>]
                    [--json] [--base-url <url>]
termal mailbox read --as-session <id> --mailbox-id <id>
                    [--after <sequence>] [--limit <count>]
                    [--json] [--base-url <url>]
termal mailbox read-message --as-session <id> --message-id <id>
                    [--json] [--base-url <url>]
termal mailbox acknowledge --as-session <id> --mailbox-id <id>
                    --expected <processedThrough> --through <processedThrough>
                    [--json] [--base-url <url>]
```

| Command | MCP tool it mirrors | Loopback request |
| --- | --- | --- |
| `sessions list` | `termal_list_sessions` | `GET /api/state`, root sessions only |
| `mailbox list` | `termal_list_mailboxes` | `GET /api/sessions/{as}/mailboxes` |
| `mailbox send` | `termal_send_to_session` | `POST /api/sessions/{as}/mailboxes/send` |
| `mailbox read` | `termal_read_mailbox` | `POST /api/sessions/{as}/mailboxes/{mailbox}/read` |
| `mailbox read-message` | `termal_read_mailbox_message` | `GET /api/sessions/{as}/mailbox-messages/{message}` |
| `mailbox acknowledge` | `termal_acknowledge_mailbox` | `POST /api/sessions/{as}/mailboxes/{mailbox}/acknowledge` |

Flags accept both `--flag value` and `--flag=value`, so Windows shells can pass
values containing spaces or `::` without quoting gymnastics. A value flag never
consumes a following option token: `--as-session --json` is a usage error, not
a session named `--json`, and `--as-session -h` prints the usage; values that
themselves look like options must use the `--flag=value` form. `--message-file` reads the body from a UTF-8 file (`-`
reads stdin); a leading byte-order mark is dropped, and the read is bounded to
the backend's 256 KiB body cap so an oversized file is refused before any
request. Exactly one of `--message` and `--message-file` must be given.

### Identity and authorization

`--as-session` is the root session the command acts as; it is the same
identity the delegation MCP bridge receives through `--parent-session-id`. The
CLI looks the session up in `/api/state` before any mailbox request:

- a delegation-child session (a spawned reviewer, explorer or worker) is
  refused, exactly as the peer MCP tools are hidden from and refused to
  children;
- an id the server does not know is refused with a pointer to
  `termal sessions list`;
- an unreachable server is reported as such rather than mistaken for a child.

`sessions list` needs no identity. When `--as-session` is supplied anyway the
same guard applies. Targets of `mailbox send` may be a session id or a session
name; names resolve case-insensitively across all projects, ambiguous names
are rejected with the candidate ids, and the caller's own session as well as
delegation children are rejected as targets by the backend.

This guard is a misuse guard, not an authenticated containment boundary. The
Phase 1 loopback API accepts any local caller, and the CLI asserts its identity
exactly as the delegation MCP bridge asserts `--parent-session-id`: a process
with shell access to the machine can list root sessions (ids, names, working
directories and the same message previews `termal_list_sessions` returns) and
claim one of those ids. Binding each session to a capability that the server
verifies is tracked as follow-up work; until then the MCP-side hiding of peer
tools from delegation children is a convenience for well-behaved agents, and
the CLI keeps parity with it rather than pretending to enforce more.

`--class` accepts only `routine`, the single message class the mailbox schema
admits; other values are usage errors.

### Idempotency, replay and acknowledgement

- `mailbox send` requires `--idempotency-key`. Re-running the identical
  command returns the original receipt with `duplicate: true`; the backend
  never appends twice for one key.
- When the server returns its typed storage-busy rejection, the bridge
  replays reads, keyed sends and acknowledgements within one request budget.
  Any other failure is surfaced after the first attempt. If a send fails
  without a receipt, the error says to retry with the same key; if an
  acknowledgement fails without a summary, list the mailboxes and retry from
  the reported `processedThrough`.
- `mailbox read` returns messages with `sequence > --after` (default 0),
  at most `--limit` (default 50, server-bounded), oldest first.
- `mailbox acknowledge` is a forward-only compare-and-set: `--expected` must
  equal the caller's current `processedThrough` and `--through` must not move
  it backwards; a mismatch is a `409` and exit code 1.

### Output and exit codes

Without `--json` the output is a concise, human-oriented rendering that may
change. Mailbox bodies, topics, previews and session names are peer-controlled
text, so the human rendering replaces every control character (C0, DEL and C1
alike) with U+FFFD; only the message body, printed as a block of its own,
keeps newlines and tabs, while single-line metadata loses them too so a peer
cannot forge rows or columns. A hostile peer cannot smuggle terminal escape
sequences through the text output. With `--json` stdout carries exactly the JSON the corresponding MCP
tool returns, pretty-printed, with the same field names (`sessions[]`,
`mailboxes[]`, the send receipt with `sessionId`/`resolvedFrom`, `messages[]`,
the acknowledged mailbox summary).

| Exit code | Meaning |
| --- | --- |
| `0` | Success. |
| `2` | Usage or argument error; nothing was sent. The message and the usage text are on stderr. |
| `1` | A request was attempted and failed: transport error, backend rejection (`4xx`/`5xx` message forwarded), or a successful response whose shape is not the tool contract (validated before anything is printed, so a malformed `2xx` never exits `0` as an empty listing). Details on stderr, flattened to one line with control characters neutralized because server errors can quote peer text. A consumer that closes stdout early (`| head`) is not a failure. |

`--base-url` defaults to `TERMAL_BASE_URL`, then
`http://127.0.0.1:<TERMAL_PORT or 8787>`.

## Examples

```
termal sessions list --json
termal mailbox list --as-session session-4983
termal mailbox send --as-session=session-4983 --to="Termal::Codex" \
  --message-file=handback.txt --idempotency-key=fable-handback-r3 \
  --topic="tm-7p9z.4 hand-back" --state-stamp=HEAD=4cacf05 --json
termal mailbox read --as-session session-4983 --mailbox-id mailbox-312caf7d \
  --after 645 --limit 5
termal mailbox acknowledge --as-session session-4983 --mailbox-id mailbox-312caf7d \
  --expected 645 --through 647
```

A typical receive loop is `mailbox list` (read the current `processedThrough`),
`mailbox read --after <processedThrough>`, act on the bodies, then
`mailbox acknowledge --expected <processedThrough> --through <last sequence>`.
