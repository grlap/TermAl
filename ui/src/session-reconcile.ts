import type {
  DelegationSummary,
  ApprovalMessage,
  CodexAppRequestMessage,
  CommandMessage,
  DiffMessage,
  FileChangesMessage,
  ImageAttachment,
  MarkdownMessage,
  Message,
  McpElicitationRequestMessage,
  McpElicitationRequestPayload,
  ParallelAgentsMessage,
  PendingPrompt,
  Session,
  SubagentResultMessage,
  TextMessage,
  ThinkingMessage,
  UserInputQuestion,
  UserInputRequestMessage,
} from "./types";
import { removePendingPromptForCreatedMessage } from "./app-utils";
import { conversationMarkerColorsMatchForState } from "./conversation-marker-state-equality";

type ReconcileSessionsOptions = {
  disableMutationStampFastPath?: boolean;
  /**
   * A targeted bounded-tail response owns transcript residency even when the
   * preceding summary already advanced count/stamp metadata. Summary payloads
   * never set this; they must continue preserving the current resident window.
   */
  adoptPartialMessages?: boolean;
  /**
   * Force `messagesLoaded` to `false` on summary sessions even when the
   * local transcript already has at least `messageCount` retained
   * messages. Set this on backend-restart adoption: persisted sessions
   * have their `sessionMutationStamp` intentionally cleared on save/load
   * (see `state-revision.ts::isStaleSameInstanceSnapshot`), so the
   * summary-session reconcile cannot otherwise prove the local transcript
   * matches the server's authoritative content. Without this flag a
   * coincidentally-matching `messageCount` would keep `messagesLoaded:
   * true` and the visible-session hydration effect would not re-fire,
   * leaving the active pane stuck on stale streaming content (e.g., a
   * partial assistant chunk that the server has since finalized) until
   * the user hard-refreshes the tab.
   */
  forceMessagesUnloaded?: boolean;
};

export function applyDelegationParentIdsFromSummaries(
  sessions: Session[],
  delegations: readonly Pick<DelegationSummary, "id" | "childSessionId">[],
): Session[] {
  if (!delegations.length) {
    return sessions;
  }

  const parentDelegationIdsByChildSessionId = new Map<string, string>();
  for (const delegation of delegations) {
    if (delegation.childSessionId && delegation.id) {
      parentDelegationIdsByChildSessionId.set(
        delegation.childSessionId,
        delegation.id,
      );
    }
  }
  if (!parentDelegationIdsByChildSessionId.size) {
    return sessions;
  }

  let changed = false;
  const nextSessions = sessions.map((session) => {
    const parentDelegationId = parentDelegationIdsByChildSessionId.get(
      session.id,
    );
    if (
      !parentDelegationId ||
      session.parentDelegationId === parentDelegationId
    ) {
      return session;
    }
    changed = true;
    return {
      ...session,
      parentDelegationId,
    };
  });

  return changed ? nextSessions : sessions;
}

export function reconcileSessions(
  previous: Session[],
  next: Session[],
  options?: ReconcileSessionsOptions,
) {
  let previousById: Map<string, Session> | null = null;
  let changed = previous.length !== next.length;

  const merged = next.map((nextSession, index) => {
    const previousSession =
      previous[index]?.id === nextSession.id
        ? previous[index]
        : (previousById ??= new Map(
            previous.map((session) => [session.id, session]),
          )).get(nextSession.id);
    if (!previousSession) {
      changed = true;
      return nextSession;
    }

    const mergedSession = reconcileSession(
      previousSession,
      nextSession,
      options,
    );
    if (
      mergedSession !== previousSession ||
      previous[index]?.id !== nextSession.id
    ) {
      changed = true;
    }
    return mergedSession;
  });

  return changed ? merged : previous;
}

/**
 * Reconciles one targeted session hydration response against the currently
 * retained session. This is the single-session form of `reconcileSessions`
 * for call sites that already resolved identity and do not need list merging.
 */
export function reconcileSingleSession(
  previous: Session,
  next: Session,
  options?: ReconcileSessionsOptions,
): Session {
  return reconcileSession(previous, next, options);
}

function sameSessionSummary(previous: Session, next: Session) {
  return (
    previous.name === next.name &&
    previous.emoji === next.emoji &&
    previous.agent === next.agent &&
    previous.workdir === next.workdir &&
    previous.projectId === next.projectId &&
    previous.remoteId === next.remoteId &&
    previous.model === next.model &&
    sameModelOptions(previous.modelOptions, next.modelOptions) &&
    previous.approvalPolicy === next.approvalPolicy &&
    previous.claudeEffort === next.claudeEffort &&
    previous.reasoningEffort === next.reasoningEffort &&
    previous.codexFastMode === next.codexFastMode &&
    previous.sandboxMode === next.sandboxMode &&
    previous.cursorMode === next.cursorMode &&
    previous.claudeApprovalMode === next.claudeApprovalMode &&
    previous.geminiApprovalMode === next.geminiApprovalMode &&
    previous.opencodeModel === next.opencodeModel &&
    previous.opencodeEffort === next.opencodeEffort &&
    previous.opencodeCurrentEffort === next.opencodeCurrentEffort &&
    sameModelOptions(previous.opencodeEffortOptions, next.opencodeEffortOptions) &&
    previous.opencodeMode === next.opencodeMode &&
    previous.opencodeCurrentMode === next.opencodeCurrentMode &&
    sameModelOptions(previous.opencodeModeOptions, next.opencodeModeOptions) &&
    previous.externalSessionId === next.externalSessionId &&
    previous.agentCommandsRevision === next.agentCommandsRevision &&
    previous.codexThreadState === next.codexThreadState &&
    (previous.liveActivity?.prompt ?? null) ===
      (next.liveActivity?.prompt ?? null) &&
    (previous.liveActivity?.command ?? null) ===
      (next.liveActivity?.command ?? null) &&
    (previous.liveActivity?.commandStatus ?? null) ===
      (next.liveActivity?.commandStatus ?? null) &&
    previous.sessionMutationStamp === next.sessionMutationStamp &&
    previous.status === next.status &&
    previous.preview === next.preview &&
    sameOptionalStringArray(previous.promptHistory, next.promptHistory) &&
    previous.promptHistoryRedacted === next.promptHistoryRedacted &&
    previous.parentDelegationId === next.parentDelegationId &&
    (previous.messageCount ?? null) === (next.messageCount ?? null) &&
    sameConversationMarkers(previous.markers, next.markers)
  );
}

function preserveExistingParentDelegationId(
  previous: Session,
  next: Session,
): Session {
  // Delegation child ownership is monotonic. Summary/hydration payloads can
  // omit the field, but omission must not make a delegated child visible.
  if (next.parentDelegationId || !previous.parentDelegationId) {
    return next;
  }

  return {
    ...next,
    parentDelegationId: previous.parentDelegationId,
  };
}

function preserveExistingPromptHistory(previous: Session, next: Session): Session {
  // Metadata summaries explicitly mark their omission. Any unmarked omission
  // comes from a targeted/full projection and authoritatively means empty.
  if (next.promptHistoryRedacted === true) {
    return previous.promptHistory === undefined
      ? next
      : { ...next, promptHistory: previous.promptHistory };
  }
  if (next.promptHistory !== undefined || previous.promptHistory === undefined) {
    return next;
  }
  return { ...next, promptHistory: [] };
}

function reconcileSession(
  previous: Session,
  next: Session,
  options?: ReconcileSessionsOptions,
): Session {
  const nextSession = preserveExistingPromptHistory(
    previous,
    preserveExistingParentDelegationId(previous, next),
  );

  if (nextSession.messagesLoaded === false) {
    return reconcileSummarySession(previous, nextSession, options);
  }

  if (
    !options?.disableMutationStampFastPath &&
    previous.sessionMutationStamp !== null &&
    previous.sessionMutationStamp !== undefined &&
    previous.sessionMutationStamp === nextSession.sessionMutationStamp &&
    previous.remoteId === nextSession.remoteId &&
    sameOptionalStringArray(previous.promptHistory, nextSession.promptHistory) &&
    (nextSession.messagesLoaded !== true || previous.messagesLoaded === true)
  ) {
    return previous;
  }
  const messages = reconcileMessages(previous.messages, nextSession.messages);
  const pendingPrompts = reconcilePendingPrompts(
    previous.pendingPrompts,
    nextSession.pendingPrompts,
  );

  if (
    sameSessionSummary(previous, nextSession) &&
    messages === previous.messages &&
    pendingPrompts === previous.pendingPrompts
  ) {
    return previous;
  }

  if (pendingPrompts) {
    return {
      ...nextSession,
      messages,
      pendingPrompts,
    };
  }

  const { pendingPrompts: _discard, ...rest } = nextSession;
  return {
    ...rest,
    messages,
  };
}

function reconcileSummarySession(
  previous: Session,
  next: Session,
  options?: ReconcileSessionsOptions,
): Session {
  if (
    !options?.disableMutationStampFastPath &&
    !options?.forceMessagesUnloaded &&
    previous.sessionMutationStamp !== null &&
    previous.sessionMutationStamp !== undefined &&
    previous.sessionMutationStamp === next.sessionMutationStamp &&
    previous.remoteId === next.remoteId &&
    sameOptionalStringArray(previous.promptHistory, next.promptHistory)
  ) {
    return previous;
  }

  const nextMessageCount =
    typeof next.messageCount === "number" ? next.messageCount : null;
  const previousMessageCount =
    typeof previous.messageCount === "number"
      ? previous.messageCount
      : previous.messages.length;
  const previousMutationStamp = previous.sessionMutationStamp ?? null;
  const nextMutationStamp = next.sessionMutationStamp ?? null;
  const hasDifferentKnownSummaryMutation =
    previousMutationStamp !== null &&
    nextMutationStamp !== null &&
    nextMutationStamp !== previousMutationStamp;
  const hasDifferentKnownMessageCount =
    nextMessageCount !== null && nextMessageCount !== previousMessageCount;
  const shouldAdoptPartialMessages =
    next.messages.length > 0 &&
    previous.messagesLoaded !== true &&
    (options?.adoptPartialMessages === true ||
      previous.messages.length < next.messages.length ||
      hasDifferentKnownSummaryMutation ||
      hasDifferentKnownMessageCount);
  const previousResidentMessageStartIndex =
    resolveResidentMessageStartIndex(previous);
  // A streaming summary carries only the transcript tail. When that window
  // overlaps the hydrated resident window, adopting it must not shrink
  // residency: keep the hydrated head and reconcile only the overlapping
  // tail, so a detached reader's anchor (and the virtualizer's total height)
  // survives live-turn summaries instead of collapsing to the summary window
  // and clamping the viewport to the bottom. Replacement remains correct
  // only when the summary window starts beyond the resident window (a real
  // gap the resident transcript does not cover).
  const summaryOverlapStartIndex =
    shouldAdoptPartialMessages &&
    next.messages.length > 0 &&
    previous.messages.length > 0
      ? previous.messages.findIndex(
          (message) => message.id === next.messages[0]!.id,
        )
      : -1;
  // Guard the overlap with index arithmetic and coverage of the resident
  // tail's end: a summary whose declared window start disagrees with the id
  // overlap, or whose window ends before the resident tail does (an interior
  // subset — a late or raced summary), must not truncate hydrated residency
  // from either side. Such an anomalous window contributes content updates
  // only; the resident window shape stays.
  const overlapIndexIsConsistent =
    typeof next.messageStartIndex !== "number" ||
    Math.max(0, next.messageStartIndex) ===
      previousResidentMessageStartIndex + summaryOverlapStartIndex;
  const summaryCoversResidentTailEnd =
    summaryOverlapStartIndex >= 0 &&
    next.messages.some(
      (message) =>
        message.id === previous.messages[previous.messages.length - 1]!.id,
    );
  const summaryAdoptionMode: "replace" | "preserve-head" | "content-only" =
    summaryOverlapStartIndex < 0
      ? "replace"
      : overlapIndexIsConsistent && summaryCoversResidentTailEnd
        ? "preserve-head"
        : "content-only";
  const adoptedMessageStartIndex = shouldAdoptPartialMessages
    ? summaryAdoptionMode === "preserve-head"
      ? previousResidentMessageStartIndex + summaryOverlapStartIndex
      : summaryAdoptionMode === "content-only"
        ? null
        : resolveAdoptedSummaryMessageStartIndex(
            previous,
            next,
            nextMessageCount,
            previousResidentMessageStartIndex,
          )
    : null;
  const messages = shouldAdoptPartialMessages
    ? summaryAdoptionMode === "preserve-head"
      ? mergePreservedHeadWithReconciledTail(
          previous.messages,
          summaryOverlapStartIndex,
          reconcileMessages(
            previous.messages.slice(summaryOverlapStartIndex),
            next.messages,
          ),
        )
      : summaryAdoptionMode === "content-only"
        ? reconcileMessageContentsById(previous.messages, next.messages)
        : reconcileMessages(previous.messages, next.messages)
    : previous.messages;
  // Metadata-only summaries redact prompt bodies; an empty list on that path
  // means "not included", not "the backend has no queued prompts". Targeted
  // bounded-tail responses are different: they carry the authoritative queue
  // and must adopt or clear it together with transcript residency. A summary
  // that does adopt newly appended messages must consume their matching local
  // optimistic entries in the same commit so the prompt never renders twice.
  // A content-only adoption deliberately adopts no message, so it must not
  // consume any pending prompt either: consuming a prompt for a message that
  // never entered the resident list would hide the prompt until a later
  // hydration.
  const pendingPrompts =
    shouldAdoptPartialMessages && summaryAdoptionMode === "content-only"
      ? previous.pendingPrompts
      : options?.adoptPartialMessages === true
        ? reconcilePendingPrompts(previous.pendingPrompts, next.pendingPrompts)
        : shouldAdoptPartialMessages
          ? reconcilePendingPromptsForAdoptedSummaryMessages(
              previous,
              next,
              adoptedMessageStartIndex,
            )
          : previous.pendingPrompts;
  const hasCompleteMessages =
    nextMessageCount === null || messages.length >= nextMessageCount;
  // After a backend restart the server's persisted `sessionMutationStamp`
  // is `null`, so `hasDifferentKnownSummaryMutation` cannot detect that
  // the new server may have advanced the transcript. The caller signals
  // restart adoption via `forceMessagesUnloaded`; in that case we cannot
  // trust the local transcript and must re-hydrate.
  const mergedMessageStartIndex = shouldAdoptPartialMessages
    ? summaryAdoptionMode === "preserve-head" ||
      summaryAdoptionMode === "content-only"
      ? previousResidentMessageStartIndex
      : (adoptedMessageStartIndex ?? next.messageStartIndex)
    : previous.messages.length > 0
      ? previousResidentMessageStartIndex
      : previous.messageStartIndex;
  // An overlap merge can complete the transcript: a resident prefix that
  // starts at 0 joined with an incoming tail that reaches messageCount. A
  // complete adopted window is authoritative on both sides, so it must not
  // keep a partial response's history flags alive and trigger redundant
  // history demands.
  const mergedWindowIsComplete =
    shouldAdoptPartialMessages &&
    options?.forceMessagesUnloaded !== true &&
    nextMessageCount !== null &&
    (mergedMessageStartIndex ?? 0) === 0 &&
    messages.length >= nextMessageCount;
  const messagesLoaded =
    mergedWindowIsComplete ||
    (options?.forceMessagesUnloaded !== true &&
      !hasDifferentKnownSummaryMutation &&
      hasCompleteMessages &&
      (previous.messagesLoaded === true || previous.messages.length > 0));
  if (
    sameSessionSummary(previous, next) &&
    pendingPrompts === previous.pendingPrompts &&
    messages === previous.messages &&
    previous.messagesLoaded === messagesLoaded
  ) {
    return previous;
  }

  const base = {
    ...next,
    messages,
    messagesLoaded,
    messageStartIndex: mergedMessageStartIndex,
    // Targeted bounded-tail hydration owns the resident window, including
    // which transcript sides remain unloaded. Metadata-only summaries do not:
    // they must preserve the reader's current attachment state.
    // A content-only adoption rejected the incoming window shape, so its
    // side flags are rejected with it; a complete merged window has no
    // unloaded side on either end.
    hasOlderHistory: mergedWindowIsComplete
      ? false
      : options?.adoptPartialMessages === true &&
          summaryAdoptionMode !== "content-only"
        ? next.hasOlderHistory
        : previous.hasOlderHistory,
    hasNewerHistory: mergedWindowIsComplete
      ? false
      : options?.adoptPartialMessages === true &&
          summaryAdoptionMode !== "content-only"
        ? next.hasNewerHistory
        : previous.hasNewerHistory,
  };
  if (pendingPrompts) {
    return {
      ...base,
      pendingPrompts,
    };
  }

  const { pendingPrompts: _discard, ...rest } = base;
  return rest;
}

function reconcilePendingPromptsForAdoptedSummaryMessages(
  previous: Session,
  next: Session,
  adoptedMessageStartIndex: number | null,
): PendingPrompt[] | undefined {
  let pendingPrompts = previous.pendingPrompts;
  if (!pendingPrompts?.length) {
    return pendingPrompts;
  }

  const previousMessageIds = new Set(
    previous.messages.map((message) => message.id),
  );
  for (const [index, message] of next.messages.entries()) {
    if (previousMessageIds.has(message.id)) {
      continue;
    }
    pendingPrompts = removePendingPromptForCreatedMessage(
      pendingPrompts,
      message,
      adoptedMessageStartIndex === null
        ? null
        : adoptedMessageStartIndex + index,
    );
    if (!pendingPrompts) {
      return undefined;
    }
  }

  return pendingPrompts;
}

function resolveResidentMessageStartIndex(session: Session) {
  if (typeof session.messageStartIndex === "number") {
    return Math.max(0, session.messageStartIndex);
  }
  if (typeof session.messageCount === "number") {
    return Math.max(0, session.messageCount - session.messages.length);
  }
  return 0;
}

function resolveAdoptedSummaryMessageStartIndex(
  previous: Session,
  next: Session,
  nextMessageCount: number | null,
  previousResidentMessageStartIndex: number,
) {
  if (typeof next.messageStartIndex === "number") {
    return Math.max(0, next.messageStartIndex);
  }
  if (nextMessageCount !== null) {
    return Math.max(0, nextMessageCount - next.messages.length);
  }

  const previousMessageIndexById = new Map(
    previous.messages.map((message, index) => [message.id, index]),
  );
  for (const [nextIndex, message] of next.messages.entries()) {
    const previousIndex = previousMessageIndexById.get(message.id);
    if (previousIndex !== undefined) {
      return Math.max(
        0,
        previousResidentMessageStartIndex + previousIndex - nextIndex,
      );
    }
  }
  return null;
}

function sameOptionalStringArray(
  previous: readonly string[] | undefined,
  next: readonly string[] | undefined,
) {
  if (previous === next) {
    return true;
  }
  if (!previous || !next || previous.length !== next.length) {
    return false;
  }
  return previous.every((value, index) => value === next[index]);
}

function sameModelOptions(
  previous?: Session["modelOptions"],
  next?: Session["modelOptions"],
) {
  if (previous === next) {
    return true;
  }
  if (!previous?.length && !next?.length) {
    return true;
  }
  if (!previous || !next || previous.length !== next.length) {
    return false;
  }

  return previous.every((option, index) => {
    const nextOption = next[index];
    const previousBadges = option.badges ?? [];
    const nextBadges = nextOption?.badges ?? [];
    const previousSupportedClaudeEfforts =
      option.supportedClaudeEffortLevels ?? [];
    const nextSupportedClaudeEfforts =
      nextOption?.supportedClaudeEffortLevels ?? [];
    const previousSupportedReasoningEfforts =
      option.supportedReasoningEfforts ?? [];
    const nextSupportedReasoningEfforts =
      nextOption?.supportedReasoningEfforts ?? [];
    const previousServiceTiers = option.serviceTiers ?? [];
    const nextServiceTiers = nextOption?.serviceTiers ?? [];
    return (
      nextOption?.label === option.label &&
      nextOption.value === option.value &&
      (nextOption.description ?? null) === (option.description ?? null) &&
      (nextOption.defaultReasoningEffort ?? null) ===
        (option.defaultReasoningEffort ?? null) &&
      previousBadges.length === nextBadges.length &&
      previousBadges.every(
        (badge, badgeIndex) => nextBadges[badgeIndex] === badge,
      ) &&
      previousSupportedClaudeEfforts.length ===
        nextSupportedClaudeEfforts.length &&
      previousSupportedClaudeEfforts.every(
        (effort, effortIndex) =>
          nextSupportedClaudeEfforts[effortIndex] === effort,
      ) &&
      previousSupportedReasoningEfforts.length ===
        nextSupportedReasoningEfforts.length &&
      previousSupportedReasoningEfforts.every(
        (effort, effortIndex) =>
          nextSupportedReasoningEfforts[effortIndex] === effort,
      ) &&
      previousServiceTiers.length === nextServiceTiers.length &&
      previousServiceTiers.every((tier, tierIndex) => {
        const nextTier = nextServiceTiers[tierIndex];
        return (
          nextTier?.id === tier.id &&
          nextTier.label === tier.label &&
          (nextTier.description ?? null) === (tier.description ?? null)
        );
      })
    );
  });
}

function sameConversationMarkers(
  previous?: Session["markers"],
  next?: Session["markers"],
) {
  if (previous === next) {
    return true;
  }
  if (!previous?.length && !next?.length) {
    return true;
  }
  if (!previous || !next || previous.length !== next.length) {
    return false;
  }

  return previous.every((marker, index) => {
    const nextMarker = next[index];
    return (
      nextMarker?.id === marker.id &&
      nextMarker.sessionId === marker.sessionId &&
      nextMarker.kind === marker.kind &&
      nextMarker.name === marker.name &&
      (nextMarker.body ?? null) === (marker.body ?? null) &&
      conversationMarkerColorsMatchForState(nextMarker.color, marker.color) &&
      nextMarker.messageId === marker.messageId &&
      nextMarker.messageIndexHint === marker.messageIndexHint &&
      (nextMarker.endMessageId ?? null) === (marker.endMessageId ?? null) &&
      (nextMarker.endMessageIndexHint ?? null) ===
        (marker.endMessageIndexHint ?? null) &&
      nextMarker.createdAt === marker.createdAt &&
      nextMarker.updatedAt === marker.updatedAt &&
      nextMarker.createdBy === marker.createdBy
    );
  });
}

function reconcileMessages(previous: Message[], next: Message[]): Message[] {
  if (previous === next) {
    return previous;
  }

  if (previous.length === next.length && previous.length > 0) {
    const previousLastMessage = previous[previous.length - 1];
    const nextLastMessage = next[next.length - 1];
    if (
      previousLastMessage &&
      nextLastMessage &&
      previousLastMessage.id === nextLastMessage.id
    ) {
      let firstChangedIndex = -1;
      for (let index = 0; index < next.length; index += 1) {
        if (previous[index]?.id !== next[index]?.id) {
          firstChangedIndex = index;
          break;
        }
        if (
          reconcileMessage(previous[index]!, next[index]!) !== previous[index]
        ) {
          firstChangedIndex = index;
          break;
        }
      }

      if (firstChangedIndex === -1) {
        return previous;
      }

      const merged = previous.slice(0, firstChangedIndex);
      for (let index = firstChangedIndex; index < next.length; index += 1) {
        const previousMessage = previous[index];
        const nextMessage = next[index];
        if (previousMessage?.id === nextMessage?.id) {
          merged.push(reconcileMessage(previousMessage, nextMessage));
          continue;
        }
        return reconcileMessagesById(previous, next);
      }
      return merged;
    }
  }

  return reconcileMessagesById(previous, next);
}

// Joins a preserved hydrated head with the reconciled overlapping tail while
// keeping referential identity: when the tail came back unchanged and adds
// nothing beyond the resident window, the previous messages array is returned
// as-is so an overlapping summary that changed only session metadata does not
// force a transcript rerender.
function mergePreservedHeadWithReconciledTail(
  previousMessages: Message[],
  overlapStartIndex: number,
  reconciledTail: Message[],
): Message[] {
  const previousTailLength = previousMessages.length - overlapStartIndex;
  if (reconciledTail.length === previousTailLength) {
    let identical = true;
    for (let index = 0; index < previousTailLength; index += 1) {
      if (reconciledTail[index] !== previousMessages[overlapStartIndex + index]) {
        identical = false;
        break;
      }
    }
    if (identical) {
      return previousMessages;
    }
  }
  return [
    ...previousMessages.slice(0, overlapStartIndex),
    ...reconciledTail,
  ];
}

// Applies per-message content updates from an anomalous summary window (an
// interior subset or an index-inconsistent overlap) without changing the
// resident window shape: no message is added, dropped, or reordered.
function reconcileMessageContentsById(
  previous: Message[],
  next: Message[],
): Message[] {
  let nextById: Map<string, Message> | null = null;
  let changed = false;
  const merged = previous.map((previousMessage) => {
    const nextMessage = (nextById ??= new Map(
      next.map((message) => [message.id, message]),
    )).get(previousMessage.id);
    if (!nextMessage) {
      return previousMessage;
    }
    const mergedMessage = reconcileMessage(previousMessage, nextMessage);
    if (mergedMessage !== previousMessage) {
      changed = true;
    }
    return mergedMessage;
  });
  return changed ? merged : previous;
}

function reconcileMessagesById(
  previous: Message[],
  next: Message[],
): Message[] {
  let previousById: Map<string, Message> | null = null;
  let changed = previous.length !== next.length;

  const merged = next.map((nextMessage, index) => {
    const previousMessage =
      previous[index]?.id === nextMessage.id
        ? previous[index]
        : (previousById ??= new Map(
            previous.map((message) => [message.id, message]),
          )).get(nextMessage.id);
    if (!previousMessage) {
      changed = true;
      return nextMessage;
    }

    const mergedMessage = reconcileMessage(previousMessage, nextMessage);
    if (
      mergedMessage !== previousMessage ||
      previous[index]?.id !== nextMessage.id
    ) {
      changed = true;
    }
    return mergedMessage;
  });

  return changed ? merged : previous;
}

function reconcileMessage(previous: Message, next: Message): Message {
  if (previous.type !== next.type) {
    return next;
  }

  switch (next.type) {
    case "text":
      return reconcileTextMessage(previous as TextMessage, next);
    case "thinking":
      return reconcileThinkingMessage(previous as ThinkingMessage, next);
    case "command":
      return reconcileCommandMessage(previous as CommandMessage, next);
    case "diff":
      return reconcileDiffMessage(previous as DiffMessage, next);
    case "markdown":
      return reconcileMarkdownMessage(previous as MarkdownMessage, next);
    case "parallelAgents":
      return reconcileParallelAgentsMessage(
        previous as ParallelAgentsMessage,
        next,
      );
    case "fileChanges":
      return reconcileFileChangesMessage(previous as FileChangesMessage, next);
    case "subagentResult":
      return reconcileSubagentResultMessage(
        previous as SubagentResultMessage,
        next,
      );
    case "approval":
      return reconcileApprovalMessage(previous as ApprovalMessage, next);
    case "userInputRequest":
      return reconcileUserInputRequestMessage(
        previous as UserInputRequestMessage,
        next,
      );
    case "mcpElicitationRequest":
      return reconcileMcpElicitationRequestMessage(
        previous as McpElicitationRequestMessage,
        next,
      );
    case "codexAppRequest":
      return reconcileCodexAppRequestMessage(
        previous as CodexAppRequestMessage,
        next,
      );
  }

  return next;
}

function reconcileTextMessage(
  previous: TextMessage,
  next: TextMessage,
): TextMessage {
  const attachments = reconcileAttachments(
    previous.attachments,
    next.attachments,
  );
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.text === next.text &&
    (previous.expandedText ?? null) === (next.expandedText ?? null) &&
    (previous.source?.sessionId ?? null) === (next.source?.sessionId ?? null) &&
    (previous.source?.name ?? null) === (next.source?.name ?? null) &&
    (previous.source?.kind ?? "peer") === (next.source?.kind ?? "peer") &&
    (previous.source?.mailbox?.mailboxId ?? null) ===
      (next.source?.mailbox?.mailboxId ?? null) &&
    (previous.source?.mailbox?.messageId ?? null) ===
      (next.source?.mailbox?.messageId ?? null) &&
    (previous.source?.mailbox?.sequence ?? null) ===
      (next.source?.mailbox?.sequence ?? null) &&
    (previous.source?.mailbox?.unreadCount ?? null) ===
      (next.source?.mailbox?.unreadCount ?? null) &&
    attachments === previous.attachments
  ) {
    return previous;
  }

  if (attachments === next.attachments) {
    return next;
  }

  if (attachments) {
    return {
      ...next,
      attachments,
    };
  }

  const { attachments: _discard, ...rest } = next;
  return rest;
}

function reconcileThinkingMessage(
  previous: ThinkingMessage,
  next: ThinkingMessage,
): ThinkingMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    stringArrayEqual(previous.lines, next.lines)
  ) {
    return previous;
  }

  return next;
}

function reconcileCommandMessage(
  previous: CommandMessage,
  next: CommandMessage,
): CommandMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.command === next.command &&
    previous.commandLanguage === next.commandLanguage &&
    previous.output === next.output &&
    previous.outputLanguage === next.outputLanguage &&
    previous.status === next.status
  ) {
    return previous;
  }

  return next;
}

function reconcileDiffMessage(
  previous: DiffMessage,
  next: DiffMessage,
): DiffMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.changeSetId === next.changeSetId &&
    previous.filePath === next.filePath &&
    previous.summary === next.summary &&
    previous.diff === next.diff &&
    previous.language === next.language &&
    previous.changeType === next.changeType
  ) {
    return previous;
  }

  return next;
}

function reconcileMarkdownMessage(
  previous: MarkdownMessage,
  next: MarkdownMessage,
): MarkdownMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.markdown === next.markdown
  ) {
    return previous;
  }

  return next;
}

function reconcileParallelAgentsMessage(
  previous: ParallelAgentsMessage,
  next: ParallelAgentsMessage,
): ParallelAgentsMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.agents.length === next.agents.length &&
    previous.agents.every((agent, index) => {
      const nextAgent = next.agents[index];
      return (
        nextAgent?.id === agent.id &&
        nextAgent.source === agent.source &&
        nextAgent.title === agent.title &&
        nextAgent.status === agent.status &&
        (nextAgent.detail ?? null) === (agent.detail ?? null)
      );
    })
  ) {
    return previous;
  }

  return next;
}
function reconcileFileChangesMessage(
  previous: FileChangesMessage,
  next: FileChangesMessage,
): FileChangesMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.files.length === next.files.length &&
    previous.files.every((file, index) => {
      const nextFile = next.files[index];
      return nextFile?.path === file.path && nextFile.kind === file.kind;
    })
  ) {
    return previous;
  }

  return next;
}
function reconcileSubagentResultMessage(
  previous: SubagentResultMessage,
  next: SubagentResultMessage,
): SubagentResultMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.summary === next.summary &&
    previous.conversationId === next.conversationId &&
    previous.turnId === next.turnId
  ) {
    return previous;
  }

  return next;
}

function reconcileApprovalMessage(
  previous: ApprovalMessage,
  next: ApprovalMessage,
): ApprovalMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.command === next.command &&
    previous.commandLanguage === next.commandLanguage &&
    previous.detail === next.detail &&
    previous.decision === next.decision
  ) {
    return previous;
  }

  return next;
}

function reconcileUserInputRequestMessage(
  previous: UserInputRequestMessage,
  next: UserInputRequestMessage,
): UserInputRequestMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.detail === next.detail &&
    previous.state === next.state &&
    sameUserInputQuestions(previous.questions, next.questions) &&
    sameSubmittedAnswers(previous.submittedAnswers, next.submittedAnswers)
  ) {
    return previous;
  }

  return next;
}

function reconcileMcpElicitationRequestMessage(
  previous: McpElicitationRequestMessage,
  next: McpElicitationRequestMessage,
): McpElicitationRequestMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.detail === next.detail &&
    previous.state === next.state &&
    previous.submittedAction === next.submittedAction &&
    sameMcpElicitationRequest(previous.request, next.request) &&
    sameJsonValue(previous.submittedContent, next.submittedContent)
  ) {
    return previous;
  }

  return next;
}

function reconcileCodexAppRequestMessage(
  previous: CodexAppRequestMessage,
  next: CodexAppRequestMessage,
): CodexAppRequestMessage {
  if (
    previous.timestamp === next.timestamp &&
    previous.author === next.author &&
    previous.title === next.title &&
    previous.detail === next.detail &&
    previous.method === next.method &&
    previous.state === next.state &&
    sameJsonValue(previous.params, next.params) &&
    sameJsonValue(previous.submittedResult, next.submittedResult)
  ) {
    return previous;
  }

  return next;
}

function sameUserInputQuestions(
  previous: UserInputQuestion[],
  next: UserInputQuestion[],
) {
  return (
    previous.length === next.length &&
    previous.every((question, index) => {
      const nextQuestion = next[index];
      if (!nextQuestion) {
        return false;
      }

      const previousOptions = question.options ?? [];
      const nextOptions = nextQuestion.options ?? [];
      return (
        question.header === nextQuestion.header &&
        question.id === nextQuestion.id &&
        question.isOther === nextQuestion.isOther &&
        question.isSecret === nextQuestion.isSecret &&
        question.question === nextQuestion.question &&
        previousOptions.length === nextOptions.length &&
        previousOptions.every(
          (option, optionIndex) =>
            option.label === nextOptions[optionIndex]?.label &&
            option.description === nextOptions[optionIndex]?.description,
        )
      );
    })
  );
}

function sameSubmittedAnswers(
  previous?: UserInputRequestMessage["submittedAnswers"],
  next?: UserInputRequestMessage["submittedAnswers"],
) {
  const previousEntries = Object.entries(previous ?? {});
  const nextEntries = Object.entries(next ?? {});
  return (
    previousEntries.length === nextEntries.length &&
    previousEntries.every(([key, value]) => {
      const nextValue = next?.[key];
      return (
        !!nextValue &&
        value.length === nextValue.length &&
        value.every((entry, index) => entry === nextValue[index])
      );
    })
  );
}

function sameMcpElicitationRequest(
  previous: McpElicitationRequestPayload,
  next: McpElicitationRequestPayload,
) {
  return sameJsonValue(previous, next);
}

function sameJsonValue(previous: unknown, next: unknown): boolean {
  if (previous === next) {
    return true;
  }
  if (previous == null || next == null) {
    return (previous ?? null) === (next ?? null);
  }
  if (typeof previous !== typeof next) {
    return false;
  }
  if (typeof previous !== "object") {
    return previous === next;
  }
  if (Array.isArray(previous)) {
    if (!Array.isArray(next) || previous.length !== next.length) {
      return false;
    }
    return previous.every((item, index) => sameJsonValue(item, next[index]));
  }
  if (Array.isArray(next)) {
    return false;
  }
  const previousObj = previous as Record<string, unknown>;
  const nextObj = next as Record<string, unknown>;
  const previousKeys = Object.keys(previousObj);
  const nextKeys = Object.keys(nextObj);
  if (previousKeys.length !== nextKeys.length) {
    return false;
  }
  return previousKeys.every(
    (key) => key in nextObj && sameJsonValue(previousObj[key], nextObj[key]),
  );
}

function reconcilePendingPrompts(
  previous: PendingPrompt[] | undefined,
  next: PendingPrompt[] | undefined,
): PendingPrompt[] | undefined {
  if (!next?.length) {
    return undefined;
  }

  if (!previous?.length) {
    return next;
  }

  let previousById: Map<string, PendingPrompt> | null = null;
  let changed = previous.length !== next.length;

  const merged = next.map((nextPrompt, index) => {
    const previousPrompt =
      previous[index]?.id === nextPrompt.id
        ? previous[index]
        : (previousById ??= new Map(
            previous.map((prompt) => [prompt.id, prompt]),
          )).get(nextPrompt.id);
    if (!previousPrompt) {
      changed = true;
      return nextPrompt;
    }

    const attachments = reconcileAttachments(
      previousPrompt.attachments,
      nextPrompt.attachments,
    );
    if (
      previousPrompt.timestamp === nextPrompt.timestamp &&
      previousPrompt.text === nextPrompt.text &&
      (previousPrompt.expandedText ?? null) ===
        (nextPrompt.expandedText ?? null) &&
      attachments === previousPrompt.attachments
    ) {
      if (previous[index]?.id !== nextPrompt.id) {
        changed = true;
      }
      return previousPrompt;
    }

    changed = true;
    if (attachments) {
      return {
        ...nextPrompt,
        attachments,
      };
    }

    const { attachments: _discard, ...rest } = nextPrompt;
    return rest;
  });

  return changed ? merged : previous;
}

function reconcileAttachments(
  previous: ImageAttachment[] | undefined,
  next: ImageAttachment[] | undefined,
): ImageAttachment[] | undefined {
  if (!next?.length) {
    return undefined;
  }

  if (!previous?.length) {
    return next;
  }

  if (previous.length !== next.length) {
    return next;
  }

  for (let index = 0; index < next.length; index += 1) {
    const previousAttachment = previous[index];
    const nextAttachment = next[index];
    if (
      previousAttachment.fileName !== nextAttachment.fileName ||
      previousAttachment.mediaType !== nextAttachment.mediaType ||
      previousAttachment.byteSize !== nextAttachment.byteSize
    ) {
      return next;
    }
  }

  return previous;
}

function stringArrayEqual(previous: string[], next: string[]) {
  if (previous.length !== next.length) {
    return false;
  }

  for (let index = 0; index < previous.length; index += 1) {
    if (previous[index] !== next[index]) {
      return false;
    }
  }

  return true;
}
