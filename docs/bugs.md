# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Implementation Tasks

- [ ] P2: Extract oversized frontend hot-path helpers:
  move JSON-first `/api/state` parsing into a focused API helper and virtualized transcript measurement/cache logic into focused helper or hook modules so the reviewed hot paths stop growing oversized frontend files.
- [ ] P2: Extract delegation outcome recovery tests and helpers:
  split delegation result-packet recovery cases out of the oversized backend delegation test/module path so future delegation lifecycle changes can be reviewed in focused files.
- [ ] P2: Extract live-state workspace reconciliation helpers:
  move delegated-child workspace pruning/readiness helpers out of `app-live-state.ts` as that hot path has crossed the active TypeScript utility threshold.
