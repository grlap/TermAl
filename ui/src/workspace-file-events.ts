import type {
  WorkspaceFileChange,
  WorkspaceFileChangeKind,
  WorkspaceFilesChangedEvent,
} from "./types";
import type { WorkspaceDiffPreviewTab } from "./workspace";

type WorkspaceFileChangeScope = {
  rootPath?: string | null;
  sessionId?: string | null;
};

export function normalizeWorkspaceFileEventPath(path: string) {
  const normalized = path.trim().replace(/\\+/g, "/").replace(/\/+/g, "/");
  if (!normalized) {
    return "";
  }

  const withoutTrailingSlash =
    normalized.length > 1 && !/^[a-z]:\/$/i.test(normalized)
      ? normalized.replace(/\/+$/g, "")
      : normalized;

  return /^[a-z]:\//i.test(withoutTrailingSlash)
    ? withoutTrailingSlash.toLowerCase()
    : withoutTrailingSlash;
}

export function workspacePathIsAbsolute(path: string) {
  const normalized = normalizeWorkspaceFileEventPath(path);
  return (
    normalized.startsWith("/") ||
    normalized.startsWith("//") ||
    /^[a-z]:\//i.test(normalized)
  );
}

export function joinWorkspacePath(
  rootPath: string | null | undefined,
  childPath: string | null | undefined,
) {
  const trimmedChild = childPath?.trim() ?? "";
  if (!trimmedChild || workspacePathIsAbsolute(trimmedChild)) {
    return trimmedChild;
  }

  const trimmedRoot = rootPath?.trim() ?? "";
  if (!trimmedRoot) {
    return trimmedChild;
  }

  return `${trimmedRoot.replace(/[\\/]+$/g, "")}/${trimmedChild.replace(/^[\\/]+/g, "")}`;
}

export function workspacePathContains(
  rootPath: string | null | undefined,
  candidatePath: string | null | undefined,
) {
  const normalizedRoot = normalizeWorkspaceFileEventPath(rootPath ?? "");
  const normalizedCandidate = normalizeWorkspaceFileEventPath(candidatePath ?? "");
  if (!normalizedRoot || !normalizedCandidate) {
    return false;
  }

  return (
    normalizedCandidate === normalizedRoot ||
    normalizedCandidate.startsWith(`${normalizedRoot}/`)
  );
}

function normalizedSessionId(sessionId: string | null | undefined) {
  return sessionId?.trim() ?? "";
}

function workspaceFileChangeMatchesScope(
  change: WorkspaceFileChange,
  scope?: WorkspaceFileChangeScope,
) {
  const targetSessionId = normalizedSessionId(scope?.sessionId);
  const changeSessionId = normalizedSessionId(change.sessionId);
  if (targetSessionId && changeSessionId && targetSessionId !== changeSessionId) {
    return false;
  }

  const targetRootPath = scope?.rootPath ?? null;
  const changeRootPath = change.rootPath ?? null;
  if (
    targetRootPath &&
    changeRootPath &&
    !workspacePathContains(targetRootPath, changeRootPath) &&
    !workspacePathContains(changeRootPath, targetRootPath)
  ) {
    return false;
  }

  return true;
}

export function workspaceFileChangeMatchesCandidate(
  changePath: string,
  candidatePath: string | null | undefined,
) {
  const normalizedChangePath = normalizeWorkspaceFileEventPath(changePath);
  const normalizedCandidatePath = normalizeWorkspaceFileEventPath(candidatePath ?? "");
  if (!normalizedChangePath || !normalizedCandidatePath) {
    return false;
  }

  if (normalizedChangePath === normalizedCandidatePath) {
    return true;
  }

  if (workspacePathIsAbsolute(candidatePath ?? "")) {
    return false;
  }

  return normalizedChangePath.endsWith(`/${normalizedCandidatePath}`);
}

export function workspaceFilesChangedEventChangeForPath(
  event: WorkspaceFilesChangedEvent,
  targetPath: string,
  scope?: WorkspaceFileChangeScope,
): WorkspaceFileChange | null {
  const normalizedTargetPath = normalizeWorkspaceFileEventPath(targetPath);
  if (!normalizedTargetPath) {
    return null;
  }

  return event.changes.find((change) => {
    if (!workspaceFileChangeMatchesScope(change, scope)) {
      return false;
    }

    return workspaceFileChangeMatchesCandidate(change.path, normalizedTargetPath);
  }) ?? null;
}

export function workspaceFilesChangedEventTouchesRoot(
  event: WorkspaceFilesChangedEvent,
  rootPath: string,
  scope?: WorkspaceFileChangeScope,
) {
  const normalizedRoot = normalizeWorkspaceFileEventPath(rootPath);
  if (!normalizedRoot) {
    return false;
  }

  return event.changes.some(
    (change) =>
      workspaceFileChangeMatchesScope(change, scope) &&
      workspacePathContains(normalizedRoot, change.path),
  );
}

export function workspaceFilesChangedEventTouchesGitDiffTab(
  event: WorkspaceFilesChangedEvent,
  tab: WorkspaceDiffPreviewTab,
) {
  const request = tab.gitDiffRequest ?? null;
  const candidates = [
    tab.filePath,
    request?.path,
    request?.originalPath,
    joinWorkspacePath(request?.workdir, request?.path),
    joinWorkspacePath(request?.workdir, request?.originalPath),
  ];

  return event.changes.some((change) =>
    workspaceFileChangeMatchesScope(change, {
      rootPath: request?.workdir ?? null,
      sessionId: tab.originSessionId,
    }) &&
    candidates.some((candidatePath) =>
      workspaceFileChangeMatchesCandidate(change.path, candidatePath),
    ),
  );
}

function mergeWorkspaceFileChangeKind(
  current: WorkspaceFileChangeKind,
  next: WorkspaceFileChangeKind,
): WorkspaceFileChangeKind {
  if (
    (current === "deleted" && next === "created") ||
    (current === "created" && next === "deleted")
  ) {
    return "modified";
  }
  if (current === "deleted" || next === "deleted") {
    return "deleted";
  }
  if (current === "created" || next === "created") {
    return "created";
  }
  if (current === "modified" || next === "modified") {
    return "modified";
  }
  return "other";
}

function workspaceFileChangeKey(change: WorkspaceFileChange) {
  return [
    normalizeWorkspaceFileEventPath(change.rootPath ?? ""),
    normalizedSessionId(change.sessionId),
    normalizeWorkspaceFileEventPath(change.path),
  ].join("\0");
}

export function mergeWorkspaceFilesChangedEvents(
  current: WorkspaceFilesChangedEvent | null,
  next: WorkspaceFilesChangedEvent,
): WorkspaceFilesChangedEvent {
  if (!current || next.revision <= current.revision) {
    return !current || next.revision > current.revision ? next : current;
  }

  const changesByKey = new Map<string, WorkspaceFileChange>();
  const orderedKeys: string[] = [];
  for (const change of [...current.changes, ...next.changes]) {
    if (!normalizeWorkspaceFileEventPath(change.path)) {
      continue;
    }

    const key = workspaceFileChangeKey(change);
    const existing = changesByKey.get(key);
    if (!existing) {
      orderedKeys.push(key);
      changesByKey.set(key, { ...change });
      continue;
    }

    changesByKey.set(key, {
      ...existing,
      ...change,
      kind: mergeWorkspaceFileChangeKind(existing.kind, change.kind),
    });
  }

  return {
    revision: next.revision,
    changes: orderedKeys
      .map((key) => changesByKey.get(key))
      .filter((change): change is WorkspaceFileChange => Boolean(change)),
  };
}
