# Feature Reference: Codex Account Sign-In

This document describes how TermAl's Codex `/login` and `/logout` change the
Codex account that TermAl's Codex sessions use, without restarting the host.

Related: [Shared Codex App-Server](./shared-codex-app-server.md) describes the
TermAl-owned Codex homes and app-server profiles these commands sign in.

## Status

Implemented for Codex sessions. Account controls answer only the local UI's
loopback origin.

## Commands

Typing `/login` or `/logout` as the whole prompt of a Codex session opens the
Codex account dialog instead of sending a prompt.

- **Standalone (the default).** TermAl uses the ordinary standalone Codex
  sign-in. `/logout` is refused until TermAl has signed in once with `/login`.
- **`/login`.** TermAl refuses the action while local Codex work is active or
  starting, including a live delegation app-server; retry once it has finished.
  Otherwise it retires its idle Codex app-servers and signs in with ChatGPT in a
  fresh Codex app-server on the default profile. The dialog shows a link that
  continues the sign-in in the browser and can cancel it. TermAl then confirms
  the account in a fresh process and copies the confirmed sign-in to every
  TermAl Codex profile. After that, a standalone Codex sign-in no longer changes
  TermAl's account. If a first sign-in is cancelled or does not complete, Codex
  work stays paused until a sign-in succeeds, and the dialog offers to return
  to standalone Codex sign-in instead. Returning to standalone removes the
  sign-in TermAl's own profiles hold, so Codex then uses only the standalone
  sign-in.
- **`/logout`.** TermAl signs the default profile out and removes the sign-in
  from the other profiles. The dialog then offers to return to standalone Codex
  sign-in.

While an account action runs, or while no usable account is signed in, TermAl
refuses prompts typed in TermAl for Codex sessions. A peer, mailbox or API
message to a Codex session that already has queued prompts can join that queue
instead; none reaches Codex before an account action leaves a usable account.
Queued mailbox and workflow prompts then resume on their own; other queued
prompts run with the session's next prompt or wake. A mailbox notice that
reaches an idle Codex session with nothing queued in that time is not queued:
its message
stays unread in the mailbox and arrives with the session's next wake or
prompt. When the host starts with
a TermAl-managed account, it checks the saved account before Codex work
resumes. TermAl saves the account mode, with a count of account actions, in
`termal.codex-account.json` beside its state database.

## Saved threads after an account action

A Codex session resumes its saved thread under the account that is signed in
now. If a thread's saved history is missing, TermAl normally starts a fresh
thread and keeps the prompt. It never does that silently for a thread saved
before the last account action: the send fails and the session keeps its
thread. A thread saved since the last account action restarts as before.

## Sign-in notice

When a current Codex runtime of a signed-in TermAl-managed account reports
Unauthorized, the dialog shows "Codex account state is unknown — sign in again
with /login." next to the account status. The notice is informational: it does
not change the account status or stop Codex work. The next account action
clears it.

## Known limitation

On Windows, each Codex app-server profile keeps its own copy of the sign-in, and
a profile refreshes its copy on its own. Codex rotates the refresh token on each
refresh, so a copy another profile still holds stops working after that
refresh. TermAl copies the default profile's sign-in to the other profiles only
during an account action and when the host starts, so it can overwrite a
profile's newer copy with the older one. A profile left with an outdated copy
reports Unauthorized, and the notice above appears. Signing in again with
`/login` gives every profile the same current sign-in. Keeping the profiles in
step between account actions is a planned follow-up.

On other platforms TermAl uses one Codex profile, so this does not occur.

A first `/login` signs in with ChatGPT only from a profile without an API-key
sign-in. If standalone Codex is signed in with an API key, TermAl's default
profile carries that sign-in and `/login` refuses it. Accepting an API-key
sign-in as the starting point for `/login` is a planned follow-up.
