# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Implementation Tasks

- [ ] P2: Extract delegation result parsing and synthesis helpers:
  move the cohesive result-packet parsing, plain-output synthesis, findings
  parsing, and summary compaction cluster out of `src/delegations.rs` so future
  delegation result changes land in a focused module.
- [ ] P2: Extract workspace session-reference helpers:
  move session-reference collection, delegated-child reference detection, and
  adjacent reconciliation helpers out of `ui/src/workspace.ts` so workspace tree
  utilities stay below the active size threshold.
