// Owns optimistic session-settings projection and rollback helpers.
// Does not own API calls, action adoption, or React state setters.
// Split from ui/src/app-session-actions.ts.

import {
  normalizedCodexReasoningEffort,
  normalizedRequestedSessionModel,
} from "./session-model-utils";
import type {
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CursorMode,
  GeminiApprovalMode,
  SandboxMode,
  Session,
  SessionSettingsField,
  SessionSettingsValue,
} from "./types";

export function buildOptimisticSessionSettingsUpdate(
  session: Session,
  field: SessionSettingsField,
  value: SessionSettingsValue,
) {
  const normalizedModelValue =
    field === "model"
      ? normalizedRequestedSessionModel(session, value as string)
      : null;

  switch (session.agent) {
    case "Codex": {
      const nextModel = normalizedModelValue ?? session.model;
      const nextReasoningEffort =
        field === "reasoningEffort"
          ? (value as CodexReasoningEffort)
          : normalizedCodexReasoningEffort(session, nextModel);
      const nextSandboxMode =
        field === "sandboxMode" ? (value as SandboxMode) : session.sandboxMode;
      const nextApprovalPolicy =
        field === "approvalPolicy"
          ? (value as ApprovalPolicy)
          : session.approvalPolicy;

      if (
        nextModel === session.model &&
        nextReasoningEffort === session.reasoningEffort &&
        nextSandboxMode === session.sandboxMode &&
        nextApprovalPolicy === session.approvalPolicy
      ) {
        return session;
      }

      return {
        ...session,
        model: nextModel,
        reasoningEffort: nextReasoningEffort,
        sandboxMode: nextSandboxMode,
        approvalPolicy: nextApprovalPolicy,
      };
    }
    case "Cursor": {
      const nextModel = normalizedModelValue ?? session.model;
      const nextCursorMode =
        field === "cursorMode" ? (value as CursorMode) : session.cursorMode;

      if (
        nextModel === session.model &&
        nextCursorMode === session.cursorMode
      ) {
        return session;
      }

      return {
        ...session,
        model: nextModel,
        cursorMode: nextCursorMode,
      };
    }
    case "Claude": {
      const nextModel = normalizedModelValue ?? session.model;
      const nextClaudeApprovalMode =
        field === "claudeApprovalMode"
          ? (value as ClaudeApprovalMode)
          : session.claudeApprovalMode;
      const nextClaudeEffort =
        field === "claudeEffort"
          ? (value as ClaudeEffortLevel)
          : session.claudeEffort;

      if (
        nextModel === session.model &&
        nextClaudeApprovalMode === session.claudeApprovalMode &&
        nextClaudeEffort === session.claudeEffort
      ) {
        return session;
      }

      return {
        ...session,
        model: nextModel,
        claudeApprovalMode: nextClaudeApprovalMode,
        claudeEffort: nextClaudeEffort,
      };
    }
    case "Gemini": {
      const nextModel = normalizedModelValue ?? session.model;
      const nextGeminiApprovalMode =
        field === "geminiApprovalMode"
          ? (value as GeminiApprovalMode)
          : session.geminiApprovalMode;

      if (
        nextModel === session.model &&
        nextGeminiApprovalMode === session.geminiApprovalMode
      ) {
        return session;
      }

      return {
        ...session,
        model: nextModel,
        geminiApprovalMode: nextGeminiApprovalMode,
      };
    }
  }
}

export function rollbackOptimisticSessionSettingsUpdate(
  currentSession: Session,
  previousSession: Session,
  optimisticSession: Session,
) {
  let changed = false;
  const nextSession = { ...currentSession };

  if (
    currentSession.model === optimisticSession.model &&
    currentSession.model !== previousSession.model
  ) {
    nextSession.model = previousSession.model;
    changed = true;
  }
  if (
    currentSession.approvalPolicy === optimisticSession.approvalPolicy &&
    currentSession.approvalPolicy !== previousSession.approvalPolicy
  ) {
    nextSession.approvalPolicy = previousSession.approvalPolicy;
    changed = true;
  }
  if (
    currentSession.reasoningEffort === optimisticSession.reasoningEffort &&
    currentSession.reasoningEffort !== previousSession.reasoningEffort
  ) {
    nextSession.reasoningEffort = previousSession.reasoningEffort;
    changed = true;
  }
  if (
    currentSession.sandboxMode === optimisticSession.sandboxMode &&
    currentSession.sandboxMode !== previousSession.sandboxMode
  ) {
    nextSession.sandboxMode = previousSession.sandboxMode;
    changed = true;
  }
  if (
    currentSession.cursorMode === optimisticSession.cursorMode &&
    currentSession.cursorMode !== previousSession.cursorMode
  ) {
    nextSession.cursorMode = previousSession.cursorMode;
    changed = true;
  }
  if (
    currentSession.claudeApprovalMode === optimisticSession.claudeApprovalMode &&
    currentSession.claudeApprovalMode !== previousSession.claudeApprovalMode
  ) {
    nextSession.claudeApprovalMode = previousSession.claudeApprovalMode;
    changed = true;
  }
  if (
    currentSession.claudeEffort === optimisticSession.claudeEffort &&
    currentSession.claudeEffort !== previousSession.claudeEffort
  ) {
    nextSession.claudeEffort = previousSession.claudeEffort;
    changed = true;
  }
  if (
    currentSession.geminiApprovalMode === optimisticSession.geminiApprovalMode &&
    currentSession.geminiApprovalMode !== previousSession.geminiApprovalMode
  ) {
    nextSession.geminiApprovalMode = previousSession.geminiApprovalMode;
    changed = true;
  }

  return changed ? nextSession : currentSession;
}

export function sessionSupportsModelRefresh(agent: AgentType) {
  return (
    agent === "Claude" ||
    agent === "Codex" ||
    agent === "Cursor" ||
    agent === "Gemini"
  );
}
