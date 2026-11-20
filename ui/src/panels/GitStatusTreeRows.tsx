// Shared folder and file rows, chevrons, and name cells from GitStatusPanel.tsx.
// GitStatusPanel retains the Git requests, actions, and expansion state.

import { Fragment, memo, type ReactNode } from "react";
import type { GitPathTreeDirectoryNode, GitPathTreeFileNode, GitPathTreeNode } from "./git-status-tree";

export type GitStatusTreeExpansion = {
  onTreeToggle: (key: string, defaultValue: boolean) => void;
  treeExpansionByKey: Record<string, boolean>;
};

export type GitStatusTreeOptions<FileNode extends GitPathTreeFileNode> = GitStatusTreeExpansion & {
  asList?: boolean;
  directoryKey: (path: string) => string;
  renderFile: (node: FileNode) => ReactNode;
  directoryActions?: (node: GitPathTreeDirectoryNode<FileNode>) => { actions: ReactNode; isPending: boolean };
  showFolderPaths?: boolean;
};

export function GitStatusTreeRows<FileNode extends GitPathTreeFileNode>({ nodes, options }: {
  nodes: GitPathTreeNode<FileNode>[];
  options: GitStatusTreeOptions<FileNode>;
}) {
  const fileOccurrences = new Map<string, number>();
  const Tree = options.asList ? "ul" : "div";
  const FileItem = options.asList ? "li" : Fragment;
  return (
    <Tree className={options.asList ? "git-status-tree git-status-tree-list" : "git-status-tree"} role={options.asList ? "list" : undefined}>
      {nodes.map((node) => {
        if (node.kind === "directory") {
          return <GitStatusDirectoryRows key={`directory:${node.path}`} node={node} options={options} />;
        }

        // Git can decode different raw filenames to the same display path.
        const occurrence = fileOccurrences.get(node.path) ?? 0;
        fileOccurrences.set(node.path, occurrence + 1);
        return <FileItem key={`file:${JSON.stringify([node.path, occurrence])}`}>{options.renderFile(node)}</FileItem>;
      })}
    </Tree>
  );
}

function GitStatusDirectoryBranch<FileNode extends GitPathTreeFileNode>({ node, options }: {
  node: GitPathTreeDirectoryNode<FileNode>;
  options: GitStatusTreeOptions<FileNode>;
}) {
  const expansionKey = options.directoryKey(node.path);
  const isExpanded = options.treeExpansionByKey[expansionKey] ?? true;
  const directoryActions = options.directoryActions?.(node);
  const folderPath = options.showFolderPaths ? node.path : undefined;
  const folderParts = folderPath?.split("/");
  const parentName = folderParts?.[folderParts.length - 2];
  const DirectoryItem = options.asList ? "li" : "div";

  return (
    <DirectoryItem className="git-status-node">
      <div className={`git-status-tree-row git-status-tree-directory-row${directoryActions?.isPending ? " pending" : ""}`}>
        <button
          className="git-status-tree-directory-toggle"
          type="button"
          aria-expanded={isExpanded}
          aria-description={folderPath}
          title={folderPath}
          onClick={() => options.onTreeToggle(expansionKey, true)}
        >
          <span className="git-tree-toggle" aria-hidden="true">
            <ChevronIcon expanded={isExpanded} />
          </span>
          <GitStatusTreeName name={node.name} parentName={parentName} />
        </button>
        <div className="git-status-tree-tail">
          {directoryActions ? <div className="git-status-tree-actions">{directoryActions.actions}</div> : null}
          <span className="git-status-tree-count" aria-hidden="true">
            {node.fileCount}
          </span>
        </div>
      </div>
      {isExpanded ? (
        <div className="git-status-tree-children">
          <GitStatusTreeRows nodes={node.children} options={options} />
        </div>
      ) : null}
    </DirectoryItem>
  );
}

// Retain the generic file-node type at the memoized branch boundary.
const GitStatusDirectoryRows = memo(GitStatusDirectoryBranch) as typeof GitStatusDirectoryBranch;

export function GitStatusTreeName({ ariaHidden, name, originalPath, parentName, title }: {
  ariaHidden?: boolean;
  name: string;
  originalPath?: string | null;
  parentName?: string;
  title?: string;
}) {
  return (
    <span className="git-status-tree-label-group" aria-hidden={ariaHidden}>
      {parentName ? (
        <span className="git-status-submodule-folder-parent" aria-hidden="true">
          <span className="git-status-tree-detail">{parentName}</span><span>/</span>
        </span>
      ) : null}
      <span className="git-status-tree-name" title={title}>{name}</span>
      {originalPath ? <span className="git-status-tree-detail">from {originalPath}</span> : null}
    </span>
  );
}

export function ChevronIcon({ expanded }: { expanded: boolean }) {
  return (
    <svg className="git-tree-chevron" viewBox="0 0 12 12" aria-hidden="true" focusable="false">
      <path
        d={expanded ? "M2.5 4.25 6 7.75l3.5-3.5" : "m4.25 2.5 3.5 3.5-3.5 3.5"}
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth="1.5"
      />
    </svg>
  );
}
