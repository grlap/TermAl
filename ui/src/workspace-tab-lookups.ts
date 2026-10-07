// Owns workspace tab lookups and returns the first matching tab and pane.
// Workspace reducers use these helpers to reuse existing tab objects.

import {
  normalizeWorkspaceIdentifier,
  normalizeWorkspacePath,
} from "./workspace-normalize";
import type {
  WorkspaceCanvasTab,
  WorkspaceControlPanelTab,
  WorkspaceDiffPreviewTab,
  WorkspaceFilesystemTab,
  WorkspaceGitStatusTab,
  WorkspaceInstructionDebuggerTab,
  WorkspaceMailboxTab,
  WorkspaceOrchestratorListTab,
  WorkspaceProjectListTab,
  WorkspaceResponseBoardTab,
  WorkspaceSessionListTab,
  WorkspaceSessionTab,
  WorkspaceSourceTab,
  WorkspaceState,
  WorkspaceTerminalTab,
  WorkspaceTestRunsTab,
  WorkspaceWorkTab,
} from "./workspace-types";

export function findSessionTab(workspace: WorkspaceState, sessionId: string) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSessionTab =>
        candidate.kind === "session" && candidate.sessionId === sessionId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findSourceTab(workspace: WorkspaceState, path: string) {
  const normalizedPath = normalizeWorkspacePath(path);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSourceTab =>
        candidate.kind === "source" &&
        normalizeWorkspacePath(candidate.path) === normalizedPath,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findFilesystemTab(workspace: WorkspaceState, rootPath: string) {
  const normalizedRootPath = normalizeWorkspacePath(rootPath);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceFilesystemTab =>
        candidate.kind === "filesystem" &&
        normalizeWorkspacePath(candidate.rootPath) === normalizedRootPath,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findGitStatusTab(workspace: WorkspaceState, workdir: string) {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceGitStatusTab =>
        candidate.kind === "gitStatus" &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findTerminalTab(
  workspace: WorkspaceState,
  workdir: string,
  originSessionId: string | null,
  originProjectId: string | null,
) {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  const normalizedOriginSessionId =
    normalizeWorkspaceIdentifier(originSessionId);
  const normalizedOriginProjectId =
    normalizeWorkspaceIdentifier(originProjectId);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceTerminalTab =>
        candidate.kind === "terminal" &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir &&
        normalizeWorkspaceIdentifier(candidate.originSessionId) ===
          normalizedOriginSessionId &&
        normalizeWorkspaceIdentifier(candidate.originProjectId ?? null) ===
          normalizedOriginProjectId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findMailboxTab(workspace: WorkspaceState, mailboxId: string) {
  const normalizedMailboxId = normalizeWorkspaceIdentifier(mailboxId);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceMailboxTab =>
        candidate.kind === "mailbox" &&
        normalizeWorkspaceIdentifier(candidate.mailboxId) ===
          normalizedMailboxId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findResponseBoardTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceResponseBoardTab =>
        candidate.kind === "responseBoard",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }
  return null;
}

export function findWorkTab(workspace: WorkspaceState, kind: "work" | "testRuns") {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceWorkTab | WorkspaceTestRunsTab => candidate.kind === kind,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }
  return null;
}

export function findControlPanelTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceControlPanelTab =>
        candidate.kind === "controlPanel",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findOrchestratorListTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceOrchestratorListTab =>
        candidate.kind === "orchestratorList",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findCanvasTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceCanvasTab =>
        candidate.kind === "canvas",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findSessionListTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceSessionListTab =>
        candidate.kind === "sessionList",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findProjectListTab(workspace: WorkspaceState) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceProjectListTab =>
        candidate.kind === "projectList",
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findInstructionDebuggerTab(
  workspace: WorkspaceState,
  workdir: string | null,
  originSessionId: string | null,
) {
  const normalizedWorkdir = normalizeWorkspacePath(workdir);
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceInstructionDebuggerTab =>
        candidate.kind === "instructionDebugger" &&
        candidate.originSessionId === originSessionId &&
        normalizeWorkspacePath(candidate.workdir) === normalizedWorkdir,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}

export function findDiffPreviewTab(
  workspace: WorkspaceState,
  changeSetId: string | null,
  diffMessageId: string,
  originSessionId: string | null,
  originProjectId: string | null,
) {
  for (const pane of workspace.panes) {
    const tab = pane.tabs.find(
      (candidate): candidate is WorkspaceDiffPreviewTab =>
        candidate.kind === "diffPreview" &&
        (changeSetId
          ? (candidate.changeSetId ?? null) === changeSetId ||
            candidate.diffMessageId === diffMessageId
          : candidate.diffMessageId === diffMessageId) &&
        candidate.originSessionId === originSessionId &&
        (candidate.originProjectId ?? null) === originProjectId,
    );
    if (tab) {
      return { paneId: pane.id, tab };
    }
  }

  return null;
}
