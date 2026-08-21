import { act, cleanup, render, screen } from "@testing-library/react";
import {
  StrictMode,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type Dispatch,
  type DragEvent as ReactDragEvent,
  type PointerEvent as ReactPointerEvent,
  type SetStateAction,
} from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useAppDragResize } from "./app-drag-resize";
import { resolveSessionPaneScrollStateKey } from "./SessionPaneView.scroll-key";
import {
  beginSessionPaneScrollPositionMigration,
  type PaneScrollPositionMigration,
  type PaneScrollPositionsByPane,
} from "./pane-scroll-position-migration";
import { attachSessionDragData } from "./session-drag";
import { TAB_DRAG_MIME_TYPE, type WorkspaceTabDrag } from "./tab-drag";
import type { ControlPanelSide } from "./workspace-storage";
import type { WorkspacePane, WorkspaceState, WorkspaceTab } from "./workspace";

class BroadcastChannelMock {
  static instances: BroadcastChannelMock[] = [];

  name: string;
  onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  close = vi.fn();
  postMessage = vi.fn();

  constructor(name: string) {
    this.name = name;
    BroadcastChannelMock.instances.push(this);
  }
}

function makeWorkspace(): WorkspaceState {
  const tabs = [
    { id: "tab-a", kind: "session", sessionId: "session-a" },
    { id: "tab-b", kind: "session", sessionId: "session-b" },
  ] as const;
  return {
    root: { type: "pane", paneId: "pane-a" },
    activePaneId: "pane-a",
    panes: [
      {
        id: "pane-a",
        activeSessionId: "session-a",
        activeTabId: "tab-a",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [...tabs],
        viewMode: "session",
      },
    ],
  };
}

function makeSplitWorkspace(): WorkspaceState {
  return {
    root: {
      id: "split-root",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: { type: "pane", paneId: "pane-a" },
      second: { type: "pane", paneId: "pane-b" },
    },
    activePaneId: "pane-a",
    panes: [
      {
        id: "pane-a",
        activeSessionId: "session-a",
        activeTabId: "tab-a",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [{ id: "tab-a", kind: "session", sessionId: "session-a" }],
        viewMode: "session",
      },
      {
        id: "pane-b",
        activeSessionId: "session-b",
        activeTabId: "tab-b",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [{ id: "tab-b", kind: "session", sessionId: "session-b" }],
        viewMode: "session",
      },
    ],
  };
}

function removeSecondPane(workspace: WorkspaceState): WorkspaceState {
  const firstPane = workspace.panes.find((pane) => pane.id === "pane-a");
  if (!firstPane) {
    throw new Error("Expected pane-a in the split workspace fixture");
  }
  return {
    ...workspace,
    root: { type: "pane", paneId: firstPane.id },
    activePaneId: firstPane.id,
    panes: [firstPane],
  };
}

function makeControlPanelSplitWorkspace(): WorkspaceState {
  return {
    root: {
      id: "split-root",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: { type: "pane", paneId: "pane-session" },
      second: { type: "pane", paneId: "pane-control" },
    },
    activePaneId: "pane-control",
    panes: [
      {
        id: "pane-session",
        activeSessionId: "session-a",
        activeTabId: "tab-session",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [
          { id: "tab-session", kind: "session", sessionId: "session-a" },
        ],
        viewMode: "session",
      },
      {
        id: "pane-control",
        activeSessionId: null,
        activeTabId: "tab-control",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [
          { id: "tab-control", kind: "controlPanel", originSessionId: null },
        ],
        viewMode: "controlPanel",
      },
    ],
  };
}

function makeDuplicateSessionWorkspace(): WorkspaceState {
  return {
    root: {
      id: "split-outer",
      type: "split",
      direction: "row",
      ratio: 0.5,
      first: {
        id: "split-inner",
        type: "split",
        direction: "row",
        ratio: 0.5,
        first: { type: "pane", paneId: "pane-a" },
        second: { type: "pane", paneId: "pane-duplicate" },
      },
      second: { type: "pane", paneId: "pane-target" },
    },
    activePaneId: "pane-a",
    panes: [
      {
        id: "pane-a",
        activeSessionId: "session-a",
        activeTabId: "tab-a",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [{ id: "tab-a", kind: "session", sessionId: "session-a" }],
        viewMode: "session",
      },
      {
        id: "pane-duplicate",
        activeSessionId: "session-a",
        activeTabId: "tab-duplicate",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [
          {
            id: "tab-duplicate",
            kind: "session",
            sessionId: "session-a",
          },
        ],
        viewMode: "session",
      },
      {
        id: "pane-target",
        activeSessionId: "session-b",
        activeTabId: "tab-target",
        lastSessionViewMode: "session",
        sourcePath: null,
        tabs: [
          { id: "tab-target", kind: "session", sessionId: "session-b" },
        ],
        viewMode: "session",
      },
    ],
  };
}

function makeDataTransfer() {
  const store = new Map<string, string>();
  return {
    effectAllowed: "all",
    getData: vi.fn((type: string) => store.get(type) ?? ""),
    setData: vi.fn((type: string, value: string) => {
      store.set(type, value);
    }),
  } as unknown as DataTransfer & {
    getData: ReturnType<typeof vi.fn>;
    setData: ReturnType<typeof vi.fn>;
  };
}

type DragResizeApi = ReturnType<typeof useAppDragResize>;

function requireDragResizeApi(api: DragResizeApi | null): DragResizeApi {
  if (!api) {
    throw new Error("useAppDragResize test API was not captured");
  }
  return api;
}

function Harness({
  applyWorkspaceAfterLayout,
  beginSessionTabScrollPositionMigration = () => null,
  copyWorkspaceAfterDropUpdate = false,
  rebuildWorkspaceAfterDropUpdate = false,
  initialWorkspace,
  layoutVersion,
  migrateSessionTabScrollPosition = () => false,
  onControlPanelLayoutSide,
  onLayout,
  onWorkspaceLayoutEffect,
  onReady,
  rebaseWorkspaceBeforeNextDrop,
  workspaceLayoutLoadPending = false,
}: {
  applyWorkspaceAfterLayout?: (workspace: WorkspaceState) => WorkspaceState;
  beginSessionTabScrollPositionMigration?: (input: {
    sessionId: string;
    sourcePaneId: string;
    targetPaneId: string;
  }) => PaneScrollPositionMigration | null;
  copyWorkspaceAfterDropUpdate?: boolean;
  rebuildWorkspaceAfterDropUpdate?: boolean;
  initialWorkspace?: WorkspaceState;
  layoutVersion: number;
  migrateSessionTabScrollPosition?: (input: {
    sessionId: string;
    sourcePaneId: string;
    targetPaneId: string;
  }) => boolean;
  onControlPanelLayoutSide?: (side: ControlPanelSide | undefined) => void;
  onLayout: (layoutVersion: number) => void;
  onWorkspaceLayoutEffect?: (workspace: WorkspaceState) => void;
  onReady?: (api: DragResizeApi) => void;
  rebaseWorkspaceBeforeNextDrop?: (
    workspace: WorkspaceState,
  ) => WorkspaceState;
  workspaceLayoutLoadPending?: boolean;
}) {
  const [workspace, setWorkspace] = useState(
    () => initialWorkspace ?? makeWorkspace(),
  );
  const [controlPanelSide, setControlPanelSide] =
    useState<ControlPanelSide>("left");
  const workspaceLayoutLoadPendingRef = useRef(false);
  const ignoreFetchedWorkspaceLayoutRef = useRef(false);
  const pendingDropRebaseRef = useRef(rebaseWorkspaceBeforeNextDrop);

  useEffect(() => {
    pendingDropRebaseRef.current = rebaseWorkspaceBeforeNextDrop;
  }, [rebaseWorkspaceBeforeNextDrop]);

  const setWorkspaceFromDrop = useCallback<
    Dispatch<SetStateAction<WorkspaceState>>
  >((update) => {
    setWorkspace((current) => {
      const rebase = pendingDropRebaseRef.current;
      pendingDropRebaseRef.current = undefined;
      const rebased = rebase?.(current) ?? current;
      const nextWorkspace =
        typeof update === "function" ? update(rebased) : update;
      if (rebuildWorkspaceAfterDropUpdate) {
        return {
          root: nextWorkspace.root,
          panes: nextWorkspace.panes,
          activePaneId: nextWorkspace.activePaneId,
          lastContentPaneId: nextWorkspace.lastContentPaneId,
          lastViewerPaneId: nextWorkspace.lastViewerPaneId,
        };
      }
      return copyWorkspaceAfterDropUpdate
        ? { ...nextWorkspace }
        : nextWorkspace;
    });
  }, [copyWorkspaceAfterDropUpdate, rebuildWorkspaceAfterDropUpdate]);
  const paneLookup = new Map(
    workspace.panes.map((pane): [string, WorkspacePane] => [pane.id, pane]),
  );

  workspaceLayoutLoadPendingRef.current = workspaceLayoutLoadPending;

  const dragResizeApi = useAppDragResize({
    windowId: "window-a",
    workspace,
    paneLookup,
    controlPanelSide,
    setControlPanelSide:
      setControlPanelSide as Dispatch<SetStateAction<ControlPanelSide>>,
    setWorkspace: setWorkspaceFromDrop,
    applyControlPanelLayout: (nextWorkspace, side) => {
      onLayout(layoutVersion);
      onControlPanelLayoutSide?.(side);
      return applyWorkspaceAfterLayout?.(nextWorkspace) ?? nextWorkspace;
    },
    workspaceLayoutLoadPendingRef,
    ignoreFetchedWorkspaceLayoutRef,
    beginSessionTabScrollPositionMigration,
    migrateSessionTabScrollPosition,
  });

  useLayoutEffect(() => {
    onWorkspaceLayoutEffect?.(workspace);
  }, [onWorkspaceLayoutEffect, workspace]);

  useEffect(() => {
    onReady?.(dragResizeApi);
  }, [dragResizeApi, onReady]);

  return (
    <>
      <div data-testid="tabs">
        {workspace.panes
          .flatMap((pane) => pane.tabs.map((tab) => tab.id))
          .join(",")}
      </div>
      <div data-testid="split-ratio">
        {workspace.root?.type === "split" ? workspace.root.ratio : "none"}
      </div>
      <div data-testid="ignore-layout">
        {ignoreFetchedWorkspaceLayoutRef.current ? "ignored" : "accepted"}
      </div>
      <div data-testid="control-panel-side">{controlPanelSide}</div>
    </>
  );
}

describe("useAppDragResize", () => {
  afterEach(() => {
    cleanup();
    if (vi.isFakeTimers()) {
      vi.useRealTimers();
    }
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    BroadcastChannelMock.instances = [];
  });

  it("keeps the tab-drag channel stable across ordinary renders", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    const onLayout = vi.fn();

    const { rerender } = render(
      <Harness layoutVersion={1} onLayout={onLayout} />,
    );
    await act(async () => {});

    const channel = BroadcastChannelMock.instances[0];
    expect(BroadcastChannelMock.instances).toHaveLength(1);

    rerender(<Harness layoutVersion={2} onLayout={onLayout} />);
    await act(async () => {});

    expect(BroadcastChannelMock.instances).toHaveLength(1);
    expect(channel.close).not.toHaveBeenCalled();

    act(() => {
      channel.onmessage?.({
        data: {
          type: "drop-commit",
          dragId: "drag-a",
          sourceWindowId: "window-a",
          sourcePaneId: "pane-a",
          tabId: "tab-a",
          targetWindowId: "window-b",
        },
      } as MessageEvent<unknown>);
    });

    expect(onLayout).toHaveBeenCalledWith(2);
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b");
  });

  it("publishes pane tab drag start and end messages", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const drag: WorkspaceTabDrag = {
      dragId: "drag-a",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-a",
      tabId: "tab-a",
      tab: { id: "tab-a", kind: "session", sessionId: "session-a" },
    };

    render(
      <Harness
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const api = requireDragResizeApi(dragResizeApi);
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      api.handleTabDragStart(drag);
    });
    expect(api.getKnownWorkspaceTabDrag()).toBe(drag);
    expect(channel.postMessage).toHaveBeenLastCalledWith({
      type: "drag-start",
      payload: drag,
    });

    act(() => {
      api.handleTabDragEnd();
    });
    expect(api.getKnownWorkspaceTabDrag()).toBeNull();
    expect(channel.postMessage).toHaveBeenLastCalledWith({
      type: "drag-end",
      dragId: "drag-a",
      sourceWindowId: "window-a",
    });
  });

  it("writes launcher drag data and clears launcher drag state", async () => {
    vi.useFakeTimers();
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();

    render(
      <Harness
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const api = requireDragResizeApi(dragResizeApi);

    act(() => {
      api.handleControlPanelLauncherDragStart(
        { dataTransfer } as unknown as ReactDragEvent<HTMLButtonElement>,
        "pane-a",
        "sessions",
        { id: "tab-c", kind: "sessionList", originSessionId: null },
      );
    });
    expect(dataTransfer.effectAllowed).toBe("copyMove");
    expect(dataTransfer.setData).toHaveBeenCalledWith(
      TAB_DRAG_MIME_TYPE,
      expect.any(String),
    );
    expect(
      api.getKnownWorkspaceTabDrag()?.sourcePaneId,
    ).toBe("control-panel-launcher:pane-a:sessions");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    act(() => {
      api.handleControlPanelLauncherDragEnd();
    });
    expect(api.getKnownWorkspaceTabDrag()).toBeNull();
  });

  it("moves a dragged tab on drop and marks the moved tab for scroll preservation", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const drag: WorkspaceTabDrag = {
      dragId: "drag-b",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-a",
      tabId: "tab-b",
      tab: { id: "tab-b", kind: "session", sessionId: "session-b" },
    };

    render(
      <Harness
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const api = requireDragResizeApi(dragResizeApi);

    act(() => {
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-a", "tabs", 0);
    });
    await act(async () => {});

    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
    expect(api.getKnownWorkspaceTabDrag()).toBeNull();
  });

  it("does not migrate scroll state when a session drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const onLayout = vi.fn();
    const migrateSessionTabScrollPosition = vi.fn(() => false);
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        migrateSessionTabScrollPosition={migrateSessionTabScrollPosition}
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-control",
        "tabs",
        undefined,
        dataTransfer,
      );
    });
    await act(async () => {});

    expect(onLayout).not.toHaveBeenCalled();
    expect(migrateSessionTabScrollPosition).not.toHaveBeenCalled();
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("migrates scroll state only when an accepted session drop commits", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const onLayout = vi.fn();
    const migrateSessionTabScrollPosition = vi.fn(() => false);
    const sourceKey = resolveSessionPaneScrollStateKey(
      "pane-a",
      "session",
      "session-a",
      null,
    );
    const targetKey = resolveSessionPaneScrollStateKey(
      "pane-b",
      "session",
      "session-a",
      null,
    );
    const paneScrollPositions: PaneScrollPositionsByPane = {
      "pane-a": {
        [sourceKey]: { shouldStick: false, top: 12_345 },
      },
    };
    const paneShouldStickToBottom: Record<string, boolean | undefined> = {
      [sourceKey]: false,
    };
    const beginSessionTabScrollPositionMigration = vi.fn((input) =>
      beginSessionPaneScrollPositionMigration({
        ...input,
        paneScrollPositions,
        paneShouldStickToBottom,
      }),
    );
    let bottomPinWrites = 0;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        beginSessionTabScrollPositionMigration={
          beginSessionTabScrollPositionMigration
        }
        migrateSessionTabScrollPosition={migrateSessionTabScrollPosition}
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        onWorkspaceLayoutEffect={(workspace) => {
          const targetPane = workspace.panes.find(
            (pane) => pane.id === "pane-b",
          );
          if (targetPane?.activeSessionId !== "session-a") {
            return;
          }
          if (!paneScrollPositions["pane-b"]?.[targetKey]) {
            bottomPinWrites += 1;
            paneScrollPositions["pane-b"] ??= {};
            paneScrollPositions["pane-b"][targetKey] = {
              shouldStick: true,
              top: Number.MAX_SAFE_INTEGER,
            };
          }
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(onLayout).toHaveBeenCalledOnce();
    expect(beginSessionTabScrollPositionMigration).toHaveBeenCalledOnce();
    expect(beginSessionTabScrollPositionMigration).toHaveBeenCalledWith({
      sessionId: "session-a",
      sourcePaneId: "pane-a",
      targetPaneId: "pane-b",
    });
    expect(migrateSessionTabScrollPosition).not.toHaveBeenCalled();
    expect(bottomPinWrites).toBe(0);
    expect(paneScrollPositions["pane-b"]?.[targetKey]).toEqual({
      shouldStick: false,
      top: 12_345,
    });
    expect(paneShouldStickToBottom[targetKey]).toBe(false);
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
  });

  it("keeps an accepted session drop committed under StrictMode updater replay", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <StrictMode>
        <Harness
          initialWorkspace={makeSplitWorkspace()}
          layoutVersion={1}
          onLayout={vi.fn()}
          onReady={(api) => {
            dragResizeApi = api;
          }}
        />
      </StrictMode>,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
  });

  it("keeps scroll markers when a session drop commits after an earlier duplicate", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeDuplicateSessionWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-target",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-duplicate,tab-target,tab-a",
    );
  });

  it("does not warn or migrate when a rebased session drop is refused despite a target duplicate", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let dragResizeApi: DragResizeApi | null = null;
    const migrateSessionTabScrollPosition = vi.fn(() => false);
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        migrateSessionTabScrollPosition={migrateSessionTabScrollPosition}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => {
          const targetPane = current.panes.find(
            (pane) => pane.id === "pane-b",
          );
          if (!targetPane) {
            throw new Error("Expected pane-b in the split workspace fixture");
          }
          const duplicateTab: WorkspaceTab = {
            id: "tab-duplicate",
            kind: "session",
            sessionId: "session-a",
          };
          const rebasedTargetPane: WorkspacePane = {
            ...targetPane,
            activeSessionId: "session-a",
            activeTabId: duplicateTab.id,
            tabs: [duplicateTab],
          };
          return {
            ...current,
            root: { type: "pane", paneId: rebasedTargetPane.id },
            activePaneId: rebasedTargetPane.id,
            panes: [rebasedTargetPane],
          };
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-duplicate");
    expect(migrateSessionTabScrollPosition).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });

  it("keeps markers when a rebase recreates the captured session tab", async () => {
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000031",
    );
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => {
          const targetPane = current.panes.find(
            (pane) => pane.id === "pane-b",
          );
          if (!targetPane) {
            throw new Error("Expected pane-b in the split workspace fixture");
          }
          return {
            ...current,
            root: { type: "pane", paneId: targetPane.id },
            activePaneId: targetPane.id,
            panes: [targetPane],
          };
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-b,00000000-0000-4000-8000-000000000031",
    );
  });

  it("rolls back a metadata-stripped session drop after rebase inserts it first", async () => {
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000032",
    );
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-c", "Session C");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => ({
          ...current,
          activePaneId: "pane-b",
          panes: current.panes.map((pane) =>
            pane.id === "pane-b"
              ? {
                  ...pane,
                  activeSessionId: "session-c",
                  activeTabId: "tab-rebased-session-c",
                  tabs: [
                    ...pane.tabs,
                    {
                      id: "tab-rebased-session-c",
                      kind: "session" as const,
                      sessionId: "session-c",
                    },
                  ],
                }
              : pane,
          ),
        })}
        rebuildWorkspaceAfterDropUpdate
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,tab-rebased-session-c",
    );
  });

  it("keeps markers when a split session drop activates one of several existing tabs", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");
    const initialWorkspace = makeDuplicateSessionWorkspace();

    render(
      <Harness
        initialWorkspace={{
          ...initialWorkspace,
          activePaneId: "pane-target",
        }}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-target",
        "right",
        undefined,
        dataTransfer,
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-duplicate,tab-target",
    );
  });

  it("does not mark sessions when a pane-tab drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const onLayout = vi.fn();
    const drag: WorkspaceTabDrag = {
      dragId: "drag-refused-pane",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-session",
      tabId: "tab-session",
      tab: { id: "tab-session", kind: "session", sessionId: "session-a" },
    };

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-control", "tabs");
    });

    expect(onLayout).not.toHaveBeenCalled();
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("does not mark sessions when a launcher drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const onLayout = vi.fn();
    const dataTransfer = makeDataTransfer();

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleControlPanelLauncherDragStart(
        { dataTransfer } as unknown as ReactDragEvent<HTMLButtonElement>,
        "pane-control",
        "sessions",
        { id: "tab-launcher", kind: "sessionList", originSessionId: null },
      );
      api.handleTabDrop("pane-control", "tabs", undefined, dataTransfer);
    });

    expect(onLayout).not.toHaveBeenCalled();
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("does not commit or mark an external tab drop that is refused", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const onLayout = vi.fn();
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-refused-external",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external",
      tab: { id: "tab-external", kind: "session", sessionId: "session-b" },
    };

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-control",
        "tabs",
      );
    });

    expect(onLayout).not.toHaveBeenCalled();
    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drag-end",
      dragId: externalDrag.dragId,
      sourceWindowId: externalDrag.sourceWindowId,
    });
  });

  it("acknowledges an external drop after its transferred tab commits", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000001",
    );
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-accepted-external",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external",
      tab: { id: "tab-external", kind: "session", sessionId: "session-c" },
    };

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,00000000-0000-4000-8000-000000000001",
    );
    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drop-commit",
      dragId: externalDrag.dragId,
      sourceWindowId: externalDrag.sourceWindowId,
      sourcePaneId: externalDrag.sourcePaneId,
      tabId: externalDrag.tabId,
      targetWindowId: "window-a",
    });
  });

  it("preserves external drop acknowledgement through a same-flush workspace spread", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000011",
    );
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-spread-external",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-spread-external",
      tab: {
        id: "tab-spread-external",
        kind: "session",
        sessionId: "session-spread",
      },
    };

    render(
      <Harness
        copyWorkspaceAfterDropUpdate
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,00000000-0000-4000-8000-000000000011",
    );
    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drop-commit",
      dragId: externalDrag.dragId,
      sourceWindowId: externalDrag.sourceWindowId,
      sourcePaneId: externalDrag.sourcePaneId,
      tabId: externalDrag.tabId,
      targetWindowId: "window-a",
    });
  });

  it("acknowledges a fresh external tab after a wrapper strips commit metadata", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000012",
    );
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-rebuilt-external",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-rebuilt-external",
      tab: {
        id: "tab-rebuilt-external",
        kind: "session",
        sessionId: "session-rebuilt",
      },
    };

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebuildWorkspaceAfterDropUpdate
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(warn).toHaveBeenCalledOnce();
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("structural fallback"),
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,00000000-0000-4000-8000-000000000012",
    );
    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drop-commit",
      dragId: externalDrag.dragId,
      sourceWindowId: externalDrag.sourceWindowId,
      sourcePaneId: externalDrag.sourcePaneId,
      tabId: externalDrag.tabId,
      targetWindowId: "window-a",
    });
  });

  it("rolls back an ambiguous existing-session drop after a wrapper strips commit metadata", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebuildWorkspaceAfterDropUpdate
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "tabs",
        undefined,
        dataTransfer,
      );
    });

    expect(warn).toHaveBeenCalledOnce();
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("Could not verify a workspace drop"),
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
  });

  it("rolls back existing-tab edge-drop markers after a wrapper strips commit metadata", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let dragResizeApi: DragResizeApi | null = null;
    const drag: WorkspaceTabDrag = {
      dragId: "drag-rebuilt-cross-pane",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-a",
      tabId: "tab-a",
      tab: { id: "tab-a", kind: "session", sessionId: "session-a" },
    };

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebuildWorkspaceAfterDropUpdate
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-b", "left");
    });

    expect(warn).toHaveBeenCalledOnce();
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("Could not verify a workspace drop"),
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
  });

  it("keeps fresh session edge-drop markers after a wrapper strips commit metadata", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    vi.spyOn(crypto, "randomUUID")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000033")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000034")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000035");
    let dragResizeApi: DragResizeApi | null = null;
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-c", "Session C");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebuildWorkspaceAfterDropUpdate
      />,
    );
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-b",
        "left",
        undefined,
        dataTransfer,
      );
    });

    expect(warn).toHaveBeenCalledOnce();
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("structural fallback"),
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,00000000-0000-4000-8000-000000000033",
    );
  });

  it("does not let a stale token acknowledge a later rebased-refused drop", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    vi.spyOn(crypto, "randomUUID")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000021")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000022");
    let dragResizeApi: DragResizeApi | null = null;
    const firstDrag: WorkspaceTabDrag = {
      dragId: "drag-sequential-accepted",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-sequential-first",
      tab: {
        id: "tab-sequential-first",
        kind: "session",
        sessionId: "session-sequential-first",
      },
    };
    const secondDrag: WorkspaceTabDrag = {
      dragId: "drag-sequential-refused",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-sequential-second",
      tab: {
        id: "tab-sequential-second",
        kind: "session",
        sessionId: "session-sequential-second",
      },
    };
    const onReady = (api: DragResizeApi) => {
      dragResizeApi = api;
    };
    const baseProps = {
      initialWorkspace: makeSplitWorkspace(),
      layoutVersion: 1,
      onLayout: vi.fn(),
      onReady,
    };

    const { rerender } = render(<Harness {...baseProps} />);
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: firstDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drop-commit",
      dragId: firstDrag.dragId,
      sourceWindowId: firstDrag.sourceWindowId,
      sourcePaneId: firstDrag.sourcePaneId,
      tabId: firstDrag.tabId,
      targetWindowId: "window-a",
    });

    rerender(
      <Harness
        {...baseProps}
        rebaseWorkspaceBeforeNextDrop={(current) => ({
          ...current,
          panes: current.panes.map((pane) =>
            pane.id === "pane-b"
              ? {
                  ...pane,
                  activeSessionId: null,
                  activeTabId: "tab-control",
                  tabs: [
                    {
                      id: "tab-control",
                      kind: "controlPanel" as const,
                      originSessionId: null,
                    },
                  ],
                  viewMode: "controlPanel" as const,
                }
              : pane,
          ),
        })}
      />,
    );
    await act(async () => {});
    channel.postMessage.mockClear();

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: secondDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-control",
    );
  });

  it("acknowledges a structurally committed external drop after layout restores other focus", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    vi.spyOn(crypto, "randomUUID").mockReturnValue(
      "00000000-0000-4000-8000-000000000002",
    );
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-layout-restored-focus",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external",
      tab: { id: "tab-external", kind: "session", sessionId: "session-c" },
    };

    render(
      <Harness
        applyWorkspaceAfterLayout={(workspace) => ({
          ...workspace,
          activePaneId: "pane-a",
          panes: workspace.panes.map((pane) =>
            pane.id === "pane-a"
              ? { ...pane, activeSessionId: "session-a", activeTabId: "tab-a" }
              : pane,
          ),
        })}
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,00000000-0000-4000-8000-000000000002",
    );
    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drop-commit",
      dragId: externalDrag.dragId,
      sourceWindowId: externalDrag.sourceWindowId,
      sourcePaneId: externalDrag.sourcePaneId,
      tabId: externalDrag.tabId,
      targetWindowId: "window-a",
    });
  });

  it("acknowledges an external edge drop after layout restructures its split", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    vi.spyOn(crypto, "randomUUID")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000003")
      .mockReturnValueOnce("00000000-0000-4000-8000-000000000004");
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-layout-restructured-edge",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external",
      tab: { id: "tab-external", kind: "session", sessionId: "session-c" },
    };

    render(
      <Harness
        applyWorkspaceAfterLayout={(workspace) => ({
          ...workspace,
          root: {
            id: "layout-root",
            type: "split",
            direction: "row",
            ratio: 0.5,
            first: {
              id: "layout-content",
              type: "split",
              direction: "row",
              ratio: 0.5,
              first: { type: "pane", paneId: "pane-session" },
              second: {
                type: "pane",
                paneId: "00000000-0000-4000-8000-000000000004",
              },
            },
            second: { type: "pane", paneId: "pane-control" },
          },
        })}
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop(
        "pane-control",
        "right",
      );
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control,00000000-0000-4000-8000-000000000003",
    );
    expect(channel.postMessage).toHaveBeenCalledWith({
      type: "drop-commit",
      dragId: externalDrag.dragId,
      sourceWindowId: externalDrag.sourceWindowId,
      sourcePaneId: externalDrag.sourcePaneId,
      tabId: externalDrag.tabId,
      targetWindowId: "window-a",
    });
  });

  it("rolls back scroll markers when a rebase refuses a same-pane reorder", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const drag: WorkspaceTabDrag = {
      dragId: "drag-rebased-same-pane",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-a",
      tabId: "tab-b",
      tab: { id: "tab-b", kind: "session", sessionId: "session-b" },
    };

    render(
      <Harness
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => ({
          ...current,
          panes: current.panes.map((pane) =>
            pane.id === "pane-a"
              ? {
                  ...pane,
                  tabs: [
                    ...pane.tabs,
                    {
                      id: "tab-control",
                      kind: "controlPanel" as const,
                      originSessionId: null,
                    },
                  ],
                }
              : pane,
          ),
        })}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-a", "tabs", 0);
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-b,tab-control",
    );
  });

  it("does not acknowledge an external drop refused by rebased workspace state", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-rebased-external",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external",
      tab: { id: "tab-external", kind: "session", sessionId: "session-c" },
    };

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => ({
          ...current,
          panes: current.panes.map((pane) =>
            pane.id === "pane-b"
              ? {
                  ...pane,
                  activeSessionId: null,
                  activeTabId: "tab-control",
                  tabs: [
                    {
                      id: "tab-control",
                      kind: "controlPanel",
                      originSessionId: null,
                    },
                  ],
                  viewMode: "controlPanel",
                }
              : pane,
          ),
        })}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});

    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-a,tab-control",
    );
  });

  it("does not reroute an external tab-rail drop when its target disappears", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-missing-target",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external",
      tab: { id: "tab-external", kind: "session", sessionId: "session-c" },
    };

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={removeSecondPane}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "tabs");
    });

    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-a");
    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
  });

  it("does not flip sides or acknowledge an external control-panel drop when its target disappears", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const externalDrag: WorkspaceTabDrag = {
      dragId: "drag-missing-control-target",
      sourceWindowId: "window-b",
      sourcePaneId: "pane-external",
      tabId: "tab-external-control",
      tab: {
        id: "tab-external-control",
        kind: "controlPanel",
        originSessionId: null,
      },
    };

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={removeSecondPane}
      />,
    );
    await act(async () => {});
    const channel = BroadcastChannelMock.instances[0];

    act(() => {
      channel.onmessage?.({
        data: { type: "drag-start", payload: externalDrag },
      } as MessageEvent<unknown>);
    });
    await act(async () => {});
    act(() => {
      requireDragResizeApi(dragResizeApi).handleTabDrop("pane-b", "right");
    });

    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("left");
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-a");
    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
  });

  it("does not flip sides when a rebased cross-pane control-panel drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const onControlPanelLayoutSide = vi.fn();
    const drag: WorkspaceTabDrag = {
      dragId: "drag-rebased-control-side",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-control",
      tabId: "tab-control",
      tab: { id: "tab-control", kind: "controlPanel", originSessionId: null },
    };

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onControlPanelLayoutSide={onControlPanelLayoutSide}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => {
          const controlPane = current.panes.find(
            (pane) => pane.id === "pane-control",
          );
          if (!controlPane) {
            throw new Error("Expected the control pane in the drop fixture");
          }
          return {
            ...current,
            activePaneId: controlPane.id,
            panes: [controlPane],
            root: { type: "pane", paneId: controlPane.id },
          };
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-session", "right");
    });

    expect(onControlPanelLayoutSide).toHaveBeenCalledOnce();
    expect(onControlPanelLayoutSide).toHaveBeenCalledWith("left");
    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("left");
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-control");
  });

  it("publishes the dock side for an accepted cross-pane control-panel drop", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const drag: WorkspaceTabDrag = {
      dragId: "drag-accepted-control-side",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-control",
      tabId: "tab-control",
      tab: { id: "tab-control", kind: "controlPanel", originSessionId: null },
    };

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-session", "right");
    });

    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("right");
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("restores the prior dock side when a wrapper strips an accepted move token", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    let dragResizeApi: DragResizeApi | null = null;
    const onControlPanelLayoutSide = vi.fn();
    const drag: WorkspaceTabDrag = {
      dragId: "drag-rebuilt-control-side",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-control",
      tabId: "tab-control",
      tab: { id: "tab-control", kind: "controlPanel", originSessionId: null },
    };

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onControlPanelLayoutSide={onControlPanelLayoutSide}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebuildWorkspaceAfterDropUpdate
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-session", "right");
    });

    expect(onControlPanelLayoutSide.mock.calls.map(([side]) => side)).toEqual([
      "right",
      "left",
    ]);
    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("left");
    expect(warn).toHaveBeenCalledWith(
      expect.stringContaining("Could not verify a workspace drop"),
    );
  });

  it("commits a control-panel side flip while its pane is inactive", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const onLayout = vi.fn();
    const splitWorkspace = makeControlPanelSplitWorkspace();
    const controlPane = splitWorkspace.panes[1]!;
    const workspace: WorkspaceState = {
      ...splitWorkspace,
      activePaneId: "pane-session",
    };
    const drag: WorkspaceTabDrag = {
      dragId: "drag-control-side",
      sourceWindowId: "window-a",
      sourcePaneId: controlPane.id,
      tabId: "tab-control",
      tab: { id: "tab-control", kind: "controlPanel", originSessionId: null },
    };

    render(
      <Harness
        initialWorkspace={workspace}
        layoutVersion={1}
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop(controlPane.id, "right");
    });

    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("right");
    expect(onLayout).toHaveBeenCalledOnce();
  });

  it("does not flip sides when a same-pane dock gesture rebases after the tab moves", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const drag: WorkspaceTabDrag = {
      dragId: "drag-rebased-same-pane-control-side",
      sourceWindowId: "window-a",
      sourcePaneId: "pane-control",
      tabId: "tab-control",
      tab: { id: "tab-control", kind: "controlPanel", originSessionId: null },
    };

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        rebaseWorkspaceBeforeNextDrop={(current) => {
          const sessionPane = current.panes.find(
            (pane) => pane.id === "pane-session",
          );
          const controlPane = current.panes.find(
            (pane) => pane.id === "pane-control",
          );
          const controlTab = controlPane?.tabs.find(
            (tab) => tab.id === "tab-control",
          );
          if (!sessionPane || !controlTab) {
            throw new Error("Expected both drop fixture panes and control tab");
          }
          const rebasedSessionPane: WorkspacePane = {
            ...sessionPane,
            tabs: [...sessionPane.tabs, controlTab],
          };
          return {
            ...current,
            root: { type: "pane", paneId: rebasedSessionPane.id },
            activePaneId: rebasedSessionPane.id,
            panes: [rebasedSessionPane],
          };
        }}
      />,
    );
    await act(async () => {});

    act(() => {
      const api = requireDragResizeApi(dragResizeApi);
      api.handleTabDragStart(drag);
      api.handleTabDrop("pane-control", "right");
    });

    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("left");
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("updates split ratio during pointer resize and ignores pending fetched layout", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const resizeParent = document.createElement("div");
    const resizeHandle = document.createElement("div");
    resizeParent.appendChild(resizeHandle);
    resizeParent.getBoundingClientRect = () =>
      ({
        width: 1000,
        height: 600,
      }) as DOMRect;

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        onLayout={vi.fn()}
        onReady={(api) => {
          dragResizeApi = api;
        }}
        workspaceLayoutLoadPending
      />,
    );
    await act(async () => {});
    const api = requireDragResizeApi(dragResizeApi);

    act(() => {
      api.handleSplitResizeStart("split-root", "row", {
        clientX: 500,
        clientY: 0,
        currentTarget: resizeHandle,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
      } as unknown as ReactPointerEvent<HTMLDivElement>);
      window.dispatchEvent(new MouseEvent("pointermove", { clientX: 700 }));
    });

    expect(screen.getByTestId("split-ratio")).toHaveTextContent("0.7");
    expect(screen.getByTestId("ignore-layout")).toHaveTextContent("ignored");
  });
});
