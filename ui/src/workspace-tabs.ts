// Owns workspace tab factory functions and their input normalization.
// Deliberately does not own reducers or pane tree mutations; this was split
// out of `workspace.ts` so state-transition code remains focused.

import type {
  GitDiffDocumentContent,
  GitDiffRequestPayload,
  GitDiffSection,
} from "./api";
import type { DiffMessage } from "./types";
import {
  EMPTY_WORKSPACE_SOURCE_FOCUS,
  canvasZoomProps,
  normalizeWorkspaceCanvasCards,
  normalizeWorkspaceCanvasZoom,
  normalizeWorkspaceIdentifier,
  normalizeWorkspacePath,
  normalizeWorkspaceSourceFocus,
  normalizeWorkspaceText,
  projectOriginProps,
  sourceFocusProps,
} from "./workspace-normalize";
import {
  WORKSPACE_CANVAS_DEFAULT_ZOOM,
  type WorkspaceCanvasCard,
  type WorkspaceCanvasTab,
  type WorkspaceControlPanelTab,
  type WorkspaceDiffPreviewTab,
  type WorkspaceFilesystemTab,
  type WorkspaceGitStatusTab,
  type WorkspaceInstructionDebuggerTab,
  type WorkspaceOrchestratorCanvasTab,
  type WorkspaceOrchestratorListTab,
  type WorkspaceProjectListTab,
  type WorkspaceSessionListTab,
  type WorkspaceSessionTab,
  type WorkspaceSourceFocus,
  type WorkspaceSourceTab,
  type WorkspaceTerminalTab,
} from "./workspace-types";

export function createSessionTab(sessionId: string): WorkspaceSessionTab {
  return {
    id: crypto.randomUUID(),
    kind: "session",
    sessionId,
  };
}

export function createSourceTab(
  path: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
  focus: WorkspaceSourceFocus = EMPTY_WORKSPACE_SOURCE_FOCUS,
): WorkspaceSourceTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  const normalizedFocus = normalizeWorkspaceSourceFocus(focus);

  return {
    id: crypto.randomUUID(),
    kind: "source",
    path: normalizeWorkspacePath(path),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
    ...sourceFocusProps(normalizedFocus),
  };
}

export function createFilesystemTab(
  rootPath: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceFilesystemTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "filesystem",
    rootPath: normalizeWorkspacePath(rootPath),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createGitStatusTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceGitStatusTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "gitStatus",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createTerminalTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceTerminalTab {
  const normalizedOriginSessionId = normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "terminal",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId: normalizedOriginSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createControlPanelTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceControlPanelTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "controlPanel",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createOrchestratorListTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceOrchestratorListTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "orchestratorList",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createCanvasTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
  cards: readonly WorkspaceCanvasCard[] = [],
  zoom: number = WORKSPACE_CANVAS_DEFAULT_ZOOM,
): WorkspaceCanvasTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "canvas",
    cards: normalizeWorkspaceCanvasCards(cards),
    ...canvasZoomProps(normalizeWorkspaceCanvasZoom(zoom)),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createOrchestratorCanvasTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
  templateId: string | null = null,
  startMode: "new" | null = null,
): WorkspaceOrchestratorCanvasTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  const normalizedTemplateId = normalizeWorkspaceIdentifier(templateId);

  return {
    id: crypto.randomUUID(),
    kind: "orchestratorCanvas",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
    ...(normalizedTemplateId ? { templateId: normalizedTemplateId } : {}),
    ...(startMode === "new" ? { startMode } : {}),
  };
}

export function createSessionListTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceSessionListTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "sessionList",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createProjectListTab(
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceProjectListTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "projectList",
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createInstructionDebuggerTab(
  workdir: string | null = null,
  originSessionId: string | null = null,
  originProjectId: string | null = null,
): WorkspaceInstructionDebuggerTab {
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);

  return {
    id: crypto.randomUUID(),
    kind: "instructionDebugger",
    workdir: normalizeWorkspacePath(workdir),
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
  };
}

export function createDiffPreviewTab({
  changeType,
  changeSetId = null,
  diff,
  documentEnrichmentNote = null,
  documentContent = null,
  diffMessageId,
  filePath = null,
  gitSectionId = null,
  language = null,
  originSessionId = null,
  originProjectId = null,
  summary,
  gitDiffRequestKey = null,
  gitDiffRequest = null,
  isLoading = false,
  loadError = null,
}: {
  changeType: DiffMessage["changeType"];
  changeSetId?: string | null;
  diff: string;
  documentEnrichmentNote?: string | null;
  documentContent?: GitDiffDocumentContent | null;
  diffMessageId: string;
  filePath?: string | null;
  gitSectionId?: GitDiffSection | null;
  language?: string | null;
  originSessionId?: string | null;
  originProjectId?: string | null;
  summary: string;
  gitDiffRequestKey?: string | null;
  gitDiffRequest?: GitDiffRequestPayload | null;
  isLoading?: boolean;
  loadError?: string | null;
}): WorkspaceDiffPreviewTab {
  const normalizedChangeSetId = normalizeWorkspaceIdentifier(changeSetId);
  const normalizedDocumentEnrichmentNote = normalizeWorkspaceText(documentEnrichmentNote);
  const normalizedOriginProjectId = normalizeWorkspaceIdentifier(originProjectId);
  const normalizedGitDiffRequestKey = normalizeWorkspaceIdentifier(gitDiffRequestKey);
  const normalizedLoadError = normalizeWorkspaceIdentifier(loadError);

  return {
    id: crypto.randomUUID(),
    kind: "diffPreview",
    changeType,
    ...(normalizedChangeSetId ? { changeSetId: normalizedChangeSetId } : {}),
    diff,
    ...(normalizedDocumentEnrichmentNote
      ? { documentEnrichmentNote: normalizedDocumentEnrichmentNote }
      : {}),
    ...(documentContent ? { documentContent } : {}),
    diffMessageId,
    filePath: normalizeWorkspacePath(filePath),
    ...(gitSectionId ? { gitSectionId } : {}),
    language,
    originSessionId,
    ...projectOriginProps(normalizedOriginProjectId),
    summary,
    ...(normalizedGitDiffRequestKey ? { gitDiffRequestKey: normalizedGitDiffRequestKey } : {}),
    ...(gitDiffRequest ? { gitDiffRequest } : {}),
    ...(isLoading ? { isLoading: true } : {}),
    ...(normalizedLoadError ? { loadError: normalizedLoadError } : {}),
  };
}
