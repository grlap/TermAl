import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { applyGitFileAction, fetchGitDiff, fetchGitStatus, type GitDiffResponse, type GitStatusFile, type GitStatusResponse } from "../api";
import { GitStatusPanel } from "./GitStatusPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    applyGitFileAction: vi.fn(),
    fetchGitDiff: vi.fn(),
    fetchGitStatus: vi.fn(),
  };
});

const applyGitFileActionMock = vi.mocked(applyGitFileAction);
const fetchGitDiffMock = vi.mocked(fetchGitDiff);
const fetchGitStatusMock = vi.mocked(fetchGitStatus);

describe("GitStatusPanel", () => {
  beforeEach(() => {
    applyGitFileActionMock.mockReset();
    fetchGitDiffMock.mockReset();
    fetchGitStatusMock.mockReset();
  });

  it("renders staged and unstaged trees and opens diff previews from git rows", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
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
      ]),
    );
    fetchGitDiffMock.mockResolvedValue(makeDiffResponse());

    const onOpenDiff = vi.fn();

    render(<GitStatusPanel workdir="/repo" onOpenDiff={onOpenDiff} onOpenWorkdir={() => {}} />);

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo");
    });

    expect(await screen.findByRole("button", { name: /^Staged\b/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Unstaged\b/i })).toBeInTheDocument();
    expect(screen.getAllByText("ui").length).toBeGreaterThan(0);
    expect(screen.getByText("ControlPanelSurface.tsx")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^ControlPanelSurface\.tsx$/i }));

    await waitFor(() => {
      expect(fetchGitDiffMock).toHaveBeenCalledWith({
        originalPath: undefined,
        path: "ui/src/panels/ControlPanelSurface.tsx",
        sectionId: "staged",
        statusCode: "A",
        workdir: "/repo",
      });
    });

    await waitFor(() => {
      expect(onOpenDiff).toHaveBeenCalledWith(makeDiffResponse());
    });

    fireEvent.click(screen.getByRole("button", { name: /^Staged\b/i }));

    expect(screen.queryByText("ControlPanelSurface.tsx")).not.toBeInTheDocument();
  });

  it("reports git status updates for badge counts", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "src/main.rs",
          worktreeStatus: "M",
        },
        {
          path: "ui/src/App.tsx",
          worktreeStatus: "M",
        },
      ]),
    );

    const onStatusChange = vi.fn();

    render(
      <GitStatusPanel
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(onStatusChange).toHaveBeenCalledWith(
        expect.objectContaining({
          files: expect.arrayContaining([
            expect.objectContaining({ path: "src/main.rs" }),
            expect.objectContaining({ path: "ui/src/App.tsx" }),
          ]),
        }),
      );
    });
  });

  it("does not refetch git status when only the callback prop changes", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "src/main.rs",
          worktreeStatus: "M",
        },
      ]),
    );

    const { rerender } = render(
      <GitStatusPanel
        workdir="/repo"
        onStatusChange={() => {}}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(screen.getAllByText("main.rs").length).toBeGreaterThan(0);
    });
    expect(fetchGitStatusMock).toHaveBeenCalledTimes(1);

    rerender(
      <GitStatusPanel
        workdir="/repo"
        onStatusChange={() => {}}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(fetchGitStatusMock).toHaveBeenCalledTimes(1);
  });

  it("applies git file actions from file rows and refreshes the tree state", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "?",
          path: "scratch.txt",
          worktreeStatus: "?",
        },
      ]),
    );
    applyGitFileActionMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "A",
          path: "scratch.txt",
        },
      ]),
    );

    render(<GitStatusPanel workdir="/repo" onOpenDiff={() => {}} onOpenWorkdir={() => {}} />);

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

  it("applies git actions from folder rows by forwarding each descendant file", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "ui/src/App.tsx",
          worktreeStatus: "M",
        },
        {
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
          worktreeStatus: "R",
        },
      ]),
    );
    applyGitFileActionMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "ui/src/App.tsx",
        },
        {
          indexStatus: "R",
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
        },
      ]),
    );

    render(<GitStatusPanel workdir="/repo" onOpenDiff={() => {}} onOpenWorkdir={() => {}} />);

    fireEvent.click(await screen.findByRole("button", { name: /Stage ui/i }));

    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledTimes(2);
    });

    const payloads = applyGitFileActionMock.mock.calls.map(([payload]) => payload);
    expect(payloads).toEqual(
      expect.arrayContaining([
        {
          action: "stage",
          originalPath: undefined,
          path: "ui/src/App.tsx",
          statusCode: "M",
          workdir: "/repo",
        },
        {
          action: "stage",
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
          statusCode: "R",
          workdir: "/repo",
        },
      ]),
    );

    expect(await screen.findByRole("button", { name: /Move ui to unstaged/i })).toBeInTheDocument();
  });

  it("applies git actions from staged folder rows and moves the folder back to unstaged", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "ui/src/App.tsx",
        },
        {
          indexStatus: "R",
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
        },
      ]),
    );
    applyGitFileActionMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "ui/src/App.tsx",
          worktreeStatus: "M",
        },
        {
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
          worktreeStatus: "R",
        },
      ]),
    );

    render(<GitStatusPanel workdir="/repo" onOpenDiff={() => {}} onOpenWorkdir={() => {}} />);

    fireEvent.click(await screen.findByRole("button", { name: /Move ui to unstaged/i }));

    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledTimes(2);
    });

    const payloads = applyGitFileActionMock.mock.calls.map(([payload]) => payload);
    expect(payloads).toEqual(
      expect.arrayContaining([
        {
          action: "unstage",
          originalPath: undefined,
          path: "ui/src/App.tsx",
          statusCode: "M",
          workdir: "/repo",
        },
        {
          action: "unstage",
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
          statusCode: "R",
          workdir: "/repo",
        },
      ]),
    );

    expect(await screen.findByRole("button", { name: /Stage ui/i })).toBeInTheDocument();
  });
});

function makeDiffResponse(): GitDiffResponse {
  return {
    changeType: "edit",
    diff: ["@@ -1 +1 @@", "-old", "+new"].join("\n"),
    diffId: "git:preview-1",
    filePath: "/repo/ui/src/panels/ControlPanelSurface.tsx",
    language: "typescript",
    summary: "Staged changes in ui/src/panels/ControlPanelSurface.tsx",
  };
}

function makeStatusResponse(files: GitStatusFile[]): GitStatusResponse {
  return {
    ahead: 0,
    behind: 0,
    branch: "main",
    files,
    isClean: files.length === 0,
    repoRoot: "/repo",
    upstream: "origin/main",
    workdir: "/repo",
  };
}
