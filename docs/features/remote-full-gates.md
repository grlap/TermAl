# Remote full gates

A remote full gate is the maintained five-stage gate
(`node scripts/test-launcher.mjs full`), run on a satellite host of the
[central control plane](central-control-plane.md) instead of the coordinator's
own machine. Its results are reported back to the central coordinator. It lets
a change be gated on a platform the coordinator does not run, such as macOS,
or on a second Windows machine.

Related:
- [Project status](project-status.md#remote-full-gates): the current results.
- [Satellite onboarding](satellite-onboarding.md): how a satellite joins the
  domain, which a remote gate needs first.
- [Native mailbox relay](native-mailbox-relay.md): the cross-host mail that
  requests the run and carries its completion.
- The [test guide](../test.md): the launcher itself, including
  [remote runners](../test.md#git-prerequisite-and-remote-runners) and
  [detached gates](../test.md#credit-for-a-background-or-detached-gate).
- [Test Runs](test-runs.md): reading runs in the UI.

## How a remote gate runs

1. **The coordinator asks for the run.** The central coordinator sends a root
   session on the satellite a task by mailbox. The mail wakes that session if
   it is idle.
2. **The runner prepares an exact input.** In its own clone, the satellite
   session creates a fresh detached worktree at the requested commit. It holds
   no untracked files. The runner checks that the commit's tree id equals the
   one the coordinator named.
   - For a change not yet landed, the integration owner first pushes a test
     ref: a detached commit, on the frozen base, of the reviewed tree. The
     runner fetches that ref. The repository's instructions set who may push
     such a ref and how it is named and deleted.
   - A commit already on master can be gated the same way, as evidence after
     the landing.
3. **The runner launches the gate detached** and sends its completion to the
   coordinator:

   ```bash
   node scripts/test-launcher.mjs full --detach --notify host:<central host UUID>::<coordinator session id>
   ```

   - On a satellite, a bare `session-*` id selects a local root, so the central
     coordinator is addressed in the explicit remote form shown. See
     [native mailbox relay](native-mailbox-relay.md).
   - The runner must be a root session that inherits its own
     `TERMAL_SESSION_ID`, as for any detached gate.
   - The launcher records its own input fingerprint before and after the run,
     and keeps complete logs and an atomic terminal result under Git metadata,
     as on any host.
4. **The runner returns the evidence** through the existing channels, never by
   pushing to Git: by mailbox to the coordinator and a note on the item. The
   evidence is the run id, the result, the per-stage counts and the launcher's
   before and after fingerprints.

## What a remote result counts for

The repository's instructions (`CLAUDE.md` and `AGENTS.md`, under "Test refs for
remote runs") are the authority. In short:

- A focused run on a test ref is evidence only.
- A full gate run by the standard launcher on the exact test ref counts as the
  landing gate for that tree on any host, with the same weight as one run on
  Windows, when two things hold:
  - its results record the same fingerprint before and after;
  - the result has been imported, on the host that evaluates and lands the
    change, as a recognized check record bound to the recorded tree id.
- **It gates only a landing whose master commit has that tree id.** Until then
  it is evidence only. A gate of a commit already on master is evidence after
  the landing, not a landing gate.
- A change that touches Windows-only code or tests still needs the relevant
  gate on Windows.
- A failed remote gate sends the input back to its owner, like any failed
  gate.

## Limits today

- A program cannot yet send a prompt straight to a chosen remote session; the
  run is requested by mailbox. See
  [project status](project-status.md#active-limitations-and-open-top-priority-problems).
- The satellite needs Git, Rust and Cargo, and Node and npm with access to the
  npm registry or a cache, plus Git Bash on Windows. The full launcher prepares
  the UI dependencies itself with `npm ci`, so a fresh worktree needs no manual
  setup. See [remote runners](../test.md#git-prerequisite-and-remote-runners)
  and the [review gate](../test.md#review-gate).
