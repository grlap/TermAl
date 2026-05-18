// Owns the AgentSessionPanel prompt composer, slash palette, prompt history,
// draft sync, and delegation/send controls. Deliberately does not own the
// transcript body or footer wrapper; this was split out of
// `AgentSessionPanel.tsx` as a pure code move.

import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import {
  resolveAgentCommand,
  type ResolveAgentCommandResponse,
} from "../api";
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
import { useComposerAutoResize } from "./useComposerAutoResize";
import type {
  AgentCommandResolverErrorState,
  PromptHistoryState,
  SessionComposerProps,
} from "./AgentSessionPanel.types";

const EMPTY_COMPOSER_ATTACHMENTS: readonly {
  byteSize: number;
  fileName: string;
  id: string;
  mediaType: string;
  previewUrl: string;
}[] = [];
const EMPTY_COMPOSER_PROMPT_HISTORY: readonly string[] = [];

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
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  showNewResponseIndicator,
  newResponseIndicatorLabel,
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
  const requestedSlashAgentCommandsRef = useRef<string | null>(null);
  const slashOptionsRef = useRef<HTMLDivElement | null>(null);
  const composerDelegateButtonRef = useRef<HTMLButtonElement | null>(null);
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
  const [slashNavModality, setSlashNavModality] = useState<"keyboard" | "mouse">("keyboard");
  const [isAgentCommandResolving, setIsAgentCommandResolving] = useState(false);
  const isAgentCommandResolvingRef = useRef(false);
  const [isDelegationSpawning, setIsDelegationSpawning] = useState(false);
  const [agentCommandResolverError, setAgentCommandResolverError] =
    useState<AgentCommandResolverErrorState | null>(null);
  const isMountedRef = useRef(true);
  const activeSessionIdRef = useRef<string | null>(null);
  const lastComposerDraftSyncPropSessionIdRef = useRef<string | null>(null);
  const lastComposerDraftSyncSessionIdRef = useRef<string | null>(null);

  // `activeSessionId` is a best-effort identity for draft bookkeeping while
  // the store snapshot catches up. Callers that need capability/session fields
  // must still check `session`.
  const activeSessionId = session?.id ?? sessionId;
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
      ),
    [
      agentCommands,
      agentCommandsError,
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
  const slashPaletteSupportsAgentRefresh =
    slashPalette.kind === "command" && Boolean(slashPalette.supportsRefresh);
  const activeSlashItem =
    slashPalette.kind === "none" || slashPalette.items.length === 0
      ? null
      : (slashPalette.items[Math.min(slashActiveIndex, slashPalette.items.length - 1)] ?? null);
  const canDelegateActiveSlashCommand =
    slashPalette.kind !== "none" && activeSlashItem?.kind === "agent-command";
  const composerInputDisabled =
    !session || isStopping || isAgentCommandResolving || isDelegationSpawning;
  const composerSendDisabled =
    !session ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    (slashPalette.kind !== "none" && slashPalette.items.length === 0);
  const composerDelegateDisabled =
    !session ||
    !canSpawnDelegation ||
    !onSpawnDelegation ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    isDelegationSpawning ||
    (slashPalette.kind !== "none" && !canDelegateActiveSlashCommand);

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

  useEffect(() => {
    onDraftCommitRef.current = onDraftCommit;
  }, [onDraftCommit]);

  useEffect(() => {
    setSlashActiveIndex(slashPalette.kind === "none" ? 0 : slashPalette.defaultActiveIndex);
  }, [activeSessionId, slashPaletteResetKey]);

  useEffect(() => {
    if (
      !session ||
      slashPalette.kind !== "choice" ||
      !slashPaletteSupportsModelRefresh ||
      !supportsLiveSessionModelOptions(session)
    ) {
      return;
    }

    if (session.modelOptions?.length) {
      requestedSlashModelOptionsRef.current = session.id;
      return;
    }

    if (isRefreshingModelOptions || requestedSlashModelOptionsRef.current === session.id) {
      return;
    }

    requestSlashModelOptions();
  }, [
    isRefreshingModelOptions,
    onRefreshSessionModelOptions,
    session,
    slashPalette.kind,
    slashPaletteSupportsModelRefresh,
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
    if (!session || !supportsLiveSessionModelOptions(session)) {
      return;
    }

    if (!force && requestedSlashModelOptionsRef.current === session.id) {
      return;
    }

    requestedSlashModelOptionsRef.current = session.id;
    void onRefreshSessionModelOptions(session.id);
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

  async function handleComposerDelegationSpawn() {
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
        composerDelegateButtonRef.current?.focus();
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
      : (agentCommandResolverError?.sessionId === activeSessionId
          ? agentCommandResolverError.message
          : (slashPalette.errorMessage ?? null));
  const slashPaletteIsRefreshing =
    slashPalette.kind === "none" ? false : Boolean(slashPalette.isRefreshing);
  const slashPaletteRefreshActionLabel =
    slashPalette.kind === "none" ? null : (slashPalette.refreshActionLabel ?? null);
  const slashPaletteSupportsRefresh =
    slashPalette.kind === "choice"
      ? slashPalette.supportsLiveRefresh
      : slashPalette.kind === "command"
        ? Boolean(slashPalette.supportsRefresh)
        : false;
  const slashPaletteStatusText =
    slashPalette.kind === "command" ? (slashPalette.statusText ?? null) : null;
  const slashPaletteHintId = `composer-slash-hint-${paneId}`;
  const keyboardDelegationHint = "Tab moves focus to Delegate.";
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
          placeholder={session ? `Send a prompt to ${session.agent}...` : "Open a session..."}
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
          {session && onSpawnDelegation && canSpawnDelegation ? (
            <button
              ref={composerDelegateButtonRef}
              className="ghost-button composer-delegate-button"
              type="button"
              onMouseDown={(event) => {
                event.preventDefault();
              }}
              onClick={() => void handleComposerDelegationSpawn()}
              disabled={composerDelegateDisabled}
              title="Spawn read-only delegation from current draft"
            >
              {isDelegationSpawning ? "Delegating..." : "Delegate"}
            </button>
          ) : null}
          <button
            className="send-button"
            type="button"
            onMouseDown={(event) => {
              event.preventDefault();
            }}
            onClick={() => void handleComposerSend()}
            disabled={composerSendDisabled}
          >
            {isSending
              ? isSessionBusy
                ? "Queueing..."
                : "Sending..."
              : isSessionBusy
                ? "Queue"
                : "Send"}
          </button>
        </div>
      </div>
      {session ? (
        <p className="composer-hint">
          Paste PNG, JPEG, GIF, or WebP images into the prompt. Drag-and-drop is not supported
          yet.
        </p>
      ) : null}
      {session && slashPalette.kind !== "none" ? (
        <div className="composer-slash-menu" role="listbox" aria-label={slashPalette.title}>
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
                    } else {
                      requestSlashAgentCommands(true);
                    }
                  }}
                  disabled={
                    (slashPalette.kind === "choice"
                      ? isRefreshingModelOptions
                      : isRefreshingAgentCommands) || isUpdating
                  }
                >
                  {slashPaletteIsRefreshing
                    ? "Loading..."
                    : (slashPaletteRefreshActionLabel ??
                        (slashPalette.kind === "choice"
                          ? "Refresh live models"
                          : "Refresh agent commands"))}
                </button>
              ) : null}
            </div>
          ) : null}
          {slashPalette.items.length > 0 ? (
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
  previous.modelOptionsError === next.modelOptionsError &&
  previous.agentCommands === next.agentCommands &&
  previous.hasLoadedAgentCommands === next.hasLoadedAgentCommands &&
  previous.isRefreshingAgentCommands === next.isRefreshingAgentCommands &&
  previous.agentCommandsError === next.agentCommandsError &&
  previous.showNewResponseIndicator === next.showNewResponseIndicator &&
  previous.newResponseIndicatorLabel === next.newResponseIndicatorLabel &&
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
