import type { GitStatusFile, GitSubmoduleFiles, GitSubmoduleStatus } from "../api";

export type GitStatusSectionId = "staged" | "unstaged";

export type GitPathTreeDirectoryNode<FileNode extends GitPathTreeFileNode> = {
  children: GitPathTreeNode<FileNode>[];
  fileCount: number;
  kind: "directory";
  name: string;
  path: string;
};

export type GitPathTreeFileNode = {
  kind: "file";
  name: string;
  path: string;
};

export type GitStatusTreeFileNode = GitPathTreeFileNode & {
  originalPath?: string | null;
  statusCode: string;
  statusLabel: string;
  submodule?: GitStatusTreeSubmodule;
};

export type GitStatusTreeSubmodule = {
  checkout: GitSubmoduleStatus["checkout"];
  commits?: GitSubmoduleFiles;
  modified?: GitSubmoduleFiles;
  untracked?: GitSubmoduleFiles;
};

export type GitPathTreeNode<FileNode extends GitPathTreeFileNode> = GitPathTreeDirectoryNode<FileNode> | FileNode;
export type GitStatusTreeDirectoryNode = GitPathTreeDirectoryNode<GitStatusTreeFileNode>;
export type GitStatusTreeNode = GitPathTreeNode<GitStatusTreeFileNode>;

export type GitStatusTreeSection = {
  fileCount: number;
  id: GitStatusSectionId;
  label: string;
  nodes: GitStatusTreeNode[];
};

type GitTreeFileEntry = Omit<GitStatusTreeFileNode, "kind" | "name">;

type GitTreeDirectoryBuilder<FileNode extends GitPathTreeFileNode> = {
  directories: Map<string, GitTreeDirectoryBuilder<FileNode>>;
  files: FileNode[];
  name: string;
  path: string;
};

const SECTION_DEFINITIONS: ReadonlyArray<Pick<GitStatusTreeSection, "id" | "label">> = [
  { id: "staged", label: "Staged" },
  { id: "unstaged", label: "Unstaged" },
];

const STATUS_LABELS: Record<string, string> = {
  "?": "Untracked",
  A: "Added",
  C: "Copied",
  D: "Deleted",
  M: "Modified",
  R: "Renamed",
  T: "Type changed",
  U: "Unmerged",
};

export function buildGitStatusTree(
  files: GitStatusFile[],
  previousSections?: readonly GitStatusTreeSection[],
): GitStatusTreeSection[] {
  const previousById = new Map(previousSections?.map((section) => [section.id, section]) ?? []);

  return SECTION_DEFINITIONS.map((definition) =>
    buildSection(definition.id, definition.label, files, previousById.get(definition.id)),
  );
}

export function gitStatusTone(statusCode: string) {
  switch (statusCode) {
    case "A":
    case "?":
      return "added";
    case "D":
      return "deleted";
    case "R":
    case "C":
      return "renamed";
    case "U":
      return "conflict";
    case "M":
    case "T":
    default:
      return "modified";
  }
}

function buildSection(
  id: GitStatusSectionId,
  label: string,
  files: GitStatusFile[],
  previousSection?: GitStatusTreeSection,
): GitStatusTreeSection {
  const entries = files
    .filter((file) => fileBelongsToSection(file, id))
    .map((file) => toTreeEntry(file, id));

  return reconcileSection(previousSection, {
    fileCount: entries.length,
    id,
    label,
    nodes: buildPathTree(entries),
  });
}

export function buildGitPathTree(paths: readonly string[]): GitPathTreeNode<GitPathTreeFileNode>[] {
  return buildPathTree(paths.map((path) => ({ path })));
}

function buildPathTree<Entry extends { path: string }>(entries: Entry[]): GitPathTreeNode<Entry & GitPathTreeFileNode>[] {
  const root = createDirectoryBuilder<Entry & GitPathTreeFileNode>("", "");
  for (const entry of entries) {
    const segments = splitGitPath(entry.path);
    insertFileEntry(root, { ...entry, kind: "file", name: segments[segments.length - 1] }, segments);
  }

  return materializeChildren(root);
}

function createDirectoryBuilder<FileNode extends GitPathTreeFileNode>(name: string, path: string): GitTreeDirectoryBuilder<FileNode> {
  return {
    directories: new Map(),
    files: [],
    name,
    path,
  };
}

function fileBelongsToSection(file: GitStatusFile, sectionId: GitStatusSectionId) {
  if (sectionId === "staged") {
    return hasTrackedChange(file.indexStatus);
  }

  return hasTrackedChange(file.worktreeStatus) || isUntracked(file);
}

function toTreeEntry(file: GitStatusFile, sectionId: GitStatusSectionId): GitTreeFileEntry {
  const statusCode = sectionId === "staged" ? normalizeGitStatus(file.indexStatus) : resolveUnstagedStatusCode(file);
  const submodule = file.submodule?.[sectionId] ? file.submodule : undefined;

  return {
    originalPath: file.originalPath,
    path: file.path,
    statusCode,
    statusLabel: STATUS_LABELS[statusCode] ?? "Changed",
    submodule: submodule && {
      checkout: submodule.checkout,
      commits: sectionId === "staged" ? submodule.stagedCommits : submodule.unstagedCommits,
      modified: sectionId === "unstaged" ? submodule.modified : undefined,
      untracked: sectionId === "unstaged" ? submodule.untracked : undefined,
    },
  };
}

function insertFileEntry<FileNode extends GitPathTreeFileNode>(
  root: GitTreeDirectoryBuilder<FileNode>,
  entry: FileNode,
  segments: string[],
) {
  let directory = root;

  for (let index = 0; index < segments.length - 1; index += 1) {
    const name = segments[index];
    const path = segments.slice(0, index + 1).join("/");
    let nextDirectory = directory.directories.get(name);
    if (!nextDirectory) {
      nextDirectory = createDirectoryBuilder<FileNode>(name, path);
      directory.directories.set(name, nextDirectory);
    }
    directory = nextDirectory;
  }

  directory.files.push(entry);
}

function materializeChildren<FileNode extends GitPathTreeFileNode>(
  directory: GitTreeDirectoryBuilder<FileNode>,
): GitPathTreeNode<FileNode>[] {
  const directories = Array.from(directory.directories.values())
    .sort((left, right) => compareGitTreeNames(left.name, right.name))
    .map(materializeDirectory);
  const files = [...directory.files].sort((left, right) => compareGitTreeNames(left.name, right.name));

  return [...directories, ...files];
}

function materializeDirectory<FileNode extends GitPathTreeFileNode>(
  directory: GitTreeDirectoryBuilder<FileNode>,
): GitPathTreeDirectoryNode<FileNode> {
  const children = materializeChildren(directory);
  const fileCount = children.reduce((count, child) => count + (child.kind === "file" ? 1 : child.fileCount), 0);

  return {
    children,
    fileCount,
    kind: "directory",
    name: directory.name,
    path: directory.path,
  };
}

function reconcileSection(
  previousSection: GitStatusTreeSection | undefined,
  nextSection: GitStatusTreeSection,
): GitStatusTreeSection {
  const reconciledNodes = reconcileNodes(previousSection?.nodes, nextSection.nodes);
  if (
    previousSection &&
    previousSection.fileCount === nextSection.fileCount &&
    previousSection.nodes.length === reconciledNodes.length &&
    previousSection.nodes.every((node, index) => node === reconciledNodes[index])
  ) {
    return previousSection;
  }

  return reconciledNodes === nextSection.nodes ? nextSection : { ...nextSection, nodes: reconciledNodes };
}

function reconcileNodes(
  previousNodes: readonly GitStatusTreeNode[] | undefined,
  nextNodes: GitStatusTreeNode[],
): GitStatusTreeNode[] {
  if (!previousNodes || previousNodes.length === 0) {
    return nextNodes;
  }

  const previousByKey = new Map(previousNodes.map((node) => [gitStatusTreeNodeKey(node), node]));
  let changed = previousNodes.length !== nextNodes.length;

  const reconciledNodes = nextNodes.map((nextNode, index) => {
    const previousNode = previousByKey.get(gitStatusTreeNodeKey(nextNode));
    if (!previousNode || previousNode.kind !== nextNode.kind) {
      changed = true;
      return nextNode;
    }

    const reconciledNode =
      nextNode.kind === "directory"
        ? reconcileDirectoryNode(previousNode, nextNode)
        : reconcileFileNode(previousNode, nextNode);

    if (previousNodes[index] !== reconciledNode) {
      changed = true;
    }

    return reconciledNode;
  });

  if (!changed && previousNodes.every((node, index) => node === reconciledNodes[index])) {
    return previousNodes as GitStatusTreeNode[];
  }

  return reconciledNodes;
}

function reconcileDirectoryNode(
  previousNode: GitStatusTreeNode,
  nextNode: GitStatusTreeDirectoryNode,
): GitStatusTreeDirectoryNode {
  if (previousNode.kind !== "directory") {
    return nextNode;
  }

  const reconciledChildren = reconcileNodes(previousNode.children, nextNode.children);
  if (
    previousNode.fileCount === nextNode.fileCount &&
    previousNode.children.length === reconciledChildren.length &&
    previousNode.children.every((child, index) => child === reconciledChildren[index])
  ) {
    return previousNode;
  }

  return reconciledChildren === nextNode.children ? nextNode : { ...nextNode, children: reconciledChildren };
}

function reconcileFileNode(previousNode: GitStatusTreeNode, nextNode: GitStatusTreeFileNode): GitStatusTreeFileNode {
  if (previousNode.kind !== "file") {
    return nextNode;
  }

  if (
    previousNode.name === nextNode.name &&
    previousNode.originalPath === nextNode.originalPath &&
    previousNode.path === nextNode.path &&
    previousNode.statusCode === nextNode.statusCode &&
    previousNode.statusLabel === nextNode.statusLabel &&
    sameSubmodule(previousNode.submodule, nextNode.submodule)
  ) {
    return previousNode;
  }

  return nextNode;
}

function sameSubmodule(left?: GitStatusTreeSubmodule, right?: GitStatusTreeSubmodule) {
  return (
    left === right ||
    (left !== undefined &&
      right !== undefined &&
      left.checkout === right.checkout &&
      sameSubmoduleFiles(left.commits, right.commits) &&
      sameSubmoduleFiles(left.modified, right.modified) &&
      sameSubmoduleFiles(left.untracked, right.untracked))
  );
}

function sameSubmoduleFiles(left?: GitSubmoduleFiles, right?: GitSubmoduleFiles) {
  if (left === right) {
    return true;
  }
  if (left?.state === "listed" && right?.state === "listed") {
    return (
      left.remaining === right.remaining &&
      Boolean(left.incomplete) === Boolean(right.incomplete) &&
      left.paths.length === right.paths.length &&
      left.paths.every((path, index) => path === right.paths[index])
    );
  }
  if (left?.state === "missingCommit" && right?.state === "missingCommit") {
    return left.commit === right.commit;
  }
  return left?.state === "unavailable" && right?.state === "unavailable";
}

function gitStatusTreeNodeKey(node: GitStatusTreeNode) {
  return `${node.kind}:${node.path}`;
}

function compareGitTreeNames(left: string, right: string) {
  return left.localeCompare(right, undefined, {
    numeric: true,
    sensitivity: "base",
  });
}

function resolveUnstagedStatusCode(file: GitStatusFile) {
  if (isUntracked(file)) {
    return "?";
  }

  return normalizeGitStatus(file.worktreeStatus);
}

function hasTrackedChange(status?: string | null) {
  const normalized = normalizeGitStatus(status);
  return normalized !== "." && normalized !== "?";
}

function isUntracked(file: GitStatusFile) {
  return normalizeGitStatus(file.indexStatus) === "?" || normalizeGitStatus(file.worktreeStatus) === "?";
}

function normalizeGitStatus(status?: string | null) {
  const code = status?.trim();
  if (!code || code === " ") {
    return ".";
  }

  return code;
}

function splitGitPath(path: string) {
  const segments = path.split("/").filter(Boolean);
  return segments.length > 0 ? segments : [path];
}
