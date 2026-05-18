// Owns workspace value normalization helpers shared by tab factories and
// reducers. Deliberately does not own workspace state transitions; this was
// split out of `workspace.ts` so reducer logic can stay easier to scan.

import {
  WORKSPACE_CANVAS_DEFAULT_ZOOM,
  WORKSPACE_CANVAS_MAX_ZOOM,
  WORKSPACE_CANVAS_MIN_ZOOM,
  type OpenSourceTabOptions,
  type WorkspaceCanvasCard,
  type WorkspaceSourceFocus,
} from "./workspace-types";

export const EMPTY_WORKSPACE_SOURCE_FOCUS: WorkspaceSourceFocus = {
  line: null,
  column: null,
  token: null,
};

export function createOpenSourceFocus(options?: OpenSourceTabOptions | null) {
  const line = normalizeWorkspaceLineNumber(options?.line);
  if (!line) {
    return EMPTY_WORKSPACE_SOURCE_FOCUS;
  }

  return normalizeWorkspaceSourceFocus({
    line,
    column: options?.column ?? null,
    token: crypto.randomUUID(),
  });
}

export function normalizeWorkspaceSourceFocus(
  focus: Partial<WorkspaceSourceFocus> | null | undefined,
): WorkspaceSourceFocus {
  const line = normalizeWorkspaceLineNumber(focus?.line);
  if (!line) {
    return EMPTY_WORKSPACE_SOURCE_FOCUS;
  }

  const token = typeof focus?.token === "string" ? focus.token.trim() : "";
  return {
    line,
    column: normalizeWorkspaceLineNumber(focus?.column),
    token: token || null,
  };
}

export function sourceFocusProps(focus: WorkspaceSourceFocus) {
  if (!focus.line) {
    return {};
  }

  return {
    focusLineNumber: focus.line,
    ...(focus.column ? { focusColumnNumber: focus.column } : {}),
    ...(focus.token ? { focusToken: focus.token } : {}),
  };
}

export function normalizeWorkspaceLineNumber(value: number | null | undefined) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return null;
  }

  const normalized = Math.trunc(value);
  return normalized >= 1 ? normalized : null;
}

const WINDOWS_UNC_VERBATIM_PREFIX = "\\\\?\\UNC\\";
const WINDOWS_VERBATIM_PREFIX = "\\\\?\\";

export function normalizeWorkspacePath(path: string | null | undefined) {
  const trimmed = path?.trim();
  if (!trimmed) {
    return null;
  }
  if (trimmed.startsWith(WINDOWS_UNC_VERBATIM_PREFIX)) {
    return `\\\\${trimmed.slice(WINDOWS_UNC_VERBATIM_PREFIX.length)}`;
  }
  if (trimmed.startsWith(WINDOWS_VERBATIM_PREFIX)) {
    return trimmed.slice(WINDOWS_VERBATIM_PREFIX.length);
  }
  return trimmed;
}

export function normalizeWorkspaceIdentifier(value: string | null | undefined) {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

export function normalizeWorkspaceText(value: string | null | undefined) {
  if (typeof value !== "string") {
    return null;
  }

  return value.trim() ? value : null;
}

export function projectOriginProps(originProjectId: string | null) {
  return originProjectId ? { originProjectId } : {};
}

export function canvasZoomProps(zoom: number) {
  return zoom === WORKSPACE_CANVAS_DEFAULT_ZOOM ? {} : { zoom };
}

export function normalizeWorkspaceCanvasZoom(value: number | null | undefined) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return WORKSPACE_CANVAS_DEFAULT_ZOOM;
  }

  const clamped = Math.min(Math.max(value, WORKSPACE_CANVAS_MIN_ZOOM), WORKSPACE_CANVAS_MAX_ZOOM);
  return Math.round(clamped * 1000) / 1000;
}

export function normalizeWorkspaceCanvasCards(cards: readonly WorkspaceCanvasCard[]) {
  const seenSessionIds = new Set<string>();
  const normalizedCards: WorkspaceCanvasCard[] = [];

  for (const card of cards) {
    const normalizedCard = normalizeWorkspaceCanvasCard(card);
    if (!normalizedCard || seenSessionIds.has(normalizedCard.sessionId)) {
      continue;
    }

    seenSessionIds.add(normalizedCard.sessionId);
    normalizedCards.push(normalizedCard);
  }

  return normalizedCards;
}

export function normalizeWorkspaceCanvasCard(card: WorkspaceCanvasCard | null | undefined) {
  const sessionId = normalizeWorkspaceIdentifier(card?.sessionId);
  if (!sessionId) {
    return null;
  }

  return {
    sessionId,
    x: normalizeWorkspaceCanvasCoordinate(card?.x),
    y: normalizeWorkspaceCanvasCoordinate(card?.y),
  };
}

function normalizeWorkspaceCanvasCoordinate(value: number | null | undefined) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return 0;
  }

  return Math.round(value);
}
