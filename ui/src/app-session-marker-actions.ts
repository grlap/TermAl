// Owns: conversation-marker action handlers for app session actions.
// Does not own: generic action-state adoption, workspace layout policy, or
// non-marker session actions.
// Split from: ui/src/app-session-actions.ts.

import {
  createConversationMarker,
  deleteConversationMarker,
  updateConversationMarker,
  type UpdateConversationMarkerRequest,
} from "./api";
import type {
  UseAppSessionActionsParams,
  UseAppSessionActionsRefs,
  UseAppSessionActionsReturn,
  UseAppSessionActionsSetters,
} from "./app-session-actions-types";
import { setSessionFlag } from "./app-utils";
import { buildCreateConversationMarkerRequest } from "./conversation-marker-requests";
import { conversationMarkerSatisfiesResponse } from "./conversation-marker-response-match";
import {
  deleteConversationMarkerLocally,
  upsertConversationMarkerLocally,
} from "./conversation-marker-session-mutations";
import { isServerInstanceMismatch } from "./state-revision";
import type { CreateConversationMarkerOptions, Session } from "./types";
import { findWorkspacePaneIdForSession, type WorkspaceState } from "./workspace";

type MarkerActions = Pick<
  UseAppSessionActionsReturn,
  | "handleCreateConversationMarker"
  | "handleUpdateConversationMarker"
  | "handleDeleteConversationMarker"
>;

type MarkerMutationResponse = {
  revision: number;
  serverInstanceId: string;
  marker?: NonNullable<Session["markers"]>[number];
  markerId?: string;
  sessionMutationStamp?: number | null;
};

type MarkerActionsDeps = {
  forceSseReconnect: UseAppSessionActionsParams["forceSseReconnect"];
  isMountedRef: UseAppSessionActionsRefs["isMountedRef"];
  lastSeenServerInstanceIdRef: UseAppSessionActionsRefs["lastSeenServerInstanceIdRef"];
  latestStateRevisionRef: UseAppSessionActionsRefs["latestStateRevisionRef"];
  reportRequestError: UseAppSessionActionsParams["reportRequestError"];
  requestActionRecoveryResync: UseAppSessionActionsParams["requestActionRecoveryResync"];
  sessionLookup: Map<string, Session>;
  sessionsRef: UseAppSessionActionsRefs["sessionsRef"];
  setRequestError: UseAppSessionActionsSetters["setRequestError"];
  setUpdatingSessionIds: UseAppSessionActionsSetters["setUpdatingSessionIds"];
  updateSessionLocally: (
    sessionId: string,
    update: (session: Session) => Session,
  ) => void;
  workspace: WorkspaceState;
};

export function createSessionMarkerActions({
  forceSseReconnect,
  isMountedRef,
  lastSeenServerInstanceIdRef,
  latestStateRevisionRef,
  reportRequestError,
  requestActionRecoveryResync,
  sessionLookup,
  sessionsRef,
  setRequestError,
  setUpdatingSessionIds,
  updateSessionLocally,
  workspace,
}: MarkerActionsDeps): MarkerActions {
  function shouldApplyMarkerMutationResponse(
    sessionId: string,
    response: MarkerMutationResponse,
    options: { deleted?: boolean } = {},
  ): "apply" | "stale-success" | "deferred" {
    const markerId = response.marker?.id ?? response.markerId;
    if (!markerId) {
      return "deferred";
    }

    if (
      isServerInstanceMismatch(
        lastSeenServerInstanceIdRef.current,
        response.serverInstanceId,
      )
    ) {
      const sseReconnectRequestId = forceSseReconnect();
      requestActionRecoveryResync({
        openSessionId: sessionId,
        paneId: findWorkspacePaneIdForSession(workspace, sessionId),
        allowUnknownServerInstance: true,
        sseReconnectRequestId,
      });
      return "deferred";
    }

    if (
      latestStateRevisionRef.current !== null &&
      response.revision <= latestStateRevisionRef.current
    ) {
      const currentSession =
        sessionsRef.current.find((session) => session.id === sessionId) ?? null;
      const currentMarker = currentSession?.markers?.find(
        (marker) => marker.id === markerId,
      );
      const responseMutationStamp = response.sessionMutationStamp ?? null;
      const currentMutationStamp = currentSession?.sessionMutationStamp ?? null;
      const targetStateMatches = options.deleted
        ? currentMarker === undefined
        : conversationMarkerSatisfiesResponse(currentMarker, response.marker);
      const hasTargetEvidence =
        currentSession !== null &&
        targetStateMatches &&
        (responseMutationStamp === null ||
          (currentMutationStamp !== null &&
            currentMutationStamp >= responseMutationStamp));
      if (hasTargetEvidence) {
        return "stale-success";
      }

      requestActionRecoveryResync({
        openSessionId: sessionId,
        paneId: findWorkspacePaneIdForSession(workspace, sessionId),
        allowUnknownServerInstance: true,
      });
      return "deferred";
    }

    latestStateRevisionRef.current = response.revision;
    return "apply";
  }

  async function handleCreateConversationMarker(
    sessionId: string,
    messageId: string,
    options: CreateConversationMarkerOptions = {},
  ) {
    const session = sessionLookup.get(sessionId);
    if (
      !session ||
      !session.messages.some((message) => message.id === messageId)
    ) {
      return false;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const response = await createConversationMarker(
        sessionId,
        buildCreateConversationMarkerRequest(messageId, options),
      );
      if (!isMountedRef.current) {
        return false;
      }

      const responseOutcome = shouldApplyMarkerMutationResponse(
        sessionId,
        response,
      );
      if (responseOutcome === "deferred") {
        return false;
      }
      if (responseOutcome === "apply") {
        updateSessionLocally(sessionId, (currentSession) =>
          upsertConversationMarkerLocally(
            currentSession,
            response.marker,
            response.sessionMutationStamp,
          ),
        );
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleUpdateConversationMarker(
    sessionId: string,
    markerId: string,
    payload: UpdateConversationMarkerRequest,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session?.markers?.some((marker) => marker.id === markerId)) {
      return false;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const response = await updateConversationMarker(
        sessionId,
        markerId,
        payload,
      );
      if (!isMountedRef.current) {
        return false;
      }

      const responseOutcome = shouldApplyMarkerMutationResponse(
        sessionId,
        response,
      );
      if (responseOutcome === "deferred") {
        return false;
      }
      if (responseOutcome === "apply") {
        updateSessionLocally(sessionId, (currentSession) =>
          upsertConversationMarkerLocally(
            currentSession,
            response.marker,
            response.sessionMutationStamp,
          ),
        );
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleDeleteConversationMarker(
    sessionId: string,
    markerId: string,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session?.markers?.some((marker) => marker.id === markerId)) {
      return false;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const response = await deleteConversationMarker(sessionId, markerId);
      if (!isMountedRef.current) {
        return false;
      }

      const responseOutcome = shouldApplyMarkerMutationResponse(
        sessionId,
        {
          revision: response.revision,
          serverInstanceId: response.serverInstanceId,
          markerId: response.markerId,
          sessionMutationStamp: response.sessionMutationStamp,
        },
        { deleted: true },
      );
      if (responseOutcome === "deferred") {
        return false;
      }
      if (responseOutcome === "apply") {
        updateSessionLocally(sessionId, (currentSession) =>
          deleteConversationMarkerLocally(
            currentSession,
            response.markerId,
            response.sessionMutationStamp,
          ),
        );
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  return {
    handleCreateConversationMarker,
    handleUpdateConversationMarker,
    handleDeleteConversationMarker,
  };
}
