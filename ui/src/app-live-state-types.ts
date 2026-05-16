// Owns: exported type contracts for the useAppLiveState hook.
// Does not own: live transport, adoption logic, retry scheduling, or hydration.
// Split from: ui/src/app-live-state.ts.

import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type {
  CreateSessionResponse,
  DelegationWaitRecord,
  StateResponse,
  WorkspaceLayoutSummary,
} from "./api";
import type {
  AgentReadiness,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CodexState,
  OrchestratorInstance,
  Project,
  RemoteConfig,
  Session,
  WorkspaceFilesChangedEvent,
} from "./types";
import type {
  DraftImageAttachment,
  SessionAgentCommandMap,
  SessionFlagMap,
} from "./app-utils";
import type { BackendConnectionState } from "./backend-connection";
import type {
  PendingSessionRename,
  SessionErrorMap,
  SessionNoticeMap,
} from "./app-shell-internals";
import type { WorkspaceState } from "./workspace";
import type { ControlPanelSide } from "./workspace-storage";

// Outcome of `adoptCreatedSessionResponse`:
//   - "adopted":    session inserted into `sessionsRef` and the
//                   workspace pane opened. Nothing for the caller
//                   to do.
//   - "stale":      revision gate rejected the write because a
//                   newer snapshot already landed. The session
//                   was NOT inserted here. Callers must verify
//                   `sessionsRef` already contains
//                   `created.sessionId` before opening a pane;
//                   unrelated state probes can advance the
//                   revision without ever inserting the created
//                   session locally.
//   - "recovering": protocol drift (`session.id !== sessionId`)
//                   or unknown cross-instance response. A resync was
//                   scheduled and the caller MUST NOT open a workspace
//                   pane for the response id. That id was never inserted
//                   into `sessionsRef`, so opening it would leave a
//                   phantom pane that persists until the resync reconciles.
//
// A plain boolean could not carry the "stale vs recovering"
// distinction, so the call-site fallback (`if (!adopted) { open
// workspace pane }`) opened a phantom on protocol mismatch.
export type AdoptCreatedSessionOutcome = "adopted" | "stale" | "recovering";

export type AdoptStateOptions = {
  force?: boolean;
  /** Allow adopting a snapshot with a lower revision than the current one.
   *  Only used for backend restart rollbacks where the revision counter resets. */
  allowRevisionDowngrade?: boolean;
  allowUnknownServerInstance?: boolean;
  disableMutationStampFastPath?: boolean;
  sseReconnectRequestId?: number | null;
  openSessionId?: string;
  paneId?: string | null;
};

export type AdoptSessionsOptions = {
  openSessionId?: string;
  paneId?: string | null;
  disableMutationStampFastPath?: boolean;
  forceMessagesUnloaded?: boolean;
};

export type SessionHydrationTarget = {
  id: string;
  messagesLoaded?: boolean | null;
};

export type UseAppLiveStateAdoptionRefs = {
  isMountedRef: MutableRefObject<boolean>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  seenServerInstanceIdsRef: MutableRefObject<Set<string>>;
  sessionsRef: MutableRefObject<Session[]>;
  draftsBySessionIdRef: MutableRefObject<Record<string, string>>;
  draftAttachmentsBySessionIdRef: MutableRefObject<
    Record<string, DraftImageAttachment[]>
  >;
  codexStateRef: MutableRefObject<CodexState>;
  agentReadinessRef: MutableRefObject<AgentReadiness[]>;
  projectsRef: MutableRefObject<Project[]>;
  orchestratorsRef: MutableRefObject<OrchestratorInstance[]>;
  delegationWaitsRef: MutableRefObject<DelegationWaitRecord[]>;
  workspaceSummariesRef: MutableRefObject<WorkspaceLayoutSummary[]>;
  refreshingAgentCommandSessionIdsRef: MutableRefObject<SessionFlagMap>;
  confirmedUnknownModelSendsRef: MutableRefObject<Set<string>>;
  activePromptPollCancelRef: MutableRefObject<(() => void) | null>;
  activePromptPollSessionIdRef: MutableRefObject<string | null>;
};

export type UseAppLiveStateStateSetters = {
  setSessions: Dispatch<SetStateAction<Session[]>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  setCodexState: Dispatch<SetStateAction<CodexState>>;
  setAgentReadiness: Dispatch<SetStateAction<AgentReadiness[]>>;
  setProjects: Dispatch<SetStateAction<Project[]>>;
  setOrchestrators: Dispatch<SetStateAction<OrchestratorInstance[]>>;
  setDelegationWaits: Dispatch<SetStateAction<DelegationWaitRecord[]>>;
  setWorkspaceSummaries: Dispatch<SetStateAction<WorkspaceLayoutSummary[]>>;
  setDraftsBySessionId: Dispatch<SetStateAction<Record<string, string>>>;
  setDraftAttachmentsBySessionId: Dispatch<
    SetStateAction<Record<string, DraftImageAttachment[]>>
  >;
  setSendingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setStoppingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setKillingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setKillRevealSessionId: Dispatch<SetStateAction<string | null>>;
  setPendingKillSessionId: Dispatch<SetStateAction<string | null>>;
  setPendingSessionRename: Dispatch<
    SetStateAction<PendingSessionRename | null>
  >;
  setUpdatingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setAgentCommandsBySessionId: Dispatch<SetStateAction<SessionAgentCommandMap>>;
  setRefreshingAgentCommandSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setAgentCommandErrors: Dispatch<SetStateAction<SessionErrorMap>>;
  setSessionSettingNotices: Dispatch<SetStateAction<SessionNoticeMap>>;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setIsLoading: Dispatch<SetStateAction<boolean>>;
  setBackendConnectionIssueDetail: Dispatch<SetStateAction<string | null>>;
  setBackendConnectionState: (next: BackendConnectionState) => void;
};

export type UseAppLiveStatePreferenceSetters = {
  setDefaultCodexModel: Dispatch<SetStateAction<string>>;
  setDefaultClaudeModel: Dispatch<SetStateAction<string>>;
  setDefaultCursorModel: Dispatch<SetStateAction<string>>;
  setDefaultGeminiModel: Dispatch<SetStateAction<string>>;
  setDefaultCodexReasoningEffort: Dispatch<
    SetStateAction<CodexReasoningEffort>
  >;
  setDefaultClaudeApprovalMode: Dispatch<SetStateAction<ClaudeApprovalMode>>;
  setDefaultClaudeEffort: Dispatch<SetStateAction<ClaudeEffortLevel>>;
  setRemoteConfigs: Dispatch<SetStateAction<RemoteConfig[]>>;
};

export type UseAppLiveStateParams = {
  adoptionRefs: UseAppLiveStateAdoptionRefs;
  stateSetters: UseAppLiveStateStateSetters;
  preferenceSetters: UseAppLiveStatePreferenceSetters;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: ControlPanelSide,
  ) => WorkspaceState;
  clearRecoveredBackendRequestError: () => void;
  reportRequestError: (error: unknown, options?: { message?: string }) => void;
  /**
   * Populated by the hook's transport useEffect on mount so the
   * App.tsx manual-retry button and browser-online handler (which
   * run outside the hook) can trigger an immediate /api/state
   * probe. The hook resets this to a no-op during cleanup so
   * stale calls after unmount do nothing. App.tsx owns the ref
   * identity because it is called from helpers declared before
   * the hook is invoked.
   */
  requestBackendReconnectRef: MutableRefObject<() => void>;
  /**
   * Populated by the hook's transport useEffect on mount so the
   * App.tsx `reportRequestError` path can probe /api/state for a
   * one-off backend-unavailable error without touching SSE-level
   * reconnect state. Reset to a no-op on cleanup.
   */
  requestActionRecoveryResyncRef: MutableRefObject<
    (options?: {
      openSessionId?: string;
      paneId?: string | null;
      allowUnknownServerInstance?: boolean;
      sseReconnectRequestId?: number;
    }) => void
  >;
  activeSession: Session | null;
  visibleSessionHydrationTargets: readonly SessionHydrationTarget[];
};

export type UseAppLiveStateReturn = {
  adoptState: (
    nextState: StateResponse,
    options?: AdoptStateOptions,
  ) => boolean;
  adoptCreatedSessionResponse: (
    created: CreateSessionResponse,
    options?: { openSessionId?: string; paneId?: string | null },
  ) => AdoptCreatedSessionOutcome;
  syncPreferencesFromState: (nextState: StateResponse) => void;
  // Action adoption clears the one-shot mismatch recovery gate after
  // authoritative adoption or stale-success, so a later mismatch can recover.
  clearHydrationMismatchSessionIds: (sessionIds: Iterable<string>) => void;
  hydratedSessionIdsRef: MutableRefObject<Set<string>>;
  hydratingSessionIdsRef: MutableRefObject<Set<string>>;
  forceAdoptNextStateEventRef: MutableRefObject<boolean>;
  /** See `forceSseReconnect` definition in the hook body for full semantics. */
  forceSseReconnect: () => number;
  workspaceFilesChangedEvent: WorkspaceFilesChangedEvent | null;
  workspaceFilesChangedEventBufferRef: MutableRefObject<WorkspaceFilesChangedEvent | null>;
  workspaceFilesChangedEventFlushTimeoutRef: MutableRefObject<number | null>;
  resetWorkspaceFilesChangedEventGate: () => void;
};
