# Project status: where TermAl stands

**As of 2026-10-29, master `3fc13725`, updated 2026-11-01 for Execution
Observability.** This page is a dated snapshot. The
tracker stays the source of truth for open work, and the feature briefs linked
below describe each mechanism. A later refresh replaces this snapshot instead of
appending to it.

Linked from the [README](../../README.md#where-termal-stands). Related briefs:
[central control plane](central-control-plane.md),
[satellite onboarding](satellite-onboarding.md),
[federation link](federation-link.md),
[remote full gates](remote-full-gates.md),
[native mailbox relay](native-mailbox-relay.md),
[Engram host adapter](engram-host-adapter.md),
[Execution Observability](execution-observability.md) and
[Test Runs](test-runs.md).

## What works now

### Federation across real machines

- One coordination domain runs on real machines. A Windows central host is
  linked to two satellites, a second Windows machine and a Mac, over the
  [federation link](federation-link.md).
- Saved federation endpoints can move from literal IP addresses to DNS
  hostnames without pairing again. The hostname is resolved again on every
  connection. Trust stays bound to the pinned host key, the host identity and
  the enrollment, not to the address. See
  [recovering a changed network address](satellite-onboarding.md#recovering-a-changed-network-address).
- On 2026-10-29, after a network renumbering:
  - The Windows satellite reconnected on its own, without being paired again.
    Mail queued while it was away was then delivered, each message once.
  - The Mac reconnected after a manual fix: an SSH key restriction on the Mac,
    pinned to the central's old IP address, was corrected by hand.
- Cross-host mailbox messages wake idle sessions on a satellite. See
  [native mailbox relay](native-mailbox-relay.md).

### Remote full gates

- The maintained full gate runs on a satellite and reports back to the
  central coordinator. See [remote full gates](remote-full-gates.md).
- On 2026-10-29 the full gate passed on master `92572b94` on both satellites,
  all five stages each:
  - on the Windows satellite, with 6842 Rust tests and 4885 Vitest tests;
  - on the Mac, with 6733 Rust tests and 4885 Vitest tests.

### Execution Observability on every harness

- [Execution Observability](execution-observability.md) is a project setting
  of the Engram integration, on by default. A turn is delivered without
  waiting for Engram, and its end reaches Engram as one host observation of
  its outcome, source change and checks. See the
  [adapter brief](engram-host-adapter.md#status).
- Codex, Claude, Cursor, Gemini, OpenCode and Kimi turns report in the same
  shape. Cursor, Gemini and OpenCode report a test's result as indeterminate
  unless their tool update carries the command's exit code.
- Tests on every harness cover this. Observing it on a running host is still
  pending: a host runs it only after it restarts on a build that includes it.
- Turn gating has been removed.

## Active limitations and open top-priority problems

- **A program cannot yet send a prompt straight to a chosen session on another
  machine** and get durable acceptance back. Today a mailbox message wakes the
  session, or someone types on that machine.
- **The SSH link diagnostics are on master but not yet on the running
  hosts.** With them, an SSH process that ends before the link's first
  handshake reports a lost link instead of a startup timeout. Each peer's
  diagnostics also show the attempt in progress and a fixed class for the SSH
  failure (name resolution, connect, authentication, host key or forwarding),
  never the raw error text. See
  [connection diagnostics](satellite-onboarding.md#connection-diagnostics).
  Until the hosts run a build with them, an early failure still looks like a
  timeout.
- **Federation recovery after a restart is not yet fully proven.** The hostname
  code is on master. A live migration on the running hosts, an independent
  evaluation, and deterministic tests that queued mail drains after a reconnect
  are still owed.
- **The older SSH project remote to the Mac stays fragile.** Its SSH key is
  restricted to one source IP, so the next address change breaks it again. A
  host-key or authentication failure shows only as a bare HTTP 502. Keeping
  trust safe when a remote's host changes is still to be built.
- **A Codex turn on a satellite can fail with an authorization error during
  compaction**, and Resume does not unpause the queue. Signing in again did not
  help; only restarting TermAl on that machine did.
- **A full gate run on 2026-10-29 left no host verification record**, either
  a pass or a refusal. It did not block a landing, but the record is missing.
- **On a busy host, the shared state lock can be held for seconds** at a time,
  because every commit rebuilds the full snapshot under it.
- **The Mac test-fixture isolation fix is not yet on master.** It is reviewed
  and waits for a Mac full gate.

## What comes next

1. **Federation diagnostics in use.** Run the landed SSH link diagnostics on
   the live hosts, then run the live hostname-migration evaluation there.
2. **State-lock hold.** Fix the multi-second hold of the shared state lock on
   a busy host.
3. **Direct remote prompts.** Add a supported way to send a prompt to a chosen
   remote session: first permissions and receiver consent, then the caller
   API, tests and docs.
4. **Unowned bugs.** The Codex authorization failure with queue recovery on a
   satellite, and the gate that left no verification record, wait for an
   owner.
