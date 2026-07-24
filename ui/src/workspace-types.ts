// Owns workspace pane/tab data shapes and workspace-level constants.
// Deliberately does not own workspace reducers, normalization, or tab factory
// functions; those stay in focused sibling modules split from `workspace.ts`.

import type {
  GitDiffDocumentContent,
  GitDiffRequestPayload,
  GitDiffSection,
} from "./api";
import type { DiffMessage } from "./types";

export type SessionPaneViewMode = "session" | "prompt" | "commands" | "diffs";
export type PaneViewMode =
  | SessionPaneViewMode
  | "canvas"
  | "controlPanel"
  | "orchestratorList"
  | "orchestratorCanvas"
  | "sessionList"
  | "projectList"
  | "source"
  | "filesystem"
  | "gitStatus"
  | "terminal"
  | "instructionDebugger"
  | "diffPreview";

export type WorkspaceSessionTab = {
  id: string;
  kind: "session";
  sessionId: string;
};

export type WorkspaceSourceTab = {
  id: string;
  kind: "source";
  path: string | null;
  focusLineNumber?: number | null;
  focusColumnNumber?: number | null;
  focusToken?: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceFilesystemTab = {
  id: string;
  kind: "filesystem";
  rootPath: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceGitStatusTab = {
  id: string;
  kind: "gitStatus";
  workdir: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceTerminalTab = {
  id: string;
  kind: "terminal";
  workdir: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceControlPanelTab = {
  id: string;
  kind: "controlPanel";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceOrchestratorListTab = {
  id: string;
  kind: "orchestratorList";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceCanvasCard = {
  sessionId: string;
  x: number;
  y: number;
};

export type WorkspaceCanvasTab = {
  id: string;
  kind: "canvas";
  cards: WorkspaceCanvasCard[];
  zoom?: number;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceOrchestratorCanvasTab = {
  id: string;
  kind: "orchestratorCanvas";
  originSessionId: string | null;
  originProjectId?: string | null;
  templateId?: string | null;
  startMode?: "new";
};

export type WorkspaceSessionListTab = {
  id: string;
  kind: "sessionList";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceProjectListTab = {
  id: string;
  kind: "projectList";
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceInstructionDebuggerTab = {
  id: string;
  kind: "instructionDebugger";
  workdir: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
};

export type WorkspaceDiffPreviewTab = {
  id: string;
  kind: "diffPreview";
  changeType: DiffMessage["changeType"];
  changeSetId?: string | null;
  diff: string;
  documentEnrichmentNote?: string | null;
  documentContent?: GitDiffDocumentContent | null;
  diffMessageId: string;
  displayPath?: string | null;
  filePath: string | null;
  gitSectionId?: GitDiffSection | null;
  language?: string | null;
  originSessionId: string | null;
  originProjectId?: string | null;
  summary: string;
  gitDiffRequestKey?: string | null;
  gitDiffRequest?: GitDiffRequestPayload | null;
  isLoading?: boolean;
  loadError?: string | null;
};

export type WorkspaceTab =
  | WorkspaceSessionTab
  | WorkspaceSourceTab
  | WorkspaceFilesystemTab
  | WorkspaceGitStatusTab
  | WorkspaceTerminalTab
  | WorkspaceControlPanelTab
  | WorkspaceOrchestratorListTab
  | WorkspaceCanvasTab
  | WorkspaceOrchestratorCanvasTab
  | WorkspaceSessionListTab
  | WorkspaceProjectListTab
  | WorkspaceInstructionDebuggerTab
  | WorkspaceDiffPreviewTab;

export type WorkspaceOriginOnlyTab =
  | WorkspaceControlPanelTab
  | WorkspaceOrchestratorListTab
  | WorkspaceSessionListTab
  | WorkspaceProjectListTab;

export type WorkspacePane = {
  id: string;
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  activeSessionId: string | null;
  viewMode: PaneViewMode;
  lastSessionViewMode: SessionPaneViewMode;
  sourcePath: string | null;
};

export type WorkspaceSourceFocus = {
  line: number | null;
  column: number | null;
  token: string | null;
};

export type OpenSourceTabOptions = {
  line?: number | null;
  column?: number | null;
  openInNewTab?: boolean;
};

export type WorkspaceNode =
  | {
      type: "pane";
      paneId: string;
    }
  | {
      id: string;
      type: "split";
      direction: "row" | "column";
      ratio: number;
      first: WorkspaceNode;
      second: WorkspaceNode;
    };

export type WorkspaceState = {
  root: WorkspaceNode | null;
  panes: WorkspacePane[];
  activePaneId: string | null;
};

export type TabDropPlacement = "left" | "right" | "top" | "bottom" | "tabs";
export const DEFAULT_CONTROL_PANEL_DOCK_WIDTH_RATIO = 0.24;
export const WORKSPACE_CANVAS_DEFAULT_ZOOM = 1;
export const WORKSPACE_CANVAS_MIN_ZOOM = 0.5;
export const WORKSPACE_CANVAS_MAX_ZOOM = 2;
