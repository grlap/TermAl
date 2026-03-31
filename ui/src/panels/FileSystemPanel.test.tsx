import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fetchDirectory, fetchGitStatus, type DirectoryResponse, type GitStatusResponse } from "../api";
import { FileSystemPanel } from "./FileSystemPanel";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    fetchDirectory: vi.fn(),
    fetchGitStatus: vi.fn(),
  };
});

const fetchDirectoryMock = vi.mocked(fetchDirectory);
const fetchGitStatusMock = vi.mocked(fetchGitStatus);

describe("FileSystemPanel", () => {
  beforeEach(() => {
    fetchDirectoryMock.mockReset();
    fetchGitStatusMock.mockReset();
  });

  it("renders a compact header spinner while the root folder is loading", async () => {
    fetchDirectoryMock.mockReturnValue(new Promise<DirectoryResponse>(() => {}));
    fetchGitStatusMock.mockReturnValue(new Promise<GitStatusResponse>(() => {}));

    const { container } = render(
      <FileSystemPanel
        rootPath="/repo"
        sessionId="session-1"
        showPathControls={false}
        onOpenPath={() => {}}
        onOpenRootPath={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchDirectoryMock).toHaveBeenCalledWith("/repo", { sessionId: "session-1", projectId: null });
    });

    expect(await screen.findByRole("status", { name: "Loading files" })).toBeInTheDocument();
    expect(screen.getByText("repo")).toBeInTheDocument();
    expect(screen.getByText("/repo")).toBeInTheDocument();
    expect(screen.queryByText("Loading folder")).not.toBeInTheDocument();
    expect(container.querySelector(".filesystem-root-loading-spinner")).not.toBeNull();
  });
  it("renders an explorer tree with git decorations when path controls are hidden", async () => {
    fetchDirectoryMock.mockImplementation(async (path) => {
      switch (path) {
        case "/repo":
          return makeDirectoryResponse("repo", "/repo", [
            { kind: "directory", name: "src", path: "/repo/src" },
            { kind: "file", name: "README.md", path: "/repo/README.md" },
          ]);
        case "/repo/src":
          return makeDirectoryResponse("src", "/repo/src", [
            { kind: "file", name: "main.rs", path: "/repo/src/main.rs" },
          ]);
        default:
          throw new Error(`Unexpected directory request: ${path}`);
      }
    });
    fetchGitStatusMock.mockResolvedValue(
      makeStatusResponse([
        {
          path: "README.md",
          worktreeStatus: "?",
        },
        {
          path: "src/main.rs",
          worktreeStatus: "M",
        },
      ]),
    );

    const onOpenPath = vi.fn();
    const { container } = render(
      <FileSystemPanel
        rootPath="/repo"
        sessionId="session-1"
        showPathControls={false}
        onOpenPath={onOpenPath}
        onOpenRootPath={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchDirectoryMock).toHaveBeenCalledWith("/repo", { sessionId: "session-1", projectId: null });
    });
    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", "session-1", { projectId: null });
    });

    const readmeButton = await screen.findByRole("button", { name: /^README\.md/i });

    expect(screen.queryByPlaceholderText("/absolute/path/to/folder")).not.toBeInTheDocument();
    expect(screen.getByText("repo")).toBeInTheDocument();
    expect(readmeButton).toBeInTheDocument();
    expect(container.querySelector(".filesystem-git-dot-modified")).not.toBeNull();
    expect(container.querySelector(".filesystem-git-badge-added")).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "src" }));

    await waitFor(() => {
      expect(fetchDirectoryMock).toHaveBeenCalledWith("/repo/src", { sessionId: "session-1", projectId: null });
    });

    const mainFileButton = await screen.findByRole("button", { name: /^main\.rs/i });
    expect(container.querySelector(".filesystem-git-badge-modified")).not.toBeNull();

    fireEvent.click(mainFileButton);

    expect(onOpenPath).toHaveBeenCalledWith("/repo/src/main.rs");
  });

  it("loads project-scoped directories without a live session", async () => {
    fetchDirectoryMock.mockResolvedValue(
      makeDirectoryResponse("repo", "/repo", [{ kind: "file", name: "main.rs", path: "/repo/main.rs" }]),
    );
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    render(
      <FileSystemPanel
        rootPath="/repo"
        sessionId={null}
        projectId="project-1"
        showPathControls={false}
        onOpenPath={() => {}}
        onOpenRootPath={() => {}}
      />,
    );

    await waitFor(() => {
      expect(fetchDirectoryMock).toHaveBeenCalledWith("/repo", { sessionId: null, projectId: "project-1" });
    });
    await waitFor(() => {
      expect(fetchGitStatusMock).toHaveBeenCalledWith("/repo", null, { projectId: "project-1" });
    });

    expect(await screen.findByRole("button", { name: /^main\.rs/i })).toBeInTheDocument();
    expect(
      screen.queryByText("This file browser is no longer associated with a live session or project."),
    ).not.toBeInTheDocument();
  });

  it("passes openInNewTab when ctrl-clicking a file", async () => {
    fetchDirectoryMock.mockResolvedValue(
      makeDirectoryResponse("repo", "/repo", [{ kind: "file", name: "main.rs", path: "/repo/main.rs" }]),
    );
    fetchGitStatusMock.mockResolvedValue(makeStatusResponse([]));

    const onOpenPath = vi.fn();

    render(
      <FileSystemPanel
        rootPath="/repo"
        sessionId="session-1"
        showPathControls={false}
        onOpenPath={onOpenPath}
        onOpenRootPath={() => {}}
      />,
    );

    const mainFileButton = await screen.findByRole("button", { name: /^main\.rs/i });

    fireEvent.click(mainFileButton, { ctrlKey: true });

    expect(onOpenPath).toHaveBeenCalledWith("/repo/main.rs", { openInNewTab: true });
  });

  it("opens a new root from the toolbar when path controls are visible", () => {
    const onOpenRootPath = vi.fn();

    render(
      <FileSystemPanel
        rootPath={null}
        sessionId="session-1"
        onOpenPath={() => {}}
        onOpenRootPath={onOpenRootPath}
      />,
    );

    fireEvent.change(screen.getByPlaceholderText("/absolute/path/to/folder"), {
      target: { value: " /repo/docs " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open" }));

    expect(onOpenRootPath).toHaveBeenCalledWith("/repo/docs");
  });
});

function makeDirectoryResponse(
  name: string,
  path: string,
  entries: DirectoryResponse["entries"],
): DirectoryResponse {
  return {
    entries,
    name,
    path,
  };
}

function makeStatusResponse(files: GitStatusResponse["files"]): GitStatusResponse {
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
