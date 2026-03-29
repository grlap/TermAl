import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  fetchGitStatus,
  pushGitChanges,
  syncGitChanges,
  type GitStatusResponse,
} from "../api";
import { copyTextToClipboard } from "../clipboard";
import { SESSION_DRAG_MIME_TYPE } from "../session-drag";
import { PaneTabs } from "./PaneTabs";
import type { CodexState, Project, RemoteConfig, Session } from "../types";
import type { WorkspaceTab } from "../workspace";

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    fetchGitStatus: vi.fn(),
    pushGitChanges: vi.fn(),
    syncGitChanges: vi.fn(),
  };
});

vi.mock("../clipboard", () => ({
  copyTextToClipboard: vi.fn(() => Promise.resolve()),
}));

const fetchGitStatusMock = vi.mocked(fetchGitStatus);
const pushGitChangesMock = vi.mocked(pushGitChanges);
const syncGitChangesMock = vi.mocked(syncGitChanges);
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
    fetchGitStatusMock.mockReset();
    pushGitChangesMock.mockReset();
    syncGitChangesMock.mockReset();
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

  it("opens a dedicated git tab context menu with repo actions", async () => {
    const onSelectTab = vi.fn();
    fetchGitStatusMock.mockResolvedValue(
      makeGitStatus("C:/repo/packages/app", {
        ahead: 2,
        behind: 1,
        files: [{ path: "src/main.ts", indexStatus: "M", worktreeStatus: "M" }],
        isClean: false,
      }),
    );
    pushGitChangesMock.mockResolvedValue({
      status: makeGitStatus("C:/repo/packages/app"),
      summary: "Pushed current branch to origin/main.",
    });

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
          id: "tab-git",
          kind: "gitStatus",
          workdir: "C:/repo/packages/app",
          originSessionId: "session-1",
        },
      ],
    });

    fireEvent.contextMenu(screen.getByRole("tab", { name: /Git: app/i }), {
      clientX: 120,
      clientY: 80,
    });

    expect(onSelectTab).not.toHaveBeenCalled();
    expect(await screen.findByRole("menu", { name: "Git tab actions" })).toBeInTheDocument();
    expect(await screen.findByText("Branch: main")).toBeInTheDocument();
    expect(screen.getByText("Upstream: origin/main (ahead 2, behind 1)")).toBeInTheDocument();
    expect(screen.getByText("Status: 1 changed file")).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: "Copy Path" })).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: "Copy Relative Path" })).not.toBeInTheDocument();
    expect(screen.getByRole("menuitem", { name: "Git Sync" })).toBeEnabled();
    expect(screen.getByRole("menuitem", { name: "Git Push" })).toBeEnabled();

    await clickAndSettle(screen.getByRole("menuitem", { name: "Git Push" }));

    await waitFor(() => {
      expect(pushGitChangesMock).toHaveBeenCalledWith({
        projectId: null,
        sessionId: "session-1",
        workdir: "C:/repo/packages/app",
      });
    });
    expect(syncGitChangesMock).not.toHaveBeenCalled();
    expect(await screen.findByText("Pushed current branch to origin/main.")).toBeInTheDocument();
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

  it("renders file-type icons for source and diff tabs", () => {
    renderPaneTabs({
      projectLookup: new Map([
        ["project-1", makeProject("project-1", "C:/repo")],
      ]),
      tabs: [
        {
          id: "tab-source",
          kind: "source",
          path: "C:/repo/src/main.rs",
          originSessionId: null,
        },
        {
          id: "tab-diff",
          kind: "diffPreview",
          changeType: "edit",
          diff: "@@ -1 +1 @@",
          diffMessageId: "diff-1",
          filePath: "C:/repo/ui/src/App.tsx",
          language: "typescript",
          originProjectId: "project-1",
          originSessionId: null,
          summary: "Updated app",
        },
      ],
    });

    const sourceTab = screen.getByRole("tab", { name: /main\.rs/i });
    const diffTab = screen.getByRole("tab", { name: /Diff: App\.tsx/i });

    expect(sourceTab.querySelector('.pane-tab-file-icon[data-file-kind="rust"]')).not.toBeNull();
    expect(diffTab.querySelector('.pane-tab-file-icon[data-file-kind="typescript"]')).not.toBeNull();
  });

  it("shows Codex global notices in the status tooltip", async () => {
    renderPaneTabs({
      codexState: {
        notices: [
          {
            kind: "configWarning",
            level: "warning",
            title: "Config warning",
            detail: "Codex is using fallback sandbox defaults.",
            timestamp: "14:05",
            code: "sandbox_fallback",
          },
        ],
      },
      sessionLookup: new Map([["session-1", makeSession("session-1", "C:/repo", "Codex Live")]]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.mouseEnter(screen.getByRole("tab", { name: /Codex Live/i }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("Notices");
    expect(tooltip).toHaveTextContent("Config warning");
    expect(tooltip).toHaveTextContent("Codex is using fallback sandbox defaults.");
    expect(screen.getByTitle("1 Codex notice")).toBeInTheDocument();
  });

  it("scrolls overflowing tabs with the mouse wheel", () => {
    renderPaneTabs({
      sessionLookup: new Map([
        ["session-1", makeSession("session-1", "C:/repo", "Session One")],
        ["session-2", makeSession("session-2", "C:/repo", "Session Two")],
        ["session-3", makeSession("session-3", "C:/repo", "Session Three")],
      ]),
      tabs: [
        {
          id: "tab-session-1",
          kind: "session",
          sessionId: "session-1",
        },
        {
          id: "tab-session-2",
          kind: "session",
          sessionId: "session-2",
        },
        {
          id: "tab-session-3",
          kind: "session",
          sessionId: "session-3",
        },
      ],
    });

    const tablist = screen.getByRole("tablist", { name: "Tile tabs" });
    let scrollLeft = 40;
    Object.defineProperty(tablist, "clientWidth", {
      configurable: true,
      value: 220,
    });
    Object.defineProperty(tablist, "scrollWidth", {
      configurable: true,
      value: 620,
    });
    Object.defineProperty(tablist, "scrollLeft", {
      configurable: true,
      get: () => scrollLeft,
      set: (value: number) => {
        scrollLeft = value;
      },
    });

    const wheelEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      deltaY: 96,
    });
    tablist.dispatchEvent(wheelEvent);
    expect(scrollLeft).toBe(136);
  });

  it("accepts a dragged session on the tab rail", () => {
    const onTabDrop = vi.fn();

    renderPaneTabs({
      onTabDrop,
      sessionLookup: new Map([["session-1", makeSession("session-1", "C:/repo", "Session One")]]),
      tabs: [
        {
          id: "tab-session-1",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    const dataTransfer = {
      dropEffect: "move",
      effectAllowed: "all",
      getData: (format: string) =>
        format === SESSION_DRAG_MIME_TYPE ? JSON.stringify({ sessionId: "session-2" }) : "",
      setData: () => {},
      types: [SESSION_DRAG_MIME_TYPE],
    };

    const tablist = screen.getByRole("tablist", { name: "Tile tabs" });
    fireEvent.dragOver(tablist, { clientX: 180, dataTransfer });
    fireEvent.drop(tablist, { clientX: 180, dataTransfer });

    expect(onTabDrop).toHaveBeenCalledWith("pane-1", "tabs", 1, dataTransfer);
  });

  it("shows project and remote info in the status tooltip", async () => {
    renderPaneTabs({
      projectLookup: new Map([
        ["project-1", makeProject("project-1", "/remote/repo", "Questica", "ssh-lab")],
      ]),
      remoteLookup: new Map([
        [
          "ssh-lab",
          {
            id: "ssh-lab",
            name: "SSH Lab",
            transport: "ssh",
            enabled: true,
            host: "lab.internal",
            port: 22,
            user: "grzeg",
          },
        ],
      ]),
      sessionLookup: new Map([
        [
          "session-1",
          {
            ...makeSession("session-1", "/remote/repo", "Codex Remote"),
            projectId: "project-1",
          },
        ],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.mouseEnter(screen.getByRole("tab", { name: /Codex Remote/i }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("Project:");
    expect(tooltip).toHaveTextContent("Questica");
    expect(tooltip).toHaveTextContent("Location:");
    expect(tooltip).toHaveTextContent("SSH Lab (grzeg@lab.internal)");
  });

  it("shows generic status details for non-Codex agents", async () => {
    renderPaneTabs({
      sessionLookup: new Map([
        [
          "session-1",
          makeSession("session-1", "C:/repo", "Claude Main", {
            agent: "Claude",
            approvalPolicy: "on-request",
            claudeApprovalMode: "ask",
            claudeEffort: "high",
            externalSessionId: "claude-session-1",
            model: "claude-sonnet-4-5",
            status: "active",
          }),
        ],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.mouseEnter(screen.getByRole("tab", { name: /Claude Main/i }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("Agent:");
    expect(tooltip).toHaveTextContent("Claude");
    expect(tooltip).toHaveTextContent("State:");
    expect(tooltip).toHaveTextContent("Active");
    expect(tooltip).toHaveTextContent("Project:");
    expect(tooltip).toHaveTextContent("Workspace only");
    expect(tooltip).toHaveTextContent("Location:");
    expect(tooltip).toHaveTextContent("Local (This machine)");
    expect(tooltip).toHaveTextContent("Model:");
    expect(tooltip).toHaveTextContent("claude-sonnet-4-5");
    expect(tooltip).toHaveTextContent("Session:");
    expect(tooltip).toHaveTextContent("claude-session-1");
    expect(tooltip).toHaveTextContent("Policy:");
    expect(tooltip).toHaveTextContent("On Request");
    expect(tooltip).toHaveTextContent("Approval:");
    expect(tooltip).toHaveTextContent("Ask");
    expect(tooltip).toHaveTextContent("Effort:");
    expect(tooltip).toHaveTextContent("High");
  });

  it("prefers the live model label in the status tooltip", async () => {
    renderPaneTabs({
      sessionLookup: new Map([
        [
          "session-1",
          makeSession("session-1", "C:/repo", "Claude Default", {
            agent: "Claude",
            model: "default",
            modelOptions: [
              {
                label: "Default (recommended)",
                value: "default",
                description: "Opus 4.6",
                badges: ["Recommended"],
              },
              {
                label: "Sonnet",
                value: "sonnet",
              },
            ],
          }),
        ],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.mouseEnter(screen.getByRole("tab", { name: /Claude Default/i }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("Model:");
    expect(tooltip).toHaveTextContent("Default (recommended)");
  });

  it("shows Auto in the status tooltip when the session uses the auto sentinel", async () => {
    renderPaneTabs({
      sessionLookup: new Map([
        [
          "session-1",
          makeSession("session-1", "C:/repo", "Cursor Auto", {
            agent: "Cursor",
            cursorMode: "agent",
            model: "auto",
          }),
        ],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.mouseEnter(screen.getByRole("tab", { name: /Cursor Auto/i }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("Model:");
    expect(tooltip).toHaveTextContent("Auto");
  });

  it("falls back to the raw model string in the status tooltip when no live option matches", async () => {
    renderPaneTabs({
      sessionLookup: new Map([
        [
          "session-1",
          makeSession("session-1", "C:/repo", "Claude Custom", {
            agent: "Claude",
            model: "claude-custom-build",
            modelOptions: [{ label: "Sonnet", value: "sonnet" }],
          }),
        ],
      ]),
      tabs: [
        {
          id: "tab-session",
          kind: "session",
          sessionId: "session-1",
        },
      ],
    });

    fireEvent.mouseEnter(screen.getByRole("tab", { name: /Claude Custom/i }));

    const tooltip = await screen.findByRole("tooltip");
    expect(tooltip).toHaveTextContent("Model:");
    expect(tooltip).toHaveTextContent("claude-custom-build");
  });
});

function renderPaneTabs({
  codexState = {},
  onCloseTab = vi.fn(),
  onTabDrop = vi.fn(),
  onRenameSessionRequest = vi.fn(),
  onSelectTab = vi.fn(),
  projectLookup = new Map<string, Project>(),
  remoteLookup = new Map<string, RemoteConfig>(),
  sessionLookup = new Map<string, Session>(),
  tabs,
}: {
  codexState?: CodexState;
  onCloseTab?: (paneId: string, tabId: string) => void;
  onTabDrop?: (
    targetPaneId: string,
    placement: "left" | "right" | "top" | "bottom" | "tabs",
    tabIndex?: number,
    dataTransfer?: DataTransfer | null,
  ) => void;
  onRenameSessionRequest?: (
    sessionId: string,
    clientX: number,
    clientY: number,
    trigger?: HTMLElement | null,
  ) => void;
  onSelectTab?: (paneId: string, tabId: string) => void;
  projectLookup?: Map<string, Project>;
  remoteLookup?: Map<string, RemoteConfig>;
  sessionLookup?: Map<string, Session>;
  tabs: WorkspaceTab[];
}) {
  return render(
    <PaneTabs
      activeTabId={tabs[0]?.id ?? null}
      codexState={codexState}
      draggedTab={null}
      onCloseTab={onCloseTab}
      onRenameSessionRequest={onRenameSessionRequest}
      onSelectTab={onSelectTab}
      onTabDragEnd={() => {}}
      onTabDragStart={() => {}}
      onTabDrop={onTabDrop}
      paneId="pane-1"
      projectLookup={projectLookup}
      remoteLookup={remoteLookup}
      sessionLookup={sessionLookup}
      tabs={tabs}
      windowId="window-1"
    />,
  );
}

function makeProject(id: string, rootPath: string, name = "Repo", remoteId?: string): Project {
  return {
    id,
    name,
    remoteId,
    rootPath,
  };
}

function makeSession(
  id: string,
  workdir: string,
  name = "Session",
  overrides: Partial<Session> = {},
): Session {
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
    ...overrides,
  };
}


function makeGitStatus(
  workdir: string,
  overrides: Partial<GitStatusResponse> = {},
): GitStatusResponse {
  return {
    ahead: 0,
    behind: 0,
    branch: "main",
    files: [],
    isClean: true,
    repoRoot: "C:/repo",
    upstream: "origin/main",
    workdir,
    ...overrides,
  };
}
