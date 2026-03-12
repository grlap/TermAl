import { useEffect, useState } from "react";
import { fetchDirectory, type DirectoryEntry, type DirectoryResponse } from "../api";

export function FileSystemPanel({
  onOpenPath,
  onOpenRootPath,
  rootPath,
}: {
  onOpenPath: (path: string) => void;
  onOpenRootPath: (path: string) => void;
  rootPath: string | null;
}) {
  const [rootDraft, setRootDraft] = useState(rootPath ?? "");
  const [directoriesByPath, setDirectoriesByPath] = useState<Record<string, DirectoryResponse | undefined>>({});
  const [errorsByPath, setErrorsByPath] = useState<Record<string, string | undefined>>({});
  const [expandedPaths, setExpandedPaths] = useState<Record<string, true | undefined>>({});
  const [loadingPaths, setLoadingPaths] = useState<Record<string, true | undefined>>({});
  const normalizedRootPath = rootPath?.trim() ?? "";
  const rootDirectory = normalizedRootPath ? directoriesByPath[normalizedRootPath] ?? null : null;
  const rootError = normalizedRootPath ? errorsByPath[normalizedRootPath] ?? null : null;
  const isRootLoading = Boolean(normalizedRootPath && loadingPaths[normalizedRootPath]);

  useEffect(() => {
    setRootDraft(rootPath ?? "");
  }, [rootPath]);

  useEffect(() => {
    if (!normalizedRootPath) {
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
  }, [normalizedRootPath]);

  async function loadDirectory(path: string, force = false) {
    if (!force && (directoriesByPath[path] || loadingPaths[path])) {
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
      const response = await fetchDirectory(path);
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
    <div className="source-pane filesystem-panel">
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

      {!normalizedRootPath ? (
        <EmptyState
          title="No folder selected"
          body="Open a workspace folder to browse files and directories in this tile."
        />
      ) : null}

      {isRootLoading ? (
        <article className="activity-card">
          <div className="activity-spinner" aria-hidden="true" />
          <div>
            <div className="card-label">Files</div>
            <h3>Loading folder</h3>
            <p>{normalizedRootPath}</p>
          </div>
        </article>
      ) : null}

      {rootError ? (
        <article className="thread-notice">
          <div className="card-label">Files</div>
          <p>{rootError}</p>
        </article>
      ) : null}

      {rootDirectory ? (
        <article className="message-card filesystem-card">
          <div className="message-meta">
            <span>Files</span>
            <span>{rootDirectory.path}</span>
          </div>
          {rootDirectory.entries.length > 0 ? (
            <DirectoryTree
              directoriesByPath={directoriesByPath}
              entries={rootDirectory.entries}
              errorsByPath={errorsByPath}
              expandedPaths={expandedPaths}
              loadingPaths={loadingPaths}
              onDirectoryToggle={handleDirectoryToggle}
              onOpenPath={onOpenPath}
            />
          ) : (
            <p className="support-copy">This folder is empty.</p>
          )}
        </article>
      ) : null}
    </div>
  );
}

function DirectoryTree({
  directoriesByPath,
  entries,
  errorsByPath,
  expandedPaths,
  loadingPaths,
  onDirectoryToggle,
  onOpenPath,
}: {
  directoriesByPath: Record<string, DirectoryResponse | undefined>;
  entries: DirectoryEntry[];
  errorsByPath: Record<string, string | undefined>;
  expandedPaths: Record<string, true | undefined>;
  loadingPaths: Record<string, true | undefined>;
  onDirectoryToggle: (path: string) => void;
  onOpenPath: (path: string) => void;
}) {
  return (
    <div className="filesystem-tree">
      {entries.map((entry) => {
        const isDirectory = entry.kind === "directory";
        const isExpanded = Boolean(expandedPaths[entry.path]);
        const directory = isDirectory ? directoriesByPath[entry.path] : undefined;
        const isLoading = Boolean(loadingPaths[entry.path]);
        const error = errorsByPath[entry.path];

        return (
          <div key={entry.path} className="filesystem-node">
            {isDirectory ? (
              <button
                className="filesystem-row filesystem-directory-row"
                type="button"
                onClick={() => onDirectoryToggle(entry.path)}
              >
                <span className="filesystem-toggle" aria-hidden="true">
                  {isExpanded ? "v" : ">"}
                </span>
                <span className="filesystem-name">{entry.name}</span>
              </button>
            ) : (
              <button
                className="filesystem-row filesystem-file-row"
                type="button"
                onClick={() => onOpenPath(entry.path)}
              >
                <span className="filesystem-toggle" aria-hidden="true">
                  -
                </span>
                <span className="filesystem-name">{entry.name}</span>
              </button>
            )}

            {isDirectory && isExpanded ? (
              <div className="filesystem-children">
                {isLoading ? <p className="support-copy">Loading...</p> : null}
                {error ? <p className="support-copy">{error}</p> : null}
                {directory && directory.entries.length > 0 ? (
                  <DirectoryTree
                    directoriesByPath={directoriesByPath}
                    entries={directory.entries}
                    errorsByPath={errorsByPath}
                    expandedPaths={expandedPaths}
                    loadingPaths={loadingPaths}
                    onDirectoryToggle={onDirectoryToggle}
                    onOpenPath={onOpenPath}
                  />
                ) : null}
                {directory && directory.entries.length === 0 && !isLoading && !error ? (
                  <p className="support-copy">Empty folder.</p>
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

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}
