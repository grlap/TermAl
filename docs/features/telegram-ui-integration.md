# Feature Brief: Telegram Relay

TermAl supports an experimental Telegram bot relay. The current implementation
has one UI-configured bot with one linked Telegram chat. Configuration lives in
app preferences, its token in the OS credential store, and forwarding state in
the runtime JSON file. The relay runs inside the main backend process.

Parent feature: [`whatsapp-integration.md`](./whatsapp-integration.md).

## Current Status

Implemented:

- Telegram settings panel for token entry, connection testing, project
  subscription, default project/session, enable toggle, and saved status.
- In-process relay startup from saved settings when the backend starts.
- One linked Telegram chat ID persisted in `~/.termal/telegram-bot.json`.
- Bot token storage exclusively in the OS credential store; runtime-file fields
  never supply configuration or secrets.
- Multiple subscribed projects in one chat.
- Telegram project switching with `/projects` and `/project <id>`.
- Telegram session switching with `/sessions` and `/session <name>`.
- Free-text forwarding into the selected session, or the active project's
  digest target when no session is selected.
- Optional assistant text forwarding back to Telegram for Telegram-originated
  prompts and for locally-entered TermAl prompts in the selected Telegram
  session when `forwardAssistantReplies` is enabled.
- Digest actions through `/approve`, `/reject`, `/continue`, `/fix`,
  `/commit`, `/iterate`, `/stop`, and `/review`.
- Digest messages use Telegram HTML parse mode with escaped content and a
  preformatted table-like layout for readability.
- Token redaction in logs/errors and bounded chunking for long forwarded/chat
  text. Digest messages are intentionally compact single messages.

Not implemented yet:

- Link-code chat binding wizard. The settings UI still has a disabled
  `Link chat` button as a placeholder.
- Multiple Telegram bots.
- Multiple linked chats per bot.
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

The relay is part of the main TermAl backend. Saving enabled settings starts,
stops, or restarts the in-process relay from the saved configuration. Telegram
permits only one `getUpdates` poller per bot token, so run one TermAl backend
per configured bot token.

## Telegram Commands

- `/projects` lists subscribed projects and marks the active one.
- `/project <id>` switches the active project.
- `/project default` returns to the saved default project.
- `/sessions` lists sessions for the active project by name, active first and
  then by latest update.
- `/session <name>` selects a session in the active project by exact name or id.
- `/session clear` returns free text to the latest promptable root session in
  the active project.

Free text is sent to the selected session when one is set. Otherwise it goes to
the latest promptable root session in the active project. The selected session
is also tailed:
assistant text produced from prompts typed directly in TermAl is forwarded back
to Telegram after the message settles.

Project digests, inline digest controls, and digest action commands are
temporarily disabled. Existing digest buttons are acknowledged as disabled and
do not dispatch backend work.

### Assistant Forwarding Boundary

The relay keeps a conservative boundary when Telegram free text is queued behind
an already-active or approval-paused TermAl turn. While that older turn is still
open, the relay treats the latest assistant text as a moving baseline and does
not forward it to Telegram. On the first settled poll, if the tracked assistant
message has already grown, the relay records the grown length as the baseline
and waits for later growth or a later assistant message.

That means same-message reply text already present before the first settled
poll is intentionally not forwarded for queued Telegram prompts. Forwarding it
would risk sending output from the pre-existing local turn. Supporting that case
requires a stronger per-turn boundary from the session or agent layer.

## Storage

UI configuration is stored only in the revisioned app state under
`preferences.telegram`, so settings saves publish ordinary state snapshots and
SSE updates. `~/.termal/telegram-bot.json` contains only relay runtime state.
The bot token is stored in the OS credential store under a TermAl service entry
scoped to the TermAl data directory. Token entry through Settings or the dedicated
config endpoint updates that credential, never app preferences or the runtime file.

Unknown keys are ignored and the file is rewritten in the current schema on save.
This is an intentional runtime-file reader/writer contract: ignored keys are not
interpreted, imported into app preferences, copied into credentials, or mirrored
back. A file with extra fields can therefore retain its current chat binding,
update offset, and session-keyed cursors across a restart without an operator reset.
Malformed current fields still produce parse failures; settings operations and
writes refuse unreadable state. The relay's existing corrupt-file quarantine
retains a hardened backup before starting with empty runtime state.

The UI config contains:

- `enabled`
- `subscribedProjectIds`
- `defaultProjectId`
- `defaultSessionId`
- `forwardAssistantReplies`

The runtime state contains fields such as:

- `chatId`
- `selectedProjectId`
- `selectedSessionId`
- `nextUpdateId`
- `lastDigestHash`
- `lastDigestMessageId`
- `assistantForwardingCursors`: a map keyed by TermAl session id, with message id,
  character count/hash, retry/chunk/footer progress, and active-turn baseline
- `forwardNextAssistantMessageSessionIds`: the ordered sessions armed by Telegram prompts

`chatId`, `nextUpdateId`, selected project/session, and digest hash/message id
are current single-bot runtime state, not session-forwarding fallbacks.
Only the session-keyed map/list controls assistant forwarding: no unscoped
cursor or single-session mirror is read or written.

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

Telegram uses a focused API surface instead of the generic `POST /api/settings`
route because the settings response must include relay lifecycle state and a
masked token without ever placing secret token material in the normal
`StateResponse` / SSE snapshot stream. The `test` route is also intentionally
separate because it performs an outbound Telegram `getMe` check without saving
configuration. The remaining config route returns a sanitized
`TelegramStatusResponse` so clients can replace their local Telegram settings
view from one response.

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/telegram/status` | Read configured/enabled/running state, lifecycle, linked chat, masked token, subscribed projects, and defaults |
| POST | `/api/telegram/config` | Update token in the OS credential store, enabled flag, subscriptions, and defaults |
| POST | `/api/telegram/test` | Validate a supplied or saved token with Telegram `getMe` |

These are the current single-bot routes. There is no alternate route or
default-bot response projection for other TermAl API shapes.

The relay itself uses existing TermAl routes:

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/state` | Read projects and sessions for `/projects`, `/sessions`, and selected-session validation |
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

## Multi-Bot Target Spec

This section describes future design, not implemented behavior. The single-bot
contract described above is the only implemented one. Bot profiles, the proposed
v2 storage shape, and the routes below will be introduced as a new contract when
the multi-bot feature is built.

The multi-bot feature should treat a bot as a named route profile. Examples:
`Personal`, `Work`, `Client A`, or `On-call`. Each profile owns:

- A stable `id` generated by TermAl. IDs must be URL-safe and must not be
  derived from the Telegram token.
- A user-visible `name`.
- `enabled`.
- A bot token stored in the OS credential store and exposed to clients only as
  `botTokenMasked`.
- One linked Telegram `chatId` in the first multi-bot version.
- `subscribedProjectIds`.
- `defaultProjectId`.
- `defaultSessionId`.
- Runtime relay state: `selectedProjectId`, `selectedSessionId`,
  `nextUpdateId`, digest message/hash state, assistant-forwarding cursors, and
  any future poll-health fields.

### Storage Shape

The proposed v2 JSON shape is:

```json
{
  "version": 2,
  "bots": [
    {
      "id": "bot-personal",
      "name": "Personal",
      "config": {
        "enabled": true,
        "subscribedProjectIds": ["project-id"],
        "defaultProjectId": "project-id",
        "defaultSessionId": null
      },
      "state": {
        "chatId": 123456789,
        "selectedProjectId": "project-id",
        "selectedSessionId": null,
        "nextUpdateId": 42
      }
    }
  ]
}
```

Each profile's token should use its own credential-store entry:
`telegram-bot-token:<data-dir-scope>:<bot-id>`. Tokens must not appear in the
profile JSON. Deleted project/session pruning must walk every bot profile.

### Runtime Model

The multi-bot runtime should use a supervised runtime map keyed by bot id:

- Start one long-polling relay per enabled, valid bot profile.
- Stop only the affected bot when its token/config becomes invalid or disabled.
- Restart only the affected bot when its fingerprint changes.
- Expose status per bot: configured, enabled, running, lifecycle, linked chat,
  masked token, subscribed projects, defaults, and later poll-health fields.
- Keep `getUpdates` cursors isolated per bot. Sharing one cursor across bots is
  invalid because Telegram update ids are scoped to each bot token.
- Do not allow duplicate bot tokens among enabled profiles. Telegram permits
  only one active `getUpdates` poller per token; duplicate tokens should fail
  validation with a user-facing error.

The existing `TelegramBotConfig` can stay as the per-runtime value, but it needs
an added `bot_id` and a per-bot `state_path` or state accessor. The relay should
never write another bot's runtime state during digest/cursor persistence.

### HTTP Surface

The proposed multi-bot API is a future contract; this table does not describe
the current single-bot endpoints or their response types:

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/telegram/status` | Return aggregate Telegram status with `bots: TelegramBotStatus[]` |
| POST | `/api/telegram/bots` | Create a bot profile with name, optional token, project subscriptions, defaults, and enabled flag |
| PATCH | `/api/telegram/bots/{bot_id}` | Update name, token, enabled flag, subscriptions, defaults, or clear token/defaults using nullable marker fields |
| DELETE | `/api/telegram/bots/{bot_id}` | Disable relay, delete token, and remove the bot profile/runtime state |
| POST | `/api/telegram/bots/{bot_id}/test` | Validate a supplied token or that bot's saved token with Telegram `getMe` |

Proposed response sketch:

```ts
type TelegramStatusResponse = {
  bots: TelegramBotStatus[];
};

type TelegramBotStatus = {
  id: string;
  name: string;
  configured: boolean;
  enabled: boolean;
  running: boolean;
  lifecycle: "inProcess";
  linkedChatId?: number | null;
  botTokenMasked?: string | null;
  subscribedProjectIds: string[];
  defaultProjectId?: string | null;
  defaultSessionId?: string | null;
};
```

PATCH fields should use a tri-state convention:

- omitted means "leave unchanged"
- `null` means "clear"
- a value means "replace"

List fields should use this convention:

- omitted or `null` means "leave unchanged"
- an array replaces the list

### Settings UI

The Settings -> Telegram panel should become a profile list plus detail editor:

- Left column or top list of bot profiles with status badges.
- `Add bot`, `Rename`, `Disable`, and `Remove` actions.
- Detail editor reuses the current token/test/subscribed-project/default-target
  controls for the selected bot.
- Avoid exposing all tokens at once. Each profile editor should keep token entry
  write-only and show only the masked saved token.

### Validation And Safety

- A bot cannot be enabled without a configured token and at least one valid
  project target.
- `defaultSessionId`, when present, must belong to the effective default project.
- Profile names are presentation only; commands and callback routing must use
  stable bot ids.
- All token, Telegram API, and callback errors must continue to sanitize token
  material in logs and API errors.
- If any one bot fails to start, other enabled bots should continue running.
- Tests must cover the v2 profile shape, per-bot keyring lookup,
  duplicate-token rejection, project/session pruning across all bots, and
  runtime start/stop/restart isolation.

## Remaining Work

- Replace manual chat binding with a one-time link-code wizard.
- Surface relay errors and poll health in Settings.
- Add a "test send" action.
- Evaluate richer digest formatting beyond the current `<pre>` table. Telegram
  Bot API HTML does not support real `<table>` markup, so future options
  include generated PNG/SVG snapshots or attached HTML files for wider tables
  and richer report layouts.
- Design a separately scoped multi-bot feature; no alternate storage or API
  contract is implemented by the current relay.
