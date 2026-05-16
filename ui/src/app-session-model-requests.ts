// Owns client-side model selection for new-session API requests.
// Does not own model option fetching, model validation UI, or session settings.
// Split from app-session-actions.ts to keep action orchestration smaller.

import { assertNever } from "./exhaustive";
import {
  isDefaultModelPreference,
  usesSessionModelPicker,
} from "./session-model-utils";
import type { AgentType } from "./types";

export type AppSessionDefaultModels = {
  Claude: string;
  Codex: string;
  Cursor: string;
  Gemini: string;
};

export function configuredDefaultModelForAgent(
  agent: AgentType,
  defaultModels: AppSessionDefaultModels,
): string {
  switch (agent) {
    case "Claude":
      return defaultModels.Claude;
    case "Codex":
      return defaultModels.Codex;
    case "Cursor":
      return defaultModels.Cursor;
    case "Gemini":
      return defaultModels.Gemini;
    default:
      return assertNever(agent, "Unhandled default model agent");
  }
}

export function requestedModelForNewSession(
  agent: AgentType,
  dialogModel: string,
  defaultModels: AppSessionDefaultModels,
): string | undefined {
  if (!usesSessionModelPicker(agent)) {
    return dialogModel.trim() || undefined;
  }

  const defaultModel = configuredDefaultModelForAgent(agent, defaultModels).trim();
  if (isDefaultModelPreference(defaultModel)) {
    return undefined;
  }

  // Send a configured default explicitly so an immediately-created session
  // observes optimistic settings changes before the backend save returns.
  return defaultModel;
}
