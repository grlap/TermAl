import { useEffect, useMemo, useState } from "react";
import { fetchGitStatus, type GitStatusFile, type GitStatusResponse } from "../api";

export function GitStatusPanel({
  onOpenPath,
  onOpenWorkdir,
  workdir,
}: {
  onOpenPath: (path: string) => void;
  onOpenWorkdir: (path: string) => void;
  workdir: string | null;
}) {
  const [workdirDraft, setWorkdirDraft] = useState(workdir ?? "");
  const [status, setStatus] = useState<GitStatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const normalizedWorkdir = workdir?.trim() ?? "";
  const changedFiles = status?.files ?? [];

  useEffect(() => {
    setWorkdirDraft(workdir ?? "");
  }, [workdir]);

  useEffect(() => {
    if (!normalizedWorkdir) {
      setStatus(null);
      setError(null);
      return;
    }

    void loadStatus(normalizedWorkdir);
  }, [normalizedWorkdir]);

  async function loadStatus(path: string) {
    setIsLoading(true);
    setError(null);
    try {
      const response = await fetchGitStatus(path);
      setStatus(response);
    } catch (nextError) {
      setStatus(null);
      setError(getErrorMessage(nextError));
    } finally {
      setIsLoading(false);
    }
  }

  const branchSummary = useMemo(() => {
    if (!status?.repoRoot) {
      return null;
    }

    const parts = [status.branch ?? "Detached HEAD"];
    if (status.upstream) {
      parts.push(`tracking ${status.upstream}`);
    }
    if (status.ahead > 0) {
      parts.push(`ahead ${status.ahead}`);
    }
    if (status.behind > 0) {
      parts.push(`behind ${status.behind}`);
    }
    return parts.join(" / ");
  }, [status]);

  return (
    <div className="source-pane git-status-panel">
      <div className="source-toolbar">
        <div className="source-path-row">
          <input
            className="source-path-input"
            type="text"
            value={workdirDraft}
            onChange={(event) => setWorkdirDraft(event.target.value)}
            placeholder="/absolute/path/to/repository"
          />
          <button
            className="ghost-button"
            type="button"
            onClick={() => onOpenWorkdir(workdirDraft.trim())}
            disabled={!workdirDraft.trim()}
          >
            Open
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={() => normalizedWorkdir && void loadStatus(normalizedWorkdir)}
            disabled={!normalizedWorkdir}
          >
            Refresh
          </button>
        </div>
      </div>

      {!normalizedWorkdir ? (
        <EmptyState
          title="No workspace selected"
          body="Open a folder or repository path to inspect its git status in this tile."
        />
      ) : null}

      {isLoading ? (
        <article className="activity-card">
          <div className="activity-spinner" aria-hidden="true" />
          <div>
            <div className="card-label">Git</div>
            <h3>Loading repository state</h3>
            <p>{normalizedWorkdir}</p>
          </div>
        </article>
      ) : null}

      {error ? (
        <article className="thread-notice">
          <div className="card-label">Git</div>
          <p>{error}</p>
        </article>
      ) : null}

      {status && !status.repoRoot ? (
        <EmptyState
          title="No git repository found"
          body="The selected folder is not inside a git repository."
        />
      ) : null}

      {status?.repoRoot ? (
        <article className="message-card git-status-card">
          <div className="message-meta">
            <span>Git</span>
            <span>{status.repoRoot}</span>
          </div>
          {branchSummary ? <p className="support-copy">{branchSummary}</p> : null}
          {status.isClean ? (
            <p className="support-copy">Working tree clean.</p>
          ) : (
            <div className="git-status-list">
              {changedFiles.map((file) => (
                <button
                  key={`${file.originalPath ?? ""}:${file.path}`}
                  className="git-status-row"
                  type="button"
                  onClick={() => onOpenPath(resolveGitFilePath(status.repoRoot ?? "", file.path))}
                >
                  <span className="git-status-code">{formatGitFileStatus(file)}</span>
                  <span className="git-status-path-group">
                    <span className="git-status-path">{file.path}</span>
                    {file.originalPath ? (
                      <span className="git-status-old-path">from {file.originalPath}</span>
                    ) : null}
                  </span>
                </button>
              ))}
            </div>
          )}
        </article>
      ) : null}
    </div>
  );
}

function formatGitFileStatus(file: GitStatusFile) {
  const indexStatus = file.indexStatus?.trim() || ".";
  const worktreeStatus = file.worktreeStatus?.trim() || ".";
  return `${indexStatus}${worktreeStatus}`;
}

function resolveGitFilePath(repoRoot: string, relativePath: string) {
  if (!relativePath || relativePath.startsWith("/")) {
    return relativePath;
  }

  return `${repoRoot.replace(/[\\/]+$/, "")}/${relativePath}`;
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
