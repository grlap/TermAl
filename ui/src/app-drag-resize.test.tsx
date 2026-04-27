import { act, cleanup, render, screen } from "@testing-library/react";
import { useRef, useState, type Dispatch, type SetStateAction } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useAppDragResize } from "./app-drag-resize";
import type { ControlPanelSide } from "./workspace-storage";
import type { WorkspacePane, WorkspaceState } from "./workspace";

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

function Harness({
  layoutVersion,
  onLayout,
}: {
  layoutVersion: number;
  onLayout: (layoutVersion: number) => void;
}) {
  const [workspace, setWorkspace] = useState(makeWorkspace);
  const [, setControlPanelSide] = useState<ControlPanelSide>("left");
  const workspaceLayoutLoadPendingRef = useRef(false);
  const ignoreFetchedWorkspaceLayoutRef = useRef(false);
  const paneLookup = new Map(
    workspace.panes.map((pane): [string, WorkspacePane] => [pane.id, pane]),
  );

  useAppDragResize({
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
    markSessionTabsForBottomAfterWorkspaceRebuild: () => {},
  });

  return (
    <div data-testid="tabs">
      {workspace.panes.flatMap((pane) => pane.tabs.map((tab) => tab.id)).join(",")}
    </div>
  );
}

describe("useAppDragResize", () => {
  afterEach(() => {
    cleanup();
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
});
