// Owns slash-command submission resolution, keyboard routing decisions, and
// safe resolver-error messages.
// Does not own slash-palette state construction or composer rendering; those
// remain in session-slash-palette.ts and AgentSessionPanel.tsx respectively.
// Split from AgentSessionPanel.tsx to keep the panel focused on composition.

import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  ApiRequestError,
  type ResolveAgentCommandResponse,
} from "../api";
import { formatUserFacingError } from "../error-messages";
import {
  parseAgentCommandDraft,
  type SlashPaletteItem,
} from "./session-slash-palette";

type AgentCommandSlashPaletteItem = Extract<
  SlashPaletteItem,
  { kind: "agent-command" }
>;

type AgentCommandSubmissionResolution =
  | { kind: "expand"; nextDraft: string }
  | {
      argumentsText: string;
      commandName: string;
      kind: "submit";
      noteText?: string;
    };

const AGENT_COMMAND_NOTE_SEPARATOR_PATTERN = /(^|\s)--(?=\s|$)/u;

/** @internal Exported for focused parser regression tests. */
export function splitAgentCommandResolverTail(argumentsText: string): {
  argumentsText: string;
  noteText?: string;
} {
  const trimmed = argumentsText.trim();
  if (!trimmed) {
    return { argumentsText: "" };
  }

  const separatorMatch = AGENT_COMMAND_NOTE_SEPARATOR_PATTERN.exec(trimmed);
  if (!separatorMatch) {
    return { argumentsText: trimmed };
  }

  const separatorStart = separatorMatch.index + (separatorMatch[1]?.length ?? 0);
  const noteText = trimmed.slice(separatorStart + 2).trim();
  return {
    argumentsText: trimmed.slice(0, separatorMatch.index).trim(),
    ...(noteText ? { noteText } : {}),
  };
}

export function prepareAgentCommandSubmission(
  item: AgentCommandSlashPaletteItem,
  draft: string,
): AgentCommandSubmissionResolution {
  const parsedDraft = parseAgentCommandDraft(draft);
  const matchesSelectedCommand =
    parsedDraft?.commandName.toLowerCase() === item.name.toLowerCase();
  if (item.hasArguments && !matchesSelectedCommand) {
    return { kind: "expand", nextDraft: `/${item.name} ` };
  }

  const { argumentsText, noteText } = splitAgentCommandResolverTail(
    matchesSelectedCommand ? (parsedDraft?.argumentsText ?? "") : "",
  );

  return {
    argumentsText,
    commandName: item.name,
    kind: "submit",
    ...(noteText ? { noteText } : {}),
  };
}

export function sendResolvedAgentCommandSubmission(
  onSend: (
    sessionId: string,
    draftText?: string,
    expandedText?: string | null,
  ) => boolean,
  sessionId: string,
  resolution: ResolveAgentCommandResponse,
) {
  return resolution.expandedPrompt == null
    ? onSend(sessionId, resolution.visiblePrompt)
    : onSend(sessionId, resolution.visiblePrompt, resolution.expandedPrompt);
}

export function formatAgentCommandResolverError(error: unknown) {
  const hasSensitiveDetail = containsLikelySensitiveResolverDetail(error);
  if (
    error instanceof ApiRequestError &&
    error.kind === "request-failed" &&
    (error.status === null || error.status >= 500 || hasSensitiveDetail)
  ) {
    return "Could not resolve the slash command. Check the command file and try again.";
  }

  if (hasSensitiveDetail) {
    return "Could not resolve the slash command. Check the command file and try again.";
  }

  return formatUserFacingError(error);
}

function containsLikelySensitiveResolverDetail(
  value: unknown,
  seen = new Set<object>(),
): boolean {
  if (typeof value === "string") {
    return containsLikelySensitiveResolverText(value);
  }
  if (value == null || typeof value !== "object") {
    return false;
  }
  if (seen.has(value)) {
    return false;
  }
  seen.add(value);
  if (value instanceof Error) {
    return (
      containsLikelySensitiveResolverText(value.message) ||
      containsLikelySensitiveResolverDetail(
        (value as { cause?: unknown }).cause,
        seen,
      )
    );
  }
  const possibleError = value as { cause?: unknown; message?: unknown };
  return (
    containsLikelySensitiveResolverDetail(possibleError.message, seen) ||
    containsLikelySensitiveResolverDetail(possibleError.cause, seen)
  );
}

function containsLikelySensitiveResolverText(value: string) {
  return (
    /\b[A-Za-z]:[\\/][^\s"'<>]+/u.test(value) ||
    /\\\\[^\\\s]+\\/u.test(value) ||
    /(?:^|[\s=:'"([{])~[\\/][^\s"'<>]+/u.test(value) ||
    /(?:^|[\s=:'"([{])\.{1,2}[\\/][^\s"'<>]+/u.test(value) ||
    /(?:^|[\s=:'"([{])\/(?:[A-Za-z]\/|Users|home|tmp|var|private|mnt|Volumes|etc|opt|root|srv|usr|proc|sys|run|workspace|workspaces|app|repo|repos|project|projects|build|source|src|code)\b/u.test(
      value,
    ) ||
    /\b(?:sk-[A-Za-z0-9_-]{8,}|sk_(?:live|test)_[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9_]{8,}|glpat-[A-Za-z0-9_-]{8,}|xox[baprs]-[A-Za-z0-9_-]{8,}|AKIA[A-Z0-9]{12,}|AIza[0-9A-Za-z_-]{20,}|Bearer\s+[A-Za-z0-9._~+/=-]+)/u.test(
      value,
    ) ||
    /-----BEGIN [A-Z ]*PRIVATE KEY-----/u.test(value) ||
    /\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b/u.test(value) ||
    /\b(?:token|password|passwd|secret|api[_-]?key)=\S+/iu.test(value)
  );
}

function slashPaletteTabAction(
  canDelegateActiveSlashCommand: boolean,
  canSpawnDelegation: boolean,
  hasSpawnDelegationHandler: boolean,
  composerDelegateDisabled: boolean,
) {
  if (!canDelegateActiveSlashCommand) {
    return "submit";
  }
  if (!canSpawnDelegation || !hasSpawnDelegationHandler) {
    return "submit";
  }
  if (composerDelegateDisabled) {
    return "none";
  }
  return "focusDelegate";
}

function slashPaletteKeyAction(
  event: ReactKeyboardEvent<HTMLTextAreaElement>,
  canDelegateActiveSlashCommand: boolean,
  canSpawnDelegation: boolean,
  hasSpawnDelegationHandler: boolean,
  composerDelegateDisabled: boolean,
) {
  if (event.key === "Enter" && !event.shiftKey) {
    return "submit";
  }
  if (event.key === "Tab" && !event.shiftKey) {
    return slashPaletteTabAction(
      canDelegateActiveSlashCommand,
      canSpawnDelegation,
      hasSpawnDelegationHandler,
      composerDelegateDisabled,
    );
  }
  return "none";
}

export function shouldFocusDelegateWithSlashPaletteKey(
  event: ReactKeyboardEvent<HTMLTextAreaElement>,
  canDelegateActiveSlashCommand: boolean,
  canSpawnDelegation: boolean,
  hasSpawnDelegationHandler: boolean,
  composerDelegateDisabled: boolean,
) {
  return (
    slashPaletteKeyAction(
      event,
      canDelegateActiveSlashCommand,
      canSpawnDelegation,
      hasSpawnDelegationHandler,
      composerDelegateDisabled,
    ) === "focusDelegate"
  );
}

export function shouldSubmitSlashPaletteKey(
  event: ReactKeyboardEvent<HTMLTextAreaElement>,
  canDelegateActiveSlashCommand: boolean,
  canSpawnDelegation: boolean,
  hasSpawnDelegationHandler: boolean,
  composerDelegateDisabled: boolean,
) {
  return (
    slashPaletteKeyAction(
      event,
      canDelegateActiveSlashCommand,
      canSpawnDelegation,
      hasSpawnDelegationHandler,
      composerDelegateDisabled,
    ) === "submit"
  );
}
