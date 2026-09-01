// Owns the AgentSessionPanel prompt composer, slash palette, prompt history,
// draft sync, and delegation/send controls. Deliberately does not own the
// transcript body or footer wrapper; this was split out of
// `AgentSessionPanel.tsx` as a pure code move.

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  fetchCodexMcpServers,
  resolveAgentCommand,
  type ResolveAgentCommandResponse,
} from "../api";
import {
  defaultComposerDelegationMode,
  defaultComposerDelegationWritePolicy,
  supportsComposerReviewer,
  type ComposerDelegationMode,
} from "../delegation-commands";
import { CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES } from "./conversation-composer-focus";
import {
  isSpaceKey,
  spawnDelegationOptionsFromResolvedCommand,
  type SpawnDelegationOptions,
} from "./agent-session-panel-helpers";
import {
  formatAgentCommandResolverError,
  prepareAgentCommandSubmission,
  sendResolvedAgentCommandSubmission,
  shouldFocusDelegateWithSlashPaletteKey,
  shouldSubmitSlashPaletteKey,
} from "./session-agent-command-submission";
import {
  buildSlashPaletteState,
  parseAgentCommandDraft,
  supportsAgentSlashCommands,
  supportsLiveSessionModelOptions,
  type SlashPaletteItem,
} from "./session-slash-palette";
import { useComposerSessionSnapshot } from "../session-store";
import {
  SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT,
  sessionModelOptionsDeferredRetryDelay,
} from "../session-model-refresh-retry";
import { useComposerAutoResize } from "./useComposerAutoResize";
import {
  ComposerActionSplitButton,
  type ComposerActionMode,
  type ComposerActionOption,
} from "./composer-action-split-button";
import type {
  AgentCommandResolverErrorState,
  PromptHistoryState,
  SessionComposerProps,
} from "./AgentSessionPanel.types";
import type { CodexMcpServerStatus } from "../types";

const EMPTY_COMPOSER_ATTACHMENTS: readonly {
  byteSize: number;
  fileName: string;
  id: string;
  mediaType: string;
  previewUrl: string;
}[] = [];
const EMPTY_COMPOSER_PROMPT_HISTORY: readonly string[] = [];
const EMPTY_CODEX_MCP_SERVERS: readonly CodexMcpServerStatus[] = [];
const CODEX_MCP_SESSION_CACHE_LIMIT = 8;
type CodexMcpRequestState = {
  error: string | null;
  servers: readonly CodexMcpServerStatus[];
  status: "idle" | "loading" | "loaded" | "error";
};
const EMPTY_CODEX_MCP_REQUEST_STATE: CodexMcpRequestState = {
  error: null,
  servers: EMPTY_CODEX_MCP_SERVERS,
  status: "idle",
};

function formatCodexMcpAuthStatus(status: string): string {
  switch (status) {
    case "notLoggedIn":
      return "Not logged in";
    case "bearerToken":
      return "Bearer token";
    case "oAuth":
      return "OAuth";
    case "unsupported":
      return "Auth unsupported";
    default:
      return status || "Auth unknown";
  }
}

function formatCodexMcpLoadError(error: unknown): string {
  return error instanceof Error && error.message.trim()
    ? error.message
    : "Failed to load Codex MCP status.";
}

export const SessionComposer = memo(function SessionComposer({
  paneId,
  isPaneActive,
  sessionId,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  isUpdating,
  isRefreshingModelOptions,
  isEngramMcpRevocationPending,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  showNewResponseIndicator,
  newResponseIndicatorLabel,
  newResponseIndicatorQueuedCount,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onSend,
  canSpawnDelegation = false,
  onSpawnDelegation,
  onSessionSettingsChange,
  onStopSession,
  onPaste,
}: SessionComposerProps) {
  const {
    composerInputRef,
    resetAndCancelScheduledComposerResize,
    resetComposerSizingState,
    cancelAndRestoreScheduledComposerTransition,
    resizeComposerInput,
    scheduleComposerResize,
  } = useComposerAutoResize(sessionId);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const onDraftCommitRef = useRef(onDraftCommit);
  const requestedSlashModelOptionsRef = useRef<string | null>(null);
  const slashModelOptionsDeferredRetryTimerRef = useRef<
    ReturnType<typeof setTimeout> | null
  >(null);
  const slashModelOptionsDeferredRetryAttemptRef = useRef(0);
  const slashModelOptionsDeferredRetryKeyRef = useRef<string | null>(null);
  const previousSlashModelOptionsBlockedRef = useRef(
    isSessionBusy || isEngramMcpRevocationPending,
  );
  const requestedSlashAgentCommandsRef = useRef<string | null>(null);
  const codexMcpNextRequestGenerationRef = useRef(0);
  const codexMcpRequestGenerationRef = useRef<Record<string, number>>({});
  const codexMcpRequestInFlightRef = useRef<Set<string>>(new Set());
  const codexMcpSessionCacheOrderRef = useRef<string[]>([]);
  const slashOptionsRef = useRef<HTMLDivElement | null>(null);
  const composerPrimaryActionButtonRef = useRef<HTMLButtonElement | null>(null);
  const session = useComposerSessionSnapshot(sessionId);
  // This state is intentionally narrow: it exists so slash-palette rendering
  // has a reactive draft. Plain prompt text lives in the uncontrolled textarea;
  // read the current prompt through `getComposerDraftValue()`.
  const [currentLocalDraftState, setCurrentLocalDraftState] = useState<{
    draft: string;
    sessionId: string | null;
  }>(() => {
    const initialSessionId = session?.id ?? sessionId;
    if (!initialSessionId) {
      return { draft: "", sessionId: null };
    }

    const initialCommittedDraft = session?.committedDraft ?? "";
    const initialLocalDraft = localDraftsRef.current[initialSessionId];
    const initialDraft =
      initialLocalDraft !== undefined ? initialLocalDraft : initialCommittedDraft;
    return {
      draft: initialDraft,
      sessionId: initialSessionId,
    };
  });
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [slashModelOptionsRetryGeneration, setSlashModelOptionsRetryGeneration] =
    useState(0);
  const [slashNavModality, setSlashNavModality] = useState<"keyboard" | "mouse">("keyboard");
  const [isAgentCommandResolving, setIsAgentCommandResolving] = useState(false);
  const isAgentCommandResolvingRef = useRef(false);
  const [isDelegationSpawning, setIsDelegationSpawning] = useState(false);
  const [composerActionModeBySessionId, setComposerActionModeBySessionId] =
    useState<Record<string, ComposerActionMode | undefined>>({});
  const [agentCommandResolverError, setAgentCommandResolverError] =
    useState<AgentCommandResolverErrorState | null>(null);
  const [codexMcpRequestStateBySessionId, setCodexMcpRequestStateBySessionId] =
    useState<Record<string, CodexMcpRequestState | undefined>>({});
  const isMountedRef = useRef(true);
  const activeSessionIdRef = useRef<string | null>(null);
  const lastComposerDraftSyncPropSessionIdRef = useRef<string | null>(null);
  const lastComposerDraftSyncSessionIdRef = useRef<string | null>(null);

  // `activeSessionId` is a best-effort identity for draft bookkeeping while
  // the store snapshot catches up. Callers that need capability/session fields
  // must still check `session`.
  const activeSessionId = session?.id ?? sessionId;
  const activeCodexMcpSessionId =
    session?.agent === "Codex" ? session.id : null;
  const defaultComposerMode = session
    ? defaultComposerDelegationMode(session.agent)
    : "reviewer";
  const composerReviewerAvailable = session
    ? supportsComposerReviewer(session.agent)
    : false;
  const composerDelegationAvailable = Boolean(
    session && onSpawnDelegation && canSpawnDelegation,
  );
  const storedComposerActionMode = activeSessionId
    ? (composerActionModeBySessionId[activeSessionId] ?? "send")
    : "send";
  const composerActionMode: ComposerActionMode = !composerDelegationAvailable
    ? "send"
    : storedComposerActionMode === "reviewer" && !composerReviewerAvailable
      ? "explorer"
      : storedComposerActionMode;
  const composerDelegationMode: ComposerDelegationMode =
    composerActionMode === "send" ? defaultComposerMode : composerActionMode;
  const composerDelegationWritePolicy = session
    ? defaultComposerDelegationWritePolicy(session.agent)
    : { kind: "readOnly" as const };
  const composerDelegationButtonTitle =
    composerDelegationMode === "reviewer"
      ? "Spawn read-only reviewer delegation from current draft"
      : composerDelegationWritePolicy.kind === "isolatedWorktree"
        ? "Spawn isolated-worktree explorer delegation from current draft"
        : "Spawn read-only explorer delegation from current draft";
  useLayoutEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);
  useEffect(() => {
    // SessionComposer is memoized; explicitly drop resolver errors when the
    // active session identity changes even if the component instance is reused.
    setAgentCommandResolverError(null);
  }, [activeSessionId]);

  const committedDraft = session?.committedDraft ?? "";
  const draftAttachments = session?.draftAttachments ?? EMPTY_COMPOSER_ATTACHMENTS;
  const promptHistory = session?.promptHistory ?? EMPTY_COMPOSER_PROMPT_HISTORY;
  const composerDraft =
    currentLocalDraftState.sessionId === activeSessionId
      ? currentLocalDraftState.draft
      : "";
  const initialComposerDraft = activeSessionId
    ? (localDraftsRef.current[activeSessionId] ?? committedDraft)
    : "";
  const activeCodexMcpRequestState = activeSessionId
    ? (codexMcpRequestStateBySessionId[activeSessionId] ??
      EMPTY_CODEX_MCP_REQUEST_STATE)
    : EMPTY_CODEX_MCP_REQUEST_STATE;
  const slashPalette = useMemo(
    () =>
      buildSlashPaletteState(
        session,
        composerDraft,
        isRefreshingModelOptions,
        modelOptionsError,
        agentCommands,
        hasLoadedAgentCommands,
        isRefreshingAgentCommands,
        agentCommandsError,
        activeCodexMcpRequestState.servers,
        activeCodexMcpRequestState.status,
        activeCodexMcpRequestState.error,
      ),
    [
      agentCommands,
      agentCommandsError,
      activeCodexMcpRequestState.error,
      activeCodexMcpRequestState.servers,
      activeCodexMcpRequestState.status,
      composerDraft,
      hasLoadedAgentCommands,
      isRefreshingAgentCommands,
      isRefreshingModelOptions,
      modelOptionsError,
      session,
    ],
  );
  const slashPaletteResetKey = slashPalette.kind === "none" ? "none" : slashPalette.resetKey;
  const slashPaletteSupportsModelRefresh =
    slashPalette.kind === "choice" && slashPalette.supportsLiveRefresh;
  const slashPaletteRequiresModelRefresh =
    slashPalette.kind === "choice" && slashPalette.requiresLiveRefresh;
  const slashModelOptionsRequestKey = session
    ? `${session.id}:${session.model}:${
        isEngramMcpRevocationPending
          ? "pending"
          : isSessionBusy
            ? "busy"
            : "ready"
      }`
    : null;
  const slashPaletteSupportsAgentRefresh =
    slashPalette.kind === "command" && Boolean(slashPalette.supportsRefresh);
  const slashPaletteSupportsMcpRefresh =
    slashPalette.kind === "mcp" && slashPalette.supportsRefresh;
  const activeSlashItem =
    slashPalette.kind === "none" || slashPalette.items.length === 0
      ? null
      : (slashPalette.items[Math.min(slashActiveIndex, slashPalette.items.length - 1)] ?? null);
  const canDelegateActiveSlashCommand =
    slashPalette.kind !== "none" && activeSlashItem?.kind === "agent-command";
  const isEngramBootRecoveryPending = session?.engramBootRecoveryPending === true;
  const composerInputDisabled =
    !session ||
    isEngramBootRecoveryPending ||
    isStopping ||
    isAgentCommandResolving ||
    isDelegationSpawning;
  const composerSendDisabled =
    !session ||
    isEngramBootRecoveryPending ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    (slashPalette.kind !== "none" && slashPalette.items.length === 0);
  const composerDelegateDisabled =
    !session ||
    isEngramBootRecoveryPending ||
    !canSpawnDelegation ||
    !onSpawnDelegation ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    isDelegationSpawning ||
    (slashPalette.kind !== "none" && !canDelegateActiveSlashCommand);
  const composerActionMenuDisabled =
    !session ||
    isEngramBootRecoveryPending ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    isDelegationSpawning;
  const composerActionOptions: readonly ComposerActionOption[] = [
    { mode: "send", label: "Send" },
    {
      mode: "reviewer",
      label: composerReviewerAvailable
        ? "Delegate · Reviewer"
        : "Delegate · Reviewer — requires Claude or Codex",
      disabled: !composerReviewerAvailable,
    },
    { mode: "explorer", label: "Delegate · Explorer" },
  ];
  const composerPrimaryDisabled =
    composerActionMode === "send"
      ? composerSendDisabled
      : composerDelegateDisabled;
  const composerPrimaryLabel =
    composerActionMode === "send"
      ? isSending
        ? isSessionBusy
          ? "Queueing..."
          : "Sending..."
        : isSessionBusy
          ? "Queue"
          : "Send"
      : isDelegationSpawning
        ? "Delegating..."
        : composerActionMode === "reviewer"
          ? "Delegate · Reviewer"
          : "Delegate · Explorer";

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  function beginAgentCommandResolution() {
    if (isAgentCommandResolvingRef.current) {
      return false;
    }
    isAgentCommandResolvingRef.current = true;
    setAgentCommandResolverError(null);
    setIsAgentCommandResolving(true);
    return true;
  }

  function finishAgentCommandResolution() {
    isAgentCommandResolvingRef.current = false;
    if (isMountedRef.current) {
      setIsAgentCommandResolving(false);
    }
  }

  const retainCodexMcpSessionCacheEntry = useCallback((requestSessionId: string) => {
    const cacheOrder = codexMcpSessionCacheOrderRef.current.filter(
      (cachedSessionId) => cachedSessionId !== requestSessionId,
    );
    cacheOrder.push(requestSessionId);
    const evictedSessionIds = cacheOrder.splice(
      0,
      Math.max(0, cacheOrder.length - CODEX_MCP_SESSION_CACHE_LIMIT),
    );
    codexMcpSessionCacheOrderRef.current = cacheOrder;
    if (evictedSessionIds.length === 0) {
      return;
    }

    // Evict completed bookkeeping together with display data, but retain
    // ownership of pending requests until they settle. Otherwise a quick
    // revisit can launch a duplicate fetch while the original request is
    // still in flight. Late results below are admitted only when the session
    // has re-entered the bounded cache.
    for (const evictedSessionId of evictedSessionIds) {
      if (!codexMcpRequestInFlightRef.current.has(evictedSessionId)) {
        delete codexMcpRequestGenerationRef.current[evictedSessionId];
      }
    }
    setCodexMcpRequestStateBySessionId((current) => {
      const next = { ...current };
      let changed = false;
      for (const evictedSessionId of evictedSessionIds) {
        if (next[evictedSessionId] !== undefined) {
          delete next[evictedSessionId];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, []);

  const requestCodexMcpServers = useCallback(
    async (force = false) => {
      if (!activeCodexMcpSessionId) {
        return;
      }

      const requestSessionId = activeCodexMcpSessionId;
      retainCodexMcpSessionCacheEntry(requestSessionId);
      if (!force && codexMcpRequestInFlightRef.current.has(requestSessionId)) {
        return;
      }

      const generation = codexMcpNextRequestGenerationRef.current + 1;
      codexMcpNextRequestGenerationRef.current = generation;
      codexMcpRequestGenerationRef.current[requestSessionId] = generation;
      codexMcpRequestInFlightRef.current.add(requestSessionId);
      setCodexMcpRequestStateBySessionId((current) => ({
        ...current,
        [requestSessionId]: {
          error: null,
          servers:
            current[requestSessionId]?.servers ?? EMPTY_CODEX_MCP_SERVERS,
          status: "loading",
        },
      }));

      try {
        const response = await fetchCodexMcpServers(requestSessionId);
        if (
          !isMountedRef.current ||
          codexMcpRequestGenerationRef.current[requestSessionId] !== generation ||
          !codexMcpSessionCacheOrderRef.current.includes(requestSessionId)
        ) {
          return;
        }
        // Cache a completed response under the session that requested it even
        // if another tab became active while the app-server was replying. The
        // derived palette state is session-keyed, so this cannot leak across
        // tabs; discarding it would leave the original tab stuck in `loading`
        // when the user returns. A newer request still wins via `generation`.
        setCodexMcpRequestStateBySessionId((current) => ({
          ...current,
          [requestSessionId]: {
            error: null,
            servers: response.servers,
            status: "loaded",
          },
        }));
      } catch (error) {
        if (
          !isMountedRef.current ||
          codexMcpRequestGenerationRef.current[requestSessionId] !== generation ||
          !codexMcpSessionCacheOrderRef.current.includes(requestSessionId)
        ) {
          return;
        }
        setCodexMcpRequestStateBySessionId((current) => ({
          ...current,
          [requestSessionId]: {
            error: formatCodexMcpLoadError(error),
            // A manual refresh is an update of known status, not a destructive
            // cache invalidation. Keep the last good inventory visible beside
            // the transient error so the user does not lose useful context.
            servers:
              current[requestSessionId]?.servers ?? EMPTY_CODEX_MCP_SERVERS,
            status: "error",
          },
        }));
      } finally {
        if (
          codexMcpRequestGenerationRef.current[requestSessionId] === generation
        ) {
          codexMcpRequestInFlightRef.current.delete(requestSessionId);
          if (!codexMcpSessionCacheOrderRef.current.includes(requestSessionId)) {
            delete codexMcpRequestGenerationRef.current[requestSessionId];
          }
        }
      }
    },
    [activeCodexMcpSessionId, retainCodexMcpSessionCacheEntry],
  );

  useEffect(() => {
    onDraftCommitRef.current = onDraftCommit;
  }, [onDraftCommit]);

  useEffect(() => {
    setSlashActiveIndex(slashPalette.kind === "none" ? 0 : slashPalette.defaultActiveIndex);
  }, [activeSessionId, slashPaletteResetKey]);

  useEffect(() => {
    requestedSlashModelOptionsRef.current = null;
    slashModelOptionsDeferredRetryAttemptRef.current = 0;
    slashModelOptionsDeferredRetryKeyRef.current = null;
    previousSlashModelOptionsBlockedRef.current = false;
    if (slashModelOptionsDeferredRetryTimerRef.current !== null) {
      clearTimeout(slashModelOptionsDeferredRetryTimerRef.current);
      slashModelOptionsDeferredRetryTimerRef.current = null;
    }
    return () => {
      if (slashModelOptionsDeferredRetryTimerRef.current !== null) {
        clearTimeout(slashModelOptionsDeferredRetryTimerRef.current);
        slashModelOptionsDeferredRetryTimerRef.current = null;
      }
    };
  }, [activeSessionId]);

  useEffect(() => {
    const blocked = isSessionBusy || isEngramMcpRevocationPending;
    const wasBlocked = previousSlashModelOptionsBlockedRef.current;
    previousSlashModelOptionsBlockedRef.current = blocked;
    if (blocked || !wasBlocked) {
      return;
    }
    requestedSlashModelOptionsRef.current = null;
    slashModelOptionsDeferredRetryAttemptRef.current = 0;
    slashModelOptionsDeferredRetryKeyRef.current = null;
    if (slashModelOptionsDeferredRetryTimerRef.current !== null) {
      clearTimeout(slashModelOptionsDeferredRetryTimerRef.current);
      slashModelOptionsDeferredRetryTimerRef.current = null;
    }
  }, [isEngramMcpRevocationPending, isSessionBusy]);

  useEffect(() => {
    if (
      !session ||
      isSessionBusy ||
      isEngramMcpRevocationPending ||
      slashPalette.kind !== "choice" ||
      !slashPaletteSupportsModelRefresh ||
      !supportsLiveSessionModelOptions(session)
    ) {
      return;
    }

    // `/fast` needs an authoritative entry for the active model, not merely a
    // non-empty catalog. Record only actual refresh attempts in the dedupe ref:
    // opening `/model` on an already loaded catalog must neither consume the
    // `/fast` refresh nor replace its key and make palette toggles refetch.
    if (session.modelOptions?.length && !slashPaletteRequiresModelRefresh) {
      return;
    }

    if (
      isRefreshingModelOptions ||
      requestedSlashModelOptionsRef.current === slashModelOptionsRequestKey
    ) {
      return;
    }

    requestSlashModelOptions();
  }, [
    isRefreshingModelOptions,
    onRefreshSessionModelOptions,
    session,
    slashPalette.kind,
    slashPaletteRequiresModelRefresh,
    slashPaletteSupportsModelRefresh,
    slashModelOptionsRequestKey,
    slashModelOptionsRetryGeneration,
  ]);

  useEffect(() => {
    if (
      !activeCodexMcpSessionId ||
      !slashPaletteSupportsMcpRefresh ||
      activeCodexMcpRequestState.status !== "idle"
    ) {
      return;
    }

    void requestCodexMcpServers();
  }, [
    activeCodexMcpSessionId,
    activeCodexMcpRequestState.status,
    requestCodexMcpServers,
    slashPaletteSupportsMcpRefresh,
  ]);

  useEffect(() => {
    if (slashPalette.kind === "none") {
      return;
    }

    const container = slashOptionsRef.current;
    if (!container) {
      return;
    }

    const activeOption = container.querySelector<HTMLButtonElement>(
      '.composer-slash-option.active[role="option"]',
    );
    if (!activeOption) {
      return;
    }

    const containerRect = container.getBoundingClientRect();
    const optionRect = activeOption.getBoundingClientRect();

    if (optionRect.top < containerRect.top) {
      container.scrollTop += optionRect.top - containerRect.top;
    } else if (optionRect.bottom > containerRect.bottom) {
      container.scrollTop += optionRect.bottom - containerRect.bottom;
    }
  }, [slashPalette.kind, slashPaletteResetKey, slashActiveIndex]);

  useEffect(() => {
    if (
      !session ||
      slashPalette.kind !== "command" ||
      !slashPaletteSupportsAgentRefresh ||
      !supportsAgentSlashCommands(session)
    ) {
      return;
    }

    const requestKey = `${session.id}:${session.workdir}:${session.agentCommandsRevision ?? 0}`;
    const requestKeyBase = `${session.id}:${session.workdir}:`;
    const alreadyRequested = requestedSlashAgentCommandsRef.current === requestKey;
    const isSameSessionRequest =
      requestedSlashAgentCommandsRef.current?.startsWith(requestKeyBase) ?? false;
    if (hasLoadedAgentCommands && !alreadyRequested && !isSameSessionRequest) {
      requestedSlashAgentCommandsRef.current = requestKey;
      return;
    }
    if (
      (hasLoadedAgentCommands && alreadyRequested) ||
      isRefreshingAgentCommands ||
      (agentCommandsError && alreadyRequested)
    ) {
      return;
    }

    requestSlashAgentCommands();
  }, [
    agentCommandsError,
    hasLoadedAgentCommands,
    isRefreshingAgentCommands,
    onRefreshAgentCommands,
    session,
    slashPalette.kind,
    slashPaletteSupportsAgentRefresh,
  ]);

  useLayoutEffect(() => {
    if (!activeSessionId) {
      if (composerInputRef.current && composerInputRef.current.value !== "") {
        composerInputRef.current.value = "";
      }
      setCurrentLocalDraftState((current) =>
        current.sessionId === null && current.draft === ""
          ? current
          : { draft: "", sessionId: null },
      );
      scheduleComposerResize(true);
      return;
    }

    const previousDraftSyncPropSessionId =
      lastComposerDraftSyncPropSessionIdRef.current;
    const isPropSessionSwitch = previousDraftSyncPropSessionId !== sessionId;
    lastComposerDraftSyncPropSessionIdRef.current = sessionId;
    const previousDraftSyncSessionId = lastComposerDraftSyncSessionIdRef.current;
    const isSessionSwitch = previousDraftSyncSessionId !== activeSessionId;
    lastComposerDraftSyncSessionIdRef.current = activeSessionId;
    const previousCommitted = committedDraftsRef.current[activeSessionId];
    const localDraft = localDraftsRef.current[activeSessionId];

    committedDraftsRef.current[activeSessionId] = committedDraft;

    const nextDraft =
      localDraft !== undefined && localDraft !== previousCommitted
        ? localDraft
        : committedDraft;
    const textarea = composerInputRef.current;
    const didUpdateDomValue = Boolean(textarea && textarea.value !== nextDraft);
    if (didUpdateDomValue && textarea) {
      textarea.value = nextDraft;
    }
    setCurrentLocalDraftState((current) =>
      (!nextDraft.startsWith("/") &&
        current.sessionId === null &&
        current.draft === "") ||
      (current.sessionId === activeSessionId && current.draft === nextDraft)
        ? current
        : nextDraft.startsWith("/")
          ? {
              draft: nextDraft,
              sessionId: activeSessionId,
            }
          : { draft: "", sessionId: null },
    );
    if (
      didUpdateDomValue &&
      !isSessionSwitch &&
      !isPropSessionSwitch &&
      previousCommitted !== undefined
    ) {
      resizeComposerInput(true);
    }
  }, [activeSessionId, committedDraft]);

  useLayoutEffect(() => {
    resetComposerSizingState();
    resetAndCancelScheduledComposerResize();
    cancelAndRestoreScheduledComposerTransition();
    resizeComposerInput(true);

    return () => {
      resetAndCancelScheduledComposerResize();
      cancelAndRestoreScheduledComposerTransition();
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    return () => {
      const latestDraft = localDraftsRef.current[activeSessionId];
      const committed = committedDraftsRef.current[activeSessionId] ?? "";
      if (latestDraft !== undefined && latestDraft !== committed) {
        committedDraftsRef.current[activeSessionId] = latestDraft;
        onDraftCommitRef.current(activeSessionId, latestDraft);
      }
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId || !isPaneActive || composerInputDisabled) {
      return;
    }

    focusComposerInput();
  }, [activeSessionId, composerInputDisabled, isPaneActive]);

  function resetPromptHistory(sessionId: string) {
    setPromptHistoryStateBySessionId((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });
  }

  function updateLocalDraft(
    sessionId: string,
    nextValue: string,
    options: { animateHeight?: boolean } = {},
  ) {
    localDraftsRef.current[sessionId] = nextValue;
    if (sessionId === activeSessionId) {
      if (composerInputRef.current && composerInputRef.current.value !== nextValue) {
        composerInputRef.current.value = nextValue;
      }
      setCurrentLocalDraftState((current) =>
        (!nextValue.startsWith("/") &&
          current.sessionId === null &&
          current.draft === "") ||
        (current.sessionId === sessionId && current.draft === nextValue)
          ? current
          : nextValue.startsWith("/")
            ? {
                draft: nextValue,
                sessionId,
              }
            : { draft: "", sessionId: null },
      );
      scheduleComposerResize(false, options.animateHeight ?? true);
    }
  }

  function commitDraft(sessionId: string, nextValue: string) {
    committedDraftsRef.current[sessionId] = nextValue;
    onDraftCommit(sessionId, nextValue);
  }

  function getComposerDraftValue() {
    return composerInputRef.current?.value ?? composerDraft;
  }

  function focusComposerInput(selectionStart?: number) {
    window.requestAnimationFrame(() => {
      const textarea = composerInputRef.current;
      if (!textarea) {
        return;
      }

      const nextSelectionStart = selectionStart ?? textarea.value.length;
      textarea.focus();
      textarea.setSelectionRange(nextSelectionStart, nextSelectionStart);
    });
  }

  function requestSlashModelOptions(force = false) {
    if (
      !session ||
      isSessionBusy ||
      isEngramMcpRevocationPending ||
      !slashModelOptionsRequestKey ||
      !supportsLiveSessionModelOptions(session)
    ) {
      return;
    }

    if (slashModelOptionsDeferredRetryKeyRef.current !== slashModelOptionsRequestKey) {
      slashModelOptionsDeferredRetryKeyRef.current = slashModelOptionsRequestKey;
      slashModelOptionsDeferredRetryAttemptRef.current = 0;
    }

    if (
      !force &&
      (requestedSlashModelOptionsRef.current === slashModelOptionsRequestKey ||
        slashModelOptionsDeferredRetryAttemptRef.current >=
          SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT)
    ) {
      return;
    }

    const requestKey = slashModelOptionsRequestKey;
    const requestSessionId = session.id;
    if (force) {
      slashModelOptionsDeferredRetryAttemptRef.current = 0;
    }
    requestedSlashModelOptionsRef.current = requestKey;
    void Promise.resolve(onRefreshSessionModelOptions(requestSessionId))
      .then((outcome) => {
        if (
          !isMountedRef.current ||
          activeSessionIdRef.current !== requestSessionId ||
          requestedSlashModelOptionsRef.current !== requestKey
        ) {
          return;
        }
        if (outcome !== "deferred") {
          slashModelOptionsDeferredRetryAttemptRef.current = 0;
          return;
        }
        if (
          slashModelOptionsDeferredRetryAttemptRef.current >=
          SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT
        ) {
          return;
        }
        const retryAttempt = slashModelOptionsDeferredRetryAttemptRef.current;
        slashModelOptionsDeferredRetryAttemptRef.current += 1;
        if (slashModelOptionsDeferredRetryTimerRef.current !== null) {
          clearTimeout(slashModelOptionsDeferredRetryTimerRef.current);
        }
        slashModelOptionsDeferredRetryTimerRef.current = setTimeout(() => {
          slashModelOptionsDeferredRetryTimerRef.current = null;
          if (
            !isMountedRef.current ||
            activeSessionIdRef.current !== requestSessionId ||
            slashModelOptionsDeferredRetryKeyRef.current !== requestKey ||
            requestedSlashModelOptionsRef.current !== requestKey
          ) {
            return;
          }
          requestedSlashModelOptionsRef.current = null;
          setSlashModelOptionsRetryGeneration((current) => current + 1);
        }, sessionModelOptionsDeferredRetryDelay(retryAttempt));
      })
      .catch(() => {
        slashModelOptionsDeferredRetryAttemptRef.current = 0;
      });
  }

  function requestSlashAgentCommands(force = false) {
    if (!session || !supportsAgentSlashCommands(session)) {
      return;
    }

    const requestKey = `${session.id}:${session.workdir}:${session.agentCommandsRevision ?? 0}`;
    if (!force && requestedSlashAgentCommandsRef.current === requestKey) {
      return;
    }

    requestedSlashAgentCommandsRef.current = requestKey;
    void onRefreshAgentCommands(session.id);
  }

  function handleComposerChange(nextValue: string) {
    if (!activeSessionId) {
      return;
    }

    resetPromptHistory(activeSessionId);
    setAgentCommandResolverError(null);
    updateLocalDraft(activeSessionId, nextValue);
  }

  function handleComposerBlur() {
    if (!activeSessionId) {
      return;
    }

    commitDraft(activeSessionId, getComposerDraftValue());
  }

  async function applySlashPaletteItem(
    item: SlashPaletteItem,
    keepPaletteOpen = false,
  ) {
    if (
      !activeSessionId ||
      !session ||
      isSending ||
      isStopping ||
      isAgentCommandResolvingRef.current
    ) {
      return;
    }

    if (item.kind === "command") {
      resetPromptHistory(activeSessionId);
      const nextDraft = `${item.command} `;
      setAgentCommandResolverError(null);
      updateLocalDraft(activeSessionId, nextDraft);
      focusComposerInput(nextDraft.length);
      return;
    }

    if (item.kind === "agent-command") {
      if (isUpdating) {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }

      const resolution = prepareAgentCommandSubmission(
        item,
        getComposerDraftValue(),
      );
      if (resolution.kind === "expand") {
        resetPromptHistory(activeSessionId);
        setAgentCommandResolverError(null);
        updateLocalDraft(activeSessionId, resolution.nextDraft);
        focusComposerInput(resolution.nextDraft.length);
        return;
      }

      const requestSessionId = activeSessionId;
      let resolved: ResolveAgentCommandResponse;
      if (!beginAgentCommandResolution()) {
        return;
      }
      try {
        resolved = await resolveAgentCommand(
          requestSessionId,
          resolution.commandName,
          {
            arguments: resolution.argumentsText,
            ...(resolution.noteText ? { note: resolution.noteText } : {}),
            intent: "send",
          },
        );
      } catch (error) {
        if (isMountedRef.current && activeSessionIdRef.current === requestSessionId) {
          setAgentCommandResolverError({
            message: formatAgentCommandResolverError(error),
            sessionId: requestSessionId,
          });
          focusComposerInput();
        }
        return;
      } finally {
        finishAgentCommandResolution();
      }

      if (!isMountedRef.current || activeSessionIdRef.current !== requestSessionId) {
        return;
      }

      const accepted = sendResolvedAgentCommandSubmission(
        onSend,
        requestSessionId,
        resolved,
      );
      if (!accepted) {
        focusComposerInput();
        return;
      }

      resetPromptHistory(requestSessionId);
      updateLocalDraft(requestSessionId, "", { animateHeight: false });
      commitDraft(requestSessionId, "");
      focusComposerInput();
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    resetPromptHistory(activeSessionId);
    void onSessionSettingsChange(activeSessionId, item.field, item.value);
    if (keepPaletteOpen) {
      focusComposerInput(getComposerDraftValue().length);
    } else {
      updateLocalDraft(activeSessionId, "");
      commitDraft(activeSessionId, "");
      focusComposerInput(0);
    }
  }

  async function handleComposerSend() {
    if (
      !activeSessionId ||
      isSending ||
      isStopping ||
      isAgentCommandResolvingRef.current
    ) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (activeSlashItem) {
        if (activeSlashItem.kind === "choice" && isUpdating) {
          focusComposerInput(getComposerDraftValue().length);
          return;
        }
        await applySlashPaletteItem(activeSlashItem);
      }
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    const draftToSend = getComposerDraftValue();
    const accepted = onSend(activeSessionId, draftToSend);
    if (!accepted) {
      focusComposerInput();
      return;
    }

    resetPromptHistory(activeSessionId);
    updateLocalDraft(activeSessionId, "", { animateHeight: false });
    commitDraft(activeSessionId, "");
    focusComposerInput();
  }

  async function handleComposerDelegationSpawn(
    delegationMode: ComposerDelegationMode = composerDelegationMode,
  ) {
    if (composerDelegateDisabled || !activeSessionId || !onSpawnDelegation) {
      focusComposerInput();
      return;
    }

    const requestSessionId = activeSessionId;
    let prompt: string;
    let delegationOptions: SpawnDelegationOptions | undefined;
    if (slashPalette.kind !== "none") {
      if (activeSlashItem?.kind !== "agent-command") {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }
      const resolution = prepareAgentCommandSubmission(
        activeSlashItem,
        getComposerDraftValue(),
      );
      if (resolution.kind === "expand") {
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, resolution.nextDraft);
        focusComposerInput(resolution.nextDraft.length);
        return;
      }
      let resolved: ResolveAgentCommandResponse;
      if (!beginAgentCommandResolution()) {
        focusComposerInput();
        return;
      }
      try {
        resolved = await resolveAgentCommand(
          requestSessionId,
          resolution.commandName,
          {
            arguments: resolution.argumentsText,
            ...(resolution.noteText ? { note: resolution.noteText } : {}),
            intent: "delegate",
          },
        );
      } catch (error) {
        if (isMountedRef.current && activeSessionIdRef.current === requestSessionId) {
          setAgentCommandResolverError({
            message: formatAgentCommandResolverError(error),
            sessionId: requestSessionId,
          });
          focusComposerInput();
        }
        return;
      } finally {
        finishAgentCommandResolution();
      }
      if (!isMountedRef.current || activeSessionIdRef.current !== requestSessionId) {
        return;
      }
      prompt = (resolved.expandedPrompt ?? resolved.visiblePrompt).trim();
      delegationOptions = spawnDelegationOptionsFromResolvedCommand(resolved);
    } else {
      prompt = getComposerDraftValue().trim();
      if (delegationMode !== defaultComposerMode) {
        delegationOptions = { mode: delegationMode };
      }
    }
    if (!prompt) {
      focusComposerInput();
      return;
    }

    setIsDelegationSpawning(true);
    let accepted = false;
    try {
      accepted = delegationOptions
        ? await onSpawnDelegation(requestSessionId, prompt, delegationOptions)
        : await onSpawnDelegation(requestSessionId, prompt);
    } catch {
      accepted = false;
    } finally {
      if (isMountedRef.current) {
        setIsDelegationSpawning(false);
      }
    }

    if (!isMountedRef.current) {
      return;
    }

    if (!accepted) {
      if (activeSessionIdRef.current !== requestSessionId) {
        return;
      }
      focusComposerInput();
      return;
    }

    if (activeSessionIdRef.current !== requestSessionId) {
      return;
    }

    resetPromptHistory(requestSessionId);
    updateLocalDraft(requestSessionId, "", { animateHeight: false });
    commitDraft(requestSessionId, "");
    focusComposerInput();
  }

  function handleComposerPrimaryAction() {
    if (composerActionMode === "send") {
      void handleComposerSend();
      return;
    }
    void handleComposerDelegationSpawn(composerActionMode);
  }

  function handleComposerKeyDown(event: ReactKeyboardEvent<HTMLTextAreaElement>) {
    if (!activeSessionId) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (event.key === "Escape") {
        event.preventDefault();
        resetPromptHistory(activeSessionId);
        setAgentCommandResolverError(null);
        updateLocalDraft(activeSessionId, "");
        commitDraft(activeSessionId, "");
        return;
      }

      if (
        shouldFocusDelegateWithSlashPaletteKey(
          event,
          canDelegateActiveSlashCommand,
          canSpawnDelegation,
          Boolean(onSpawnDelegation),
          composerDelegateDisabled,
        )
      ) {
        event.preventDefault();
        composerPrimaryActionButtonRef.current?.focus();
        return;
      }

      if (
        shouldSubmitSlashPaletteKey(
          event,
          canDelegateActiveSlashCommand,
          canSpawnDelegation,
          Boolean(onSpawnDelegation),
          composerDelegateDisabled,
        )
      ) {
        event.preventDefault();
        void handleComposerSend();
        return;
      }

      if (
        isSpaceKey(event) &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey
      ) {
        if (activeSlashItem) {
          if (activeSlashItem.kind === "choice") {
            event.preventDefault();
            void applySlashPaletteItem(activeSlashItem, true);
          } else if (activeSlashItem.kind === "command") {
            event.preventDefault();
            void applySlashPaletteItem(activeSlashItem);
          } else {
            const parsedDraft = parseAgentCommandDraft(getComposerDraftValue());
            const matchesSelectedCommand =
              parsedDraft?.commandName.toLowerCase() ===
              activeSlashItem.name.toLowerCase();
            if (!matchesSelectedCommand) {
              event.preventDefault();
              resetPromptHistory(activeSessionId);
              const nextDraft = `/${activeSlashItem.name} `;
              setAgentCommandResolverError(null);
              updateLocalDraft(activeSessionId, nextDraft);
              focusComposerInput(nextDraft.length);
            }
          }
        }
        return;
      }

      if (
        (event.key === "ArrowUp" || event.key === "ArrowDown") &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey
      ) {
        event.preventDefault();
        setSlashNavModality("keyboard");
        if (slashPalette.items.length === 0) {
          return;
        }

        setSlashActiveIndex((current) => {
          if (event.key === "ArrowUp") {
            return current <= 0 ? slashPalette.items.length - 1 : current - 1;
          }

          return current >= slashPalette.items.length - 1 ? 0 : current + 1;
        });
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void handleComposerSend();
      return;
    }

    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") {
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return;
    }

    const textarea = event.currentTarget;
    if (textarea.selectionStart !== 0 || textarea.selectionEnd !== 0) {
      return;
    }

    if (promptHistory.length === 0) {
      return;
    }

    const historyState = promptHistoryStateBySessionId[activeSessionId];
    if (event.key === "ArrowDown" && !historyState) {
      return;
    }

    event.preventDefault();

    if (event.key === "ArrowUp") {
      const nextIndex = historyState
        ? Math.max(historyState.index - 1, 0)
        : promptHistory.length - 1;
      const draftSnapshot = historyState?.draft ?? getComposerDraftValue();

      setPromptHistoryStateBySessionId((current) => ({
        ...current,
        [activeSessionId]: {
          index: nextIndex,
          draft: draftSnapshot,
        },
      }));
      updateLocalDraft(activeSessionId, promptHistory[nextIndex]);
    } else {
      const currentHistoryState = historyState;
      if (!currentHistoryState) {
        return;
      }

      if (currentHistoryState.index >= promptHistory.length - 1) {
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, currentHistoryState.draft);
      } else {
        const nextIndex = currentHistoryState.index + 1;
        setPromptHistoryStateBySessionId((current) => ({
          ...current,
          [activeSessionId]: {
            index: nextIndex,
            draft: currentHistoryState.draft,
          },
        }));
        updateLocalDraft(activeSessionId, promptHistory[nextIndex]);
      }
    }

    window.requestAnimationFrame(() => {
      textarea.setSelectionRange(0, 0);
    });
  }

  const slashPaletteErrorMessage =
    slashPalette.kind === "none"
      ? null
      : slashPalette.kind !== "mcp" &&
          agentCommandResolverError?.sessionId === activeSessionId
        ? agentCommandResolverError.message
        : (slashPalette.errorMessage ?? null);
  const slashPaletteIsRefreshing =
    slashPalette.kind === "none" ? false : Boolean(slashPalette.isRefreshing);
  const slashPaletteRefreshActionLabel =
    slashPalette.kind === "none" ? null : (slashPalette.refreshActionLabel ?? null);
  const slashPaletteSupportsRefresh =
    slashPalette.kind === "choice"
      ? slashPalette.supportsLiveRefresh
      : slashPalette.kind === "command"
        ? Boolean(slashPalette.supportsRefresh)
        : slashPalette.kind === "mcp"
          ? slashPalette.supportsRefresh
          : false;
  const slashPaletteStatusText =
    slashPalette.kind === "command" || slashPalette.kind === "mcp"
      ? (slashPalette.statusText ?? null)
      : null;
  const slashPaletteHintId = `composer-slash-hint-${paneId}`;
  const keyboardDelegationHint =
    "Tab moves focus to the composer action; use Arrow Down to choose delegation.";
  const slashPaletteHint =
    slashPalette.kind !== "none" &&
    canDelegateActiveSlashCommand &&
    !composerDelegateDisabled
      ? [slashPalette.hint, keyboardDelegationHint].filter(Boolean).join(" ")
      : slashPalette.kind !== "none"
        ? slashPalette.hint
        : null;
  const showSlashPaletteStatus =
    slashPalette.kind !== "none" &&
    (
      slashPaletteSupportsRefresh ||
      Boolean(slashPaletteErrorMessage) ||
      Boolean(slashPaletteStatusText) ||
      (slashPalette.kind === "choice" && isUpdating)
    );

  return (
    <footer className="composer">
      {showNewResponseIndicator ? (
        <button className="new-response-indicator" type="button" onClick={onScrollToLatest}>
          {newResponseIndicatorLabel}
          {newResponseIndicatorQueuedCount > 0 ? (
            <span className="new-response-indicator-queued-count">
              {newResponseIndicatorQueuedCount} queued
            </span>
          ) : null}
        </button>
      ) : null}
      {draftAttachments.length > 0 ? (
        <div className="composer-attachments" aria-label="Draft image attachments">
          {draftAttachments.map((attachment) => (
            <article key={attachment.id} className="composer-attachment-card">
              <img
                className="composer-attachment-preview"
                src={attachment.previewUrl}
                alt={attachment.fileName}
              />
              <div className="composer-attachment-copy">
                <strong className="composer-attachment-name">{attachment.fileName}</strong>
                <span className="composer-attachment-meta">
                  {formatByteSize(attachment.byteSize)} | {attachment.mediaType}
                </span>
              </div>
              <button
                className="composer-attachment-remove"
                type="button"
                onClick={() => activeSessionId && onDraftAttachmentRemove(activeSessionId, attachment.id)}
                aria-label={`Remove ${attachment.fileName}`}
                disabled={composerInputDisabled}
              >
                Remove
              </button>
            </article>
          ))}
        </div>
      ) : null}
      <div className="composer-row">
        <textarea
          id={`prompt-${paneId}`}
          ref={composerInputRef}
          className="composer-input"
          {...CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES}
          aria-label={session ? `Message ${session.name}` : "Message session"}
          aria-describedby={slashPaletteHint ? slashPaletteHintId : undefined}
          defaultValue={initialComposerDraft}
          onChange={(event) => handleComposerChange(event.target.value)}
          onBlur={handleComposerBlur}
          disabled={composerInputDisabled}
          onKeyDown={handleComposerKeyDown}
          onPaste={onPaste}
          placeholder={
            isEngramBootRecoveryPending
              ? "Resuming Engram recovery after restart..."
              : session
                ? `Send a prompt to ${session.agent}...`
                : "Open a session..."
          }
          rows={1}
        />
        <div className="composer-actions">
          {session && (isSessionBusy || isStopping) ? (
            <button
              className="ghost-button composer-stop-button"
              type="button"
              onClick={() => activeSessionId && onStopSession(activeSessionId)}
              disabled={isStopping}
            >
              {isStopping ? "Stopping..." : "Stop"}
            </button>
          ) : null}
          {composerDelegationAvailable ? (
            <ComposerActionSplitButton
              ref={composerPrimaryActionButtonRef}
              actionLabel={composerPrimaryLabel}
              actionTitle={
                composerActionMode === "send"
                  ? undefined
                  : composerDelegationButtonTitle
              }
              disabled={composerPrimaryDisabled}
              menuDisabled={composerActionMenuDisabled}
              onAction={handleComposerPrimaryAction}
              onModeChange={(mode) => {
                if (!activeSessionId) {
                  return;
                }
                setComposerActionModeBySessionId((current) => ({
                  ...current,
                  [activeSessionId]: mode,
                }));
              }}
              options={composerActionOptions}
              primaryClassName={
                composerActionMode === "send" && isSessionBusy
                  ? "composer-queue-button"
                  : undefined
              }
              selectedMode={composerActionMode}
            />
          ) : (
            <button
              ref={composerPrimaryActionButtonRef}
              className={`send-button${isSessionBusy ? " composer-queue-button" : ""}`}
              type="button"
              onMouseDown={(event) => {
                event.preventDefault();
              }}
              onClick={() => void handleComposerSend()}
              disabled={composerSendDisabled}
            >
              {composerPrimaryLabel}
            </button>
          )}
        </div>
      </div>
      {session ? (
        <p className="composer-hint">
          {isEngramBootRecoveryPending
            ? "Opening this session resumes Engram recovery. Prompts will be available when its background retry finishes."
            : "Paste PNG, JPEG, GIF, or WebP images into the prompt. Drag-and-drop is not supported yet."}
          {!isEngramBootRecoveryPending && composerActionMode !== "send"
            ? ` Enter still sends normally; click ${composerPrimaryLabel} to delegate.`
            : null}
        </p>
      ) : null}
      {session && slashPalette.kind !== "none" ? (
        <div
          className="composer-slash-menu"
          role={slashPalette.kind === "mcp" ? "region" : "listbox"}
          aria-label={slashPalette.title}
        >
          <div className="composer-slash-header">
            <strong className="composer-slash-title">{slashPalette.title}</strong>
            <span id={slashPaletteHintId} className="composer-slash-hint">
              {slashPaletteHint}
            </span>
          </div>
          {showSlashPaletteStatus ? (
            <div className="composer-slash-status">
              {slashPaletteErrorMessage ? (
                <p className="composer-slash-error" role="alert">
                  {slashPaletteErrorMessage}
                </p>
              ) : slashPalette.kind === "choice" ? (
                <p className="composer-slash-status-text" aria-live="polite">
                  {isUpdating ? (
                    <span className="composer-slash-status-inline">
                      <span className="composer-slash-status-spinner" aria-hidden="true" />
                      Applying setting...
                    </span>
                  ) : slashPalette.isRefreshing ? (
                    "Loading live model options..."
                  ) : slashPalette.supportsLiveRefresh ? (
                    "Refresh live models to update this list from the active session."
                  ) : null}
                </p>
              ) : slashPaletteStatusText ? (
                <p className="composer-slash-status-text" aria-live="polite">
                  {slashPaletteIsRefreshing ? (
                    <span className="composer-slash-status-inline">
                      <span className="composer-slash-status-spinner" aria-hidden="true" />
                      {slashPaletteStatusText}
                    </span>
                  ) : (
                    slashPaletteStatusText
                  )}
                </p>
              ) : null}
              {slashPaletteSupportsRefresh ? (
                <button
                  className="ghost-button composer-slash-refresh-button"
                  type="button"
                  onClick={() => {
                    if (slashPalette.kind === "choice") {
                      requestSlashModelOptions(true);
                    } else if (slashPalette.kind === "mcp") {
                      void requestCodexMcpServers(true);
                    } else {
                      requestSlashAgentCommands(true);
                    }
                  }}
                  disabled={
                    (slashPalette.kind === "choice"
                      ? isRefreshingModelOptions ||
                        isSessionBusy ||
                        isEngramMcpRevocationPending
                      : slashPalette.kind === "mcp"
                        ? activeCodexMcpRequestState.status === "loading"
                        : isRefreshingAgentCommands) || isUpdating
                  }
                >
                  {slashPaletteIsRefreshing
                    ? "Loading..."
                    : (slashPaletteRefreshActionLabel ??
                      (slashPalette.kind === "choice"
                        ? "Refresh live models"
                        : slashPalette.kind === "mcp"
                          ? "Refresh MCP status"
                          : "Refresh agent commands"))}
                </button>
              ) : null}
            </div>
          ) : null}
          {slashPalette.kind === "mcp" && slashPalette.servers.length > 0 ? (
            <div className="composer-mcp-servers" role="list">
              {slashPalette.servers.map((server) => (
                <div
                  className="composer-mcp-server"
                  key={server.name}
                  role="listitem"
                >
                  <div className="composer-mcp-server-header">
                    <span className="composer-slash-option-copy">
                      <span className="composer-slash-option-label">
                        {server.name}
                      </span>
                      <span className="composer-slash-option-detail">
                        {server.tools.length} tool
                        {server.tools.length === 1 ? "" : "s"}
                      </span>
                    </span>
                    <span className="composer-slash-option-badge">
                      {formatCodexMcpAuthStatus(server.authStatus)}
                    </span>
                  </div>
                  {slashPalette.verbose ? (
                    server.tools.length > 0 ? (
                      <div className="composer-mcp-tools" role="list">
                        {server.tools.map((tool) => (
                          <div
                            className="composer-mcp-tool"
                            key={tool.name}
                            role="listitem"
                          >
                            <code>{tool.name}</code>
                            <span>
                              {tool.description ??
                                (tool.title && tool.title !== tool.name
                                  ? tool.title
                                  : "No description reported.")}
                            </span>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <p className="composer-mcp-empty-tools">
                        No tools reported.
                      </p>
                    )
                  ) : null}
                </div>
              ))}
            </div>
          ) : slashPalette.items.length > 0 ? (
            <div
              ref={slashOptionsRef}
              className={`composer-slash-options modality-${slashNavModality}`}
            >
              {slashPalette.items.map((item, index) => {
                const isActive = activeSlashItem?.key === item.key && index === slashActiveIndex;

                return (
                  <div key={item.key} className="composer-slash-option-group">
                    {item.sectionLabel ? (
                      <div className="composer-slash-section-label">{item.sectionLabel}</div>
                    ) : null}
                    <button
                      className={`composer-slash-option${isActive ? " active" : ""}`}
                      type="button"
                      role="option"
                      aria-selected={isActive}
                      onMouseDown={(event) => {
                        event.preventDefault();
                      }}
                      onMouseMove={() => {
                        setSlashNavModality("mouse");
                        if (slashActiveIndex !== index) {
                          setSlashActiveIndex(index);
                        }
                      }}
                      onClick={() => void applySlashPaletteItem(item)}
                      disabled={(item.kind === "choice" || item.kind === "agent-command") && isUpdating}
                    >
                      <span className="composer-slash-option-copy">
                        <span className="composer-slash-option-label">{item.label}</span>
                        <span className="composer-slash-option-detail">{item.detail}</span>
                      </span>
                      {item.kind === "choice" && item.isCurrent ? (
                        isUpdating ? (
                          <span className="composer-slash-option-badge pending">
                            <span className="composer-slash-option-spinner" aria-hidden="true" />
                            Applying
                          </span>
                        ) : (
                          <span className="composer-slash-option-badge">Current</span>
                        )
                      ) : item.kind === "agent-command" ? (
                        <span className="composer-slash-option-badge">Agent</span>
                      ) : null}
                    </button>
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="composer-slash-empty">
              {slashPalette.emptyMessage}
              {slashPalette.kind === "choice" &&
              slashPalette.supportsLiveRefresh &&
              slashPalette.isRefreshing
                ? " Live options will appear here as soon as they load."
                : slashPalette.kind === "command" && slashPaletteIsRefreshing
                  ? " Agent commands will appear here as soon as they load."
                  : slashPalette.kind === "mcp" && slashPaletteIsRefreshing
                    ? " MCP servers will appear here as soon as they load."
                    : null}
            </p>
          )}
        </div>
      ) : null}
    </footer>
  );
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.isPaneActive === next.isPaneActive &&
  previous.sessionId === next.sessionId &&
  previous.formatByteSize === next.formatByteSize &&
  previous.isSending === next.isSending &&
  previous.isStopping === next.isStopping &&
  previous.isSessionBusy === next.isSessionBusy &&
  previous.isUpdating === next.isUpdating &&
  previous.isRefreshingModelOptions === next.isRefreshingModelOptions &&
  previous.isEngramMcpRevocationPending ===
    next.isEngramMcpRevocationPending &&
  previous.modelOptionsError === next.modelOptionsError &&
  previous.agentCommands === next.agentCommands &&
  previous.hasLoadedAgentCommands === next.hasLoadedAgentCommands &&
  previous.isRefreshingAgentCommands === next.isRefreshingAgentCommands &&
  previous.agentCommandsError === next.agentCommandsError &&
  previous.showNewResponseIndicator === next.showNewResponseIndicator &&
  previous.newResponseIndicatorLabel === next.newResponseIndicatorLabel &&
  previous.newResponseIndicatorQueuedCount === next.newResponseIndicatorQueuedCount &&
  previous.onScrollToLatest === next.onScrollToLatest &&
  previous.onDraftCommit === next.onDraftCommit &&
  previous.onDraftAttachmentRemove === next.onDraftAttachmentRemove &&
  previous.onRefreshSessionModelOptions === next.onRefreshSessionModelOptions &&
  previous.onRefreshAgentCommands === next.onRefreshAgentCommands &&
  previous.onSend === next.onSend &&
  previous.canSpawnDelegation === next.canSpawnDelegation &&
  previous.onSpawnDelegation === next.onSpawnDelegation &&
  previous.onSessionSettingsChange === next.onSessionSettingsChange &&
  previous.onStopSession === next.onStopSession &&
  previous.onPaste === next.onPaste
);
