// Owns client-side construction of session-settings API payloads.
// Does not own optimistic UI projection, rollback, or action-state adoption;
// see app-session-settings-optimism.ts for the sibling local projection path.
// Split from app-session-actions.ts to keep action orchestration smaller.

import { assertNever } from "./exhaustive";
import { codexModelSupportsFast } from "./session-model-options";
import {
  normalizedCodexReasoningEffort,
  normalizedRequestedSessionModel,
} from "./session-model-utils";
import type {
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

export type SessionSettingsPayload = {
  model?: string;
  sandboxMode?: SandboxMode;
  approvalPolicy?: ApprovalPolicy;
  claudeEffort?: ClaudeEffortLevel;
  reasoningEffort?: CodexReasoningEffort;
  codexFastMode?: boolean;
  cursorMode?: CursorMode;
  claudeApprovalMode?: ClaudeApprovalMode;
  geminiApprovalMode?: GeminiApprovalMode;
  opencodeEffort?: string;
  opencodeMode?: string;
};

export function buildSessionSettingsPayload(
  session: Session,
  field: SessionSettingsField,
  value: SessionSettingsValue,
): SessionSettingsPayload | null {
  const normalizedModelValue =
    field === "model"
      ? normalizedRequestedSessionModel(session, value as string)
      : null;

  switch (session.agent) {
    case "Codex": {
      const nextModel =
        field === "model"
          ? (normalizedModelValue ?? (value as string))
          : session.model;
      return {
        ...(field === "model" ? { model: nextModel } : {}),
        codexFastMode:
          field === "codexFastMode"
            ? (value as string) === "on"
            : field === "model" && nextModel !== session.model
              ? Boolean(session.codexFastMode && codexModelSupportsFast(session, nextModel))
              : Boolean(session.codexFastMode),
        reasoningEffort:
          field === "reasoningEffort"
            ? (value as CodexReasoningEffort)
            : normalizedCodexReasoningEffort(
                session,
                field === "model"
                  ? (normalizedModelValue ?? (value as string))
                  : session.model,
              ),
        sandboxMode:
          field === "sandboxMode"
            ? (value as SandboxMode)
            : (session.sandboxMode ?? "workspace-write"),
        approvalPolicy:
          field === "approvalPolicy"
            ? (value as ApprovalPolicy)
            : (session.approvalPolicy ?? "never"),
      };
    }
    case "Cursor":
      if (field === "model") {
        return { model: normalizedModelValue ?? (value as string) };
      }
      if (field === "cursorMode") {
        return { cursorMode: value as CursorMode };
      }
      return null;
    case "Claude":
      if (field === "model") {
        return { model: normalizedModelValue ?? (value as string) };
      }
      if (field === "claudeApprovalMode") {
        return { claudeApprovalMode: value as ClaudeApprovalMode };
      }
      if (field === "claudeEffort") {
        return { claudeEffort: value as ClaudeEffortLevel };
      }
      return null;
    case "Gemini":
      if (field === "model") {
        return { model: normalizedModelValue ?? (value as string) };
      }
      if (field === "geminiApprovalMode") {
        return { geminiApprovalMode: value as GeminiApprovalMode };
      }
      return null;
    case "OpenCode":
      if (field === "model") {
        return { model: normalizedModelValue ?? (value as string) };
      }
      if (field === "opencodeEffort") {
        return { opencodeEffort: value as string };
      }
      if (field === "opencodeMode") {
        return { opencodeMode: value as string };
      }
      return null;
    default:
      return assertNever(session.agent, "Unhandled session settings agent");
  }
}
