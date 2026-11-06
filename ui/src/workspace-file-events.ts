import type {
  WorkspaceFileChange,
  WorkspaceFileChangeKind,
  WorkspaceFilesChangedEvent,
} from "./types";
import type { WorkspaceDiffPreviewTab } from "./workspace-types";

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
    normalizedCandidate.startsWith(normalizedRoot.endsWith("/") ? normalizedRoot : `${normalizedRoot}/`)
  );
}

function normalizedSessionId(sessionId: string | null | undefined) {
  return sessionId?.trim() ?? "";
}

export function workspaceFileChangeMatchesScope(
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

    return workspaceChangeTouchesCandidate(change, normalizedTargetPath, scope?.rootPath);
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
      (workspacePathContains(normalizedRoot, change.path) ||
        change.kind === "subtree" && (!change.path || workspacePathContains(change.path, normalizedRoot))),
  );
}

export function workspaceFilesChangedEventTouchesGitDiffTab(
  event: WorkspaceFilesChangedEvent,
  tab: WorkspaceDiffPreviewTab,
) {
  const request = tab.gitDiffRequest ?? null;
  const candidates = [
    tab.displayPath,
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
      workspaceChangeTouchesCandidate(change, candidatePath, request?.workdir),
    ),
  );
}

function mergeWorkspaceFileChangeKind(
  current: WorkspaceFileChangeKind,
  next: WorkspaceFileChangeKind,
): WorkspaceFileChangeKind {
  if (current === "subtree" || next === "subtree") return "subtree";
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
  if (!current) {
    return next;
  }

  if (next.revision < current.revision) {
    return current;
  }

  const changesByKey = new Map<string, WorkspaceFileChange>();
  for (const change of [...current.changes, ...next.changes]) {
    if (!normalizeWorkspaceFileEventPath(change.path) && change.kind !== "subtree") {
      continue;
    }

    const key = workspaceFileChangeKey(change);
    const existing = changesByKey.get(key);
    if (!existing) {
      changesByKey.set(key, { ...change });
      continue;
    }

    changesByKey.set(key, {
      ...existing,
      ...change,
      kind: mergeWorkspaceFileChangeKind(existing.kind, change.kind),
    });
  }

  // Index covering prefixes once; exact-heavy bursts do no pairwise scans.
  // A covering marker supersedes exact hints regardless of arrival order.
  const markers = new Map<string, WorkspaceFileChange>();
  for (const change of changesByKey.values()) {
    if (change.kind === "subtree" && !change.sessionId && !change.rootPath) {
      markers.set(normalizeWorkspaceFileEventPath(change.path), change);
    }
  }
  const covered = (change: WorkspaceFileChange) => {
    const global = markers.get("");
    if (global) return global !== change;
    const path = normalizeWorkspaceFileEventPath(change.path);
    const same = markers.get(path);
    if (same && same !== change) return true;
    for (let end = path.lastIndexOf("/"); end >= 0; end = path.lastIndexOf("/", end - 1)) {
      const cover = markers.get(path.slice(0, end)) ?? markers.get(path.slice(0, end + 1));
      if (cover && cover !== change) return true;
      if (end === 0) break;
    }
    return false;
  };
  return {
    revision: next.revision,
    changes: Array.from(changesByKey.values()).filter((change) => !covered(change)),
  };
}

function workspaceChangeTouchesCandidate(
  change: WorkspaceFileChange,
  candidate: string | null | undefined,
  root?: string | null,
) {
  const resolved = joinWorkspacePath(root, candidate);
  if (change.kind !== "subtree") return workspaceFileChangeMatchesCandidate(change.path, resolved);
  if (!change.path) return true;
  // With no root a relative target has no provably disjoint workspace.
  if (!root && candidate && !workspacePathIsAbsolute(candidate)) return true;
  return workspacePathContains(change.path, resolved);
}
