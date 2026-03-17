import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { copyTextToClipboard } from "../clipboard";
import { PaneTabs } from "./PaneTabs";
import type { Project, Session } from "../types";
import type { WorkspaceTab } from "../workspace";

vi.mock("../clipboard", () => ({
  copyTextToClipboard: vi.fn(() => Promise.resolve()),
}));

const copyTextToClipboardMock = vi.mocked(copyTextToClipboard);

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}

async function clickAndSettle(target: HTMLElement) {
  await act(async () => {
    fireEvent.click(target);
    await Promise.resolve();
  });
}

describe("PaneTabs", () => {
  const originalResizeObserver = globalThis.ResizeObserver;

  beforeEach(() => {
    copyTextToClipboardMock.mockReset();
    copyTextToClipboardMock.mockResolvedValue(undefined);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
  });

  afterEach(() => {
    cleanup();
    if (originalResizeObserver === undefined) {
      delete (globalThis as Partial<typeof globalThis>).ResizeObserver;
    } else {
      globalThis.ResizeObserver = originalResizeObserver;
    }
  });

  it("opens a file tab context menu on an inactive tab without selecting it first", async () => {
    const onSelectTab = vi.fn();

    renderPaneTabs({
      onSelectTab,
      sessionLookup: new Map([
        ["session-1", makeSession("session-1", "C:/repo")],
        ["session-2", makeSession("session-2", "C:/repo", "Other Session")],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-2",
        },
        {
          id: "tab-source",
          kind: "source",
          path: "C:/repo/src/main.rs",
          originSessionId: "session-1",
        },
      ],
    });

    fireEvent.contextMenu(screen.getByRole("tab", { name: /main\.rs/i }), {
      clientX: 120,
      clientY: 80,
    });

    expect(onSelectTab).not.toHaveBeenCalled();
    expect(await screen.findByRole("menu", { name: "File tab actions" })).toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Copy Relative Path" })).toBeEnabled();

    await clickAndSettle(screen.getByRole("menuitem", { name: "Copy Path" }));

    await waitFor(() => {
      expect(copyTextToClipboardMock).toHaveBeenCalledWith("C:/repo/src/main.rs");
    });
    expect(screen.queryByRole("menu", { name: "File tab actions" })).not.toBeInTheDocument();
  });

  it("copies a workspace-relative path for project-backed file tabs", async () => {
    renderPaneTabs({
      projectLookup: new Map([
        ["project-1", makeProject("project-1", "C:/repo")],
      ]),
      tabs: [
        {
          id: "tab-diff",
          kind: "diffPreview",
          changeType: "edit",
          diff: "@@ -1 +1 @@",
          diffMessageId: "diff-1",
          filePath: "C:/repo/src/lib.rs",
          originProjectId: "project-1",
          originSessionId: null,
          summary: "Updated lib",
        },
      ],
    });

    fireEvent.contextMenu(screen.getByRole("tab", { name: /Diff: lib\.rs/i }), {
      clientX: 120,
      clientY: 80,
    });

    expect(await screen.findByRole("menu", { name: "File tab actions" })).toBeInTheDocument();
    await clickAndSettle(screen.getByRole("menuitem", { name: "Copy Relative Path" }));

    await waitFor(() => {
      expect(copyTextToClipboardMock).toHaveBeenCalledWith("src/lib.rs");
    });
  });

  it("closes the file tab from the context menu", async () => {
    const onCloseTab = vi.fn();

    renderPaneTabs({
      onCloseTab,
      sessionLookup: new Map([
        ["session-1", makeSession("session-1", "C:/repo")],
      ]),
      tabs: [
        {
          id: "tab-source",
          kind: "source",
          path: "C:/repo/src/main.rs",
          originSessionId: "session-1",
        },
      ],
    });

    fireEvent.contextMenu(screen.getByRole("tab", { name: /main\.rs/i }), {
      clientX: 120,
      clientY: 80,
    });

    expect(await screen.findByRole("menu", { name: "File tab actions" })).toBeInTheDocument();
    await clickAndSettle(screen.getByRole("menuitem", { name: "Close" }));

    expect(onCloseTab).toHaveBeenCalledWith("pane-1", "tab-source");
  });

  it("keeps session-tab right click mapped to rename", () => {
    const onRenameSessionRequest = vi.fn();

    renderPaneTabs({
      onRenameSessionRequest,
      sessionLookup: new Map([
        ["session-1", makeSession("session-1", "C:/repo", "TermAlMain")],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.contextMenu(screen.getByRole("tab", { name: /TermAlMain/i }), {
      clientX: 220,
      clientY: 140,
    });

    expect(onRenameSessionRequest).toHaveBeenCalledWith(
      "session-1",
      220,
      140,
      expect.any(HTMLDivElement),
    );
    expect(screen.queryByRole("menu", { name: "File tab actions" })).not.toBeInTheDocument();
  });
});

function renderPaneTabs({
  onCloseTab = vi.fn(),
  onRenameSessionRequest = vi.fn(),
  onSelectTab = vi.fn(),
  projectLookup = new Map<string, Project>(),
  sessionLookup = new Map<string, Session>(),
  tabs,
}: {
  onCloseTab?: (paneId: string, tabId: string) => void;
  onRenameSessionRequest?: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  onSelectTab?: (paneId: string, tabId: string) => void;
  projectLookup?: Map<string, Project>;
  sessionLookup?: Map<string, Session>;
  tabs: WorkspaceTab[];
}) {
  return render(
    <PaneTabs
      activeTabId={tabs[0]?.id ?? null}
      codexState={{}}
      draggedTab={null}
      onCloseTab={onCloseTab}
      onRenameSessionRequest={onRenameSessionRequest}
      onSelectTab={onSelectTab}
      onTabDragEnd={() => {}}
      onTabDragStart={() => {}}
      onTabDrop={() => {}}
      paneId="pane-1"
      projectLookup={projectLookup}
      sessionLookup={sessionLookup}
      tabs={tabs}
      windowId="window-1"
    />,
  );
}

function makeProject(id: string, rootPath: string): Project {
  return {
    id,
    name: "Repo",
    rootPath,
  };
}

function makeSession(id: string, workdir: string, name = "Session"): Session {
  return {
    agent: "Codex",
    emoji: "O",
    id,
    messages: [],
    model: "gpt-5.4",
    name,
    preview: "Ready",
    status: "idle",
    workdir,
  };
}
