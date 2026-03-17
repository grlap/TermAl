import type { GitStatusFile } from "../api";

export type GitStatusSectionId = "staged" | "unstaged";

export type GitStatusTreeDirectoryNode = {
  children: GitStatusTreeNode[];
  fileCount: number;
  kind: "directory";
  name: string;
  path: string;
};

export type GitStatusTreeFileNode = {
  kind: "file";
  name: string;
  originalPath?: string | null;
  path: string;
  statusCode: string;
  statusLabel: string;
};

export type GitStatusTreeNode = GitStatusTreeDirectoryNode | GitStatusTreeFileNode;

export type GitStatusTreeSection = {
  fileCount: number;
  id: GitStatusSectionId;
  label: string;
  nodes: GitStatusTreeNode[];
};

type GitTreeFileEntry = {
  name: string;
  originalPath?: string | null;
  path: string;
  statusCode: string;
  statusLabel: string;
};

type GitTreeDirectoryBuilder = {
  directories: Map<string, GitTreeDirectoryBuilder>;
  files: GitTreeFileEntry[];
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
  const root = createDirectoryBuilder("", "");

  for (const entry of entries) {
    insertFileEntry(root, entry);
  }

  return reconcileSection(previousSection, {
    fileCount: entries.length,
    id,
    label,
    nodes: materializeChildren(root),
  });
}

function createDirectoryBuilder(name: string, path: string): GitTreeDirectoryBuilder {
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
  const segments = splitGitPath(file.path);

  return {
    name: segments[segments.length - 1] ?? file.path,
    originalPath: file.originalPath,
    path: file.path,
    statusCode,
    statusLabel: STATUS_LABELS[statusCode] ?? "Changed",
  };
}

function insertFileEntry(root: GitTreeDirectoryBuilder, entry: GitTreeFileEntry) {
  const segments = splitGitPath(entry.path);
  const fileName = segments[segments.length - 1] ?? entry.path;
  let directory = root;

  for (let index = 0; index < segments.length - 1; index += 1) {
    const name = segments[index];
    const path = segments.slice(0, index + 1).join("/");
    let nextDirectory = directory.directories.get(name);
    if (!nextDirectory) {
      nextDirectory = createDirectoryBuilder(name, path);
      directory.directories.set(name, nextDirectory);
    }
    directory = nextDirectory;
  }

  directory.files.push({
    ...entry,
    name: fileName,
  });
}

function materializeChildren(directory: GitTreeDirectoryBuilder): GitStatusTreeNode[] {
  const directories = Array.from(directory.directories.values())
    .sort((left, right) => compareGitTreeNames(left.name, right.name))
    .map(materializeDirectory);
  const files = [...directory.files]
    .sort((left, right) => compareGitTreeNames(left.name, right.name))
    .map(
      (file): GitStatusTreeFileNode => ({
        kind: "file",
        name: file.name,
        originalPath: file.originalPath,
        path: file.path,
        statusCode: file.statusCode,
        statusLabel: file.statusLabel,
      }),
    );

  return [...directories, ...files];
}

function materializeDirectory(directory: GitTreeDirectoryBuilder): GitStatusTreeDirectoryNode {
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
    previousNode.statusLabel === nextNode.statusLabel
  ) {
    return previousNode;
  }

  return nextNode;
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
