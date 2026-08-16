import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  applyGitFileAction,
  commitGitChanges,
  fetchGitStatus,
  type GitDiffRequestPayload,
  type GitDiffSection,
  type GitFileAction,
  type GitStatusResponse,
} from "../api";
import {
  buildGitStatusTree,
  gitStatusTone,
  type GitStatusSectionId,
  type GitStatusTreeDirectoryNode,
  type GitStatusTreeFileNode,
  type GitStatusTreeNode,
  type GitStatusTreeSection,
} from "./git-status-tree";

type GitStatusPanelCacheEntry = {
  status: GitStatusResponse;
  treeExpansionByKey: Record<string, boolean>;
};

type GitActionTarget = {
  originalPath?: string | null;
  path: string;
  statusCode?: string | null;
};
type GitDiffOpenOptions = {
  openInNewTab?: boolean;
  sectionId?: GitDiffSection;
};

const gitStatusPanelCache = new Map<string, GitStatusPanelCacheEntry>();
const MAX_GIT_STATUS_PANEL_CACHE_ENTRIES = 16;
const GIT_STATUS_VISIBLE_REFRESH_MS = 10_000;

function buildGitStatusPanelCacheKey(projectId: string, sessionId: string, workdir: string) {
  return JSON.stringify([projectId, sessionId, workdir]);
}

function readGitStatusPanelCache(key: string) {
  const entry = gitStatusPanelCache.get(key) ?? null;
  if (entry) {
    // Refresh insertion order so the bounded module cache behaves as an LRU.
    gitStatusPanelCache.delete(key);
    gitStatusPanelCache.set(key, entry);
  }
  return entry;
}

function writeGitStatusPanelCache(key: string, entry: GitStatusPanelCacheEntry) {
  gitStatusPanelCache.delete(key);
  gitStatusPanelCache.set(key, entry);
  while (gitStatusPanelCache.size > MAX_GIT_STATUS_PANEL_CACHE_ENTRIES) {
    const oldestKey = gitStatusPanelCache.keys().next().value;
    if (typeof oldestKey !== "string") {
      break;
    }
    gitStatusPanelCache.delete(oldestKey);
  }
}

function gitStatusResponsesEqual(left: GitStatusResponse | null, right: GitStatusResponse) {
  return left !== null && JSON.stringify(left) === JSON.stringify(right);
}

export function GitStatusPanel({
  onStatusChange,
  onOpenDiff,
  onOpenWorkdir,
  projectId = null,
  sessionId = null,
  workdir,
  showPathControls = true,
}: {
  onStatusChange?: (status: GitStatusResponse | null) => void;
  onOpenDiff: (request: GitDiffRequestPayload, options?: GitDiffOpenOptions) => Promise<void> | void;
  onOpenWorkdir: (path: string) => void;
  projectId?: string | null;
  sessionId?: string | null;
  workdir: string | null;
  showPathControls?: boolean;
}) {
  const normalizedProjectId = projectId?.trim() ?? "";
  const normalizedSessionId = sessionId?.trim() ?? "";
  const normalizedWorkdir = workdir?.trim() ?? "";
  const panelScopeKey = normalizedWorkdir
    ? buildGitStatusPanelCacheKey(normalizedProjectId, normalizedSessionId, normalizedWorkdir)
    : "";
  const cachedPanelState = panelScopeKey ? gitStatusPanelCache.get(panelScopeKey) ?? null : null;
  const [workdirDraft, setWorkdirDraft] = useState(workdir ?? "");
  const [status, setStatus] = useState<GitStatusResponse | null>(() => cachedPanelState?.status ?? null);
  const [statusCacheKey, setStatusCacheKey] = useState<string | null>(() =>
    cachedPanelState?.status ? panelScopeKey : null,
  );
  const [commitMessage, setCommitMessage] = useState("");
  const [commitNotice, setCommitNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [backgroundError, setBackgroundError] = useState<string | null>(null);
  const [isCommitting, setIsCommitting] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [pendingActionKey, setPendingActionKey] = useState<string | null>(null);
  const [treeExpansionByKey, setTreeExpansionByKey] = useState<Record<string, boolean>>(
    () => cachedPanelState?.treeExpansionByKey ?? {},
  );
  const onStatusChangeRef = useRef(onStatusChange);
  const statusRef = useRef(status);
  const isMountedRef = useRef(true);
  const latestLoadRequestIdRef = useRef(0);
  const activeLoadRef = useRef<{ background: boolean; requestId: number; scopeKey: string } | null>(null);
  const activePanelScopeKeyRef = useRef(panelScopeKey);
  const latestDiffOperationIdRef = useRef(0);
  const latestMutationOperationIdRef = useRef(0);
  const gitMutationActiveRef = useRef(false);
  const previousSectionsRef = useRef<GitStatusTreeSection[] | null>(null);
  const previousSectionsWorkdirRef = useRef<string | null>(null);
  const visibleStatus = status;
  const visibleError = error ?? backgroundError;
  const changedFiles = visibleStatus?.files ?? [];
  const hasStagedChanges = changedFiles.some((file) => Boolean(file.indexStatus));
  const isTreeMutationPending = pendingActionKey !== null && !pendingActionKey.endsWith(":open");
  const isGitMutationPending = isCommitting || isTreeMutationPending;
  const sections = useMemo(() => {
    const previousSections =
      previousSectionsWorkdirRef.current === normalizedWorkdir ? (previousSectionsRef.current ?? undefined) : undefined;
    return buildGitStatusTree(changedFiles, previousSections);
  }, [changedFiles, normalizedWorkdir]);
  useLayoutEffect(() => {
    // Publish the new scope in the same commit, before a promise callback can
    // apply an old scope's result to the newly rendered panel.
    activePanelScopeKeyRef.current = panelScopeKey;
    statusRef.current = status;
  }, [panelScopeKey, status]);

  useEffect(() => {
    onStatusChangeRef.current = onStatusChange;
  }, [onStatusChange]);

  useEffect(() => {
    previousSectionsRef.current = sections;
    previousSectionsWorkdirRef.current = normalizedWorkdir || null;
  }, [sections, normalizedWorkdir]);

  useEffect(() => {
    if (!statusCacheKey || !status) {
      return;
    }

    writeGitStatusPanelCache(statusCacheKey, {
      status,
      treeExpansionByKey,
    });
  }, [status, statusCacheKey, treeExpansionByKey]);

  useEffect(() => {
    setWorkdirDraft(workdir ?? "");
  }, [workdir]);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
      latestLoadRequestIdRef.current += 1;
      latestDiffOperationIdRef.current += 1;
      latestMutationOperationIdRef.current += 1;
      activeLoadRef.current = null;
      gitMutationActiveRef.current = false;
    };
  }, []);

  const loadStatus = useCallback(async (options?: {
    background?: boolean;
    preserveVisibleStatus?: boolean;
  }) => {
    if (!normalizedWorkdir || !panelScopeKey) {
      return;
    }
    const requestScopeKey = panelScopeKey;
    const background = options?.background ?? false;
    const activeLoad = activeLoadRef.current;
    if (activeLoad?.scopeKey === requestScopeKey) {
      // Background work never displaces a request already representing this
      // scope. A user-triggered foreground refresh may supersede a background
      // poll so recovery actions are never silently ignored.
      if (background || !activeLoad.background) {
        return;
      }
    }

    const requestId = latestLoadRequestIdRef.current + 1;
    latestLoadRequestIdRef.current = requestId;
    activeLoadRef.current = { background, requestId, scopeKey: requestScopeKey };
    const preserveVisibleStatus = options?.preserveVisibleStatus;
    if (!background) {
      setIsLoading(true);
      setError(null);
      setBackgroundError(null);
    }
    try {
      const response = await fetchGitStatus(normalizedWorkdir, normalizedSessionId || null, {
        projectId: normalizedProjectId || null,
      });
      if (
        !isMountedRef.current ||
        latestLoadRequestIdRef.current !== requestId ||
        activePanelScopeKeyRef.current !== requestScopeKey
      ) {
        return;
      }
      if (background && gitStatusResponsesEqual(statusRef.current, response)) {
        setBackgroundError(null);
        return;
      }
      statusRef.current = response;
      setStatus(response);
      setStatusCacheKey(requestScopeKey);
      if (background) {
        setBackgroundError(null);
      }
      onStatusChangeRef.current?.(response);
    } catch (nextError) {
      if (
        !isMountedRef.current ||
        latestLoadRequestIdRef.current !== requestId ||
        activePanelScopeKeyRef.current !== requestScopeKey
      ) {
        return;
      }
      if (!preserveVisibleStatus) {
        setStatus(null);
        setStatusCacheKey(null);
        onStatusChangeRef.current?.(null);
      }
      if (background) {
        setBackgroundError(getErrorMessage(nextError));
      } else {
        setError(getErrorMessage(nextError));
      }
    } finally {
      if (
        !background &&
        isMountedRef.current &&
        latestLoadRequestIdRef.current === requestId &&
        activePanelScopeKeyRef.current === requestScopeKey
      ) {
        setIsLoading(false);
      }
      if (activeLoadRef.current?.requestId === requestId) {
        activeLoadRef.current = null;
      }
    }
  }, [normalizedProjectId, normalizedSessionId, normalizedWorkdir, panelScopeKey]);

  useEffect(() => {
    latestLoadRequestIdRef.current += 1;
    latestDiffOperationIdRef.current += 1;
    latestMutationOperationIdRef.current += 1;
    activeLoadRef.current = null;
    gitMutationActiveRef.current = false;
    setIsLoading(false);
    setIsCommitting(false);
    setPendingActionKey(null);
    setCommitMessage("");
    setCommitNotice(null);
    if (!normalizedWorkdir || !panelScopeKey) {
      setStatus(null);
      setStatusCacheKey(null);
      setError(null);
      setBackgroundError(null);
      setTreeExpansionByKey({});
      onStatusChangeRef.current?.(null);
      return;
    }

    const cachedState = readGitStatusPanelCache(panelScopeKey);
    setTreeExpansionByKey(cachedState?.treeExpansionByKey ?? {});
    setError(null);
    setBackgroundError(null);
    if (cachedState?.status) {
      setStatus(cachedState.status);
      setStatusCacheKey(panelScopeKey);
      onStatusChangeRef.current?.(cachedState.status);
    } else {
      setStatus(null);
      setStatusCacheKey(null);
      onStatusChangeRef.current?.(null);
    }

    // Cached state keeps the panel visually stable, but Git remains the
    // source of truth. Always reconcile after mounting or changing scope so
    // commits made by another process cannot leave phantom file counts.
    void loadStatus({
      background: Boolean(cachedState?.status),
      preserveVisibleStatus: Boolean(cachedState?.status),
    });
  }, [loadStatus, normalizedWorkdir, panelScopeKey]);

  useEffect(() => {
    if (!normalizedWorkdir) {
      return;
    }

    const refresh = () => {
      if (document.visibilityState === "hidden" || gitMutationActiveRef.current) {
        return;
      }
      void loadStatus({ background: true, preserveVisibleStatus: true });
    };
    const handleVisibilityChange = () => {
      if (document.visibilityState === "visible" && document.hasFocus()) {
        refresh();
      }
    };
    const refreshIfFocused = () => {
      if (document.hasFocus()) {
        refresh();
      }
    };

    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", handleVisibilityChange);
    const intervalId = window.setInterval(refreshIfFocused, GIT_STATUS_VISIBLE_REFRESH_MS);

    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.clearInterval(intervalId);
    };
  }, [loadStatus, normalizedWorkdir]);

  const handleOpenDiff = useCallback(
    async (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, options?: GitDiffOpenOptions) => {
      const activeWorkdir = visibleStatus?.workdir ?? normalizedWorkdir;
      if (!activeWorkdir) {
        return;
      }

      const requestScopeKey = panelScopeKey;
      const operationId = latestDiffOperationIdRef.current + 1;
      latestDiffOperationIdRef.current = operationId;
      const isCurrentOperation = () =>
        isMountedRef.current &&
        latestDiffOperationIdRef.current === operationId &&
        activePanelScopeKeyRef.current === requestScopeKey;
      const actionKey = gitFileOpenKey(sectionId, node.path);
      setPendingActionKey(actionKey);
      setError(null);
      setBackgroundError(null);
      setCommitNotice(null);

      try {
        const request: GitDiffRequestPayload = {
          originalPath: node.originalPath,
          path: node.path,
          projectId: normalizedProjectId || null,
          sectionId,
          sessionId: normalizedSessionId || null,
          statusCode: node.statusCode,
          workdir: activeWorkdir,
        };
        if (options?.openInNewTab) {
          await onOpenDiff(request, { openInNewTab: true, sectionId });
        } else {
          await onOpenDiff(request, { sectionId });
        }
      } catch (nextError) {
        if (!isCurrentOperation()) {
          return;
        }
        const diffError = getErrorMessage(nextError);
        await loadStatus({ preserveVisibleStatus: true });
        if (isCurrentOperation()) {
          setError(diffError);
        }
      } finally {
        if (isCurrentOperation()) {
          setPendingActionKey((current) => (current === actionKey ? null : current));
        }
      }
    },
    [
      loadStatus,
      normalizedProjectId,
      normalizedSessionId,
      normalizedWorkdir,
      onOpenDiff,
      panelScopeKey,
      visibleStatus?.workdir,
    ],
  );

  const handleTreeAction = useCallback(
    async (
      sectionId: GitStatusSectionId,
      actionPath: string,
      targets: GitActionTarget[],
      action: GitFileAction,
    ) => {
      const activeWorkdir = visibleStatus?.workdir ?? normalizedWorkdir;
      if (!activeWorkdir || targets.length === 0 || gitMutationActiveRef.current) {
        return;
      }

      const requestScopeKey = panelScopeKey;
      const operationId = latestMutationOperationIdRef.current + 1;
      latestMutationOperationIdRef.current = operationId;
      const isCurrentOperation = () =>
        isMountedRef.current &&
        latestMutationOperationIdRef.current === operationId &&
        activePanelScopeKeyRef.current === requestScopeKey;
      const actionKey = gitFileActionKey(sectionId, actionPath, action);
      gitMutationActiveRef.current = true;
      latestLoadRequestIdRef.current += 1;
      activeLoadRef.current = null;
      setIsLoading(false);
      setPendingActionKey(actionKey);
      setError(null);
      setBackgroundError(null);
      setCommitNotice(null);

      try {
        let response: GitStatusResponse | null = null;

        for (const target of targets) {
          response = await applyGitFileAction({
            action,
            originalPath: target.originalPath,
            path: target.path,
            projectId: normalizedProjectId || null,
            sessionId: normalizedSessionId || null,
            statusCode: target.statusCode,
            workdir: activeWorkdir,
          });
          if (!isCurrentOperation()) {
            return;
          }
        }

        if (response && isCurrentOperation()) {
          setStatus(response);
          setStatusCacheKey(requestScopeKey);
          onStatusChangeRef.current?.(response);
        }
      } catch (nextError) {
        if (!isCurrentOperation()) {
          return;
        }
        setError(getErrorMessage(nextError));
        if (targets.length > 1) {
          try {
            const refreshedStatus = await fetchGitStatus(normalizedWorkdir, normalizedSessionId || null, {
              projectId: normalizedProjectId || null,
            });
            if (!isCurrentOperation()) {
              return;
            }
            setStatus(refreshedStatus);
            setStatusCacheKey(requestScopeKey);
            onStatusChangeRef.current?.(refreshedStatus);
          } catch {
            // Keep the action error visible if the follow-up refresh also fails.
          }
        }
      } finally {
        if (isCurrentOperation()) {
          gitMutationActiveRef.current = false;
          setPendingActionKey((current) => (current === actionKey ? null : current));
        }
      }
    },
    [normalizedProjectId, normalizedSessionId, normalizedWorkdir, panelScopeKey, visibleStatus?.workdir],
  );

  const handleFileAction = useCallback(
    async (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => {
      await handleTreeAction(sectionId, node.path, [toGitActionTarget(node)], action);
    },
    [handleTreeAction],
  );

  const handleDirectoryAction = useCallback(
    async (sectionId: GitStatusSectionId, node: GitStatusTreeDirectoryNode, action: GitFileAction) => {
      await handleTreeAction(sectionId, node.path, collectDirectoryTargets(node), action);
    },
    [handleTreeAction],
  );

  const handleSectionAction = useCallback(
    async (sectionId: GitStatusSectionId, nodes: GitStatusTreeNode[], action: GitFileAction) => {
      await handleTreeAction(sectionId, sectionId, collectGitActionTargets(nodes), action);
    },
    [handleTreeAction],
  );

  function isTreeItemExpanded(key: string, defaultValue: boolean) {
    return treeExpansionByKey[key] ?? defaultValue;
  }

  const toggleTreeItem = useCallback((key: string, defaultValue: boolean) => {
    setTreeExpansionByKey((current) => ({
      ...current,
      [key]: !(current[key] ?? defaultValue),
    }));
  }, []);

  function submitWorkdir() {
    const nextWorkdir = workdirDraft.trim();
    if (!nextWorkdir) {
      return;
    }

    onOpenWorkdir(nextWorkdir);
  }

  function refreshCurrentStatus() {
    if (!normalizedWorkdir || isLoading || gitMutationActiveRef.current) {
      return;
    }

    void loadStatus({ preserveVisibleStatus: true });
  }

  async function submitCommit() {
    const activeWorkdir = visibleStatus?.workdir ?? normalizedWorkdir;
    const nextMessage = commitMessage.trim();
    if (
      !activeWorkdir ||
      !nextMessage ||
      !hasStagedChanges ||
      isCommitting ||
      gitMutationActiveRef.current
    ) {
      return;
    }

    const requestScopeKey = panelScopeKey;
    const operationId = latestMutationOperationIdRef.current + 1;
    latestMutationOperationIdRef.current = operationId;
    const isCurrentOperation = () =>
      isMountedRef.current &&
      latestMutationOperationIdRef.current === operationId &&
      activePanelScopeKeyRef.current === requestScopeKey;
    gitMutationActiveRef.current = true;
    latestLoadRequestIdRef.current += 1;
    activeLoadRef.current = null;
    setIsLoading(false);
    setIsCommitting(true);
    setError(null);
    setBackgroundError(null);
    setCommitNotice(null);

    try {
      const response = await commitGitChanges({
        message: nextMessage,
        projectId: normalizedProjectId || null,
        sessionId: normalizedSessionId || null,
        workdir: activeWorkdir,
      });
      if (!isCurrentOperation()) {
        return;
      }
      setStatus(response.status);
      setStatusCacheKey(requestScopeKey);
      setCommitMessage("");
      setCommitNotice(response.summary);
      onStatusChangeRef.current?.(response.status);
    } catch (nextError) {
      if (isCurrentOperation()) {
        setError(getErrorMessage(nextError));
      }
    } finally {
      if (isCurrentOperation()) {
        gitMutationActiveRef.current = false;
        setIsCommitting(false);
      }
    }
  }

  const branchName = visibleStatus?.branch ?? "Detached HEAD";

  return (
    <div className="source-pane git-status-panel">
      {showPathControls ? (
        <form
          className="source-toolbar git-status-toolbar"
          onSubmit={(event) => {
            event.preventDefault();
            submitWorkdir();
          }}
        >
          <div className="source-path-row git-status-path-row">
            <input
              className="source-path-input"
              type="text"
              value={workdirDraft}
              onChange={(event) => setWorkdirDraft(event.target.value)}
              placeholder="C:\\path\\to\\repo or any folder inside it"
            />
            <div className="git-status-path-actions">
              <button className="ghost-button git-status-load-button" type="submit" disabled={!workdirDraft.trim()}>
                Load repo
              </button>
              <button
                className="command-icon-button git-status-refresh-button"
                type="button"
                onClick={refreshCurrentStatus}
                disabled={!normalizedWorkdir || isLoading || isGitMutationPending}
                aria-label="Refresh git status"
                title="Refresh git status"
              >
                {isLoading ? (
                  <span className="activity-spinner git-status-refresh-spinner" aria-hidden="true" />
                ) : (
                  <RefreshIcon />
                )}
              </button>
            </div>
          </div>
        </form>
      ) : null}

      {!normalizedWorkdir ? (
        <EmptyState
          title="No workspace selected"
          body="Load a folder path to inspect the git repository for this tile. TermAl resolves the containing repo."
        />
      ) : null}

      {isLoading && !visibleStatus ? (
        <div
          className="git-status-loading-state"
          role="status"
          aria-label="Loading git status"
          title={normalizedWorkdir}
        >
          <span className="activity-spinner git-status-loading-spinner" aria-hidden="true" />
        </div>
      ) : null}

      {visibleError ? (
        <article className="thread-notice">
          <div className="card-label">Git</div>
          <p>{visibleError}</p>
        </article>
      ) : null}

      {visibleStatus && !visibleStatus.repoRoot ? (
        <EmptyState
          title="No git repository found"
          body="The selected folder is not inside a git repository."
        />
      ) : null}

      {visibleStatus?.repoRoot ? (
        <article className="message-card git-status-card">
          <div className="git-status-meta">
            <div className="git-status-meta-topline">
              <span className="chip git-status-branch-chip" title={branchName}>
                <BranchIcon />
                <span className="git-status-branch-chip-text">{branchName}</span>
              </span>
              {!showPathControls ? (
                <button
                  className="command-icon-button git-status-refresh-button"
                  type="button"
                  onClick={refreshCurrentStatus}
                  disabled={!normalizedWorkdir || isLoading || isGitMutationPending}
                  aria-label="Refresh git status"
                  title="Refresh git status"
                >
                  {isLoading ? (
                    <span className="activity-spinner git-status-refresh-spinner" aria-hidden="true" />
                  ) : (
                    <RefreshIcon />
                  )}
                </button>
              ) : null}
            </div>
          </div>
          {visibleStatus.isClean ? (
            <p className="support-copy git-status-empty-copy">Working tree clean.</p>
          ) : (
            <div className="git-status-sections">
              {sections.map((section) => (
                <GitStatusSection
                  key={section.id}
                  isExpanded={isTreeItemExpanded(sectionExpansionKey(section.id), section.fileCount > 0)}
                  onDirectoryAction={handleDirectoryAction}
                  onFileAction={handleFileAction}
                  onOpenDiff={handleOpenDiff}
                  onSectionAction={handleSectionAction}
                  onTreeToggle={toggleTreeItem}
                  mutationDisabled={isGitMutationPending}
                  pendingActionKey={pendingActionKey}
                  repoRoot={visibleStatus.repoRoot ?? ""}
                  section={section}
                  treeExpansionByKey={treeExpansionByKey}
                />
              ))}
            </div>
          )}
          <form
            className="git-status-commit-panel"
            onSubmit={(event) => {
              event.preventDefault();
              void submitCommit();
            }}
          >
            <label className="git-status-commit-label session-control-label" htmlFor="git-status-commit-message">
              Commit
            </label>
            <textarea
              id="git-status-commit-message"
              className="themed-input git-status-commit-input"
              value={commitMessage}
              onChange={(event) => setCommitMessage(event.target.value)}
              placeholder="Commit message"
              rows={3}
            />
            {commitNotice ? <p className="session-control-notice git-status-commit-notice">{commitNotice}</p> : null}
            <div className="git-status-commit-actions">
              <p className="support-copy git-status-commit-hint">
                {hasStagedChanges
                  ? "Staged changes are ready to commit."
                  : "Stage changes to enable commit."}
              </p>
              <button
                className="send-button git-status-commit-button"
                type="submit"
                disabled={!hasStagedChanges || !commitMessage.trim() || isGitMutationPending}
              >
                {isCommitting ? "Committing..." : "Commit"}
              </button>
            </div>
          </form>
        </article>
      ) : null}
    </div>
  );
}

const GitStatusSection = memo(function GitStatusSection({
  isExpanded,
  mutationDisabled,
  onDirectoryAction,
  onFileAction,
  onOpenDiff,
  onSectionAction,
  onTreeToggle,
  pendingActionKey,
  repoRoot,
  section,
  treeExpansionByKey,
}: {
  isExpanded: boolean;
  mutationDisabled: boolean;
  onDirectoryAction: (
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) => void;
  onFileAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenDiff: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, options?: GitDiffOpenOptions) => void;
  onSectionAction: (sectionId: GitStatusSectionId, nodes: GitStatusTreeNode[], action: GitFileAction) => void;
  onTreeToggle: (key: string, defaultValue: boolean) => void;
  pendingActionKey: string | null;
  repoRoot: string;
  section: GitStatusTreeSection;
  treeExpansionByKey: Record<string, boolean>;
}) {
  const isStaged = section.id === "staged";
  const sectionAction: GitFileAction = isStaged ? "unstage" : "stage";
  const sectionActionLabel = isStaged ? "Unstage all files" : "Stage all files";

  return (
    <section className="git-status-section">
      <div className="git-status-section-header">
        <button
          className="git-status-section-toggle"
          type="button"
          aria-expanded={isExpanded}
          onClick={() => onTreeToggle(sectionExpansionKey(section.id), section.fileCount > 0)}
        >
          <span className="git-tree-toggle" aria-hidden="true">
            <ChevronIcon expanded={isExpanded} />
          </span>
          <span className="git-status-section-label">{section.label}</span>
        </button>
        {section.fileCount > 0 ? (
          <button
            className="git-status-action-button git-status-section-action"
            type="button"
            onClick={() => onSectionAction(section.id, section.nodes, sectionAction)}
            aria-label={sectionActionLabel}
            title={sectionActionLabel}
            disabled={mutationDisabled}
          >
            {isStaged ? <UnstageIcon /> : <StageIcon />}
          </button>
        ) : null}
        <span className="git-status-section-count" aria-hidden="true">
          {section.fileCount}
        </span>
      </div>

      {isExpanded ? (
        section.fileCount > 0 ? (
          <GitStatusTree
            mutationDisabled={mutationDisabled}
            nodes={section.nodes}
            onDirectoryAction={onDirectoryAction}
            onFileAction={onFileAction}
            onOpenDiff={onOpenDiff}
            onTreeToggle={onTreeToggle}
            pendingActionKey={pendingActionKey}
            repoRoot={repoRoot}
            sectionId={section.id}
            treeExpansionByKey={treeExpansionByKey}
          />
        ) : (
          <p className="support-copy git-status-empty-copy">No {section.label.toLowerCase()} changes.</p>
        )
      ) : null}
    </section>
  );
});

const GitStatusTree = memo(function GitStatusTree({
  mutationDisabled,
  nodes,
  onDirectoryAction,
  onFileAction,
  onOpenDiff,
  onTreeToggle,
  pendingActionKey,
  repoRoot,
  sectionId,
  treeExpansionByKey,
}: {
  mutationDisabled: boolean;
  nodes: GitStatusTreeNode[];
  onDirectoryAction: (
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) => void;
  onFileAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenDiff: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, options?: GitDiffOpenOptions) => void;
  onTreeToggle: (key: string, defaultValue: boolean) => void;
  pendingActionKey: string | null;
  repoRoot: string;
  sectionId: GitStatusSectionId;
  treeExpansionByKey: Record<string, boolean>;
}) {
  return (
    <div className="git-status-tree">
      {nodes.map((node) =>
        node.kind === "directory" ? (
          <GitStatusDirectoryNode
            key={`${sectionId}:${node.path}`}
            mutationDisabled={mutationDisabled}
            node={node}
            onDirectoryAction={onDirectoryAction}
            onFileAction={onFileAction}
            onOpenDiff={onOpenDiff}
            onTreeToggle={onTreeToggle}
            pendingActionKey={pendingActionKey}
            repoRoot={repoRoot}
            sectionId={sectionId}
            treeExpansionByKey={treeExpansionByKey}
          />
        ) : (
          <GitStatusFileRow
            key={`${sectionId}:${node.path}`}
            isPending={pendingActionKey !== null && pendingActionKey.startsWith(`${sectionId}:${node.path}:`)}
            mutationDisabled={mutationDisabled}
            node={node}
            onAction={onFileAction}
            onOpenDiff={onOpenDiff}
            repoRoot={repoRoot}
            sectionId={sectionId}
          />
        ),
      )}
    </div>
  );
});

const GitStatusDirectoryNode = memo(function GitStatusDirectoryNode({
  mutationDisabled,
  node,
  onDirectoryAction,
  onFileAction,
  onOpenDiff,
  onTreeToggle,
  pendingActionKey,
  repoRoot,
  sectionId,
  treeExpansionByKey,
}: {
  mutationDisabled: boolean;
  node: GitStatusTreeDirectoryNode;
  onDirectoryAction: (
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) => void;
  onFileAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenDiff: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, options?: GitDiffOpenOptions) => void;
  onTreeToggle: (key: string, defaultValue: boolean) => void;
  pendingActionKey: string | null;
  repoRoot: string;
  sectionId: GitStatusSectionId;
  treeExpansionByKey: Record<string, boolean>;
}) {
  const expansionKey = directoryExpansionKey(sectionId, node.path);
  const isExpanded = treeExpansionByKey[expansionKey] ?? true;
  const isStaged = sectionId === "staged";
  const action: GitFileAction = isStaged ? "unstage" : "stage";
  const actionLabel = formatGitStageActionLabel(node.name, isStaged);
  const isPending = pendingActionKey === gitFileActionKey(sectionId, node.path, action);

  return (
    <div className="git-status-node">
      <div className={`git-status-tree-row git-status-tree-directory-row${isPending ? " pending" : ""}`}>
        <button
          className="git-status-tree-directory-toggle"
          type="button"
          aria-expanded={isExpanded}
          onClick={() => onTreeToggle(expansionKey, true)}
        >
          <span className="git-tree-toggle" aria-hidden="true">
            <ChevronIcon expanded={isExpanded} />
          </span>
          <span className="git-status-tree-label-group">
            <span className="git-status-tree-name">{node.name}</span>
          </span>
        </button>
        <div className="git-status-tree-tail">
          <div className="git-status-tree-actions">
            <button
              className="git-status-action-button"
              type="button"
              onClick={() => onDirectoryAction(sectionId, node, action)}
              aria-label={actionLabel}
              title={actionLabel}
              disabled={mutationDisabled}
            >
              {isStaged ? <UnstageIcon /> : <StageIcon />}
            </button>
          </div>
          <span className="git-status-tree-count" aria-hidden="true">
            {node.fileCount}
          </span>
        </div>
      </div>

      {isExpanded ? (
        <div className="git-status-tree-children">
          <GitStatusTree
            mutationDisabled={mutationDisabled}
            nodes={node.children}
            onDirectoryAction={onDirectoryAction}
            onFileAction={onFileAction}
            onOpenDiff={onOpenDiff}
            onTreeToggle={onTreeToggle}
            pendingActionKey={pendingActionKey}
            repoRoot={repoRoot}
            sectionId={sectionId}
            treeExpansionByKey={treeExpansionByKey}
          />
        </div>
      ) : null}
    </div>
  );
});

const GitStatusFileRow = memo(function GitStatusFileRow({
  isPending,
  mutationDisabled,
  node,
  onAction,
  onOpenDiff,
  repoRoot,
  sectionId,
}: {
  isPending: boolean;
  mutationDisabled: boolean;
  node: GitStatusTreeFileNode;
  onAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenDiff: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, options?: GitDiffOpenOptions) => void;
  repoRoot: string;
  sectionId: GitStatusSectionId;
}) {
  const tone = gitStatusTone(node.statusCode);
  const isStaged = sectionId === "staged";
  const stageActionLabel = formatGitStageActionLabel(node.name, isStaged);

  return (
    <div className={`git-status-tree-row git-status-tree-file-row${isPending ? " pending" : ""}`}>
      <button
        className="git-status-tree-open-button"
        type="button"
        onClick={(event) =>
          event.ctrlKey || event.metaKey
            ? onOpenDiff(sectionId, node, { openInNewTab: true })
            : onOpenDiff(sectionId, node)
        }
        disabled={isPending}
      >
        <span className="git-tree-toggle git-tree-toggle-placeholder" aria-hidden="true" />
        <span className="git-status-tree-label-group">
          <span className="git-status-tree-name">{node.name}</span>
          {node.originalPath ? <span className="git-status-tree-detail">from {node.originalPath}</span> : null}
        </span>
      </button>

      <div className="git-status-tree-tail">
        <div className="git-status-tree-actions">
          {!isStaged ? (
            <button
              className="git-status-action-button"
              type="button"
              onClick={() => onAction(sectionId, node, "revert")}
              aria-label={`Revert ${node.name}`}
              title={`Revert ${node.name}`}
              disabled={mutationDisabled}
            >
              <RevertIcon />
            </button>
          ) : null}
          <button
            className="git-status-action-button"
            type="button"
            onClick={() => onAction(sectionId, node, isStaged ? "unstage" : "stage")}
            aria-label={stageActionLabel}
            title={stageActionLabel}
            disabled={mutationDisabled}
          >
            {isStaged ? <UnstageIcon /> : <StageIcon />}
          </button>
        </div>

        <span
          className={`git-status-tree-status git-status-tree-status-${tone}`}
          title={node.statusLabel}
          aria-label={node.statusLabel}
        >
          {node.statusCode}
        </span>
      </div>
    </div>
  );
});

function toGitActionTarget(node: GitStatusTreeFileNode): GitActionTarget {
  return {
    originalPath: node.originalPath,
    path: node.path,
    statusCode: node.statusCode,
  };
}

function collectDirectoryTargets(node: GitStatusTreeDirectoryNode) {
  return collectGitActionTargets(node.children);
}

function collectGitActionTargets(nodes: GitStatusTreeNode[]): GitActionTarget[] {
  return nodes.flatMap((node) =>
    node.kind === "directory" ? collectGitActionTargets(node.children) : [toGitActionTarget(node)],
  );
}

function formatGitStageActionLabel(name: string, isStaged: boolean) {
  return isStaged ? `Move ${name} to unstaged` : `Stage ${name}`;
}

function sectionExpansionKey(sectionId: GitStatusSectionId) {
  return `section:${sectionId}`;
}

function directoryExpansionKey(sectionId: GitStatusSectionId, path: string) {
  return `directory:${sectionId}:${path}`;
}

function gitFileActionKey(sectionId: GitStatusSectionId, path: string, action: GitFileAction) {
  return `${sectionId}:${path}:${action}`;
}

function gitFileOpenKey(sectionId: GitStatusSectionId, path: string) {
  return `${sectionId}:${path}:open`;
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

function ChevronIcon({ expanded }: { expanded: boolean }) {
  return (
    <svg className="git-tree-chevron" viewBox="0 0 12 12" aria-hidden="true" focusable="false">
      {expanded ? (
        <path
          d="M2.5 4.25 6 7.75l3.5-3.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="1.5"
        />
      ) : (
        <path
          d="m4.25 2.5 3.5 3.5-3.5 3.5"
          fill="none"
          stroke="currentColor"
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth="1.5"
        />
      )}
    </svg>
  );
}

function BranchIcon() {
  return (
    <svg className="git-status-branch-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <circle cx="4" cy="4" r="1.55" fill="none" stroke="currentColor" strokeWidth="1.35" />
      <circle cx="4" cy="12" r="1.55" fill="none" stroke="currentColor" strokeWidth="1.35" />
      <circle cx="12" cy="8" r="1.55" fill="none" stroke="currentColor" strokeWidth="1.35" />
      <path
        d="M5.55 4v4a2.45 2.45 0 0 0 2.45 2.45H10.3"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.35"
      />
      <path
        d="M5.55 12V8a2.45 2.45 0 0 1 2.45-2.45H10.3"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.35"
      />
    </svg>
  );
}

function RefreshIcon() {
  return (
    <svg className="command-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M12.2 5.9A5 5 0 1 0 13 8"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.5"
      />
      <path
        d="M10.1 3.9h2.7v2.7"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}

function StageIcon() {
  return (
    <svg className="git-status-action-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M8 3.25v9.5M3.25 8h9.5"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.6"
      />
    </svg>
  );
}

function UnstageIcon() {
  return (
    <svg className="git-status-action-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M3.25 8h9.5"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.6"
      />
    </svg>
  );
}

function RevertIcon() {
  return (
    <svg className="git-status-action-icon" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M6.1 4.1 3.6 6.6l2.5 2.5"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.45"
      />
      <path
        d="M4.1 6.6h4.7a3.15 3.15 0 1 1 0 6.3H7.45"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.45"
      />
    </svg>
  );
}

function getErrorMessage(error: unknown) {
  if (error instanceof Error) {
    return error.message;
  }

  return "The request failed.";
}

export function __resetGitStatusPanelCacheForTests() {
  gitStatusPanelCache.clear();
}

export function __getGitStatusPanelCacheSizeForTests() {
  return gitStatusPanelCache.size;
}
