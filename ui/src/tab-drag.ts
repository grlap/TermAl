import type { WorkspaceTab } from "./workspace";
import { isWorkspaceTab } from "./workspace-tab-validation";

export const TAB_DRAG_CHANNEL_NAME = "termal-workspace-tab-drag";
export const TAB_DRAG_MIME_TYPE = "application/x-termal-workspace-tab";

export type WorkspaceTabDrag = {
  dragId: string;
  sourceWindowId: string;
  sourcePaneId: string;
  tabId: string;
  tab: WorkspaceTab;
};

export type WorkspaceTabDragChannelMessage =
  | {
      type: "drag-start";
      payload: WorkspaceTabDrag;
    }
  | {
      type: "drag-end";
      dragId: string;
      sourceWindowId: string;
    }
  | {
      type: "drop-commit";
      dragId: string;
      sourceWindowId: string;
      sourcePaneId: string;
      tabId: string;
      targetWindowId: string;
    };

export function createWorkspaceTabDrag(
  sourceWindowId: string,
  sourcePaneId: string,
  tab: WorkspaceTab,
): WorkspaceTabDrag {
  return {
    dragId: crypto.randomUUID(),
    sourceWindowId,
    sourcePaneId,
    tabId: tab.id,
    tab,
  };
}

export function attachWorkspaceTabDragData(
  dataTransfer: Pick<DataTransfer, "setData">,
  drag: WorkspaceTabDrag,
) {
  dataTransfer.setData(TAB_DRAG_MIME_TYPE, JSON.stringify(drag));
  dataTransfer.setData("text/plain", formatWorkspaceTabDragLabel(drag));
}

export function readWorkspaceTabDragData(
  dataTransfer: Pick<DataTransfer, "getData"> | null | undefined,
): WorkspaceTabDrag | null {
  const raw = dataTransfer?.getData(TAB_DRAG_MIME_TYPE) ?? "";
  if (!raw) {
    return null;
  }

  try {
    const parsed: unknown = JSON.parse(raw);
    return isWorkspaceTabDrag(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export function isWorkspaceTabDragChannelMessage(
  value: unknown,
): value is WorkspaceTabDragChannelMessage {
  if (!isRecord(value) || typeof value.type !== "string") {
    return false;
  }

  switch (value.type) {
    case "drag-start":
      return isWorkspaceTabDrag(value.payload);
    case "drag-end":
      return typeof value.dragId === "string" && typeof value.sourceWindowId === "string";
    case "drop-commit":
      return (
        typeof value.dragId === "string" &&
        typeof value.sourceWindowId === "string" &&
        typeof value.sourcePaneId === "string" &&
        typeof value.tabId === "string" &&
        typeof value.targetWindowId === "string"
      );
    default:
      return false;
  }
}

function formatWorkspaceTabDragLabel(drag: WorkspaceTabDrag) {
  switch (drag.tab.kind) {
    case "session":
      return `TermAl tab ${drag.tab.sessionId}`;
    case "source":
      return `TermAl file ${drag.tab.path ?? "untitled"}`;
    case "filesystem":
      return `TermAl files ${drag.tab.rootPath ?? "workspace"}`;
    case "gitStatus":
      return `TermAl git ${drag.tab.workdir ?? "workspace"}`;
    case "controlPanel":
      return "TermAl control panel";
    case "orchestratorList":
      return "TermAl orchestrators";
    case "canvas":
      return "TermAl canvas";
    case "orchestratorCanvas":
      return drag.tab.templateId
        ? `TermAl orchestration ${drag.tab.templateId}`
        : "TermAl orchestration canvas";
    case "sessionList":
      return "TermAl sessions";
    case "projectList":
      return "TermAl projects";
    case "instructionDebugger":
      return `TermAl instructions ${drag.tab.workdir ?? "workspace"}`;
    case "diffPreview":
      return `TermAl diff ${drag.tab.filePath ?? drag.tab.summary}`;
  }
}

function isWorkspaceTabDrag(value: unknown): value is WorkspaceTabDrag {
  return (
    isRecord(value) &&
    typeof value.dragId === "string" &&
    typeof value.sourceWindowId === "string" &&
    typeof value.sourcePaneId === "string" &&
    typeof value.tabId === "string" &&
    isWorkspaceTab(value.tab)
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
