# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Windows read-only Codex delegations run with full-access sandboxing

**Severity:** High - Windows Codex reviewer delegations marked read-only can still write through the Codex process sandbox and gain network egress.

`src/delegations.rs` maps read-only Codex delegations to `CodexSandboxMode::DangerFullAccess` on Windows to avoid the current `windows sandbox: spawn setup refresh` failure. That makes the delegation usable, but it also means the child agent's own shell/apply_patch tools are no longer sandbox-enforced as read-only. TermAl's mediated file API can still reject writes, but the child process sandbox is broader than the documented "enforced isolation" contract.

**Current behavior:**
- Windows Codex delegations created with `writePolicy: readOnly` use `danger-full-access`.
- The delegated child can use Codex-owned tools with full filesystem access and network access despite the read-only policy.
- Project docs still describe read-only delegation isolation as enforced rather than prompt-only.

**Proposal:**
- Prefer a narrower Windows fallback if it avoids the Codex sandbox setup failure, or reject Windows Codex read-only delegation with a clear platform error.
- If a temporary full-access fallback remains, surface the reduced enforcement in the UI and update the delegation architecture docs.
- Track restoring true read-only sandboxing once the upstream Windows Codex sandbox issue is fixed.

## Implementation Tasks

- [ ] P2: Extract delegation child-interaction detail helpers:
  move child pending-interaction detail synthesis and parent-card refresh
  helpers out of `src/delegations.rs` into a focused delegation status module.
- [ ] P2: Resolve `flow.md` placement:
  decide whether the untracked review-flow note belongs under `docs/`, should
  be added to an ignore list, or should remain local-only outside version
  control.
- [ ] P2: Extract delegation result parsing and synthesis helpers:
  move the cohesive result-packet parsing, plain-output synthesis, findings
  parsing, and summary compaction cluster out of `src/delegations.rs` so future
  delegation result changes land in a focused module.
- [ ] P2: Extract workspace session-reference helpers:
  move session-reference collection, delegated-child reference detection, and
  adjacent reconciliation helpers out of `ui/src/workspace.ts` so workspace tree
  utilities stay below the active size threshold.
