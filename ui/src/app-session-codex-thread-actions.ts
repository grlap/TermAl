// Owns: Codex thread action handlers for app session actions.
// Does not own: generic session settings, prompt send, or marker actions.
// Split from: ui/src/app-session-actions.ts.

import {
  archiveCodexThread,
  compactCodexThread,
  forkCodexThread,
  rollbackCodexThread,
  unarchiveCodexThread,
  type StateResponse,
} from "./api";
import type {
  UseAppSessionActionsParams,
  UseAppSessionActionsRefs,
  UseAppSessionActionsReturn,
  UseAppSessionActionsSetters,
} from "./app-session-actions-types";
import { setSessionFlag } from "./app-utils";
import type { Session } from "./types";
import { openSessionInWorkspaceState } from "./workspace";

type CodexThreadActions = Pick<
  UseAppSessionActionsReturn,
  | "handleForkCodexThread"
  | "handleArchiveCodexThread"
  | "handleUnarchiveCodexThread"
  | "handleCompactCodexThread"
  | "handleRollbackCodexThread"
>;

type CodexThreadActionsDeps = {
  adoptCreatedSessionResponse: UseAppSessionActionsParams["adoptCreatedSessionResponse"];
  adoptSessionActionState: (sessionId: string, state: StateResponse) => boolean;
  applyControlPanelLayout: UseAppSessionActionsParams["applyControlPanelLayout"];
  isMountedRef: UseAppSessionActionsRefs["isMountedRef"];
  reportRequestError: UseAppSessionActionsParams["reportRequestError"];
  requestActionRecoveryResync: UseAppSessionActionsParams["requestActionRecoveryResync"];
  sessionsRef: UseAppSessionActionsRefs["sessionsRef"];
  setRequestError: UseAppSessionActionsSetters["setRequestError"];
  setSessionSettingNotices: UseAppSessionActionsSetters["setSessionSettingNotices"];
  setUpdatingSessionIds: UseAppSessionActionsSetters["setUpdatingSessionIds"];
  setWorkspace: UseAppSessionActionsSetters["setWorkspace"];
};

export function createCodexThreadActions({
  adoptCreatedSessionResponse,
  adoptSessionActionState,
  applyControlPanelLayout,
  isMountedRef,
  reportRequestError,
  requestActionRecoveryResync,
  sessionsRef,
  setRequestError,
  setSessionSettingNotices,
  setUpdatingSessionIds,
  setWorkspace,
}: CodexThreadActionsDeps): CodexThreadActions {
  async function runCodexThreadStateAction(
    sessionId: string,
    request: () => Promise<StateResponse>,
    successNotice: string,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await request();
      if (!isMountedRef.current) {
        return;
      }
      if (!adoptSessionActionState(sessionId, state)) {
        return;
      }
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: successNotice,
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleForkCodexThread(
    sessionId: string,
    preferredPaneId: string | null,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const created = await forkCodexThread(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      const adopted = adoptCreatedSessionResponse(created, {
        openSessionId: created.sessionId,
        paneId: preferredPaneId,
      });
      const canOpenStaleCreatedSession =
        adopted === "stale" &&
        sessionsRef.current.some(
          (session: Session) => session.id === created.sessionId,
        );
      if (adopted === "stale" && !canOpenStaleCreatedSession) {
        requestActionRecoveryResync({
          openSessionId: created.sessionId,
          paneId: preferredPaneId,
          allowUnknownServerInstance: true,
        });
      }
      const canUseCreatedSession =
        adopted === "adopted" || canOpenStaleCreatedSession;
      if (canOpenStaleCreatedSession) {
        setWorkspace((current) =>
          applyControlPanelLayout(
            openSessionInWorkspaceState(
              current,
              created.sessionId,
              preferredPaneId,
            ),
          ),
        );
      }
      if (canUseCreatedSession) {
        setSessionSettingNotices((current) => ({
          ...current,
          [sessionId]: "Forked the live Codex thread into a new session.",
          [created.sessionId]:
            "This session is attached to a forked Codex thread. Earlier Codex history was restored from Codex where available.",
        }));
      }
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleArchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => archiveCodexThread(sessionId),
      "Archived the live Codex thread for this session.",
    );
  }

  async function handleUnarchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => unarchiveCodexThread(sessionId),
      "Restored the archived Codex thread for this session.",
    );
  }

  async function handleCompactCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => compactCodexThread(sessionId),
      "Started Codex context compaction for this session.",
    );
  }

  async function handleRollbackCodexThread(
    sessionId: string,
    numTurns: number,
  ) {
    const turnLabel = numTurns === 1 ? "turn" : "turns";
    await runCodexThreadStateAction(
      sessionId,
      () => rollbackCodexThread(sessionId, numTurns),
      `Rolled the live Codex thread back by ${numTurns} ${turnLabel}.`,
    );
  }

  return {
    handleForkCodexThread,
    handleArchiveCodexThread,
    handleUnarchiveCodexThread,
    handleCompactCodexThread,
    handleRollbackCodexThread,
  };
}
