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

export function buildGitStatusTree(files: GitStatusFile[]): GitStatusTreeSection[] {
  return SECTION_DEFINITIONS.map((definition) => buildSection(definition.id, definition.label, files));
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

function buildSection(id: GitStatusSectionId, label: string, files: GitStatusFile[]): GitStatusTreeSection {
  const entries = files
    .filter((file) => fileBelongsToSection(file, id))
    .map((file) => toTreeEntry(file, id));
  const root = createDirectoryBuilder("", "");

  for (const entry of entries) {
    insertFileEntry(root, entry);
  }

  return {
    fileCount: entries.length,
    id,
    label,
    nodes: materializeChildren(root),
  };
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
