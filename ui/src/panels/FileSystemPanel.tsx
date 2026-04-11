import { useEffect, useRef, useState } from "react";
import {
  fetchDirectory,
  fetchGitStatus,
  type DirectoryEntry,
  type DirectoryResponse,
  type GitStatusFile,
  type GitStatusResponse,
} from "../api";
import type { WorkspaceFilesChangedEvent } from "../types";
import { workspaceFilesChangedEventTouchesRoot } from "../workspace-file-events";
import { gitStatusTone } from "./git-status-tree";

type GitDecorationTone = ReturnType<typeof gitStatusTone>;

type FileDecoration = {
  label: string;
  statusLabel: string;
  tone: GitDecorationTone;
};

type FileSystemGitDecorations = {
  directoriesByPath: Record<string, GitDecorationTone | undefined>;
  filesByPath: Record<string, FileDecoration | undefined>;
};
type FileSystemOpenOptions = {
  openInNewTab?: boolean;
};


const GIT_STATUS_LABELS: Record<string, string> = {
  "?": "Untracked",
  A: "Added",
  C: "Copied",
  D: "Deleted",
  M: "Modified",
  R: "Renamed",
  T: "Type changed",
  U: "Unmerged",
};

const GIT_TONE_PRIORITY: Record<GitDecorationTone, number> = {
  added: 1,
  renamed: 2,
  modified: 3,
  deleted: 4,
  conflict: 5,
};

export function FileSystemPanel({
  onOpenPath,
  onOpenRootPath,
  rootPath,
  sessionId,
  projectId = null,
  workspaceFilesChangedEvent = null,
  showPathControls = true,
}: {
  onOpenPath: (path: string, options?: FileSystemOpenOptions) => void;
  onOpenRootPath: (path: string) => void;
  rootPath: string | null;
  sessionId: string | null;
  projectId?: string | null;
  workspaceFilesChangedEvent?: WorkspaceFilesChangedEvent | null;
  showPathControls?: boolean;
}) {
  const [rootDraft, setRootDraft] = useState(rootPath ?? "");
  const [directoriesByPath, setDirectoriesByPath] = useState<Record<string, DirectoryResponse | undefined>>({});
  const [errorsByPath, setErrorsByPath] = useState<Record<string, string | undefined>>({});
  const [expandedPaths, setExpandedPaths] = useState<Record<string, true | undefined>>({});
  const [loadingPaths, setLoadingPaths] = useState<Record<string, true | undefined>>({});
  const [gitDecorations, setGitDecorations] = useState<FileSystemGitDecorations>(createEmptyGitDecorations());
  const directoriesByPathRef = useRef(directoriesByPath);
  const expandedPathsRef = useRef(expandedPaths);
  const normalizedRootPath = rootPath?.trim() ?? "";
  const normalizedSessionId = sessionId?.trim() ?? "";
  const normalizedProjectId = projectId?.trim() ?? "";
  const hasScope = Boolean(normalizedSessionId || normalizedProjectId);
  const rootDirectory = normalizedRootPath ? directoriesByPath[normalizedRootPath] ?? null : null;
  const rootError = normalizedRootPath ? errorsByPath[normalizedRootPath] ?? null : null;
  const isRootLoading = Boolean(normalizedRootPath && loadingPaths[normalizedRootPath]);
  const normalizedRootDecorationPath = normalizePath(normalizedRootPath);
  const rootTone = normalizedRootDecorationPath
    ? gitDecorations.directoriesByPath[normalizedRootDecorationPath] ?? null
    : null;
  const rootDisplayName = rootDirectory?.name || pathBaseName(normalizedRootPath) || normalizedRootPath;

  useEffect(() => {
    setRootDraft(rootPath ?? "");
  }, [rootPath]);

  useEffect(() => {
    directoriesByPathRef.current = directoriesByPath;
  }, [directoriesByPath]);

  useEffect(() => {
    expandedPathsRef.current = expandedPaths;
  }, [expandedPaths]);

  useEffect(() => {
    if (!normalizedRootPath) {
      return;
    }

    if (!hasScope) {
      setDirectoriesByPath({});
      setErrorsByPath({
        [normalizedRootPath]: "This file browser is no longer associated with a live session or project.",
      });
      return;
    }

    setExpandedPaths((current) =>
      current[normalizedRootPath]
        ? current
        : {
            ...current,
            [normalizedRootPath]: true,
          },
    );
    void loadDirectory(normalizedRootPath, true);
  }, [hasScope, normalizedRootPath, normalizedProjectId, normalizedSessionId]);

  useEffect(() => {
    let cancelled = false;

    if (!normalizedRootPath) {
      setGitDecorations(createEmptyGitDecorations());
      return;
    }

    if (!hasScope) {
      setGitDecorations(createEmptyGitDecorations());
      return;
    }

    void fetchGitStatus(normalizedRootPath, normalizedSessionId || null, {
      projectId: normalizedProjectId || null,
    })
      .then((status) => {
        if (cancelled) {
          return;
        }

        setGitDecorations(buildGitDecorations(status, normalizedRootPath));
      })
      .catch(() => {
        if (cancelled) {
          return;
        }

        setGitDecorations(createEmptyGitDecorations());
      });

    return () => {
      cancelled = true;
    };
  }, [hasScope, normalizedProjectId, normalizedRootPath, normalizedSessionId]);

  useEffect(() => {
    if (
      !workspaceFilesChangedEvent ||
      !normalizedRootPath ||
      !hasScope ||
      !workspaceFilesChangedEventTouchesRoot(
        workspaceFilesChangedEvent,
        normalizedRootPath,
        {
          rootPath: normalizedRootPath,
          sessionId: normalizedSessionId || null,
        },
      )
    ) {
      return;
    }

    let cancelled = false;

    async function refreshVisibleTree() {
      try {
        const statusPromise = fetchGitStatus(normalizedRootPath, normalizedSessionId || null, {
          projectId: normalizedProjectId || null,
        });
        const expandedDirectoryPaths = Object.keys(expandedPathsRef.current).filter(
          (path) => directoriesByPathRef.current[path],
        );
        const directoryPromises = expandedDirectoryPaths.map(async (path) => {
          try {
            const response = await fetchDirectory(path, {
              sessionId: normalizedSessionId || null,
              projectId: normalizedProjectId || null,
            });
            return { path, response };
          } catch (error) {
            return { path, error };
          }
        });

        const [statusResult, directoryResults] = await Promise.all([
          statusPromise.then(
            (status) => ({ status }),
            () => ({ status: null }),
          ),
          Promise.all(directoryPromises),
        ]);
        if (cancelled) {
          return;
        }

        if (statusResult.status) {
          setGitDecorations(buildGitDecorations(statusResult.status, normalizedRootPath));
        }

        setDirectoriesByPath((current) => {
          let nextState = current;
          for (const result of directoryResults) {
            if (!("response" in result)) {
              continue;
            }
            if (nextState === current) {
              nextState = { ...current };
            }
            nextState[result.path] = result.response;
          }
          return nextState;
        });
        setErrorsByPath((current) => {
          let nextState = current;
          for (const result of directoryResults) {
            if (!("error" in result)) {
              continue;
            }
            if (nextState === current) {
              nextState = { ...current };
            }
            nextState[result.path] = getErrorMessage(result.error);
          }
          return nextState;
        });
      } catch {
        // Watcher events are refresh hints. Ignore transient failures here; the
        // explicit folder refresh button still surfaces request errors.
      }
    }

    void refreshVisibleTree();

    return () => {
      cancelled = true;
    };
  }, [
    hasScope,
    normalizedProjectId,
    normalizedRootPath,
    normalizedSessionId,
    workspaceFilesChangedEvent,
  ]);

  async function loadDirectory(path: string, force = false) {
    if (!force && (directoriesByPath[path] || loadingPaths[path])) {
      return;
    }

    if (!hasScope) {
      setErrorsByPath((current) => ({
        ...current,
        [path]: "This file browser is no longer associated with a live session or project.",
      }));
      return;
    }

    setLoadingPaths((current) => ({
      ...current,
      [path]: true,
    }));
    setErrorsByPath((current) => {
      if (!current[path]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[path];
      return nextState;
    });

    try {
      const response = await fetchDirectory(path, {
        sessionId: normalizedSessionId || null,
        projectId: normalizedProjectId || null,
      });
      setDirectoriesByPath((current) => ({
        ...current,
        [path]: response,
      }));
    } catch (error) {
      setErrorsByPath((current) => ({
        ...current,
        [path]: getErrorMessage(error),
      }));
    } finally {
      setLoadingPaths((current) => {
        if (!current[path]) {
          return current;
        }

        const nextState = { ...current };
        delete nextState[path];
        return nextState;
      });
    }
  }

  function handleDirectoryToggle(path: string) {
    const willExpand = !expandedPaths[path];
    setExpandedPaths((current) => {
      const nextState = { ...current };
      if (willExpand) {
        nextState[path] = true;
      } else {
        delete nextState[path];
      }
      return nextState;
    });

    if (willExpand) {
      void loadDirectory(path);
    }
  }

  return (
    <div className={`source-pane filesystem-panel${rootDirectory ? " has-filetree" : ""}`}>
      {showPathControls ? (
        <div className="source-toolbar">
          <div className="source-path-row">
            <input
              className="source-path-input"
              type="text"
              value={rootDraft}
              onChange={(event) => setRootDraft(event.target.value)}
              placeholder="/absolute/path/to/folder"
            />
            <button
              className="ghost-button"
              type="button"
              onClick={() => onOpenRootPath(rootDraft.trim())}
              disabled={!rootDraft.trim()}
            >
              Open
            </button>
            <button
              className="ghost-button"
              type="button"
              onClick={() => normalizedRootPath && void loadDirectory(normalizedRootPath, true)}
              disabled={!normalizedRootPath}
            >
              Refresh
            </button>
          </div>
        </div>
      ) : null}

      {!normalizedRootPath ? (
        <EmptyState
          title="No folder selected"
          body="Open a workspace folder to browse files and directories in this tile."
        />
      ) : null}
      {rootError ? (
        <article className="thread-notice">
          <div className="card-label">Files</div>
          <p>{rootError}</p>
        </article>
      ) : null}
      {normalizedRootPath && (rootDirectory || isRootLoading) ? (
        <section className="filesystem-explorer" aria-label="Files">
          <div
            className={`filesystem-root-header${isRootLoading ? " filesystem-root-header-loading" : ""}`}
            role={isRootLoading ? "status" : undefined}
            aria-label={isRootLoading ? "Loading files" : undefined}
          >
            <div className="filesystem-root-copy">
              <span className="filesystem-root-name">{rootDisplayName}</span>
              <span className="filesystem-root-path" title={rootDirectory?.path ?? normalizedRootPath}>
                {rootDirectory?.path ?? normalizedRootPath}
              </span>
            </div>
            {isRootLoading ? (
              <span className="activity-spinner filesystem-root-loading-spinner" aria-hidden="true" />
            ) : rootTone ? (
              <span className={`filesystem-git-dot filesystem-git-dot-${rootTone}`} aria-hidden="true" />
            ) : null}
          </div>
          {rootDirectory ? (
            rootDirectory.entries.length > 0 ? (
              <DirectoryTree
                directoriesByPath={directoriesByPath}
                entries={rootDirectory.entries}
                errorsByPath={errorsByPath}
                expandedPaths={expandedPaths}
                gitDecorations={gitDecorations}
                loadingPaths={loadingPaths}
                onDirectoryToggle={handleDirectoryToggle}
                onOpenPath={onOpenPath}
              />
            ) : (
              <p className="support-copy filesystem-support-copy">This folder is empty.</p>
            )
          ) : null}
        </section>
      ) : null}
    </div>
  );
}

function DirectoryTree({
  directoriesByPath,
  entries,
  errorsByPath,
  expandedPaths,
  gitDecorations,
  loadingPaths,
  onDirectoryToggle,
  onOpenPath,
}: {
  directoriesByPath: Record<string, DirectoryResponse | undefined>;
  entries: DirectoryEntry[];
  errorsByPath: Record<string, string | undefined>;
  expandedPaths: Record<string, true | undefined>;
  gitDecorations: FileSystemGitDecorations;
  loadingPaths: Record<string, true | undefined>;
  onDirectoryToggle: (path: string) => void;
  onOpenPath: (path: string, options?: FileSystemOpenOptions) => void;
}) {
  return (
    <div className="filesystem-tree">
      {entries.map((entry) => {
        const isDirectory = entry.kind === "directory";
        const isExpanded = Boolean(expandedPaths[entry.path]);
        const directory = isDirectory ? directoriesByPath[entry.path] : undefined;
        const isLoading = Boolean(loadingPaths[entry.path]);
        const error = errorsByPath[entry.path];
        const normalizedEntryPath = normalizePath(entry.path);
        const directoryTone = isDirectory
          ? gitDecorations.directoriesByPath[normalizedEntryPath] ?? null
          : null;
        const fileDecoration = isDirectory
          ? null
          : gitDecorations.filesByPath[normalizedEntryPath] ?? null;

        return (
          <div key={entry.path} className="filesystem-node">
            {isDirectory ? (
              <button
                className={`filesystem-row filesystem-directory-row${isExpanded ? " expanded" : ""}`}
                type="button"
                onClick={() => onDirectoryToggle(entry.path)}
              >
                <span className={`filesystem-toggle${isExpanded ? " expanded" : ""}`} aria-hidden="true">
                  <ChevronIcon expanded={isExpanded} />
                </span>
                <span
                  className={`filesystem-entry-icon filesystem-entry-icon-directory${isExpanded ? " open" : ""}`}
                  aria-hidden="true"
                >
                  <FolderIcon open={isExpanded} />
                </span>
                <span className="filesystem-name">{entry.name}</span>
                {directoryTone ? (
                  <span className={`filesystem-git-dot filesystem-git-dot-${directoryTone}`} aria-hidden="true" />
                ) : null}
              </button>
            ) : (
              <button
                className="filesystem-row filesystem-file-row"
                type="button"
                onClick={(event) =>
                  event.ctrlKey || event.metaKey
                    ? onOpenPath(entry.path, { openInNewTab: true })
                    : onOpenPath(entry.path)
                }
              >
                <span className="filesystem-toggle filesystem-toggle-placeholder" aria-hidden="true">
                  <ChevronIcon expanded={false} />
                </span>
                <span className="filesystem-entry-icon filesystem-entry-icon-file" aria-hidden="true">
                  <FileIcon />
                </span>
                <span className="filesystem-name">{entry.name}</span>
                {fileDecoration ? (
                  <span
                    className={`filesystem-git-badge filesystem-git-badge-${fileDecoration.tone}`}
                    title={fileDecoration.statusLabel}
                  >
                    {fileDecoration.label}
                  </span>
                ) : null}
              </button>
            )}

            {isDirectory && isExpanded ? (
              <div className="filesystem-children">
                {isLoading ? <p className="support-copy filesystem-support-copy">Loading...</p> : null}
                {error ? <p className="support-copy filesystem-support-copy">{error}</p> : null}
                {directory && directory.entries.length > 0 ? (
                  <DirectoryTree
                    directoriesByPath={directoriesByPath}
                    entries={directory.entries}
                    errorsByPath={errorsByPath}
                    expandedPaths={expandedPaths}
                    gitDecorations={gitDecorations}
                    loadingPaths={loadingPaths}
                    onDirectoryToggle={onDirectoryToggle}
                    onOpenPath={onOpenPath}
                  />
                ) : null}
                {directory && directory.entries.length === 0 && !isLoading && !error ? (
                  <p className="support-copy filesystem-support-copy">Empty folder.</p>
                ) : null}
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state-card">
      <div className="card-label">Workspace</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function ChevronIcon({ expanded }: { expanded: boolean }) {
  return (
    <svg
      className={`filesystem-chevron${expanded ? " expanded" : ""}`}
      viewBox="0 0 12 12"
      focusable="false"
      aria-hidden="true"
    >
      <path
        d="M4 2.75 7.75 6 4 9.25"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

function FolderIcon({ open }: { open: boolean }) {
  return open ? (
    <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
      <path
        d="M2.5 5.5A1.5 1.5 0 0 1 4 4h2.7l1 1.15H13A1.5 1.5 0 0 1 14.5 6.7l-.55 5.55A1.5 1.5 0 0 1 12.45 13.6H3.6A1.5 1.5 0 0 1 2.1 12.1V5.5Z"
        fill="none"
        stroke="currentColor"
        strokeLinejoin="round"
        strokeWidth="1.35"
      />
      <path d="M2.7 6.25h11" fill="none" stroke="currentColor" strokeWidth="1.35" />
    </svg>
  ) : (
    <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
      <path
        d="M2.5 5.4A1.4 1.4 0 0 1 3.9 4h2.55l.95 1.1h4.7a1.4 1.4 0 0 1 1.4 1.4v5.6a1.4 1.4 0 0 1-1.4 1.4H3.9a1.4 1.4 0 0 1-1.4-1.4V5.4Z"
        fill="none"
        stroke="currentColor"
        strokeLinejoin="round"
        strokeWidth="1.35"
      />
      <path d="M2.5 6.45h11" fill="none" stroke="currentColor" strokeWidth="1.35" />
    </svg>
  );
}

function FileIcon() {
  return (
    <svg viewBox="0 0 16 16" focusable="false" aria-hidden="true">
      <path
        d="M4 2.4h5l3 3v8.2A1.4 1.4 0 0 1 10.6 15H4A1.4 1.4 0 0 1 2.6 13.6V3.8A1.4 1.4 0 0 1 4 2.4Z"
        fill="none"
        stroke="currentColor"
        strokeLinejoin="round"
        strokeWidth="1.35"
      />
      <path d="M9 2.75v3.1h3.1" fill="none" stroke="currentColor" strokeWidth="1.35" />
    </svg>
  );
}

function createEmptyGitDecorations(): FileSystemGitDecorations {
  return {
    directoriesByPath: {},
    filesByPath: {},
  };
}

function buildGitDecorations(status: GitStatusResponse, rootPath: string): FileSystemGitDecorations {
  if (!status.repoRoot) {
    return createEmptyGitDecorations();
  }

  const normalizedRoot = normalizePath(rootPath);
  const filesByPath: Record<string, FileDecoration | undefined> = {};
  const directoriesByPath: Record<string, GitDecorationTone | undefined> = {};

  for (const file of status.files) {
    const statusCode = resolveGitStatusCode(file);
    if (!statusCode) {
      continue;
    }

    const absolutePath = joinNormalizedPath(status.repoRoot, file.path);
    if (!isPathWithinRoot(absolutePath, normalizedRoot)) {
      continue;
    }

    const tone = gitStatusTone(statusCode);
    filesByPath[absolutePath] = {
      label: statusCode,
      statusLabel: GIT_STATUS_LABELS[statusCode] ?? "Changed",
      tone,
    };

    for (let ancestor = parentPath(absolutePath); ancestor && isPathWithinRoot(ancestor, normalizedRoot); ancestor = parentPath(ancestor)) {
      directoriesByPath[ancestor] = strongerTone(directoriesByPath[ancestor], tone);
      if (ancestor === normalizedRoot) {
        break;
      }
    }
  }

  return {
    directoriesByPath,
    filesByPath,
  };
}

function resolveGitStatusCode(file: GitStatusFile) {
  return normalizeGitStatus(file.worktreeStatus) ?? normalizeGitStatus(file.indexStatus);
}

function normalizeGitStatus(status?: string | null) {
  const normalized = status?.trim().toUpperCase() ?? "";
  if (!normalized || normalized === ".") {
    return null;
  }

  return normalized[0] ?? null;
}

function strongerTone(
  currentTone: GitDecorationTone | undefined,
  nextTone: GitDecorationTone,
): GitDecorationTone {
  if (!currentTone) {
    return nextTone;
  }

  return GIT_TONE_PRIORITY[nextTone] > GIT_TONE_PRIORITY[currentTone] ? nextTone : currentTone;
}

function isPathWithinRoot(path: string, root: string) {
  return path === root || path.startsWith(`${root}/`);
}

function parentPath(path: string) {
  const lastSlashIndex = path.lastIndexOf("/");
  if (lastSlashIndex <= 0) {
    return null;
  }

  const parent = path.slice(0, lastSlashIndex);
  return /^[a-z]:$/i.test(parent) ? `${parent}/` : parent;
}

function joinNormalizedPath(basePath: string, relativePath: string) {
  const normalizedBase = normalizePath(basePath);
  const normalizedRelative = relativePath.trim().split(/[\\/]+/).filter(Boolean).join("/");
  return normalizePath(`${normalizedBase}/${normalizedRelative}`);
}

function normalizePath(path: string) {
  const normalized = path.trim().replace(/\\+/g, "/").replace(/\/+/g, "/");
  if (!normalized) {
    return "";
  }

  if (/^[a-z]:\/$/i.test(normalized)) {
    return normalized;
  }

  return normalized.endsWith("/") ? normalized.slice(0, -1) : normalized;
}

function pathBaseName(path: string) {
  return path.trim().split(/[\\/]+/).filter(Boolean).pop() ?? "";
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
