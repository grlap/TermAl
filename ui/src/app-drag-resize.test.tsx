import { act, cleanup, render, screen } from "@testing-library/react";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type DragEvent as ReactDragEvent,
  type PointerEvent as ReactPointerEvent,
  type SetStateAction,
} from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useAppDragResize } from "./app-drag-resize";
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
  initialWorkspace,
  layoutVersion,
  markSessionTabsForBottomAfterWorkspaceRebuild = () => {},
  onLayout,
  onReady,
  rebaseWorkspaceBeforeNextDrop,
  workspaceLayoutLoadPending = false,
}: {
  initialWorkspace?: WorkspaceState;
  layoutVersion: number;
  markSessionTabsForBottomAfterWorkspaceRebuild?: (
    workspaceState: WorkspaceState,
    options?: {
      sessionIds?: string[];
      tabs?: WorkspaceTab[];
    },
  ) => (() => void) | void;
  onLayout: (layoutVersion: number) => void;
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
  const setWorkspaceFromDrop = useCallback<
    Dispatch<SetStateAction<WorkspaceState>>
  >((update) => {
    setWorkspace((current) => {
      const rebase = pendingDropRebaseRef.current;
      pendingDropRebaseRef.current = undefined;
      const rebased = rebase?.(current) ?? current;
      return typeof update === "function" ? update(rebased) : update;
    });
  }, []);
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
    applyControlPanelLayout: (nextWorkspace) => {
      onLayout(layoutVersion);
      return nextWorkspace;
    },
    workspaceLayoutLoadPendingRef,
    ignoreFetchedWorkspaceLayoutRef,
    markSessionTabsForBottomAfterWorkspaceRebuild,
  });

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
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledWith(
      expect.any(Object),
      { tabs: [drag.tab] },
    );
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
    expect(api.getKnownWorkspaceTabDrag()).toBeNull();
  });

  it("does not mark sessions for scroll restoration when a session drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
    const onLayout = vi.fn();
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).not.toHaveBeenCalled();
    expect(onLayout).not.toHaveBeenCalled();
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("marks sessions only when an accepted session drop commits", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
    const onLayout = vi.fn();
    const dataTransfer = makeDataTransfer();
    attachSessionDragData(dataTransfer, "session-a", "Session A");

    render(
      <Harness
        initialWorkspace={makeSplitWorkspace()}
        layoutVersion={1}
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
        onLayout={onLayout}
        onReady={(api) => {
          dragResizeApi = api;
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledOnce();
    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledWith(
      expect.any(Object),
      { sessionIds: ["session-a"] },
    );
    expect(onLayout).toHaveBeenCalledOnce();
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-b,tab-a");
  });

  it("does not mark sessions when a pane-tab drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).not.toHaveBeenCalled();
    expect(onLayout).not.toHaveBeenCalled();
    expect(screen.getByTestId("tabs")).toHaveTextContent(
      "tab-session,tab-control",
    );
  });

  it("does not mark sessions when a launcher drop is refused", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
    const onLayout = vi.fn();
    const dataTransfer = makeDataTransfer();

    render(
      <Harness
        initialWorkspace={makeControlPanelSplitWorkspace()}
        layoutVersion={1}
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).not.toHaveBeenCalled();
    expect(onLayout).not.toHaveBeenCalled();
  });

  it("does not commit or mark an external tab drop that is refused", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).not.toHaveBeenCalled();
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
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledOnce();
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

  it("does not acknowledge an external drop refused by rebased workspace state", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const rollbackScrollMarkers = vi.fn();
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn(() =>
      rollbackScrollMarkers,
    );
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledOnce();
    expect(rollbackScrollMarkers).toHaveBeenCalledOnce();
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
    const rollbackScrollMarkers = vi.fn();
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn(() =>
      rollbackScrollMarkers,
    );
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledOnce();
    expect(rollbackScrollMarkers).toHaveBeenCalledOnce();
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-a");
    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
  });

  it("does not flip sides or acknowledge an external control-panel drop when its target disappears", async () => {
    vi.stubGlobal("BroadcastChannel", BroadcastChannelMock);
    let dragResizeApi: DragResizeApi | null = null;
    const rollbackScrollMarkers = vi.fn();
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn(() =>
      rollbackScrollMarkers,
    );
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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

    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledOnce();
    expect(rollbackScrollMarkers).toHaveBeenCalledOnce();
    expect(screen.getByTestId("control-panel-side")).toHaveTextContent("left");
    expect(screen.getByTestId("tabs")).toHaveTextContent("tab-a");
    expect(channel.postMessage).not.toHaveBeenCalledWith(
      expect.objectContaining({ type: "drop-commit" }),
    );
  });

  it("commits a control-panel side flip while its pane is inactive", async () => {
    let dragResizeApi: DragResizeApi | null = null;
    const markSessionTabsForBottomAfterWorkspaceRebuild = vi.fn();
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
        markSessionTabsForBottomAfterWorkspaceRebuild={
          markSessionTabsForBottomAfterWorkspaceRebuild
        }
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
    expect(markSessionTabsForBottomAfterWorkspaceRebuild).toHaveBeenCalledOnce();
    expect(onLayout).toHaveBeenCalledOnce();
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
