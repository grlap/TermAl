import type { PaneViewMode, SessionPaneViewMode, WorkspaceTab } from "./workspace";

const DIFF_CHANGE_TYPES = ["edit", "create"] as const;
const SESSION_PANE_VIEW_MODES: readonly SessionPaneViewMode[] = [
  "session",
  "prompt",
  "commands",
  "diffs",
];
const PANE_VIEW_MODES: readonly PaneViewMode[] = [
  ...SESSION_PANE_VIEW_MODES,
  "canvas",
  "controlPanel",
  "orchestratorList",
  "orchestratorCanvas",
  "sessionList",
  "projectList",
  "source",
  "filesystem",
  "gitStatus",
  "instructionDebugger",
  "diffPreview",
];

export function isPaneViewMode(value: unknown): value is PaneViewMode {
  return PANE_VIEW_MODES.includes(value as PaneViewMode);
}

export function isSessionPaneViewMode(value: unknown): value is SessionPaneViewMode {
  return SESSION_PANE_VIEW_MODES.includes(value as SessionPaneViewMode);
}

export function isWorkspaceTab(value: unknown): value is WorkspaceTab {
  if (!isRecord(value) || !isString(value.id) || !isString(value.kind)) {
    return false;
  }

  switch (value.kind) {
    case "session":
      return isString(value.sessionId);
    case "source":
      return (
        isNullableString(value.path) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "filesystem":
      return (
        isNullableString(value.rootPath) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "gitStatus":
      return (
        isNullableString(value.workdir) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "controlPanel":
      return (
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "orchestratorList":
      return (
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "canvas":
      return (
        Array.isArray(value.cards) &&
        value.cards.every((card) => isWorkspaceCanvasCard(card)) &&
        isOptionalWorkspaceCanvasZoom(value.zoom) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "orchestratorCanvas":
      return (
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId) &&
        isOptionalNullableString(value.templateId) &&
        (typeof value.startMode === "undefined" || value.startMode === "new")
      );
    case "sessionList":
      return (
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "projectList":
      return (
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "instructionDebugger":
      return (
        isNullableString(value.workdir) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId)
      );
    case "diffPreview":
      return (
        isString(value.diff) &&
        isOptionalNullableString(value.changeSetId) &&
        isString(value.diffMessageId) &&
        isNullableString(value.filePath) &&
        isOptionalNullableString(value.language) &&
        isNullableString(value.originSessionId) &&
        isOptionalNullableString(value.originProjectId) &&
        isString(value.summary) &&
        isDiffChangeType(value.changeType)
      );
    default:
      return false;
  }
}

function isWorkspaceCanvasCard(value: unknown) {
  return (
    isRecord(value) &&
    isString(value.sessionId) &&
    typeof value.x === "number" &&
    Number.isFinite(value.x) &&
    typeof value.y === "number" &&
    Number.isFinite(value.y)
  );
}

function isOptionalWorkspaceCanvasZoom(value: unknown) {
  return typeof value === "undefined" || isWorkspaceCanvasZoom(value);
}

function isWorkspaceCanvasZoom(value: unknown) {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

function isDiffChangeType(value: unknown): value is (typeof DIFF_CHANGE_TYPES)[number] {
  return DIFF_CHANGE_TYPES.includes(value as (typeof DIFF_CHANGE_TYPES)[number]);
}

function isOptionalNullableString(value: unknown): value is string | null | undefined {
  return typeof value === "undefined" || isNullableString(value);
}

function isNullableString(value: unknown): value is string | null {
  return value === null || isString(value);
}

function isString(value: unknown): value is string {
  return typeof value === "string";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
