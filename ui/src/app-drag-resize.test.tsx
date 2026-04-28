import { act, cleanup, render, screen } from "@testing-library/react";
import {
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
  ) => void;
  onLayout: (layoutVersion: number) => void;
  onReady?: (api: DragResizeApi) => void;
  workspaceLayoutLoadPending?: boolean;
}) {
  const [workspace, setWorkspace] = useState(
    () => initialWorkspace ?? makeWorkspace(),
  );
  const [, setControlPanelSide] = useState<ControlPanelSide>("left");
  const workspaceLayoutLoadPendingRef = useRef(false);
  const ignoreFetchedWorkspaceLayoutRef = useRef(false);
  const paneLookup = new Map(
    workspace.panes.map((pane): [string, WorkspacePane] => [pane.id, pane]),
  );

  workspaceLayoutLoadPendingRef.current = workspaceLayoutLoadPending;

  const dragResizeApi = useAppDragResize({
    windowId: "window-a",
    workspace,
    paneLookup,
    controlPanelSide: "left",
    setControlPanelSide:
      setControlPanelSide as Dispatch<SetStateAction<ControlPanelSide>>,
    setWorkspace,
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
    </>
  );
}

describe("useAppDragResize", () => {
  afterEach(() => {
    cleanup();
    if (vi.isFakeTimers()) {
      vi.useRealTimers();
    }
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
