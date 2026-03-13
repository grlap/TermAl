import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { applyGitFileAction, fetchGitStatus } from "../api";
import { GitStatusPanel } from "./GitStatusPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    applyGitFileAction: vi.fn(),
    fetchGitStatus: vi.fn(),
  };
});

const applyGitFileActionMock = vi.mocked(applyGitFileAction);
const fetchGitStatusMock = vi.mocked(fetchGitStatus);

describe("GitStatusPanel", () => {
  beforeEach(() => {
    applyGitFileActionMock.mockReset();
    fetchGitStatusMock.mockReset();
  });

  it("renders staged and unstaged trees and opens files relative to the repo root", async () => {
    fetchGitStatusMock.mockResolvedValue({
      ahead: 0,
      behind: 0,
      branch: "main",
      files: [
        {
          indexStatus: "M",
          path: "ui/src/App.tsx",
          worktreeStatus: "M",
        },
        {
          indexStatus: "A",
          path: "ui/src/panels/ControlPanelSurface.tsx",
        },
        {
          indexStatus: "?",
          path: "ui/src/agent-icon.tsx",
          worktreeStatus: "?",
        },
      ],
      isClean: false,
      repoRoot: "/repo",
      upstream: "origin/main",
      workdir: "/repo",
    });

    const onOpenPath = vi.fn();

    render(<GitStatusPanel workdir="/repo" onOpenPath={onOpenPath} onOpenWorkdir={() => {}} />);

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo");
    });

    expect(await screen.findByRole("button", { name: /^Staged\b/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Unstaged\b/i })).toBeInTheDocument();
    expect(screen.getAllByText("ui").length).toBeGreaterThan(0);
    expect(screen.getByText("ControlPanelSurface.tsx")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^ControlPanelSurface\.tsx$/i }));

    expect(onOpenPath).toHaveBeenCalledWith("/repo/ui/src/panels/ControlPanelSurface.tsx");

    fireEvent.click(screen.getByRole("button", { name: /^Staged\b/i }));

    expect(screen.queryByText("ControlPanelSurface.tsx")).not.toBeInTheDocument();
  });

  it("applies git file actions from file rows and refreshes the tree state", async () => {
    fetchGitStatusMock.mockResolvedValue({
      ahead: 0,
      behind: 0,
      branch: "main",
      files: [
        {
          indexStatus: "?",
          path: "scratch.txt",
          worktreeStatus: "?",
        },
      ],
      isClean: false,
      repoRoot: "/repo",
      upstream: "origin/main",
      workdir: "/repo",
    });
    applyGitFileActionMock.mockResolvedValue({
      ahead: 0,
      behind: 0,
      branch: "main",
      files: [
        {
          indexStatus: "A",
          path: "scratch.txt",
        },
      ],
      isClean: false,
      repoRoot: "/repo",
      upstream: "origin/main",
      workdir: "/repo",
    });

    render(<GitStatusPanel workdir="/repo" onOpenPath={() => {}} onOpenWorkdir={() => {}} />);

    await screen.findByText("scratch.txt");

    fireEvent.click(screen.getByRole("button", { name: /Stage scratch\.txt/i }));

    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledWith({
        action: "stage",
        originalPath: undefined,
        path: "scratch.txt",
        statusCode: "?",
        workdir: "/repo",
      });
    });

    expect(await screen.findByRole("button", { name: /Move scratch\.txt to unstaged/i })).toBeInTheDocument();
  });
});
