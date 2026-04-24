// Narrow frontend session store used by incremental responsiveness refactors.
//
// What this file owns:
//   - A small `useSyncExternalStore`-backed store for session slices that need
//     stronger identity guarantees than broad prop-threading currently offers.
//   - The first migrated slices:
//       * `ComposerSessionSnapshot` for the prompt composer/footer path
//       * `SessionSummarySnapshot` for pane/tab surfaces that do not need
//         transcript-bearing session objects
//   - Structural-sharing sync helpers so unchanged slices preserve object/array
//     identity even when the broader `sessions[]` tree updates.
//
// What this file does NOT own:
//   - The authoritative application state. `App.tsx` still owns the source
//     state and bridges selected slices into this store.
//   - Live transport / revision semantics. `app-live-state.ts` remains the
//     source of truth for snapshot and delta adoption.
//   - Full session rendering or transcript data. This store is intentionally
//     narrow and exists to decouple the prompt path first.

import { useSyncExternalStore } from "react";
import type {
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CursorMode,
  GeminiApprovalMode,
  Message,
  Session,
  SessionModelOption,
} from "./types";
import type { DraftImageAttachment } from "./app-utils";

export type ComposerDraftAttachment = Readonly<{
  byteSize: number;
  fileName: string;
  id: string;
  mediaType: string;
  previewUrl: string;
}>;

export type ComposerSessionSnapshot = Readonly<{
  approvalPolicy?: ApprovalPolicy | null;
  agent: AgentType;
  agentCommandsRevision?: number;
  claudeApprovalMode?: ClaudeApprovalMode | null;
  claudeEffort?: ClaudeEffortLevel | null;
  committedDraft: string;
  cursorMode?: CursorMode | null;
  draftAttachments: readonly ComposerDraftAttachment[];
  geminiApprovalMode?: GeminiApprovalMode | null;
  id: string;
  model: string;
  modelOptions?: readonly SessionModelOption[];
  name: string;
  promptHistory: readonly string[];
  reasoningEffort?: CodexReasoningEffort | null;
  sandboxMode?: Session["sandboxMode"];
  workdir: string;
}>;

export type SessionSummarySnapshot = Readonly<{
  approvalPolicy?: ApprovalPolicy | null;
  agent: AgentType;
  agentCommandsRevision?: number;
  claudeApprovalMode?: ClaudeApprovalMode | null;
  claudeEffort?: ClaudeEffortLevel | null;
  codexThreadState?: Session["codexThreadState"];
  cursorMode?: CursorMode | null;
  externalSessionId?: string | null;
  geminiApprovalMode?: GeminiApprovalMode | null;
  id: string;
  model: string;
  modelOptions?: readonly SessionModelOption[];
  name: string;
  projectId?: string | null;
  reasoningEffort?: CodexReasoningEffort | null;
  sandboxMode?: Session["sandboxMode"];
  status: Session["status"];
  workdir: string;
}>;

type SessionStoreState = Readonly<{
  composerSessionsById: Readonly<Record<string, ComposerSessionSnapshot>>;
  sessionRecordsById: Readonly<Record<string, Session>>;
  sessionSummariesById: Readonly<Record<string, SessionSummarySnapshot>>;
}>;

type SyncComposerSessionsParams = Readonly<{
  draftAttachmentsBySessionId: Readonly<Record<string, DraftImageAttachment[]>>;
  draftsBySessionId: Readonly<Record<string, string>>;
  sessions: readonly Session[];
}>;

type SyncComposerSessionsStoreIncrementalParams = Readonly<{
  changedSessions: readonly Session[];
  draftAttachmentsBySessionId: Readonly<Record<string, DraftImageAttachment[]>>;
  draftsBySessionId: Readonly<Record<string, string>>;
  removedSessionIds?: readonly string[];
}>;

type SyncComposerDraftForSessionParams = Readonly<{
  draftAttachments: readonly DraftImageAttachment[];
  committedDraft: string;
  sessionId: string;
}>;

type UpsertSessionStoreSessionParams = Readonly<{
  draftAttachments: readonly DraftImageAttachment[];
  committedDraft: string;
  session: Session;
}>;

const EMPTY_PROMPT_HISTORY: readonly string[] = [];
const EMPTY_DRAFT_ATTACHMENTS: readonly ComposerDraftAttachment[] = [];
const EMPTY_COMPOSER_SESSIONS_BY_ID: Readonly<Record<string, ComposerSessionSnapshot>> = {};
const EMPTY_SESSION_RECORDS_BY_ID: Readonly<Record<string, Session>> = {};
const EMPTY_SESSION_SUMMARIES_BY_ID: Readonly<Record<string, SessionSummarySnapshot>> = {};
const INITIAL_STATE: SessionStoreState = {
  composerSessionsById: EMPTY_COMPOSER_SESSIONS_BY_ID,
  sessionRecordsById: EMPTY_SESSION_RECORDS_BY_ID,
  sessionSummariesById: EMPTY_SESSION_SUMMARIES_BY_ID,
};

let currentState: SessionStoreState = INITIAL_STATE;
const listeners = new Set<() => void>();

function emitStoreChange() {
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function sameStringArray(
  previous: readonly string[] | undefined,
  next: readonly string[] | undefined,
) {
  if (previous === next) {
    return true;
  }
  if ((previous?.length ?? 0) !== (next?.length ?? 0)) {
    return false;
  }
  for (let index = 0; index < (previous?.length ?? 0); index += 1) {
    if (previous?.[index] !== next?.[index]) {
      return false;
    }
  }
  return true;
}

function sameSessionModelOptions(
  previous: readonly SessionModelOption[] | undefined,
  next: readonly SessionModelOption[] | undefined,
) {
  if (previous === next) {
    return true;
  }
  if ((previous?.length ?? 0) !== (next?.length ?? 0)) {
    return false;
  }
  for (let index = 0; index < (previous?.length ?? 0); index += 1) {
    const previousOption = previous?.[index];
    const nextOption = next?.[index];
    if (!previousOption || !nextOption) {
      return false;
    }
    if (
      previousOption.label !== nextOption.label ||
      previousOption.value !== nextOption.value ||
      previousOption.description !== nextOption.description ||
      !sameStringArray(previousOption.badges, nextOption.badges) ||
      !sameStringArray(
        previousOption.supportedClaudeEffortLevels,
        nextOption.supportedClaudeEffortLevels,
      ) ||
      !sameStringArray(
        previousOption.supportedReasoningEfforts,
        nextOption.supportedReasoningEfforts,
      ) ||
      previousOption.defaultReasoningEffort !== nextOption.defaultReasoningEffort
    ) {
      return false;
    }
  }
  return true;
}

function collectUserPromptHistory(messages: readonly Message[]) {
  const prompts = messages.flatMap((message) => {
    if (message.type !== "text" || message.author !== "you") {
      return [];
    }

    const prompt = message.text.trim();
    return prompt ? [prompt] : [];
  });
  return prompts.length === 0 ? EMPTY_PROMPT_HISTORY : prompts;
}

function sameDraftAttachments(
  previous: readonly ComposerDraftAttachment[] | undefined,
  next: readonly ComposerDraftAttachment[] | undefined,
) {
  if (previous === next) {
    return true;
  }
  if ((previous?.length ?? 0) !== (next?.length ?? 0)) {
    return false;
  }
  for (let index = 0; index < (previous?.length ?? 0); index += 1) {
    const previousAttachment = previous?.[index];
    const nextAttachment = next?.[index];
    if (!previousAttachment || !nextAttachment) {
      return false;
    }
    if (
      previousAttachment.id !== nextAttachment.id ||
      previousAttachment.fileName !== nextAttachment.fileName ||
      previousAttachment.mediaType !== nextAttachment.mediaType ||
      previousAttachment.byteSize !== nextAttachment.byteSize ||
      previousAttachment.previewUrl !== nextAttachment.previewUrl
    ) {
      return false;
    }
  }
  return true;
}

function buildDraftAttachmentsSnapshot(
  draftAttachments: readonly DraftImageAttachment[],
  previous: readonly ComposerDraftAttachment[] | undefined,
) {
  if (draftAttachments.length === 0) {
    return previous?.length ? EMPTY_DRAFT_ATTACHMENTS : (previous ?? EMPTY_DRAFT_ATTACHMENTS);
  }

  const nextAttachments = draftAttachments.map((attachment) => ({
    byteSize: attachment.byteSize,
    fileName: attachment.fileName,
    id: attachment.id,
    mediaType: attachment.mediaType,
    previewUrl: attachment.previewUrl,
  }));

  return sameDraftAttachments(previous, nextAttachments) ? (previous ?? nextAttachments) : nextAttachments;
}

function buildComposerSessionSnapshot(
  session: Session,
  committedDraft: string,
  draftAttachments: readonly DraftImageAttachment[],
  previousSession: Session | undefined,
  previous: ComposerSessionSnapshot | undefined,
) {
  const nextPromptHistory = resolvePromptHistory(
    session,
    previousSession,
    previous,
  );
  const promptHistory =
    previous && sameStringArray(previous.promptHistory, nextPromptHistory)
      ? previous.promptHistory
      : nextPromptHistory;
  const nextModelOptions =
    previous && sameSessionModelOptions(previous.modelOptions, session.modelOptions)
      ? previous.modelOptions
      : session.modelOptions;
  const nextDraftAttachments = buildDraftAttachmentsSnapshot(
    draftAttachments,
    previous?.draftAttachments,
  );

  if (
    previous &&
    previous.id === session.id &&
    previous.name === session.name &&
    previous.agent === session.agent &&
    previous.workdir === session.workdir &&
    previous.model === session.model &&
    previous.modelOptions === nextModelOptions &&
    previous.approvalPolicy === session.approvalPolicy &&
    previous.claudeEffort === session.claudeEffort &&
    previous.reasoningEffort === session.reasoningEffort &&
    previous.sandboxMode === session.sandboxMode &&
    previous.cursorMode === session.cursorMode &&
    previous.claudeApprovalMode === session.claudeApprovalMode &&
    previous.geminiApprovalMode === session.geminiApprovalMode &&
    previous.agentCommandsRevision === session.agentCommandsRevision &&
    previous.committedDraft === committedDraft &&
    previous.draftAttachments === nextDraftAttachments &&
    previous.promptHistory === promptHistory
  ) {
    return previous;
  }

  return {
    approvalPolicy: session.approvalPolicy,
    agent: session.agent,
    agentCommandsRevision: session.agentCommandsRevision,
    claudeApprovalMode: session.claudeApprovalMode,
    claudeEffort: session.claudeEffort,
    committedDraft,
    cursorMode: session.cursorMode,
    draftAttachments: nextDraftAttachments,
    geminiApprovalMode: session.geminiApprovalMode,
    id: session.id,
    model: session.model,
    modelOptions: nextModelOptions,
    name: session.name,
    promptHistory,
    reasoningEffort: session.reasoningEffort,
    sandboxMode: session.sandboxMode,
    workdir: session.workdir,
  };
}

function resolvePromptHistory(
  session: Session,
  previousSession: Session | undefined,
  previousSnapshot: ComposerSessionSnapshot | undefined,
) {
  if (!previousSnapshot) {
    return collectUserPromptHistory(session.messages);
  }
  if (!previousSession) {
    return collectUserPromptHistory(session.messages);
  }
  if (previousSession.messages === session.messages) {
    return previousSnapshot.promptHistory;
  }

  const previousMessages = previousSession.messages;
  const nextMessages = session.messages;
  const previousLength = previousMessages.length;
  const nextLength = nextMessages.length;

  if (nextLength < previousLength) {
    return collectUserPromptHistory(nextMessages);
  }

  const previousLastMessage =
    previousLength > 0 ? previousMessages[previousLength - 1] : null;
  const nextPreviousBoundaryMessage =
    previousLength > 0 ? nextMessages[previousLength - 1] : null;
  if (
    previousLastMessage &&
    nextPreviousBoundaryMessage?.id !== previousLastMessage.id
  ) {
    return collectUserPromptHistory(nextMessages);
  }

  if (nextLength === previousLength) {
    const nextLastMessage = nextMessages[nextLength - 1];
    if (
      nextLastMessage &&
      nextLastMessage.id === previousLastMessage?.id &&
      nextLastMessage.type === "text" &&
      nextLastMessage.author !== "you"
    ) {
      return previousSnapshot.promptHistory;
    }

    return collectUserPromptHistory(nextMessages);
  }

  const appendedMessages = nextMessages.slice(previousLength);
  if (
    appendedMessages.length > 0 &&
    appendedMessages.every(
      (message) => message.type !== "text" || message.author !== "you",
    )
  ) {
    return previousSnapshot.promptHistory;
  }

  return collectUserPromptHistory(nextMessages);
}

function buildSessionSummarySnapshot(
  session: Session,
  previous: SessionSummarySnapshot | undefined,
) {
  const nextModelOptions =
    previous && sameSessionModelOptions(previous.modelOptions, session.modelOptions)
      ? previous.modelOptions
      : session.modelOptions;

  if (
    previous &&
    previous.id === session.id &&
    previous.name === session.name &&
    previous.agent === session.agent &&
    previous.workdir === session.workdir &&
    previous.projectId === session.projectId &&
    previous.status === session.status &&
    previous.model === session.model &&
    previous.modelOptions === nextModelOptions &&
    previous.approvalPolicy === session.approvalPolicy &&
    previous.claudeEffort === session.claudeEffort &&
    previous.reasoningEffort === session.reasoningEffort &&
    previous.sandboxMode === session.sandboxMode &&
    previous.cursorMode === session.cursorMode &&
    previous.claudeApprovalMode === session.claudeApprovalMode &&
    previous.geminiApprovalMode === session.geminiApprovalMode &&
    previous.externalSessionId === session.externalSessionId &&
    previous.agentCommandsRevision === session.agentCommandsRevision &&
    previous.codexThreadState === session.codexThreadState
  ) {
    return previous;
  }

  return {
    approvalPolicy: session.approvalPolicy,
    agent: session.agent,
    agentCommandsRevision: session.agentCommandsRevision,
    claudeApprovalMode: session.claudeApprovalMode,
    claudeEffort: session.claudeEffort,
    codexThreadState: session.codexThreadState,
    cursorMode: session.cursorMode,
    externalSessionId: session.externalSessionId,
    geminiApprovalMode: session.geminiApprovalMode,
    id: session.id,
    model: session.model,
    modelOptions: nextModelOptions,
    name: session.name,
    projectId: session.projectId,
    reasoningEffort: session.reasoningEffort,
    sandboxMode: session.sandboxMode,
    status: session.status,
    workdir: session.workdir,
  };
}

export function syncComposerSessionsStore({
  sessions,
  draftsBySessionId,
  draftAttachmentsBySessionId,
}: SyncComposerSessionsParams) {
  const previousComposerById = currentState.composerSessionsById;
  const previousRecordsById = currentState.sessionRecordsById;
  const previousSummaryById = currentState.sessionSummariesById;
  let nextComposerById: Record<string, ComposerSessionSnapshot> | null = null;
  let nextRecordsById: Record<string, Session> | null = null;
  let nextSummaryById: Record<string, SessionSummarySnapshot> | null = null;
  const seenSessionIds = new Set<string>();

  sessions.forEach((session) => {
    seenSessionIds.add(session.id);
    const previousComposer = previousComposerById[session.id];
    const nextComposer = buildComposerSessionSnapshot(
      session,
      draftsBySessionId[session.id] ?? "",
      draftAttachmentsBySessionId[session.id] ?? [],
      previousRecordsById[session.id],
      previousComposer,
    );
    if (nextComposer !== previousComposer) {
      nextComposerById ??= { ...previousComposerById };
      nextComposerById[session.id] = nextComposer;
    }

    const previousRecord = previousRecordsById[session.id];
    if (previousRecord !== session) {
      nextRecordsById ??= { ...previousRecordsById };
      nextRecordsById[session.id] = session;
    }

    const previousSummary = previousSummaryById[session.id];
    const nextSummary = buildSessionSummarySnapshot(session, previousSummary);
    if (nextSummary !== previousSummary) {
      nextSummaryById ??= { ...previousSummaryById };
      nextSummaryById[session.id] = nextSummary;
    }
  });

  Object.keys(previousComposerById).forEach((sessionId) => {
    if (seenSessionIds.has(sessionId)) {
      return;
    }
    nextComposerById ??= { ...previousComposerById };
    delete nextComposerById[sessionId];
  });

  Object.keys(previousRecordsById).forEach((sessionId) => {
    if (seenSessionIds.has(sessionId)) {
      return;
    }
    nextRecordsById ??= { ...previousRecordsById };
    delete nextRecordsById[sessionId];
  });

  Object.keys(previousSummaryById).forEach((sessionId) => {
    if (seenSessionIds.has(sessionId)) {
      return;
    }
    nextSummaryById ??= { ...previousSummaryById };
    delete nextSummaryById[sessionId];
  });

  if (!nextComposerById && !nextRecordsById && !nextSummaryById) {
    return;
  }

  currentState = {
    composerSessionsById:
      nextComposerById
        ? (Object.keys(nextComposerById).length === 0
            ? EMPTY_COMPOSER_SESSIONS_BY_ID
            : nextComposerById)
        : previousComposerById,
    sessionRecordsById:
      nextRecordsById
        ? (Object.keys(nextRecordsById).length === 0
            ? EMPTY_SESSION_RECORDS_BY_ID
            : nextRecordsById)
        : previousRecordsById,
    sessionSummariesById:
      nextSummaryById
        ? (Object.keys(nextSummaryById).length === 0
            ? EMPTY_SESSION_SUMMARIES_BY_ID
            : nextSummaryById)
        : previousSummaryById,
  };
  emitStoreChange();
}

export function syncComposerSessionsStoreIncremental({
  changedSessions,
  draftsBySessionId,
  draftAttachmentsBySessionId,
  removedSessionIds = [],
}: SyncComposerSessionsStoreIncrementalParams) {
  const previousComposerById = currentState.composerSessionsById;
  const previousRecordsById = currentState.sessionRecordsById;
  const previousSummaryById = currentState.sessionSummariesById;
  let nextComposerById: Record<string, ComposerSessionSnapshot> | null = null;
  let nextRecordsById: Record<string, Session> | null = null;
  let nextSummaryById: Record<string, SessionSummarySnapshot> | null = null;

  changedSessions.forEach((session) => {
    const previousComposer = previousComposerById[session.id];
    const previousRecord = previousRecordsById[session.id];
    const previousSummary = previousSummaryById[session.id];
    const nextComposer = buildComposerSessionSnapshot(
      session,
      draftsBySessionId[session.id] ?? "",
      draftAttachmentsBySessionId[session.id] ?? [],
      previousRecord,
      previousComposer,
    );
    const nextSummary = buildSessionSummarySnapshot(session, previousSummary);

    if (nextComposer !== previousComposer) {
      nextComposerById ??= { ...previousComposerById };
      nextComposerById[session.id] = nextComposer;
    }
    if (previousRecord !== session) {
      nextRecordsById ??= { ...previousRecordsById };
      nextRecordsById[session.id] = session;
    }
    if (nextSummary !== previousSummary) {
      nextSummaryById ??= { ...previousSummaryById };
      nextSummaryById[session.id] = nextSummary;
    }
  });

  removedSessionIds.forEach((sessionId) => {
    if (sessionId in previousComposerById) {
      nextComposerById ??= { ...previousComposerById };
      delete nextComposerById[sessionId];
    }
    if (sessionId in previousRecordsById) {
      nextRecordsById ??= { ...previousRecordsById };
      delete nextRecordsById[sessionId];
    }
    if (sessionId in previousSummaryById) {
      nextSummaryById ??= { ...previousSummaryById };
      delete nextSummaryById[sessionId];
    }
  });

  if (!nextComposerById && !nextRecordsById && !nextSummaryById) {
    return;
  }

  currentState = {
    composerSessionsById:
      nextComposerById
        ? (Object.keys(nextComposerById).length === 0
            ? EMPTY_COMPOSER_SESSIONS_BY_ID
            : nextComposerById)
        : previousComposerById,
    sessionRecordsById:
      nextRecordsById
        ? (Object.keys(nextRecordsById).length === 0
            ? EMPTY_SESSION_RECORDS_BY_ID
            : nextRecordsById)
        : previousRecordsById,
    sessionSummariesById:
      nextSummaryById
        ? (Object.keys(nextSummaryById).length === 0
            ? EMPTY_SESSION_SUMMARIES_BY_ID
            : nextSummaryById)
        : previousSummaryById,
  };
  emitStoreChange();
}

export function upsertSessionStoreSession({
  session,
  committedDraft,
  draftAttachments,
}: UpsertSessionStoreSessionParams) {
  const previousComposerById = currentState.composerSessionsById;
  const previousRecordsById = currentState.sessionRecordsById;
  const previousSummaryById = currentState.sessionSummariesById;
  const previousComposer = previousComposerById[session.id];
  const previousRecord = previousRecordsById[session.id];
  const previousSummary = previousSummaryById[session.id];
  const nextComposer = buildComposerSessionSnapshot(
    session,
    committedDraft,
    draftAttachments,
    previousRecord,
    previousComposer,
  );
  const nextSummary = buildSessionSummarySnapshot(session, previousSummary);

  if (
    previousComposer === nextComposer &&
    previousRecord === session &&
    previousSummary === nextSummary
  ) {
    return;
  }

  currentState = {
    composerSessionsById:
      previousComposer === nextComposer
        ? previousComposerById
        : {
            ...previousComposerById,
            [session.id]: nextComposer,
          },
    sessionRecordsById:
      previousRecord === session
        ? previousRecordsById
        : {
            ...previousRecordsById,
            [session.id]: session,
          },
    sessionSummariesById:
      previousSummary === nextSummary
        ? previousSummaryById
        : {
            ...previousSummaryById,
            [session.id]: nextSummary,
          },
  };
  emitStoreChange();
}

export function syncComposerDraftForSession({
  sessionId,
  committedDraft,
  draftAttachments,
}: SyncComposerDraftForSessionParams) {
  const session = currentState.sessionRecordsById[sessionId];
  if (!session) {
    return;
  }

  const previousComposerById = currentState.composerSessionsById;
  const previousComposer = previousComposerById[sessionId];
  const nextComposer = buildComposerSessionSnapshot(
    session,
    committedDraft,
    draftAttachments,
    session,
    previousComposer,
  );
  if (nextComposer === previousComposer) {
    return;
  }

  currentState = {
    ...currentState,
    composerSessionsById: {
      ...previousComposerById,
      [sessionId]: nextComposer,
    },
  };
  emitStoreChange();
}

export function useSessionSummariesById() {
  return useSyncExternalStore(
    subscribe,
    () => currentState.sessionSummariesById,
    () => currentState.sessionSummariesById,
  );
}

export function useSessionSummarySnapshot(sessionId: string | null) {
  return useSyncExternalStore(
    subscribe,
    () => (sessionId ? currentState.sessionSummariesById[sessionId] ?? null : null),
    () => (sessionId ? currentState.sessionSummariesById[sessionId] ?? null : null),
  );
}

export function useSessionRecordSnapshot(sessionId: string | null) {
  return useSyncExternalStore(
    subscribe,
    () => (sessionId ? currentState.sessionRecordsById[sessionId] ?? null : null),
    () => (sessionId ? currentState.sessionRecordsById[sessionId] ?? null : null),
  );
}

export function useComposerSessionSnapshot(sessionId: string | null) {
  return useSyncExternalStore(
    subscribe,
    () => (sessionId ? currentState.composerSessionsById[sessionId] ?? null : null),
    () => (sessionId ? currentState.composerSessionsById[sessionId] ?? null : null),
  );
}

export function resetSessionStoreForTesting() {
  currentState = INITIAL_STATE;
  emitStoreChange();
}

export function getComposerSessionSnapshotForTesting(sessionId: string) {
  return currentState.composerSessionsById[sessionId] ?? null;
}

export function getSessionRecordSnapshotForTesting(sessionId: string) {
  return currentState.sessionRecordsById[sessionId] ?? null;
}

export function getSessionSummarySnapshotForTesting(sessionId: string) {
  return currentState.sessionSummariesById[sessionId] ?? null;
}
