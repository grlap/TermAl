# Feature Brief: Telegram Relay

TermAl supports an experimental Telegram bot relay for one linked Telegram chat.
The normal path is now UI-configured and runs inside the main backend process;
the older `cargo run -- telegram` mode remains as a debugging escape hatch.

Parent feature: [`whatsapp-integration.md`](./whatsapp-integration.md).

## Current Status

Implemented:

- Telegram settings panel for token entry, connection testing, project
  subscription, default project/session, enable toggle, and saved status.
- In-process relay startup from saved settings when the backend starts.
- One linked Telegram chat ID persisted in `~/.termal/telegram-bot.json`.
- Bot token storage in the OS credential store, with legacy plaintext
  `config.botToken` values migrated out of `telegram-bot.json` on read.
- Multiple subscribed projects in one chat.
- Telegram project switching with `/projects` and `/project <id>`.
- Telegram session switching with `/sessions` and `/session <id>`.
- Free-text forwarding into the selected session, or the active project's
  digest target when no session is selected.
- Assistant text forwarding back to Telegram for Telegram-originated prompts and
  for locally-entered TermAl prompts in the selected Telegram session.
- Digest actions through `/approve`, `/reject`, `/continue`, `/fix`,
  `/commit`, `/iterate`, `/stop`, and `/review`.
- Digest messages use Telegram HTML parse mode with escaped content and a
  preformatted table-like layout for readability.
- Token redaction in logs/errors and bounded chunking for long forwarded/chat
  text. Digest messages are intentionally compact single messages.

Not implemented yet:

- Link-code chat binding wizard. The settings UI still has a disabled
  `Link chat` button as a placeholder.
- Multiple Telegram bots or multiple Telegram chats.
- Webhook mode.
- A UI "test send" button.
- Full relay runtime stats such as last poll time, rolling error counts, or a
  visible last-error panel.

## Setup

1. Create a Telegram bot with `@BotFather` and copy the token.
2. Open TermAl Settings -> Telegram.
3. Paste the token and click `Test connection`.
4. Choose subscribed projects and an optional default project/session.
5. Enable the relay and save.
6. Open the bot in Telegram and send `/start`.

The relay is part of the main TermAl backend. Do not start a second
`cargo run -- telegram` process for the same bot token while the in-process
relay is enabled; Telegram permits only one `getUpdates` poller per bot and
will return API 409 conflicts.

## Telegram Commands

- `/status` shows the active project's digest and available actions.
- `/projects` lists subscribed projects and marks the active one.
- `/project <id>` switches the active project.
- `/project default` returns to the saved default project.
- `/sessions` lists sessions for the active project.
- `/session <id>` selects a session in the active project.
- `/session clear` returns free text to the active project's current/default
  digest target.
- `/approve`, `/reject`, `/continue`, `/fix`, `/commit`, `/iterate`, `/stop`,
  and `/review` dispatch project digest actions.

Free text is sent to the selected session when one is set. Otherwise it goes to
the active project's current digest target. The selected session is also tailed:
assistant text produced from prompts typed directly in TermAl is forwarded back
to Telegram after the message settles.

## Storage

Runtime and UI configuration metadata are stored together in
`~/.termal/telegram-bot.json`. The bot token itself is stored in the OS
credential store under a TermAl service entry scoped to the TermAl data
directory. Existing plaintext `config.botToken` values from older releases are
migrated into the credential store and removed from the JSON file the next time
the Telegram settings are read or updated.

The UI-owned config block contains:

- `enabled`
- `subscribedProjectIds`
- `defaultProjectId`
- `defaultSessionId`

The runtime state contains fields such as:

- `chatId`
- `selectedProjectId`
- `selectedSessionId`
- `nextUpdateId`
- `lastDigestHash`
- `lastDigestMessageId`
- assistant forwarding cursors

The full bot token is never returned through `/api/telegram/status` or persisted
back to `telegram-bot.json`; status responses expose only a masked suffix.

Platform credential-store coverage is split intentionally:

- Normal backend tests use `keyring_core::mock` so they are deterministic and do
  not write secrets to the developer machine.
- The ignored smoke test
  `telegram_bot_token_native_credential_store_round_trips` writes and deletes a
  disposable entry in the real OS credential store through the same platform
  store-selection helper used by production initialization. Run it explicitly on
  Windows, macOS, or Linux with:

```bash
cargo test --bin termal telegram_bot_token_native_credential_store_round_trips -- --ignored
```

Linux runs require a usable desktop Secret Service/keyring session.

## HTTP Surface

Current routes:

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/telegram/status` | Read configured/enabled/running state, lifecycle, linked chat, masked token, subscribed projects, and defaults |
| POST | `/api/telegram/config` | Update token in the OS credential store, enabled flag, subscriptions, and defaults |
| POST | `/api/telegram/test` | Validate a supplied or saved token with Telegram `getMe` |

The relay itself uses existing TermAl routes:

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/state` | Read projects and sessions for `/projects`, `/sessions`, and selected-session validation |
| GET | `/api/projects/{id}/digest` | Read active project digest |
| POST | `/api/projects/{id}/actions/{action_id}` | Dispatch digest action |
| GET | `/api/sessions/{id}` | Read settled assistant messages for forwarding |
| POST | `/api/sessions/{id}/messages` | Forward Telegram free text into TermAl |

Telegram endpoints return the standard TermAl API error shape, `{ "error":
"..." }`, with a human-readable diagnostic. Treat that message as
presentation text, not a stable discriminator. In particular,
`/api/telegram/test` can return `422` for both local config validation failures
and Telegram `getMe` validation/auth failures; clients should present the
message and branch on request context or status, not parse English text.
Config validation also checks that referenced projects/sessions still exist
before checking default-project membership, so orphaned defaults can report
`unknown ... project/session` wording instead of an older membership-specific
message.

`POST /api/telegram/config` returns the sanitized current settings after the
patch is applied, not an echo of request fields. Omitted or `null` patch fields
leave the matching setting unchanged, but stale persisted project/session
references can still be scrubbed from the response when they no longer exist.
Clients should replace local Telegram settings state with the response instead
of diffing request fields against response fields.

## Legacy CLI Mode

`cargo run -- telegram` still works for debugging and reads:

- `TERMAL_TELEGRAM_BOT_TOKEN`
- `TERMAL_TELEGRAM_PROJECT_ID`
- `TERMAL_TELEGRAM_CHAT_ID`
- `TERMAL_TELEGRAM_API_BASE_URL`
- `TERMAL_TELEGRAM_PUBLIC_BASE_URL`
- `TERMAL_TELEGRAM_POLL_TIMEOUT_SECS`

This mode should not be used at the same time as the in-process relay for the
same bot token.

## Remaining Work

- Replace manual chat binding with a one-time link-code wizard.
- Surface relay errors and poll health in Settings.
- Add a "test send" action.
- Evaluate richer digest formatting beyond the current `<pre>` table. Telegram
  Bot API HTML does not support real `<table>` markup, so future options
  include generated PNG/SVG snapshots or attached HTML files for wider tables
  and richer report layouts.
- Add per-chat prompt/action rate limiting.
- Decide whether to deprecate or remove the legacy CLI relay.
- Consider multi-chat or multi-bot support only after the one-chat path is
  stable.
