import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  applyGitFileAction,
  commitGitChanges,
  fetchGitDiff,
  fetchGitStatus,
  type GitDiffResponse,
  type GitStatusFile,
  type GitStatusResponse,
} from "../api";
import { GitStatusPanel } from "./GitStatusPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    applyGitFileAction: vi.fn(),
    commitGitChanges: vi.fn(),
    fetchGitDiff: vi.fn(),
    fetchGitStatus: vi.fn(),
  };
});

const applyGitFileActionMock = vi.mocked(applyGitFileAction);
const commitGitChangesMock = vi.mocked(commitGitChanges);
const fetchGitDiffMock = vi.mocked(fetchGitDiff);
const fetchGitStatusMock = vi.mocked(fetchGitStatus);
const SESSION_ID = "session-1";

describe("GitStatusPanel", () => {
  beforeEach(() => {
    applyGitFileActionMock.mockReset();
    commitGitChangesMock.mockReset();
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

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null);
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

  it("passes openInNewTab when ctrl-clicking a git row", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "ui/src/ControlPanelSurface.tsx",
        },
      ]),
    );
    fetchGitDiffMock.mockResolvedValue(makeDiffResponse());

    const onOpenDiff = vi.fn();

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    const fileButton = await screen.findByRole("button", { name: /^ControlPanelSurface\.tsx$/i });

    fireEvent.click(fileButton, { ctrlKey: true });

    await waitFor(() => {
      expect(onOpenDiff).toHaveBeenCalledWith(makeDiffResponse(), { openInNewTab: true });
    });
  });

  it("loads a drafted repo path from the toolbar", () => {
    const onOpenWorkdir = vi.fn();

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir={null}
        onOpenDiff={() => {}}
        onOpenWorkdir={onOpenWorkdir}
      />,
    );

    fireEvent.change(screen.getByPlaceholderText(/folder inside it/i), {
      target: { value: "/repo/subdir" },
    });
    fireEvent.click(screen.getByRole("button", { name: /Load repo/i }));

    expect(onOpenWorkdir).toHaveBeenCalledWith("/repo/subdir");
  });

  it("refreshes the current repo from the icon button", async () => {
    fetchGitStatusMock
      .mockResolvedValueOnce(makeStatusResponse([]))
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "scratch.txt",
            worktreeStatus: "?",
          },
        ]),
      );

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("Working tree clean.");

    fireEvent.click(screen.getByRole("button", { name: /Refresh git status/i }));

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
    });
    expect(await screen.findByText("scratch.txt")).toBeInTheDocument();
  });

  it("keeps the current tree visible while a refresh is in flight", async () => {
    const refreshResponse = createDeferred<GitStatusResponse>();
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "scratch.txt",
            worktreeStatus: "?",
          },
        ]),
      )
      .mockImplementationOnce(() => refreshResponse.promise);

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("scratch.txt");

    fireEvent.click(screen.getByRole("button", { name: /Refresh git status/i }));

    expect(screen.getByText("scratch.txt")).toBeInTheDocument();
    expect(screen.queryByText(/Loading repository state/i)).not.toBeInTheDocument();

    refreshResponse.resolve(
      makeStatusResponse([
        {
          path: "next.txt",
          worktreeStatus: "?",
        },
      ]),
    );

    expect(await screen.findByText("next.txt")).toBeInTheDocument();
  });

  it("keeps a branch summary header when the parent supplies project scope", async () => {
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("Working tree clean.");

    expect(screen.queryByRole("button", { name: /Load repo/i })).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/folder inside it/i)).not.toBeInTheDocument();
    expect(screen.getByText("main")).toBeInTheDocument();
    expect(screen.queryByText("/repo")).not.toBeInTheDocument();
    expect(screen.queryByText(/tracking origin\/main/i)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Refresh git status/i })).toBeInTheDocument();
  });

  it("refreshes the current repo from the branch header when path controls are hidden", async () => {
    fetchGitStatusMock
      .mockResolvedValueOnce(makeStatusResponse([]))
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "scratch.txt",
            worktreeStatus: "?",
          },
        ]),
      );

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("Working tree clean.");

    fireEvent.click(screen.getByRole("button", { name: /Refresh git status/i }));

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
    });
    expect(await screen.findByText("scratch.txt")).toBeInTheDocument();
  });

  it("commits staged changes from the footer composer", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "src/main.rs",
        },
      ]),
    );
    commitGitChangesMock.mockResolvedValue({
      status: makeStatusResponse([]),
      summary: "Created commit: Tighten git footer",
    });

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("main.rs");

    fireEvent.change(screen.getByLabelText(/Commit/i), {
      target: { value: "Tighten git footer" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^Commit$/i }));

    await waitFor(() => {
      expect(commitGitChangesMock).toHaveBeenCalledWith({
        message: "Tighten git footer",
        workdir: "/repo",
      });
    });

    expect(await screen.findByText("Created commit: Tighten git footer")).toBeInTheDocument();
    expect(screen.getByText("Working tree clean.")).toBeInTheDocument();
  });

  it("loads git status without a live session when a repo path is available", async () => {
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    render(
      <GitStatusPanel
        sessionId={null}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null);
    });

    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
  });

  it("opens git diffs without a live session when a repo path is available", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "src/main.rs",
          worktreeStatus: "M",
        },
      ]),
    );
    fetchGitDiffMock.mockResolvedValue(makeDiffResponse());

    render(
      <GitStatusPanel
        sessionId={null}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null);
    });

    fireEvent.click(await screen.findByRole("button", { name: /^main\.rs$/i }));

    await waitFor(() => {
      expect(fetchGitDiffMock).toHaveBeenCalledWith({
        originalPath: undefined,
        path: "src/main.rs",
        sectionId: "unstaged",
        statusCode: "M",
        workdir: "/repo",
      });
    });
  });

  it("renders changed files when Windows path casing differs between request and response", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse(
        [
          {
            path: "src/main.rs",
            worktreeStatus: "M",
          },
        ],
        {
          repoRoot: "C:/Repo",
          workdir: "C:/Repo",
        },
      ),
    );

    render(
      <GitStatusPanel
        sessionId={null}
        workdir={"c:\\Repo\\"}
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(await screen.findByText("main.rs")).toBeInTheDocument();
  });

  it("renders changed files when the response workdir is a canonical Windows path alias", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse(
        [
          {
            path: "src/main.rs",
            worktreeStatus: "M",
          },
        ],
        {
          repoRoot: "D:/src/repo",
          workdir: "D:/src/repo",
        },
      ),
    );

    render(
      <GitStatusPanel
        sessionId={null}
        workdir={"Q:\\repo"}
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(await screen.findByText("main.rs")).toBeInTheDocument();
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
        sessionId={SESSION_ID}
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
        sessionId={SESSION_ID}
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
        sessionId={SESSION_ID}
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

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

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

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

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

  it("stages all files from the unstaged section header", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "src/main.rs",
          worktreeStatus: "M",
        },
        {
          path: "ui/src/App.tsx",
          worktreeStatus: "M",
        },
      ]),
    );
    applyGitFileActionMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "src/main.rs",
        },
        {
          indexStatus: "M",
          path: "ui/src/App.tsx",
        },
      ]),
    );

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Stage all files" }));

    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledTimes(2);
    });
  });

  it("unstages all files from the staged section header", async () => {
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          indexStatus: "M",
          path: "src/main.rs",
        },
        {
          indexStatus: "A",
          path: "ui/src/App.tsx",
        },
      ]),
    );
    applyGitFileActionMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "src/main.rs",
          worktreeStatus: "M",
        },
        {
          path: "ui/src/App.tsx",
          worktreeStatus: "M",
        },
      ]),
    );

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "Unstage all files" }));

    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledTimes(2);
    });
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

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

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

function makeStatusResponse(
  files: GitStatusFile[],
  overrides?: Partial<Pick<GitStatusResponse, "repoRoot" | "workdir">>,
): GitStatusResponse {
  return {
    ahead: 0,
    behind: 0,
    branch: "main",
    files,
    isClean: files.length === 0,
    repoRoot: overrides?.repoRoot ?? "/repo",
    upstream: "origin/main",
    workdir: overrides?.workdir ?? "/repo",
  };
}

function createDeferred<T>() {
  let resolve: ((value: T) => void) | undefined;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });

  return {
    promise,
    resolve(value: T) {
      resolve?.(value);
    },
  };
}
