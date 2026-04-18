// Pure helpers for adopting a remote `StateResponse` snapshot into
// the UI's in-memory state.
//
// What this file owns:
//   - `resolveAdoptedStateSlices` — merges a partial next snapshot
//     into the current slices the shell cares about (`codex`,
//     `agentReadiness`, `projects`, `orchestrators`, `workspaces`).
//     Fields absent from the partial (`undefined`) are left
//     untouched; fields present (including explicitly empty
//     arrays) replace the current value. Used both on full state
//     adoption and on delta application.
//   - `resolveRecoveredWorkspaceLayoutRequestError` — after a
//     successful workspace-layout restart, clears the "request
//     error" banner if it matched the workspace-layout restart
//     error message (i.e., the banner was showing that specific
//     error). Returns `null` to clear, or the current error
//     verbatim when the two don't match (an unrelated error should
//     stay visible).
//
// What this file does NOT own:
//   - Dispatching state updates (`setState` calls, revision gating,
//     delta application) — those live in `App.tsx`. This module
//     only produces the next slice values.
//   - The SSE or POST pipelines that fetch the remote state.
//   - Error UI — the banner itself is rendered elsewhere; the
//     helper just decides whether to clear or keep the message.
//
// Split out of `ui/src/App.tsx`. Same signatures and behaviour as
// the inline definitions they replaced; consumers (including
// `App.test.tsx`) import from here directly.

import type { StateResponse, WorkspaceLayoutSummary } from "./api";
import type {
  AgentReadiness,
  CodexState,
  OrchestratorInstance,
  Project,
} from "./types";

export function resolveRecoveredWorkspaceLayoutRequestError(
  currentRequestError: string | null,
  workspaceLayoutRestartErrorMessage: string | null,
) {
  if (workspaceLayoutRestartErrorMessage === null) {
    return currentRequestError;
  }

  return currentRequestError === workspaceLayoutRestartErrorMessage
    ? null
    : currentRequestError;
}

export function resolveAdoptedStateSlices(
  current: {
    codex: CodexState;
    agentReadiness: AgentReadiness[];
    projects: Project[];
    orchestrators: OrchestratorInstance[];
    workspaces: WorkspaceLayoutSummary[];
  },
  nextState: Partial<
    Pick<
      StateResponse,
      "codex" | "agentReadiness" | "projects" | "orchestrators" | "workspaces"
    >
  >,
) {
  return {
    codex: nextState.codex !== undefined ? nextState.codex : current.codex,
    agentReadiness:
      nextState.agentReadiness !== undefined
        ? nextState.agentReadiness
        : current.agentReadiness,
    projects:
      nextState.projects !== undefined ? nextState.projects : current.projects,
    orchestrators:
      nextState.orchestrators !== undefined
        ? nextState.orchestrators
        : current.orchestrators,
    workspaces:
      nextState.workspaces !== undefined
        ? nextState.workspaces
        : current.workspaces,
  };
}
