# Feature Brief: Telegram Relay — UI Configuration & In-Process Lifecycle

This document describes the design for moving the Telegram relay from a manual,
env-var-driven, separate-process setup into a UI-driven, supervised in-process
runtime.

Parent feature: [`whatsapp-integration.md`](./whatsapp-integration.md) (the
transport-agnostic mobile-notifications layer; this brief is the Phase 1
Telegram-specific UI integration).

Backlog source: [`docs/bugs.md`](../bugs.md) — the active Telegram-relay
findings (first-touch chat-binding security risk, no length cap, unbounded
update batch, unredacted error logs, stringly-typed status gate, etc.) all land
on top of this brief and should be folded into the build sequence below.

## Status

Proposed. The Phase 1 Telegram relay itself ships today (`src/telegram.rs`,
`cargo run -- telegram` mode) but configuration is via three env vars
(`TERMAL_TELEGRAM_BOT_TOKEN`, `TERMAL_TELEGRAM_PROJECT_ID`, optional
`TERMAL_TELEGRAM_CHAT_ID`) and the relay runs as a separate long-poll process
the user has to keep alive in a second terminal. First-touch chat binding is
the current linking mechanism. No UI surface exists.

## Problem

The current setup flow is three steps of typing in a terminal plus an
implicit step where the first chat to message the bot wins:

1. Get token from BotFather, paste into env var.
2. Set the project id env var.
3. Run `cargo run -- telegram` in a second terminal and leave it open.
4. Send `/start` from any Telegram chat — first one wins.

Every part of this is friction: the user has to redo step 1–3 on every machine
restart, the token sits in shell history, the relay disappears when the
terminal closes, and step 4 is the security finding flagged in the latest
review (anyone with the token plus an unset chat id has effective shell access
to the local machine).

## Goal

The user configures Telegram once from the TermAl UI: paste the token, pick a
project, click "Link your chat" (which produces a one-time code to send to the
bot), toggle the relay on. After that the relay runs as part of the main
TermAl backend — it starts when TermAl starts, restarts on transient failure,
and surfaces status in the UI.

## Non-goals

- Multi-bot configurations (one bot, one project for v1; multi-bot is later).
- Webhook mode / public tunnels (Phase 1 stays local-first long-polling).
- Mobile-app push notifications via Telegram (separate transport, see parent
  doc for the broader transport-agnostic plan).
- Replacing `cargo run -- telegram` immediately — that mode stays as a
  deprecated escape hatch during the transition and is removed once the
  in-process path is the documented happy path.

## Decision: Option A — in-process tokio task

The relay runs as a tokio task supervised by the main TermAl backend, in the
same process. Configuration lives in `~/.termal/telegram-bot.json` and is
edited via dedicated REST endpoints. The relay is started/stopped/restarted by
a supervisor task that watches the config for changes.

### Why this over the alternatives

| Option | Why not |
|---|---|
| **B. Separate child process spawned by backend** | Cross-platform process management (Windows job objects, POSIX session handling, zombie reaping) is fiddly; failure isolation is a real win but the relay is a long-poll loop against an HTTPS endpoint — its failure modes are well-bounded with `?` propagation and a supervisor that restarts after backoff. Two binaries to ship. |
| **C. Webhook (no polling)** | Requires a public tunnel (Cloudflare Tunnel, ngrok, custom relay). Breaks the "local-first" model documented across the project. Phase 4 work at the earliest. |
| **A. Same process (chosen)** | One binary; live status visible to UI; token rotation hot-applies without restart; reuses the backend's HTTP client and `AppState`; supervisor pattern is well-trodden in tokio (`tokio::spawn` + `tokio::sync::watch` shutdown channel + restart loop with exponential backoff). |

## Proposed UI

A new "Mobile / Telegram" section in Settings:

```
┌─ Mobile / Telegram ──────────────────────────────┐
│                                                  │
│  Bot token   [••••••••••••••••••AAEgVBUDfPbcH5•] │
│              [Test connection]   ✓ termal_user_bot
│                                                  │
│  Project     [TermAl                          ▾] │
│                                                  │
│  Linked chat ⚠ Not linked yet                    │
│              [Link your chat]                    │
│                                                  │
│  ┌─ Linking wizard (modal) ────────────────────┐ │
│  │ 1. Open @termal_user_bot in Telegram        │ │
│  │ 2. Send this exact message:                 │ │
│  │      /start TA9K-7ZQ2                       │ │
│  │ 3. Waiting for link...  (polling)           │ │
│  └─────────────────────────────────────────────┘ │
│                                                  │
│  Status      ● Polling · last sync 2s ago        │
│              Forwarded 14 turns · 3 errors       │
│                                                  │
│  Enable      [████] ON                           │
│                                                  │
│  [Unlink chat]   [Reset state]   [Test send]     │
└──────────────────────────────────────────────────┘
```

Token input is password-style; once saved it shows as masked with the last
8 chars visible (matching the GitHub PAT pattern). Project picker is a
dropdown of existing projects. The status row updates every few seconds via
either polling `GET /api/telegram/status` or an SSE delta event.

## Architecture

```
┌────────────────────────────────────┐
│  TermAl backend process            │
│                                    │
│  ┌──────────────┐                  │
│  │ HTTP server  │ ── /api/telegram/* (config / status / link)
│  │ (axum)       │                  │
│  └──────┬───────┘                  │
│         │                          │
│  ┌──────▼──────────────────┐       │
│  │ Telegram relay          │       │
│  │ supervisor task         │       │
│  │ (tokio::spawn)          │       │
│  └──────┬──────────────────┘       │
│         │ watch::Sender<Config>    │
│         │ watch::Sender<Shutdown>  │
│  ┌──────▼──────────────────┐       │
│  │ Telegram poll loop      │ ──── HTTPS ───→ api.telegram.org
│  │ (existing run_telegram_ │                  /getUpdates
│  │  bot, refactored to     │                  /sendMessage
│  │  take config + signal)  │                  /editMessageText
│  └─────────────────────────┘       │
│                                    │
└────────────────────────────────────┘
                │
                │  loopback
                ▼
        127.0.0.1:8787 (self)
        /api/projects/{id}/digest
        /api/projects/{id}/actions/{id}
        /api/sessions/{id}
        /api/sessions/{id}/messages
```

The relay still talks to TermAl's REST API over HTTP rather than reaching into
`AppState` directly. That separation is a feature: the relay is a transport
adapter and uses the same public API surface a future external relay would.

### State storage

`~/.termal/telegram-bot.json` (already exists, currently holds chat binding +
dedupe markers) grows to hold the full config:

```json
{
  "config": {
    "enabled": true,
    "bot_token": "8788914592:AAEgVBUD…",
    "project_id": "project-1"
  },
  "binding": {
    "chat_id": 8389943079,
    "linked_at": "2026-05-04T17:12:33Z"
  },
  "runtime": {
    "next_update_id": 63454113,
    "last_digest_hash": "…",
    "last_digest_message_id": 47,
    "last_forwarded_assistant_message_id": "msg-…",
    "last_forwarded_assistant_message_text_chars": 4123
  },
  "stats": {
    "forwarded_turn_count": 14,
    "error_count_24h": 3,
    "last_error": null,
    "last_poll_at": "2026-05-04T17:18:01Z"
  }
}
```

Token never flows through `/api/state`. Status surface (`GET
/api/telegram/status`) returns a redacted view: token shown as
`••••<last 8 chars>`, last error trimmed to one line and 256 chars.

### Lifecycle

The supervisor task:

1. Reads `telegram-bot.json` on startup.
2. If `config.enabled && config.bot_token && config.project_id`, spawns the
   relay loop.
3. Watches a `tokio::sync::watch::Sender<TelegramConfig>` so a `POST
   /api/telegram/config` mutation broadcasts the new config to the supervisor.
4. On config change, gracefully shuts down the current loop (via the
   shutdown signal) and re-spawns with the new config.
5. On loop error, restarts with exponential backoff (1s → 2s → 4s → … capped
   at 60s), persisting the error to `stats.last_error` and bumping
   `stats.error_count_24h`.

The relay loop itself is the existing `run_telegram_bot()` body refactored
to:

- accept `(config, shutdown_signal)` instead of reading env vars + state file;
- accept a stats handle so it can update counters;
- exit cleanly when the shutdown signal flips.

### Linking flow (closes the security finding)

1. User clicks "Link your chat" in the UI.
2. UI calls `POST /api/telegram/start-link` → backend generates a random
   `linkCode` (8 chars, Crockford base32 alphabet, displayed as `TA9K-7ZQ2`)
   and stores only its server-side record in memory with a 5-minute TTL.
3. UI shows the code: *"Send `/start TA9K-7ZQ2` to your bot."*
4. Relay's `/start` handler:
   - With code, code matches, code not expired, code unused → bind chat, clear
     code, send welcome digest.
   - With code, code mismatch or expired → reply *"Invalid or expired link
     code. Generate a new one in TermAl."*
   - Without code (and no chat is bound yet) → reply with instructions to
     start the link from TermAl (no auto-bind).
   - Without code, chat is already bound → ignore (treat as a re-greeting).
5. UI polls `GET /api/telegram/link/status` every ~1s during the wizard; flips
   to "Linked" once `binding.chatId` is populated.
6. Code expires automatically after 5 minutes. Invalid attempts are rate-limited
   per chat and receive the same generic failure text so the relay does not leak
   "right format, wrong value" details.

This kills the first-touch hazard. A leaked token without a paired live link
code can no longer attach a chat.

### Security model

- The in-process REST surface is local-admin only. The backend confirms it is
  listening on loopback for `/api/telegram/*` mutations and rejects mutating
  Telegram requests from non-loopback listeners.
- Browser-originated Telegram mutations require the expected TermAl Origin
  header. Missing or foreign origins are rejected before token, prompt, or link
  code validation runs.
- Token updates are write-only: request bodies may include `botToken`, but
  responses only return the redacted suffix and never echo the full token.
- Link codes are single-use, 8-character Crockford base32 values with a
  5-minute TTL. Already-bound replay is idempotent for the linked chat and
  rejected generically for any other chat.

## API additions

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/telegram/status` | Current relay state (configured, enabled, running, linked chat id, last poll, last error, counters) |
| POST | `/api/telegram/config` | Update token and project id — body validated, token never echoed back |
| POST | `/api/telegram/relay/start` | Start the configured relay loop |
| POST | `/api/telegram/relay/stop` | Stop the relay loop without repeating the token in the request body |
| POST | `/api/telegram/test` | Validate the supplied token by calling Telegram `getMe` and return `{ botName, botUsername }` |
| POST | `/api/telegram/start-link` | Generate `{ linkCode, expiresAt }` for a one-time link code with 5-minute TTL |
| GET | `/api/telegram/link/status` | Poll the active link attempt (`pending`, `linked`, `expired`, `none`) |
| POST | `/api/telegram/link/cancel` | Cancel the active link attempt and invalidate its code |
| POST | `/api/telegram/unlink` | Clear `binding.chatId` |
| POST | `/api/telegram/test-send` | Send a one-line digest to the linked chat to verify end-to-end delivery |

All routes return JSON; all errors return `ApiError` with appropriate status
codes. `/api/telegram/test` uses 400 for parsed local validation failures such
as a missing token, 422 for JSON syntax/data-shape rejections or Telegram
`getMe` token/auth rejections, 429 for local or Telegram `getMe` rate limits,
and 502 for Telegram transport/decode/upstream failures. Clients should use the
error message to distinguish local request-shape 422s from Telegram token/auth
422s.

Compatibility note: validation now reports a session whose project was deleted
as `unknown default Telegram session project` before checking whether that
session matches the configured default project. Callers should not branch on the
older `default Telegram session must belong to the default project` text for
orphaned sessions.

## Build sequence (incremental, each step shippable)

### Step 1 — Backend lifecycle move (no UI yet)

- Refactor `run_telegram_bot()` to take `(config: TelegramBotConfig,
  shutdown: watch::Receiver<bool>, stats: Arc<RuntimeStats>)`.
- Add a supervisor task in `app_boot` that reads `telegram-bot.json`, spawns
  the relay if `enabled && configured`, restarts with exponential backoff.
- `cargo run -- telegram` still works (calls the same loop with env-var-derived
  config) but becomes a deprecated escape hatch.
- Folds in the **stringly-typed status gate** fix (parse `SessionStatus` as a
  proper enum with `#[serde(other)] Unknown`).

### Step 2 — Status endpoint

- `GET /api/telegram/status` returns the full status shape.
- Counters (`forwarded_turn_count`, `error_count_24h`, `last_error`,
  `last_poll_at`) updated by the relay loop via `Arc<Mutex<RuntimeStats>>`.
- Token redacted (last 8 chars only).
- Folds in the **error-bodies-bubbled-unredacted** fix (truncate non-JSON error
  payloads to 256 bytes; prefer structured `error.error` field; replace
  `{err:#}` with category-only logging).

### Step 3 — Config endpoints + UI panel

- `POST /api/telegram/config` (token, projectId).
- `POST /api/telegram/relay/start|stop` controls runtime lifecycle without
  repeating the token in request bodies.
- `POST /api/telegram/test` (calls `getMe` against the supplied token; returns
  bot username so the UI can show *"✓ termal_user_bot"*).
- New "Mobile / Telegram" section in Settings: token field, project dropdown,
  test button, enable toggle, status row.
- Token persisted via `telegram-bot.json`, never via `/api/state`.

### Step 4 — Code-based linking wizard

- `POST /api/telegram/start-link` → returns `{ linkCode, expiresAt }`.
- `GET /api/telegram/link/status` polls the active linking attempt.
- `POST /api/telegram/link/cancel` invalidates the active link code.
- Relay's `/start` handler validates against the active code; rejects free
  binding.
- UI modal shows code + polls `GET /api/telegram/link/status` until linked.
- Closes the **first-touch chat-binding** security finding.

### Step 5 — Polish + remaining review fixes

- Length cap on Telegram-forwarded text (consistent with
  `MAX_DELEGATION_PROMPT_CHARS = 256k chars` from the delegation surface, or
  tighter — the cap should be visible in the UI as well so users know what
  Telegram messages will be rejected).
- Per-minute prompt-rate cap.
- `getUpdates` `limit` param explicitly capped (e.g., 25).
- Per-update `next_update_id` persistence (instead of once-per-iteration).
- "Test send" button (sends a one-line digest to the linked chat to verify
  end-to-end delivery).
- "Reset state" button (clears `runtime` block — chat binding, dedupe
  markers — without removing config).
- Last-error display + a "View logs" button (shows the last N error lines
  from `stats.last_error`).

## Decisions captured from this round

- **Same-process / in-process tokio task** is the chosen architecture (Option
  A above).

## Open questions

These are explicitly not decided yet — they need answers before the
implementation lands. Capturing them here so they don't get lost between this
brainstorm and the actual code change.

### 1. Token storage shape

Should `~/.termal/telegram-bot.json` grow into a single config-plus-runtime
file (current draft above), or should config and runtime be split into two
files (e.g., `telegram-config.json` for user-edited fields and
`telegram-runtime.json` for counters and binding) so the user can backup /
sync the config without leaking dedupe markers?

### 2. Disable behavior

When the user disables the bot in the UI, do we (a) immediately kill the
polling loop with the shutdown signal, or (b) finish the current poll
iteration cleanly and exit? (b) is safer — no risk of cancelling a
mid-write `sendMessage` — but (a) is more responsive (the user sees the
status flip to "off" within a second instead of after the long-poll
timeout). Compromise: drop the `getUpdates` long-poll timeout to 1s when a
shutdown is pending so (b) feels like (a).

### 3. CLI escape hatch

Keep `cargo run -- telegram` working as-is during the transition (reads env
vars, runs the same loop), or remove it as part of Step 1? Keeping it adds
a small maintenance cost; removing it forces UI configuration which is the
goal but breaks anyone scripting the relay today. Recommendation: keep
through Step 4, remove in Step 5 with a deprecation notice in the README.

### 4. Multi-tab UX

If the user has TermAl open in two browser tabs and toggles the bot in one,
the other tab needs to see the new state. Two options: (a) emit a
`telegramConfigUpdated` SSE delta event so all tabs refresh; (b) rely on
the next polled `GET /api/telegram/status` (every 5s in the UI). (a) is
nicer but adds a new event type; (b) is good enough for v1 since toggle
changes are rare.

### 5. Project switching

If the user changes the `projectId` in the config while the chat is
already linked, do we (a) keep the chat bound and just retarget action
dispatches to the new project, or (b) require re-linking? (a) is friendlier
but means the chat history in Telegram now spans two projects without a
visible separator; (b) is more explicit but adds a setup step every time
the user wants to follow a different project. Recommendation: (a) by
default, with a "Switching project — past messages stay in this chat" note
in the UI when the user picks a different project.

### 6. Stats retention

`stats.error_count_24h` implies a rolling window. Where is the window
implemented? Options: (a) the relay tracks a `VecDeque<DateTime>` of error
timestamps and prunes to the last 24h on each error; (b) a single counter
that's reset every 24h on first error after the boundary; (c) drop the
"_24h" qualifier and just track lifetime errors. (a) is most informative;
(c) is simplest. Recommendation: (c) for v1, upgrade later.

### 7. `botUsername` cache lifetime

Once the user passes "Test connection," do we cache the result so the UI can
show *"✓ termal_user_bot"* on subsequent loads, or re-call `getMe` on every
status fetch? Caching adds invalidation complexity (token change → cache
stale); not caching wastes a round-trip per UI load. Recommendation: cache
in `stats.bot_username`, invalidate on `POST /api/telegram/config` when the
token changes.

### 8. Linking UX when the bot username is unknown

If the user enters a token but skips "Test connection," the linking wizard
shows *"Open @<bot username>"* — but we don't know the username yet. Do we
(a) call `getMe` automatically inside `start-link`, (b) require the user to
click "Test connection" first (gate the link button), or (c) show
instructions without the @ mention? Recommendation: (a) — auto-call
`getMe` and show the username; falls back to *"Open your bot in Telegram"*
on `getMe` failure with a clear error.

## Cross-references

- Parent / transport-agnostic mobile-relay design:
  [`whatsapp-integration.md`](./whatsapp-integration.md)
- Active Telegram-relay bugs (security, length cap, batch limits, error
  redaction, status gate): [`docs/bugs.md`](../bugs.md) — search for
  "Telegram"
- Existing implementation: `src/telegram.rs` (relay loop, API client,
  digest sync, message forwarding, link binding)
- Storage path: `~/.termal/telegram-bot.json` (already used for chat
  binding; to grow into the full config-plus-runtime shape above)
