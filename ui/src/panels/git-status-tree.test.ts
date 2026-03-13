import { describe, expect, it } from "vitest";

import type { GitStatusFile } from "../api";
import {
  buildGitStatusTree,
  type GitStatusTreeDirectoryNode,
  type GitStatusTreeFileNode,
  type GitStatusTreeNode,
} from "./git-status-tree";

describe("buildGitStatusTree", () => {
  it("splits files into staged and unstaged sections while preserving directories", () => {
    const [staged, unstaged] = buildGitStatusTree([
      makeGitStatusFile("ui/src/App.tsx", { indexStatus: "M", worktreeStatus: "M" }),
      makeGitStatusFile("ui/src/agent-icon.tsx", { indexStatus: "?", worktreeStatus: "?" }),
      makeGitStatusFile("ui/src/panels/ControlPanelSurface.tsx", { indexStatus: "A" }),
      makeGitStatusFile("src/main.rs", { indexStatus: "R", originalPath: "src/old-main.rs" }),
    ]);

    expect(staged.fileCount).toBe(3);
    expect(unstaged.fileCount).toBe(2);

    const stagedUiDirectory = findDirectory(staged.nodes, "ui");
    const stagedUiSrcDirectory = findDirectory(stagedUiDirectory.children, "src");
    const stagedPanelsDirectory = findDirectory(stagedUiSrcDirectory.children, "panels");
    const stagedAppFile = findRequiredFile(stagedUiSrcDirectory.children, "App.tsx");
    const stagedPanelFile = findRequiredFile(stagedPanelsDirectory.children, "ControlPanelSurface.tsx");
    const stagedMainDirectory = findDirectory(staged.nodes, "src");
    const stagedMainFile = findRequiredFile(stagedMainDirectory.children, "main.rs");

    expect(stagedUiDirectory.fileCount).toBe(2);
    expect(stagedAppFile.statusCode).toBe("M");
    expect(stagedPanelFile.statusCode).toBe("A");
    expect(stagedMainFile.statusCode).toBe("R");
    expect(stagedMainFile.originalPath).toBe("src/old-main.rs");

    const unstagedUiDirectory = findDirectory(unstaged.nodes, "ui");
    const unstagedUiSrcDirectory = findDirectory(unstagedUiDirectory.children, "src");
    const unstagedAppFile = findRequiredFile(unstagedUiSrcDirectory.children, "App.tsx");
    const unstagedAgentFile = findRequiredFile(unstagedUiSrcDirectory.children, "agent-icon.tsx");

    expect(unstagedAppFile.statusCode).toBe("M");
    expect(unstagedAgentFile.statusCode).toBe("?");
    expect(findFile(stagedUiSrcDirectory.children, "agent-icon.tsx")).toBeUndefined();
  });
});

function makeGitStatusFile(path: string, overrides?: Partial<GitStatusFile>): GitStatusFile {
  return {
    path,
    ...overrides,
  };
}

function findDirectory(nodes: GitStatusTreeNode[], name: string): GitStatusTreeDirectoryNode {
  const directory = nodes.find(
    (node): node is GitStatusTreeDirectoryNode => node.kind === "directory" && node.name === name,
  );

  expect(directory).toBeDefined();
  return directory as GitStatusTreeDirectoryNode;
}

function findFile(nodes: GitStatusTreeNode[], name: string): GitStatusTreeFileNode | undefined {
  return nodes.find((node): node is GitStatusTreeFileNode => node.kind === "file" && node.name === name);
}

function findRequiredFile(nodes: GitStatusTreeNode[], name: string): GitStatusTreeFileNode {
  const file = findFile(nodes, name);

  expect(file).toBeDefined();
  return file as GitStatusTreeFileNode;
}
