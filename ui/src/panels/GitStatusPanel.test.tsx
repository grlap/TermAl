import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  applyGitFileAction,
  commitGitChanges,
  fetchGitStatus,
  type GitStatusFile,
  type GitStatusResponse,
} from "../api";
import {
  __getGitStatusPanelCacheSizeForTests,
  __resetGitStatusPanelCacheForTests,
  GitStatusPanel,
} from "./GitStatusPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    applyGitFileAction: vi.fn(),
    commitGitChanges: vi.fn(),
    fetchGitStatus: vi.fn(),
  };
});

const applyGitFileActionMock = vi.mocked(applyGitFileAction);
const commitGitChangesMock = vi.mocked(commitGitChanges);
const fetchGitStatusMock = vi.mocked(fetchGitStatus);
const PROJECT_ID = "project-1";
const SESSION_ID = "session-1";

async function clickAndSettle(target: HTMLElement, eventInit?: MouseEventInit) {
  await act(async () => {
    fireEvent.click(target, eventInit);
    await Promise.resolve();
  });
}

describe("GitStatusPanel", () => {
  beforeEach(() => {
    __resetGitStatusPanelCacheForTests();
    applyGitFileActionMock.mockReset();
    commitGitChangesMock.mockReset();
    fetchGitStatusMock.mockReset();
  });

  it("keeps tree actions keyboard reachable without reserving filename width", async () => {
    const user = userEvent.setup();
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "long-file-name-that-needs-the-full-row.tsx",
          worktreeStatus: "M",
        },
      ]),
    );
    applyGitFileActionMock.mockResolvedValue(makeStatusResponse([]));

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    const openButton = await screen.findByRole("button", {
      name: "long-file-name-that-needs-the-full-row.tsx",
    });
    const revertButton = screen.getByRole("button", {
      name: "Revert long-file-name-that-needs-the-full-row.tsx",
    });
    const stageButton = screen.getByRole("button", {
      name: "Stage long-file-name-that-needs-the-full-row.tsx",
    });

    for (let index = 0; index < 20 && document.activeElement !== openButton; index += 1) {
      await user.tab();
    }
    expect(openButton).toHaveFocus();
    await user.tab();
    expect(revertButton).toHaveFocus();
    await user.tab();
    expect(stageButton).toHaveFocus();

    await user.keyboard("{Enter}");
    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledWith({
        action: "stage",
        originalPath: undefined,
        path: "long-file-name-that-needs-the-full-row.tsx",
        projectId: null,
        sessionId: SESSION_ID,
        statusCode: "M",
        workdir: "/repo",
      });
    });
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
    const deferredOpen = createDeferred<void>();
    const onOpenDiff = vi.fn(() => deferredOpen.promise);

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", SESSION_ID, { projectId: null });
    });

    expect(await screen.findByRole("button", { name: /^Staged\b/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Unstaged\b/i })).toBeInTheDocument();
    expect(screen.getAllByText("ui").length).toBeGreaterThan(0);
    expect(screen.getByText("ControlPanelSurface.tsx")).toBeInTheDocument();

    const fileButton = screen.getByRole("button", { name: /^ControlPanelSurface\.tsx$/i });
    // Keep the open request pending so the disabled row state is observable.
    // An async act boundary would remain open until `deferredOpen` resolves and
    // overlap the later resolution boundary.
    act(() => {
      fireEvent.click(fileButton);
    });

    expect(onOpenDiff).toHaveBeenCalledWith(
      {
        originalPath: undefined,
        path: "ui/src/panels/ControlPanelSurface.tsx",
        projectId: null,
        sectionId: "staged",
        sessionId: SESSION_ID,
        statusCode: "A",
        workdir: "/repo",
      },
      { sectionId: "staged" },
    );
    expect(fileButton).toBeDisabled();

    await act(async () => {
      deferredOpen.resolve();
      await Promise.resolve();
    });
    expect(fileButton).not.toBeDisabled();

    await clickAndSettle(screen.getByRole("button", { name: /^Staged\b/i }));

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
    const onOpenDiff = vi.fn().mockResolvedValue(undefined);

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    const fileButton = await screen.findByRole("button", { name: /^ControlPanelSurface\.tsx$/i });

    await clickAndSettle(fileButton, { ctrlKey: true });

    await waitFor(() => {
      expect(onOpenDiff).toHaveBeenCalledWith(
        {
          originalPath: undefined,
          path: "ui/src/ControlPanelSurface.tsx",
          projectId: null,
          sectionId: "staged",
          sessionId: SESSION_ID,
          statusCode: "M",
          workdir: "/repo",
        },
        { openInNewTab: true, sectionId: "staged" },
      );
    });
  });

  it("loads a drafted repo path from the toolbar", async () => {
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
    await clickAndSettle(screen.getByRole("button", { name: /Load repo/i }));

    expect(onOpenWorkdir).toHaveBeenCalledWith("/repo/subdir");
  });

  it("renders an icon-only loading state during the first repo load", () => {
    const response = createDeferred<GitStatusResponse>();
    fetchGitStatusMock.mockImplementationOnce(() => response.promise);

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(screen.getByRole("status", { name: /Loading git status/i })).toBeInTheDocument();
    expect(screen.queryByText(/Loading repository state/i)).not.toBeInTheDocument();
    expect(screen.queryByText("/repo")).not.toBeInTheDocument();
  });

  it("completes the authoritative status load after StrictMode effect replay", async () => {
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    render(
      <StrictMode>
        <GitStatusPanel
          sessionId={SESSION_ID}
          workdir="/repo"
          onOpenDiff={() => {}}
          onOpenWorkdir={() => {}}
        />
      </StrictMode>,
    );

    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
    expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
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

    await clickAndSettle(screen.getByRole("button", { name: /Refresh git status/i }));

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
    });
    expect(await screen.findByText("scratch.txt")).toBeInTheDocument();
  });

  it("shows cached tree state immediately but reconciles it after remount", async () => {
    const remountRefresh = createDeferred<GitStatusResponse>();
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "scratch.txt",
            worktreeStatus: "?",
          },
        ]),
      )
      .mockImplementationOnce(() => remountRefresh.promise);

    const firstRender = render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("scratch.txt");
    await clickAndSettle(screen.getByRole("button", { name: /^Unstaged\b/i }));
    expect(screen.queryByText("scratch.txt")).not.toBeInTheDocument();

    firstRender.unmount();

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: /^Unstaged\b/i })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("scratch.txt")).not.toBeInTheDocument();

    remountRefresh.resolve(makeStatusResponse([]));

    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
  });

  it("refreshes a visible repo on window focus after an external commit", async () => {
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "scratch.txt",
            worktreeStatus: "?",
          },
        ]),
      )
      .mockResolvedValueOnce(makeStatusResponse([]));

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("scratch.txt");
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });

    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
    expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
  });

  it("does not publish an unchanged background status", async () => {
    const unchangedStatus = makeStatusResponse([
      { path: "unchanged.txt", worktreeStatus: "?" },
    ]);
    const onStatusChange = vi.fn();
    fetchGitStatusMock
      .mockResolvedValueOnce(unchangedStatus)
      .mockResolvedValueOnce(structuredClone(unchangedStatus));

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("unchanged.txt");
    onStatusChange.mockClear();
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => expect(fetchGitStatusMock).toHaveBeenCalledTimes(2));
    await act(async () => {
      await Promise.resolve();
    });

    expect(onStatusChange).not.toHaveBeenCalled();
    expect(screen.getByText("unchanged.txt")).toBeInTheDocument();
  });

  it("lets a foreground refresh supersede a background poll without disabling refresh", async () => {
    const backgroundResponse = createDeferred<GitStatusResponse>();
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([{ path: "initial.txt", worktreeStatus: "?" }]),
      )
      .mockImplementationOnce(() => backgroundResponse.promise)
      .mockResolvedValueOnce(
        makeStatusResponse([{ path: "foreground.txt", worktreeStatus: "?" }]),
      );

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("initial.txt");
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => expect(fetchGitStatusMock).toHaveBeenCalledTimes(2));

    const refreshButton = screen.getByRole("button", { name: /Refresh git status/i });
    expect(refreshButton).toBeEnabled();
    await clickAndSettle(refreshButton);

    expect(await screen.findByText("foreground.txt")).toBeInTheDocument();
    expect(fetchGitStatusMock).toHaveBeenCalledTimes(3);

    await act(async () => {
      backgroundResponse.resolve(
        makeStatusResponse([{ path: "stale-background.txt", worktreeStatus: "?" }]),
      );
      await backgroundResponse.promise;
    });
    expect(screen.getByText("foreground.txt")).toBeInTheDocument();
    expect(screen.queryByText("stale-background.txt")).not.toBeInTheDocument();
  });

  it("does not let a background refresh clear an action error", async () => {
    const status = makeStatusResponse([
      {
        path: "still-changing.txt",
        worktreeStatus: "M",
      },
    ]);
    fetchGitStatusMock.mockResolvedValue(status);
    const onOpenDiff = vi.fn().mockRejectedValue(new Error("Unable to open this diff."));

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    await clickAndSettle(await screen.findByRole("button", { name: /^still-changing\.txt$/i }));
    expect(await screen.findByText("Unable to open this diff.")).toBeInTheDocument();

    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(3);
    });
    expect(screen.getByText("Unable to open this diff.")).toBeInTheDocument();
  });

  it("ignores an in-flight status response after unmount", async () => {
    const response = createDeferred<GitStatusResponse>();
    const onStatusChange = vi.fn();
    fetchGitStatusMock.mockImplementationOnce(() => response.promise);

    const view = render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(1);
    });
    onStatusChange.mockClear();
    view.unmount();

    await act(async () => {
      response.resolve(makeStatusResponse([]));
      await response.promise;
    });

    expect(onStatusChange).not.toHaveBeenCalled();
  });

  it("pauses interval refresh while the document is unfocused", async () => {
    vi.useFakeTimers();
    const hasFocus = vi.spyOn(document, "hasFocus").mockReturnValue(false);
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    const view = render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    try {
      await act(async () => {
        await Promise.resolve();
      });
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(1);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(1);

      hasFocus.mockReturnValue(true);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(10_000);
      });
      expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
    } finally {
      view.unmount();
      hasFocus.mockRestore();
      vi.useRealTimers();
    }
  });

  it("does not reuse cached status across different session scopes", async () => {
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "first-session.txt",
            worktreeStatus: "?",
          },
        ]),
      )
      .mockResolvedValueOnce(makeStatusResponse([]));

    const firstRender = render(
      <GitStatusPanel
        sessionId="session-a"
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );
    await screen.findByText("first-session.txt");
    firstRender.unmount();

    render(
      <GitStatusPanel
        sessionId="session-b"
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(screen.queryByText("first-session.txt")).not.toBeInTheDocument();
    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
    expect(fetchGitStatusMock).toHaveBeenLastCalledWith("/repo", "session-b", { projectId: null });
  });

  it("evicts the least recently used session scope from the bounded cache", async () => {
    for (let index = 0; index < 17; index += 1) {
      const scopedWorkdir = `/repo-${index}`;
      fetchGitStatusMock.mockResolvedValueOnce(
        makeStatusResponse(
          [{ path: `scope-${index}.txt`, worktreeStatus: "?" }],
          { repoRoot: scopedWorkdir, workdir: scopedWorkdir },
        ),
      );
      const view = render(
        <GitStatusPanel
          sessionId={`session-${index}`}
          workdir={scopedWorkdir}
          onOpenDiff={() => {}}
          onOpenWorkdir={() => {}}
        />,
      );
      await screen.findByText(`scope-${index}.txt`);
      view.unmount();
    }

    expect(__getGitStatusPanelCacheSizeForTests()).toBe(16);

    const revisit = createDeferred<GitStatusResponse>();
    fetchGitStatusMock.mockImplementationOnce(() => revisit.promise);
    render(
      <GitStatusPanel
        sessionId="session-0"
        workdir="/repo-0"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    expect(screen.queryByText("scope-0.txt")).not.toBeInTheDocument();
    await act(async () => {
      revisit.resolve(makeStatusResponse([], { repoRoot: "/repo-0", workdir: "/repo-0" }));
      await revisit.promise;
    });
    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
  });

  it("refreshes stale status after a diff open fails", async () => {
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([
          {
            path: "already-committed.txt",
            worktreeStatus: "M",
          },
        ]),
      )
      .mockResolvedValueOnce(makeStatusResponse([]));
    const onOpenDiff = vi.fn().mockRejectedValue(new Error("already-committed.txt now matches HEAD"));

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    await clickAndSettle(await screen.findByRole("button", { name: /^already-committed\.txt$/i }));

    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
    expect(screen.getByText("already-committed.txt now matches HEAD")).toBeInTheDocument();
    expect(fetchGitStatusMock).toHaveBeenCalledTimes(2);
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

    await clickAndSettle(screen.getByRole("button", { name: /Refresh git status/i }));

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

    await clickAndSettle(screen.getByRole("button", { name: /Refresh git status/i }));

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
    await clickAndSettle(screen.getByRole("button", { name: /^Commit$/i }));

    await waitFor(() => {
      expect(commitGitChangesMock).toHaveBeenCalledWith({
        message: "Tighten git footer",
        projectId: null,
        sessionId: SESSION_ID,
        workdir: "/repo",
      });
    });

    expect(await screen.findByText("Created commit: Tighten git footer")).toBeInTheDocument();
    expect(screen.getByText("Working tree clean.")).toBeInTheDocument();
  });

  it("ignores a completed commit after the panel changes session scope", async () => {
    const commitResponse = createDeferred<Awaited<ReturnType<typeof commitGitChanges>>>();
    const onStatusChange = vi.fn();
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([{ indexStatus: "M", path: "old-session.rs" }]),
      )
      .mockResolvedValueOnce(makeStatusResponse([]));
    commitGitChangesMock.mockImplementationOnce(() => commitResponse.promise);

    const { rerender } = render(
      <GitStatusPanel
        sessionId="session-a"
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("old-session.rs");
    fireEvent.change(screen.getByLabelText(/Commit/i), {
      target: { value: "Old session commit" },
    });
    await clickAndSettle(screen.getByRole("button", { name: /^Commit$/i }));
    expect(commitGitChangesMock).toHaveBeenCalledTimes(1);

    rerender(
      <GitStatusPanel
        sessionId="session-b"
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );
    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
    onStatusChange.mockClear();

    await act(async () => {
      commitResponse.resolve({
        status: makeStatusResponse([{ path: "stale-commit.txt", worktreeStatus: "?" }]),
        summary: "Created commit in old session",
      });
      await commitResponse.promise;
    });

    expect(screen.queryByText("stale-commit.txt")).not.toBeInTheDocument();
    expect(screen.queryByText("Created commit in old session")).not.toBeInTheDocument();
    expect(onStatusChange).not.toHaveBeenCalled();
  });

  it("blocks file actions until an in-flight commit settles", async () => {
    const status = makeStatusResponse([
      { indexStatus: "M", path: "staged.rs" },
      { path: "working.rs", worktreeStatus: "M" },
    ]);
    const commitResponse = createDeferred<Awaited<ReturnType<typeof commitGitChanges>>>();
    fetchGitStatusMock.mockResolvedValue(status);
    commitGitChangesMock.mockImplementationOnce(() => commitResponse.promise);

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("working.rs");
    fireEvent.change(screen.getByLabelText(/Commit/i), {
      target: { value: "Keep mutations serialized" },
    });
    await clickAndSettle(screen.getByRole("button", { name: /^Commit$/i }));
    expect(commitGitChangesMock).toHaveBeenCalledTimes(1);

    const stageButton = screen.getByRole("button", { name: /Stage working\.rs/i });
    expect(stageButton).toBeDisabled();
    fireEvent.click(stageButton);
    expect(applyGitFileActionMock).not.toHaveBeenCalled();

    await act(async () => {
      commitResponse.resolve({ status, summary: "Serialized commit completed" });
      await commitResponse.promise;
    });

    expect(await screen.findByText("Serialized commit completed")).toBeInTheDocument();
    expect(stageButton).toBeEnabled();
    expect(screen.getByRole("button", { name: /Refresh git status/i })).toBeEnabled();
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
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null, { projectId: null });
    });

    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
  });

  it("loads git status with project scope when a project tab provides one", async () => {
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    render(
      <GitStatusPanel
        projectId={PROJECT_ID}
        sessionId={null}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null, { projectId: PROJECT_ID });
    });

    expect(screen.queryByRole("button", { name: /Load repo/i })).not.toBeInTheDocument();
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
    const onOpenDiff = vi.fn().mockResolvedValue(undefined);

    render(
      <GitStatusPanel
        sessionId={null}
        workdir="/repo"
        showPathControls={false}
        onOpenDiff={onOpenDiff}
        onOpenWorkdir={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null, { projectId: null });
    });

    await clickAndSettle(await screen.findByRole("button", { name: /^main\.rs$/i }));

    await waitFor(() => {
      expect(onOpenDiff).toHaveBeenCalledWith(
        {
          originalPath: undefined,
          path: "src/main.rs",
          projectId: null,
          sectionId: "unstaged",
          sessionId: null,
          statusCode: "M",
          workdir: "/repo",
        },
        { sectionId: "unstaged" },
      );
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

    await clickAndSettle(screen.getByRole("button", { name: /Stage scratch\.txt/i }));

    await waitFor(() => {
      expect(applyGitFileActionMock).toHaveBeenCalledWith({
        action: "stage",
        originalPath: undefined,
        path: "scratch.txt",
        projectId: null,
        sessionId: SESSION_ID,
        statusCode: "?",
        workdir: "/repo",
      });
    });

    expect(await screen.findByRole("button", { name: /Move scratch\.txt to unstaged/i })).toBeInTheDocument();
  });

  it("ignores a failed file action after the panel changes session scope", async () => {
    const actionResponse = createDeferred<GitStatusResponse>();
    const onStatusChange = vi.fn();
    fetchGitStatusMock
      .mockResolvedValueOnce(
        makeStatusResponse([{ path: "old-action.txt", worktreeStatus: "?" }]),
      )
      .mockResolvedValueOnce(makeStatusResponse([]));
    applyGitFileActionMock.mockImplementationOnce(() => actionResponse.promise);

    const { rerender } = render(
      <GitStatusPanel
        sessionId="session-a"
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("old-action.txt");
    await clickAndSettle(
      screen.getByRole("button", { name: /Stage old-action\.txt/i }),
    );
    expect(applyGitFileActionMock).toHaveBeenCalledTimes(1);

    rerender(
      <GitStatusPanel
        sessionId="session-b"
        workdir="/repo"
        onStatusChange={onStatusChange}
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );
    expect(await screen.findByText("Working tree clean.")).toBeInTheDocument();
    onStatusChange.mockClear();

    await act(async () => {
      actionResponse.reject(new Error("Old session action failed"));
      await actionResponse.promise.catch(() => undefined);
    });

    expect(screen.queryByText("Old session action failed")).not.toBeInTheDocument();
    expect(onStatusChange).not.toHaveBeenCalled();
  });

  it("blocks commits until an in-flight file action settles", async () => {
    const status = makeStatusResponse([
      { indexStatus: "M", path: "staged.rs" },
      { path: "working.rs", worktreeStatus: "M" },
    ]);
    const actionResponse = createDeferred<GitStatusResponse>();
    fetchGitStatusMock.mockResolvedValue(status);
    applyGitFileActionMock.mockImplementationOnce(() => actionResponse.promise);

    render(
      <GitStatusPanel
        sessionId={SESSION_ID}
        workdir="/repo"
        onOpenDiff={() => {}}
        onOpenWorkdir={() => {}}
      />,
    );

    await screen.findByText("working.rs");
    fireEvent.change(screen.getByLabelText(/Commit/i), {
      target: { value: "Wait for the action" },
    });
    await clickAndSettle(
      screen.getByRole("button", { name: /Stage working\.rs/i }),
    );
    expect(applyGitFileActionMock).toHaveBeenCalledTimes(1);

    const commitButton = screen.getByRole("button", { name: /^Commit$/i });
    expect(commitButton).toBeDisabled();
    fireEvent.click(commitButton);
    expect(commitGitChangesMock).not.toHaveBeenCalled();

    await act(async () => {
      actionResponse.resolve(status);
      await actionResponse.promise;
    });

    await waitFor(() => expect(commitButton).toBeEnabled());
    expect(screen.getByRole("button", { name: /Refresh git status/i })).toBeEnabled();
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

    await clickAndSettle(await screen.findByRole("button", { name: /Stage ui/i }));

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
          projectId: null,
          sessionId: SESSION_ID,
          statusCode: "M",
          workdir: "/repo",
        },
        {
          action: "stage",
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
          projectId: null,
          sessionId: SESSION_ID,
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

    await clickAndSettle(await screen.findByRole("button", { name: "Stage all files" }));

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

    await clickAndSettle(await screen.findByRole("button", { name: "Unstage all files" }));

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

    await clickAndSettle(await screen.findByRole("button", { name: /Move ui to unstaged/i }));

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
          projectId: null,
          sessionId: SESSION_ID,
          statusCode: "M",
          workdir: "/repo",
        },
        {
          action: "unstage",
          originalPath: "legacy/Widget.tsx",
          path: "ui/src/Widget.tsx",
          projectId: null,
          sessionId: SESSION_ID,
          statusCode: "R",
          workdir: "/repo",
        },
      ]),
    );

    expect(await screen.findByRole("button", { name: /Stage ui/i })).toBeInTheDocument();
  });
});

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
  let reject: ((error: unknown) => void) | undefined;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });

  return {
    promise,
    resolve(value: T) {
      resolve?.(value);
    },
    reject(error: unknown) {
      reject?.(error);
    },
  };
}
