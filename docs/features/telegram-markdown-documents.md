# Feature Brief: Send Markdown Files to Telegram

An agent or session can send one Markdown file to the operator's linked
Telegram chat as a document. It names the file and an optional caption; TermAl
reads the file, checks it, and uploads it through the bot that the
[Telegram relay](./telegram-ui-integration.md) already uses.

Nothing is sent on its own. A document goes out only when a session makes an
explicit call to the route below.

## Route

```
POST /api/sessions/{sessionId}/telegram/documents
Content-Type: application/json

{ "path": "docs/report.md", "caption": "Daily report" }
```

- `path` (required): a relative path, resolved against the session's workdir,
  or an absolute path.
- `caption` (optional): plain text up to 1024 characters (UTF-16 units, as
  Telegram counts them). It is trimmed, and an empty caption is dropped. No
  parse mode is applied.

Success returns `200`:

```json
{ "messageId": 812, "fileName": "report.md", "path": "C:\\repo\\docs\\report.md", "sizeBytes": 2048 }
```

`messageId` is the Telegram message id of the sent document. `path` is the
canonical path of the file that was sent.

## Usage From an Agent

Agents run inside TermAl with `TERMAL_SESSION_ID` and `TERMAL_BASE_URL` set.

```sh
curl -sS -X POST \
  "$TERMAL_BASE_URL/api/sessions/$TERMAL_SESSION_ID/telegram/documents" \
  -H 'content-type: application/json' \
  -d '{"path":"docs/report.md","caption":"Daily report"}'
```

The caller never handles the bot token. TermAl reads the token from the OS
credential store inside its own process.

## Which Files Are Allowed

- The file must lie inside the session's workdir or, for a session in a local
  project, inside the project root. A delegated worker's worktree is its
  workdir, so a worker can send files from its own worktree.
- A path with a `..` component is refused outright.
- Raw Windows network, device and drive-relative document paths are refused
  before resolving the requested path. Symlink resolution itself can still
  touch a network target before containment refuses that target.
- Containment is checked on the canonical path, after every symlink is
  followed. A symlink inside the roots that points outside them is refused.
- The opened handle's real path is checked before and after reading. Its type
  and size come from that same handle; replacement links and special files
  cannot redirect the read or wait for a FIFO writer. The kernel-reported
  handle path is compared directly, without resolving its name again.
- The canonical file must be a regular file with a `.md` extension (any case).
  The type is judged on the canonical name, so a `notes.md` symlink to a
  `.txt` file is refused.
- The content must be non-empty UTF-8 text with no NUL bytes.
- Windows reparse-point files are refused, including OneDrive placeholders
  and deduplicated files with non-link reparse tags.
- Containment is judged by path. Hard links are not traced to their other names.
- The size limit is 1 MiB (1,048,576 bytes), far below Telegram's 50 MB bot
  upload limit.

Remote-proxy sessions (including unmapped or incomplete remote identities),
sessions in a remote project, and read-only delegated sessions while their
delegation runs, are refused.

## Telegram Prerequisites

1. A bot token is saved in Settings -> Telegram.
2. The relay is enabled in Settings -> Telegram.
3. A chat is linked: the operator opened the bot in Telegram and sent
   `/start`.

## Errors

Every refusal returns `{ "error": "<message>", "code": "<code>" }`:

| HTTP | `code` | Meaning |
|------|--------|---------|
| 400 | `invalid_request` | Malformed JSON, an unknown field, a blank `path`, an invalid or Windows drive-relative name, or a caption over the limit. |
| 404 | `session_not_found` | No such session. |
| 400 / 403 / 409 / 503 | `session_refused` | A remote/proxy session, a read-only delegated session, a workdir that does not resolve, or the delegation inventory still starting. |
| 409 | `telegram_not_configured` | No bot token is saved. |
| 409 | `telegram_not_connected` | The relay is disabled, or no chat has been linked with `/start`. |
| 403 | `path_outside_allowed_roots` | A `..` component, raw Windows network/device namespace, a symlink loop or replacement, an outside path, or a handle path that changed. |
| 404 | `file_not_found` | Missing file, a path through a non-directory, or a deleted handle path. |
| 415 | `not_markdown` | Not `.md`, not regular, a Windows reparse point, or not UTF-8 text without NUL bytes. |
| 400 | `file_empty` | The file is empty. |
| 413 | `file_too_large` | Over 1 MiB; the body also carries `sizeBytes` and `maxBytes`. |
| 502 | `telegram_api_error` | Telegram rejected the upload or could not be reached; `telegramErrorCode` is present when Telegram returned one. |
| 500 | `internal_error` | A local failure, such as reading the credential store. |

## Secrets

- The bot token is never put in argv, never returned, and never logged.
- Upload errors can carry the Bot API URL, which contains the token. They pass
  through the relay's token redaction before they are logged or returned.

## Implementation

- `src/telegram_documents.rs`: the route, typed errors, session roots and
  Telegram link checks.
- `src/telegram_document_files.rs`: path, type and size admission.
- `src/telegram_document_handle.rs`: platform handle and namespace checks.
- `src/telegram_document_upload.rs`: the multipart `sendDocument` upload and
  the `TelegramDocumentSender` seam.
- Tests: `src/tests/telegram_documents.rs` and its included
  `src/tests/telegram_document_safety.rs`, using a fake sender. No test reaches
  Telegram: under `cfg(test)` the route's real client refuses to connect.

## Not Implemented Yet

- An MCP tool or `termal` CLI subcommand wrapping the route.
- Other file types (only `.md` is accepted).
- Any rate limit beyond Telegram's own.
