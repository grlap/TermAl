import { useEffect, useMemo, useRef, useState } from "react";
import {
  applyGitFileAction,
  fetchGitStatus,
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

type GitActionTarget = {
  originalPath?: string | null;
  path: string;
  statusCode?: string | null;
};

export function GitStatusPanel({
  onStatusChange,
  onOpenPath,
  onOpenWorkdir,
  workdir,
}: {
  onStatusChange?: (status: GitStatusResponse | null) => void;
  onOpenPath: (path: string) => void;
  onOpenWorkdir: (path: string) => void;
  workdir: string | null;
}) {
  const [workdirDraft, setWorkdirDraft] = useState(workdir ?? "");
  const [status, setStatus] = useState<GitStatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [pendingActionKey, setPendingActionKey] = useState<string | null>(null);
  const [treeExpansionByKey, setTreeExpansionByKey] = useState<Record<string, boolean>>({});
  const onStatusChangeRef = useRef(onStatusChange);
  const normalizedWorkdir = workdir?.trim() ?? "";
  const changedFiles = status?.files ?? [];
  const sections = useMemo(() => buildGitStatusTree(changedFiles), [changedFiles]);

  useEffect(() => {
    onStatusChangeRef.current = onStatusChange;
  }, [onStatusChange]);

  useEffect(() => {
    setWorkdirDraft(workdir ?? "");
  }, [workdir]);

  useEffect(() => {
    setPendingActionKey(null);
    setTreeExpansionByKey({});
  }, [normalizedWorkdir]);

  useEffect(() => {
    if (!normalizedWorkdir) {
      setStatus(null);
      setError(null);
      onStatusChangeRef.current?.(null);
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
      onStatusChangeRef.current?.(response);
    } catch (nextError) {
      setStatus(null);
      setError(getErrorMessage(nextError));
      onStatusChangeRef.current?.(null);
    } finally {
      setIsLoading(false);
    }
  }

  async function handleFileAction(
    sectionId: GitStatusSectionId,
    node: GitStatusTreeFileNode,
    action: GitFileAction,
  ) {
    await handleTreeAction(sectionId, node.path, [toGitActionTarget(node)], action);
  }

  async function handleDirectoryAction(
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) {
    await handleTreeAction(sectionId, node.path, collectDirectoryTargets(node), action);
  }

  async function handleTreeAction(
    sectionId: GitStatusSectionId,
    actionPath: string,
    targets: GitActionTarget[],
    action: GitFileAction,
  ) {
    const activeWorkdir = status?.workdir ?? normalizedWorkdir;
    if (!activeWorkdir || targets.length === 0) {
      return;
    }

    const actionKey = gitFileActionKey(sectionId, actionPath, action);
    setPendingActionKey(actionKey);
    setError(null);

    try {
      let response: GitStatusResponse | null = null;

      for (const target of targets) {
        response = await applyGitFileAction({
          action,
          originalPath: target.originalPath,
          path: target.path,
          statusCode: target.statusCode,
          workdir: activeWorkdir,
        });
      }

      if (response) {
        setStatus(response);
        onStatusChangeRef.current?.(response);
      }
    } catch (nextError) {
      setError(getErrorMessage(nextError));
      if (targets.length > 1) {
        try {
          const refreshedStatus = await fetchGitStatus(activeWorkdir);
          setStatus(refreshedStatus);
          onStatusChangeRef.current?.(refreshedStatus);
        } catch {
          // Keep the action error visible if the follow-up refresh also fails.
        }
      }
    } finally {
      setPendingActionKey((current) => (current === actionKey ? null : current));
    }
  }

  function isTreeItemExpanded(key: string, defaultValue: boolean) {
    return treeExpansionByKey[key] ?? defaultValue;
  }

  function toggleTreeItem(key: string, defaultValue: boolean) {
    setTreeExpansionByKey((current) => ({
      ...current,
      [key]: !(current[key] ?? defaultValue),
    }));
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
            disabled={!normalizedWorkdir || isLoading}
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
          <div className="git-status-meta">
            <span className="git-status-meta-label">Git</span>
            <span className="git-status-meta-path" title={status.repoRoot}>
              {status.repoRoot}
            </span>
          </div>
          {branchSummary ? (
            <p className="git-status-branch-summary support-copy" title={branchSummary}>
              {branchSummary}
            </p>
          ) : null}
          {status.isClean ? (
            <p className="support-copy git-status-empty-copy">Working tree clean.</p>
          ) : (
            <div className="git-status-sections">
              {sections.map((section) => (
                <GitStatusSection
                  key={section.id}
                  isExpanded={isTreeItemExpanded(sectionExpansionKey(section.id), section.fileCount > 0)}
                  onDirectoryAction={handleDirectoryAction}
                  onFileAction={handleFileAction}
                  onOpenPath={onOpenPath}
                  onToggle={(defaultValue) => toggleTreeItem(sectionExpansionKey(section.id), defaultValue)}
                  onTreeToggle={toggleTreeItem}
                  pendingActionKey={pendingActionKey}
                  repoRoot={status.repoRoot ?? ""}
                  section={section}
                  treeExpansionByKey={treeExpansionByKey}
                />
              ))}
            </div>
          )}
        </article>
      ) : null}
    </div>
  );
}

function GitStatusSection({
  isExpanded,
  onDirectoryAction,
  onFileAction,
  onOpenPath,
  onToggle,
  onTreeToggle,
  pendingActionKey,
  repoRoot,
  section,
  treeExpansionByKey,
}: {
  isExpanded: boolean;
  onDirectoryAction: (
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) => void;
  onFileAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenPath: (path: string) => void;
  onToggle: (defaultValue: boolean) => void;
  onTreeToggle: (key: string, defaultValue: boolean) => void;
  pendingActionKey: string | null;
  repoRoot: string;
  section: GitStatusTreeSection;
  treeExpansionByKey: Record<string, boolean>;
}) {
  return (
    <section className="git-status-section">
      <button
        className="git-status-section-toggle"
        type="button"
        aria-expanded={isExpanded}
        onClick={() => onToggle(section.fileCount > 0)}
      >
        <span className="git-tree-toggle" aria-hidden="true">
          <ChevronIcon expanded={isExpanded} />
        </span>
        <span className="git-status-section-label">{section.label}</span>
        <span className="git-status-section-count" aria-hidden="true">
          {section.fileCount}
        </span>
      </button>

      {isExpanded ? (
        section.fileCount > 0 ? (
          <GitStatusTree
            nodes={section.nodes}
            onDirectoryAction={onDirectoryAction}
            onFileAction={onFileAction}
            onOpenPath={onOpenPath}
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
}

function GitStatusTree({
  nodes,
  onDirectoryAction,
  onFileAction,
  onOpenPath,
  onTreeToggle,
  pendingActionKey,
  repoRoot,
  sectionId,
  treeExpansionByKey,
}: {
  nodes: GitStatusTreeNode[];
  onDirectoryAction: (
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) => void;
  onFileAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenPath: (path: string) => void;
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
            node={node}
            onDirectoryAction={onDirectoryAction}
            onFileAction={onFileAction}
            onOpenPath={onOpenPath}
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
            node={node}
            onAction={onFileAction}
            onOpenPath={onOpenPath}
            repoRoot={repoRoot}
            sectionId={sectionId}
          />
        ),
      )}
    </div>
  );
}

function GitStatusDirectoryNode({
  node,
  onDirectoryAction,
  onFileAction,
  onOpenPath,
  onTreeToggle,
  pendingActionKey,
  repoRoot,
  sectionId,
  treeExpansionByKey,
}: {
  node: GitStatusTreeDirectoryNode;
  onDirectoryAction: (
    sectionId: GitStatusSectionId,
    node: GitStatusTreeDirectoryNode,
    action: GitFileAction,
  ) => void;
  onFileAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenPath: (path: string) => void;
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
        <div className="git-status-tree-actions">
          <button
            className="git-status-action-button"
            type="button"
            onClick={() => onDirectoryAction(sectionId, node, action)}
            aria-label={actionLabel}
            title={actionLabel}
            disabled={isPending}
          >
            {isStaged ? <UnstageIcon /> : <StageIcon />}
          </button>
        </div>
        <span className="git-status-tree-count" aria-hidden="true">
          {node.fileCount}
        </span>
      </div>

      {isExpanded ? (
        <div className="git-status-tree-children">
          <GitStatusTree
            nodes={node.children}
            onDirectoryAction={onDirectoryAction}
            onFileAction={onFileAction}
            onOpenPath={onOpenPath}
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
}

function GitStatusFileRow({
  isPending,
  node,
  onAction,
  onOpenPath,
  repoRoot,
  sectionId,
}: {
  isPending: boolean;
  node: GitStatusTreeFileNode;
  onAction: (sectionId: GitStatusSectionId, node: GitStatusTreeFileNode, action: GitFileAction) => void;
  onOpenPath: (path: string) => void;
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
        onClick={() => onOpenPath(resolveGitFilePath(repoRoot, node.path))}
      >
        <span className="git-tree-toggle git-tree-toggle-placeholder" aria-hidden="true" />
        <span className="git-status-tree-label-group">
          <span className="git-status-tree-name">{node.name}</span>
          {node.originalPath ? <span className="git-status-tree-detail">from {node.originalPath}</span> : null}
        </span>
      </button>

      <div className="git-status-tree-actions">
        {!isStaged ? (
          <button
            className="git-status-action-button"
            type="button"
            onClick={() => onAction(sectionId, node, "revert")}
            aria-label={`Revert ${node.name}`}
            title={`Revert ${node.name}`}
            disabled={isPending}
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
          disabled={isPending}
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
  );
}

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
