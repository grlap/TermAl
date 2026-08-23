import { useState } from "react";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createResponseBoardTab,
  createResponseBoardCard,
  deleteResponseBoardTab,
  deleteResponseBoardCard,
  fetchResponseBoard,
  fetchResponseBoardTab,
  fetchResponseBoardTabs,
  reorderResponseBoardTabs,
  stageResponseBoardCard,
  updateResponseBoardCard,
  updateResponseBoardTab,
  type ResponseBoardCard,
} from "../api";
import {
  notifyResponseBoardChanged,
  RESPONSE_BOARD_MESSAGE_MIME,
  writeResponseBoardMessageDragData,
} from "../response-board";
import {
  RESPONSE_BOARD_ZOOM_STORAGE_KEY,
  ResponseBoardPanel,
  responseBoardViewShowsAnyCard,
} from "./ResponseBoardPanel";

vi.mock("../api", () => ({
  RESPONSE_BOARD_DEFAULT_TAB_ID: "response-board-default",
  createResponseBoardTab: vi.fn(),
  createResponseBoardCard: vi.fn(),
  deleteResponseBoardTab: vi.fn(),
  deleteResponseBoardCard: vi.fn(),
  fetchResponseBoard: vi.fn(),
  fetchResponseBoardTab: vi.fn(),
  fetchResponseBoardTabs: vi.fn(),
  reorderResponseBoardTabs: vi.fn(),
  stageResponseBoardCard: vi.fn(),
  updateResponseBoardCard: vi.fn(),
  updateResponseBoardTab: vi.fn(),
}));

class MemoryDataTransfer {
  dropEffect = "none";
  effectAllowed = "none";
  private readonly values = new Map<string, string>();

  get types() {
    return [...this.values.keys()];
  }

  getData(type: string) {
    return this.values.get(type) ?? "";
  }

  setData(type: string, value: string) {
    this.values.set(type, value);
  }
}

const pinnedCard: ResponseBoardCard = {
  id: "card-1",
  tabId: "response-board-default",
  placement: "placed",
  hasCanvasPosition: true,
  x: 120,
  y: 80,
  w: 360,
  h: 420,
  snapshot: {
    id: "message-1",
    type: "text",
    author: "assistant",
    timestamp: "12:34:56",
    text: "Server-owned immutable response",
  },
  sourceSessionId: "session-1",
  sourceMessageId: "message-1",
  sourceMessagePosition: 42,
  sourceSessionName: "Codex research",
  sourceAgent: "Codex",
  createdAt: "2026-07-31T12:34:56Z",
};

function mockSurfaceRect(surface: Element) {
  vi.spyOn(surface, "getBoundingClientRect").mockReturnValue({
    bottom: 650,
    height: 600,
    left: 100,
    right: 900,
    top: 50,
    width: 800,
    x: 100,
    y: 50,
    toJSON: () => ({}),
  });
}

function readBoardTransform(plane: HTMLElement) {
  const match = plane.style.transform.match(
    /^translate\(([-\d.]+)px, ([-\d.]+)px\) scale\(([-\d.]+)\)$/,
  );
  expect(match).toBeTruthy();
  return {
    panX: Number(match?.[1]),
    panY: Number(match?.[2]),
    zoom: Number(match?.[3]),
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

describe("ResponseBoardPanel", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    window.localStorage.removeItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY);
    vi.mocked(fetchResponseBoard).mockReset();
    vi.mocked(fetchResponseBoardTabs).mockReset();
    vi.mocked(fetchResponseBoardTab).mockReset();
    vi.mocked(reorderResponseBoardTabs).mockReset();
    vi.mocked(createResponseBoardCard).mockReset();
    vi.mocked(stageResponseBoardCard).mockReset();
    vi.mocked(createResponseBoardTab).mockReset();
    vi.mocked(updateResponseBoardTab).mockReset();
    vi.mocked(deleteResponseBoardTab).mockReset();
    vi.mocked(updateResponseBoardCard).mockReset();
    vi.mocked(deleteResponseBoardCard).mockReset();
  });

  it("renders partition tabs and places a staged snapshot without changing it", async () => {
    const stagedCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "staged-1",
      tabId: "project-tab",
      placement: "staged",
      hasCanvasPosition: false,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 1,
      tabs: [
        {
          id: "response-board-default",
          name: "Board",
          kind: "custom",
          projectId: null,
          sortOrder: 0,
          createdAt: "2026-07-31T12:00:00Z",
          placedCardCount: 0,
        },
        {
          id: "project-tab",
          name: "Project A",
          kind: "projectDefault",
          projectId: "project-a",
          sortOrder: 1,
          createdAt: "2026-07-31T12:01:00Z",
          placedCardCount: 0,
        },
      ],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab: {
        id: "project-tab",
        name: "Project A",
        kind: "projectDefault",
        projectId: "project-a",
        sortOrder: 1,
        createdAt: "2026-07-31T12:01:00Z",
        placedCardCount: 0,
      },
      cards: [],
      stagedCards: [stagedCard],
    });
    vi.mocked(updateResponseBoardCard).mockResolvedValue({
      ...stagedCard,
      placement: "placed",
      hasCanvasPosition: true,
      x: 72,
      y: 72,
    });
    const onWorkspaceStateChange = vi.fn();

    render(
      <ResponseBoardPanel
        refreshToken="refresh-partitioned"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="project-tab"
        boardViews={{}}
        onWorkspaceStateChange={onWorkspaceStateChange}
        onOpenSource={() => {}}
      />,
    );

    expect(await screen.findByRole("tab", { name: /Project A/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    const staging = screen.getByRole("region", { name: "Staging tray" });
    const canvas = screen.getByRole("tabpanel", { name: /Project A/ });
    expect(
      staging.compareDocumentPosition(canvas) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("tab", { name: /^Board/ }));
    await waitFor(() =>
      expect(fetchResponseBoardTab).toHaveBeenCalledWith("response-board-default"),
    );
    expect(screen.getByRole("listitem")).toBeTruthy();
    fireEvent.click(screen.getByRole("tab", { name: /Project A/ }));
    fireEvent.click(
      await screen.findByRole("button", { name: /Termal::Codex|Codex research/ }),
    );
    expect(screen.getByText("Server-owned immutable response")).toBeTruthy();
    expect(
      screen.getByRole("region", { name: "Staged card preview" }).parentElement,
    ).toBe(canvas);
    fireEvent.click(screen.getByRole("button", { name: "Place on Project A" }));

    await waitFor(() =>
      expect(updateResponseBoardCard).toHaveBeenCalledWith(
        "staged-1",
        expect.objectContaining({ placement: "placed", tabId: "project-tab" }),
      ),
    );
    await waitFor(() =>
      expect(onWorkspaceStateChange).toHaveBeenCalledWith(
        "workspace-board-1",
        "project-tab",
        expect.objectContaining({ zoom: 1 }),
      ),
    );
  });

  it("refreshes another open board after a card mutation", async () => {
    const stagedCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "shared-staged-card",
      placement: "staged",
      hasCanvasPosition: false,
    };
    const tab = {
      id: "response-board-default",
      name: "Board",
      kind: "custom" as const,
      projectId: null,
      sortOrder: 0,
      createdAt: "2026-07-31T12:00:00Z",
      placedCardCount: 0,
    };
    let isPlaced = false;
    vi.mocked(fetchResponseBoardTabs).mockImplementation(async () => ({
      stagedCardCount: isPlaced ? 0 : 1,
      tabs: [{ ...tab, placedCardCount: isPlaced ? 1 : 0 }],
    }));
    vi.mocked(fetchResponseBoardTab).mockImplementation(async () => ({
      tab: { ...tab, placedCardCount: isPlaced ? 1 : 0 },
      cards: isPlaced
        ? [
            {
              ...stagedCard,
              placement: "placed",
              hasCanvasPosition: true,
              x: 64,
              y: 64,
            },
          ]
        : [],
      stagedCards: isPlaced ? [] : [stagedCard],
    }));
    vi.mocked(updateResponseBoardCard).mockImplementation(
      async (_cardId, update) => {
        isPlaced = true;
        return {
          ...stagedCard,
          ...update,
          placement: "placed",
          hasCanvasPosition: true,
        };
      },
    );

    render(
      <>
        <div data-testid="first-board">
          <ResponseBoardPanel
            refreshToken="shared-board-one"
            workspaceTabId="workspace-board-one"
            activeBoardTabId={tab.id}
            onOpenSource={() => {}}
          />
        </div>
        <div data-testid="second-board">
          <ResponseBoardPanel
            refreshToken="shared-board-two"
            workspaceTabId="workspace-board-two"
            activeBoardTabId={tab.id}
            onOpenSource={() => {}}
          />
        </div>
      </>,
    );

    await waitFor(() => expect(fetchResponseBoardTab).toHaveBeenCalledTimes(2));
    const firstBoard = within(screen.getByTestId("first-board"));
    const secondBoard = within(screen.getByTestId("second-board"));
    fireEvent.click(
      firstBoard.getByRole("button", { name: /Codex research/ }),
    );
    fireEvent.click(
      firstBoard.getByRole("button", { name: "Place on Board" }),
    );

    await waitFor(() => expect(updateResponseBoardCard).toHaveBeenCalledOnce());
    await waitFor(() => expect(fetchResponseBoardTab).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(fetchResponseBoardTabs).toHaveBeenCalledTimes(4));
    expect(
      await firstBoard.findByText("Server-owned immutable response"),
    ).toBeTruthy();
    expect(
      await secondBoard.findByText("Server-owned immutable response"),
    ).toBeTruthy();
    expect(
      within(secondBoard.getByRole("region", { name: "Staging tray" })).getByText(
        "Pin responses here, then place them on any board.",
      ),
    ).toBeTruthy();
  });

  it("refreshes in place without interrupting an active card drag", async () => {
    const tab = {
      id: "response-board-default",
      name: "Board",
      kind: "custom" as const,
      projectId: null,
      sortOrder: 0,
      createdAt: "2026-07-31T12:00:00Z",
      placedCardCount: 1,
    };
    const backgroundRefresh = deferred<{
      tab: typeof tab;
      cards: ResponseBoardCard[];
      stagedCards: ResponseBoardCard[];
    }>();
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [tab],
    });
    vi.mocked(fetchResponseBoardTab)
      .mockResolvedValueOnce({ tab, cards: [pinnedCard], stagedCards: [] })
      .mockReturnValueOnce(backgroundRefresh.promise);

    const { container } = render(
      <ResponseBoardPanel
        refreshToken="background-refresh-drag"
        workspaceTabId="workspace-board-one"
        activeBoardTabId={tab.id}
        onOpenSource={() => {}}
      />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const header = container.querySelector(".response-board-card-header");
    const card = container.querySelector(".response-board-card") as HTMLElement;
    fireEvent.pointerDown(header as Element, {
      button: 0,
      pointerId: 91,
      clientX: 100,
      clientY: 100,
    });
    fireEvent.pointerMove(card, {
      pointerId: 91,
      clientX: 180,
      clientY: 150,
    });
    expect(card.style.left).toBe("200px");
    expect(card.style.top).toBe("130px");

    act(() => notifyResponseBoardChanged(null));
    await waitFor(() => expect(fetchResponseBoardTab).toHaveBeenCalledTimes(2));
    expect(screen.queryByText("Loading response board…")).toBeNull();
    expect(screen.getByText("Server-owned immutable response")).toBeTruthy();

    await act(async () => {
      backgroundRefresh.resolve({
        tab,
        cards: [pinnedCard],
        stagedCards: [],
      });
      await backgroundRefresh.promise;
    });
    expect(card.style.left).toBe("200px");
    expect(card.style.top).toBe("130px");
    fireEvent.pointerUp(card, { pointerId: 91 });
  });

  it("uses a browser-compatible drop effect for staged cards and transcript messages", async () => {
    const stagedCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "staged-drag-effect",
      tabId: "project-tab",
      placement: "staged",
      hasCanvasPosition: false,
    };
    const tab = {
      id: "project-tab",
      name: "Project A",
      kind: "projectDefault" as const,
      projectId: "project-a",
      sortOrder: 0,
      createdAt: "2026-07-31T12:01:00Z",
      placedCardCount: 0,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 1,
      tabs: [tab],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab,
      cards: [],
      stagedCards: [stagedCard],
    });

    render(
      <ResponseBoardPanel
        refreshToken="drag-effect"
        workspaceTabId="workspace-board-1"
        activeBoardTabId={tab.id}
        onOpenSource={() => {}}
      />,
    );

    const surface = await screen.findByRole("tabpanel", { name: /Project A/ });
    const stagedTransfer = new MemoryDataTransfer();
    fireEvent.dragStart(
      screen.getByRole("button", { name: /Codex research/ }),
      { dataTransfer: stagedTransfer },
    );
    expect(stagedTransfer.effectAllowed).toBe("move");
    fireEvent.dragOver(surface, { dataTransfer: stagedTransfer });
    expect(stagedTransfer.dropEffect).toBe("move");

    const transcriptTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      transcriptTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );
    fireEvent.dragOver(surface, { dataTransfer: transcriptTransfer });
    expect(transcriptTransfer.dropEffect).toBe("copy");
  });

  it("repairs an off-screen persisted camera when opening a board with cards", async () => {
    const projectCard = { ...pinnedCard, tabId: "project-tab" };
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      bottom: 600,
      height: 600,
      left: 0,
      right: 800,
      top: 0,
      width: 800,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [
        {
          id: "project-tab",
          name: "Project A",
          kind: "projectDefault",
          projectId: "project-a",
          sortOrder: 0,
          createdAt: "2026-07-31T12:01:00Z",
          placedCardCount: 1,
        },
      ],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab: {
        id: "project-tab",
        name: "Project A",
        kind: "projectDefault",
        projectId: "project-a",
        sortOrder: 0,
        createdAt: "2026-07-31T12:01:00Z",
        placedCardCount: 1,
      },
      cards: [projectCard],
      stagedCards: [],
    });

    const { container } = render(
      <ResponseBoardPanel
        refreshToken="refresh-offscreen"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="project-tab"
        boardViews={{
          "project-tab": { panX: -10_000, panY: -10_000, zoom: 1 },
        }}
        onOpenSource={() => {}}
      />,
    );

    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    const repairedView = readBoardTransform(plane);
    expect(
      responseBoardViewShowsAnyCard(repairedView, [projectCard], 800, 600),
    ).toBe(true);
    expect(repairedView.panX).not.toBe(-10_000);
  });

  it("does not restore an older persisted camera during a workspace round-trip", async () => {
    const projectCard = { ...pinnedCard, tabId: "project-tab" };
    const tabsResponse = {
      stagedCardCount: 0,
      tabs: [
        {
          id: "project-tab",
          name: "Project A",
          kind: "projectDefault" as const,
          projectId: "project-a",
          sortOrder: 0,
          createdAt: "2026-07-31T12:01:00Z",
          placedCardCount: 1,
        },
      ],
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue(tabsResponse);
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab: tabsResponse.tabs[0],
      cards: [projectCard],
      stagedCards: [],
    });
    const props = {
      refreshToken: "camera-round-trip",
      workspaceTabId: "workspace-board-1",
      activeBoardTabId: "project-tab",
      boardViews: { "project-tab": { panX: 0, panY: 0, zoom: 1 } },
      onOpenSource: () => {},
    };
    const { container, rerender } = render(<ResponseBoardPanel {...props} />);
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    fireEvent.pointerDown(surface, {
      button: 0,
      pointerId: 44,
      clientX: 100,
      clientY: 100,
    });
    fireEvent.pointerMove(surface, {
      pointerId: 44,
      clientX: 145,
      clientY: 125,
    });
    fireEvent.pointerUp(surface, { pointerId: 44 });
    expect(readBoardTransform(plane)).toMatchObject({ panX: 45, panY: 25 });

    rerender(<ResponseBoardPanel {...props} boardViews={{ ...props.boardViews }} />);
    expect(readBoardTransform(plane)).toMatchObject({ panX: 45, panY: 25 });
  });

  it("flushes the latest camera for the previous tab before a fast tab switch", async () => {
    const tabs = [
      {
        id: "tab-a",
        name: "Board A",
        kind: "custom" as const,
        projectId: null,
        sortOrder: 0,
        createdAt: "2026-07-31T12:00:00Z",
        placedCardCount: 0,
      },
      {
        id: "tab-b",
        name: "Board B",
        kind: "custom" as const,
        projectId: null,
        sortOrder: 1,
        createdAt: "2026-07-31T12:01:00Z",
        placedCardCount: 0,
      },
    ];
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs,
    });
    vi.mocked(fetchResponseBoardTab).mockImplementation(async (tabId) => ({
      tab: tabs.find((tab) => tab.id === tabId) ?? tabs[0],
      cards: [],
      stagedCards: [],
    }));
    const onWorkspaceStateChange = vi.fn();
    const { container } = render(
      <ResponseBoardPanel
        refreshToken="camera-fast-switch"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="tab-a"
        onWorkspaceStateChange={onWorkspaceStateChange}
        onOpenSource={() => {}}
      />,
    );

    expect(await screen.findByRole("tab", { name: /Board A/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await waitFor(() => expect(onWorkspaceStateChange).toHaveBeenCalled());
    onWorkspaceStateChange.mockClear();
    vi.useFakeTimers();
    try {
      const surface = container.querySelector(
        ".response-board-surface",
      ) as HTMLElement;
      fireEvent.pointerDown(surface, {
        button: 0,
        pointerId: 55,
        clientX: 100,
        clientY: 100,
      });
      fireEvent.pointerMove(surface, {
        pointerId: 55,
        clientX: 148,
        clientY: 126,
      });
      fireEvent.pointerUp(surface, { pointerId: 55 });

      await act(async () => {
        fireEvent.click(screen.getByRole("tab", { name: /Board B/ }));
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(onWorkspaceStateChange).toHaveBeenCalledWith(
        "workspace-board-1",
        "tab-a",
        { panX: 48, panY: 26, zoom: 1 },
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("flushes the latest camera when the board panel unmounts before debounce", async () => {
    const tab = {
      id: "tab-a",
      name: "Board A",
      kind: "custom" as const,
      projectId: null,
      sortOrder: 0,
      createdAt: "2026-07-31T12:00:00Z",
      placedCardCount: 0,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [tab],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab,
      cards: [],
      stagedCards: [],
    });
    const onWorkspaceStateChange = vi.fn();
    const { container, unmount } = render(
      <ResponseBoardPanel
        refreshToken="camera-unmount"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="tab-a"
        onWorkspaceStateChange={onWorkspaceStateChange}
        onOpenSource={() => {}}
      />,
    );
    await screen.findByRole("tab", { name: /Board A/ });
    await waitFor(() => expect(onWorkspaceStateChange).toHaveBeenCalled());
    onWorkspaceStateChange.mockClear();
    vi.useFakeTimers();
    try {
      const surface = container.querySelector(
        ".response-board-surface",
      ) as HTMLElement;
      fireEvent.pointerDown(surface, {
        button: 0,
        pointerId: 56,
        clientX: 100,
        clientY: 100,
      });
      fireEvent.pointerMove(surface, {
        pointerId: 56,
        clientX: 142,
        clientY: 124,
      });
      fireEvent.pointerUp(surface, { pointerId: 56 });

      unmount();

      expect(onWorkspaceStateChange).toHaveBeenCalledWith(
        "workspace-board-1",
        "tab-a",
        { panX: 42, panY: 24, zoom: 1 },
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not re-enter loading when workspace persistence echoes the selected tab", async () => {
    const projectTab = {
      id: "project-tab",
      name: "Project A",
      kind: "projectDefault" as const,
      projectId: "project-a",
      sortOrder: 0,
      createdAt: "2026-07-31T12:01:00Z",
      placedCardCount: 0,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [projectTab],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab: projectTab,
      cards: [],
      stagedCards: [],
    });

    const onWorkspaceStateChange = vi.fn();
    function WorkspaceFeedbackHarness() {
      const [activeTabId, setActiveTabId] = useState<string | null>(null);
      return (
        <ResponseBoardPanel
          refreshToken="workspace-feedback"
          workspaceTabId="workspace-board-1"
          activeBoardTabId={activeTabId}
          onWorkspaceStateChange={(...args) => {
            onWorkspaceStateChange(...args);
            setActiveTabId(args[1]);
          }}
          onOpenSource={() => {}}
        />
      );
    }
    render(<WorkspaceFeedbackHarness />);

    expect(
      await screen.findByText(
        "Drag a staged response here, or drop a transcript message.",
      ),
    ).toBeTruthy();
    await waitFor(() => expect(onWorkspaceStateChange).toHaveBeenCalled());
    expect(screen.queryByText("Loading response board…")).toBeNull();
    expect(fetchResponseBoardTabs).toHaveBeenCalledOnce();
    expect(fetchResponseBoardTab).toHaveBeenCalledOnce();
  });

  it("does not apply a late placed-card response to a different selected tab", async () => {
    const stagedCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "staged-late",
      tabId: "tab-a",
      placement: "staged",
      hasCanvasPosition: false,
    };
    const tabs = [
      {
        id: "tab-a",
        name: "Board A",
        kind: "custom" as const,
        projectId: null,
        sortOrder: 0,
        createdAt: "2026-07-31T12:00:00Z",
        placedCardCount: 0,
      },
      {
        id: "tab-b",
        name: "Board B",
        kind: "custom" as const,
        projectId: null,
        sortOrder: 1,
        createdAt: "2026-07-31T12:01:00Z",
        placedCardCount: 0,
      },
    ];
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 1,
      tabs,
    });
    vi.mocked(fetchResponseBoardTab).mockImplementation(async (tabId) => ({
      tab: tabs.find((tab) => tab.id === tabId)!,
      cards: [],
      stagedCards: [stagedCard],
    }));
    const update = deferred<ResponseBoardCard>();
    vi.mocked(updateResponseBoardCard).mockReturnValue(update.promise);

    const { container } = render(
      <ResponseBoardPanel
        refreshToken="late-card-response"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="tab-a"
        onOpenSource={() => {}}
      />,
    );
    fireEvent.click(
      await screen.findByRole("button", { name: /Termal::Codex|Codex research/ }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Place on Board A" }));
    await waitFor(() => expect(updateResponseBoardCard).toHaveBeenCalledOnce());

    fireEvent.click(screen.getByRole("tab", { name: /Board B/ }));
    await waitFor(() =>
      expect(fetchResponseBoardTab).toHaveBeenCalledWith("tab-b"),
    );
    await act(async () => {
      await Promise.resolve();
    });
    expect(container.querySelector(".response-board-card")).toBeNull();

    await act(async () => {
      update.resolve({
        ...stagedCard,
        tabId: "tab-a",
        placement: "placed",
        hasCanvasPosition: true,
      });
      await update.promise;
    });
    expect(screen.getByRole("tab", { name: /Board B/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(container.querySelector(".response-board-card")).toBeNull();
  });

  it("serializes tab reorder requests and supports arrow-key tab navigation", async () => {
    const tabsResponse = {
      stagedCardCount: 0,
      tabs: [
        {
          id: "response-board-default",
          name: "Board",
          kind: "custom" as const,
          projectId: null,
          sortOrder: 0,
          createdAt: "2026-07-31T12:00:00Z",
          placedCardCount: 0,
        },
        {
          id: "custom-tab",
          name: "Research",
          kind: "custom" as const,
          projectId: null,
          sortOrder: 1,
          createdAt: "2026-07-31T12:01:00Z",
          placedCardCount: 0,
        },
      ],
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue(tabsResponse);
    vi.mocked(fetchResponseBoardTab).mockImplementation(async (tabId) => ({
      tab: tabsResponse.tabs.find((tab) => tab.id === tabId)!,
      cards: [],
      stagedCards: [],
    }));
    const reorder = deferred<typeof tabsResponse>();
    vi.mocked(reorderResponseBoardTabs).mockReturnValue(reorder.promise);
    render(
      <ResponseBoardPanel
        refreshToken="reorder"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="custom-tab"
        onOpenSource={() => {}}
      />,
    );

    const research = await screen.findByRole("tab", { name: /Research/ });
    fireEvent.keyDown(research, { key: "ArrowLeft" });
    const board = screen.getByRole("tab", { name: /^Board/ });
    expect(board).toHaveFocus();
    expect(board).toHaveAttribute("aria-selected", "true");
    fireEvent.click(research);
    fireEvent.click(screen.getByRole("button", { name: "Move Research left" }));
    fireEvent.click(screen.getByRole("button", { name: "Move Research right" }));
    expect(reorderResponseBoardTabs).toHaveBeenCalledOnce();

    await act(async () => {
      reorder.resolve({
        ...tabsResponse,
        tabs: [tabsResponse.tabs[1], tabsResponse.tabs[0]],
      });
      await reorder.promise;
    });
  });

  it("reports a tab refresh failure after a successful create", async () => {
    const defaultTab = {
      id: "response-board-default",
      name: "Board",
      kind: "custom" as const,
      projectId: null,
      sortOrder: 0,
      createdAt: "2026-07-31T12:00:00Z",
      placedCardCount: 0,
    };
    vi.mocked(fetchResponseBoardTabs)
      .mockResolvedValueOnce({ stagedCardCount: 0, tabs: [defaultTab] })
      .mockRejectedValueOnce(new Error("tab refresh failed"));
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab: defaultTab,
      cards: [],
      stagedCards: [],
    });
    vi.mocked(createResponseBoardTab).mockResolvedValue({
      ...defaultTab,
      id: "new-tab",
      name: "New tab",
      sortOrder: 1,
    });
    render(
      <ResponseBoardPanel
        refreshToken="create-refresh-failure"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="response-board-default"
        onOpenSource={() => {}}
      />,
    );
    await screen.findByRole("tab", { name: /^Board/ });
    fireEvent.click(screen.getByRole("button", { name: "Add response board tab" }));
    const input = screen.getByRole("textbox", { name: "New board tab name" });
    fireEvent.change(input, { target: { value: "New tab" } });
    fireEvent.submit(input.closest("form")!);
    expect(await screen.findByRole("alert")).toHaveTextContent("tab refresh failed");
  });

  it("returns placed cards to global staging and moves cards between boards", async () => {
    const secondCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "card-2",
      sourceMessageId: "message-2",
      snapshot: {
        id: "message-2",
        type: "text",
        author: "assistant",
        timestamp: "12:35:00",
        text: "Second immutable response",
      },
    };
    const tabs = [
      {
        id: "response-board-default",
        name: "Board",
        kind: "custom" as const,
        projectId: null,
        sortOrder: 0,
        createdAt: "2026-07-31T12:00:00Z",
        placedCardCount: 2,
      },
      {
        id: "project-tab",
        name: "Project A",
        kind: "projectDefault" as const,
        projectId: "project-a",
        sortOrder: 1,
        createdAt: "2026-07-31T12:01:00Z",
        placedCardCount: 0,
      },
    ];
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs,
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab: tabs[0],
      cards: [pinnedCard, secondCard],
      stagedCards: [],
    });
    vi.mocked(updateResponseBoardCard).mockImplementation(
      async (cardId, update) => ({
        ...(cardId === pinnedCard.id ? pinnedCard : secondCard),
        ...update,
      }),
    );
    render(
      <ResponseBoardPanel
        refreshToken="card-workflow"
        workspaceTabId="workspace-board-1"
        activeBoardTabId="response-board-default"
        onOpenSource={() => {}}
      />,
    );
    await screen.findByText("Second immutable response");

    fireEvent.click(
      screen.getAllByRole("button", { name: "Return to staging" })[0],
    );
    await waitFor(() =>
      expect(updateResponseBoardCard).toHaveBeenCalledWith("card-1", {
        placement: "staged",
      }),
    );
    expect(await screen.findByRole("listitem")).toBeTruthy();

    fireEvent.change(screen.getByRole("combobox", { name: "Move" }), {
      target: { value: "project-tab" },
    });
    await waitFor(() =>
      expect(updateResponseBoardCard).toHaveBeenCalledWith("card-2", {
        tabId: "project-tab",
      }),
    );
    expect(screen.queryByText("Second immutable response")).toBeNull();
  });

  afterEach(() => {
    window.localStorage.removeItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY);
  });

  it("mounts beside a session fixture and replaces an ids-only drop with the server snapshot", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [] });
    vi.mocked(createResponseBoardCard).mockResolvedValue(pinnedCard);
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );

    const { container } = render(
      <div className="response-board-test-workspace">
        <section data-testid="session-pane-fixture">Session transcript</section>
        <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />
      </div>,
    );

    expect(await screen.findByText("Drop an agent response anywhere on the board.")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface");
    expect(surface).toBeTruthy();
    fireEvent.drop(surface as Element, {
      dataTransfer,
      clientX: 300,
      clientY: 180,
    });

    await waitFor(() => expect(createResponseBoardCard).toHaveBeenCalledOnce());
    const request = vi.mocked(createResponseBoardCard).mock.calls[0]?.[0];
    expect(request).toMatchObject({
      sessionId: "session-1",
      messageId: "message-1",
    });
    expect(Number.isFinite(request?.x)).toBe(true);
    expect(Number.isFinite(request?.y)).toBe(true);
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    expect(screen.getByText("Codex research")).toBeTruthy();
    expect(dataTransfer.getData(RESPONSE_BOARD_MESSAGE_MIME)).not.toContain(
      "Server-owned immutable response",
    );
  });

  it("stages and places a transcript drop on the selected partitioned tab", async () => {
    const tab = {
      id: "project-tab",
      name: "Project A",
      kind: "projectDefault" as const,
      projectId: "project-a",
      sortOrder: 0,
      createdAt: "2026-07-31T12:01:00Z",
      placedCardCount: 0,
    };
    const stagedCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "staged-drop",
      tabId: tab.id,
      placement: "staged",
      hasCanvasPosition: false,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [tab],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab,
      cards: [],
      stagedCards: [],
    });
    vi.mocked(stageResponseBoardCard).mockResolvedValue(stagedCard);
    vi.mocked(updateResponseBoardCard).mockImplementation(
      async (_cardId, update) => ({ ...stagedCard, ...update }),
    );
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );
    const { container } = render(
      <ResponseBoardPanel
        refreshToken="partitioned-drop"
        workspaceTabId="workspace-board-1"
        activeBoardTabId={tab.id}
        onOpenSource={() => {}}
      />,
    );
    await screen.findByRole("tab", { name: /Project A/ });
    const surface = container.querySelector(
      ".response-board-surface",
    ) as HTMLElement;
    mockSurfaceRect(surface);

    fireEvent.drop(surface, {
      dataTransfer,
      clientX: 400,
      clientY: 250,
    });

    await waitFor(() =>
      expect(stageResponseBoardCard).toHaveBeenCalledWith({
        sessionId: "session-1",
        messageId: "message-1",
        tabId: tab.id,
      }),
    );
    expect(updateResponseBoardCard).toHaveBeenCalledWith(
      "staged-drop",
      expect.objectContaining({
        tabId: tab.id,
        placement: "placed",
        x: expect.any(Number),
        y: expect.any(Number),
      }),
    );
  });

  it("surfaces a staging failure from a partitioned transcript drop", async () => {
    const tab = {
      id: "project-tab",
      name: "Project A",
      kind: "projectDefault" as const,
      projectId: "project-a",
      sortOrder: 0,
      createdAt: "2026-07-31T12:01:00Z",
      placedCardCount: 0,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [tab],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab,
      cards: [],
      stagedCards: [],
    });
    vi.mocked(stageResponseBoardCard).mockRejectedValue(
      new Error("Could not stage response"),
    );
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );
    const { container } = render(
      <ResponseBoardPanel
        refreshToken="partitioned-drop-error"
        workspaceTabId="workspace-board-1"
        activeBoardTabId={tab.id}
        onOpenSource={() => {}}
      />,
    );
    await screen.findByRole("tab", { name: /Project A/ });

    fireEvent.drop(container.querySelector(".response-board-surface") as Element, {
      dataTransfer,
      clientX: 400,
      clientY: 250,
    });

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not stage response",
    );
    expect(updateResponseBoardCard).not.toHaveBeenCalled();
  });

  it("keeps a newly staged card visible when transcript-drop placement fails", async () => {
    const tab = {
      id: "project-tab",
      name: "Project A",
      kind: "projectDefault" as const,
      projectId: "project-a",
      sortOrder: 0,
      createdAt: "2026-07-31T12:01:00Z",
      placedCardCount: 0,
    };
    const stagedCard: ResponseBoardCard = {
      ...pinnedCard,
      id: "staged-after-place-failure",
      tabId: tab.id,
      placement: "staged",
      hasCanvasPosition: false,
    };
    vi.mocked(fetchResponseBoardTabs).mockResolvedValue({
      stagedCardCount: 0,
      tabs: [tab],
    });
    vi.mocked(fetchResponseBoardTab).mockResolvedValue({
      tab,
      cards: [],
      stagedCards: [],
    });
    vi.mocked(stageResponseBoardCard).mockResolvedValue(stagedCard);
    vi.mocked(updateResponseBoardCard).mockRejectedValue(
      new Error("Could not place staged response"),
    );
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );
    const { container } = render(
      <ResponseBoardPanel
        refreshToken="partitioned-place-error"
        workspaceTabId="workspace-board-1"
        activeBoardTabId={tab.id}
        onOpenSource={() => {}}
      />,
    );
    await screen.findByRole("tab", { name: /Project A/ });

    fireEvent.drop(container.querySelector(".response-board-surface") as Element, {
      dataTransfer,
      clientX: 400,
      clientY: 250,
    });

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not place staged response",
    );
    expect(
      screen.getByRole("button", { name: /Codex research/ }),
    ).toBeInTheDocument();
  });

  it("debounces explicit move geometry and removes cards through the API", async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
      vi.mocked(updateResponseBoardCard).mockImplementation(
        async (_cardId, geometry) => ({ ...pinnedCard, ...geometry }),
      );
      vi.mocked(deleteResponseBoardCard).mockResolvedValue();
      const { container } = render(
        <div className="response-board-test-workspace">
          <section data-testid="session-pane-fixture">Session transcript</section>
          <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />
        </div>,
      );
      await act(async () => Promise.resolve());

      const header = container.querySelector(".response-board-card-header");
      const card = container.querySelector(".response-board-card");
      expect(header).toBeTruthy();
      expect(card).toBeTruthy();
      fireEvent.pointerDown(header as Element, {
        button: 0,
        pointerId: 7,
        clientX: 100,
        clientY: 100,
      });
      fireEvent.pointerMove(card as Element, {
        pointerId: 7,
        clientX: 180,
        clientY: 150,
      });
      expect(updateResponseBoardCard).not.toHaveBeenCalled();
      await act(async () => {
        vi.advanceTimersByTime(250);
        await Promise.resolve();
      });
      expect(updateResponseBoardCard).toHaveBeenCalledWith("card-1", {
        x: 200,
        y: 130,
        w: 360,
        h: 420,
      });

      fireEvent.click(
        screen.getByRole("button", {
          name: "Remove response from Codex research",
        }),
      );
      await act(async () => Promise.resolve());
      expect(deleteResponseBoardCard).toHaveBeenCalledWith("card-1");
      expect(screen.queryByText("Server-owned immutable response")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("sizes the board from its cards without forcing an oversized scroll plane", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });

    const { container } = render(
      <div className="response-board-test-workspace">
        <section data-testid="session-pane-fixture">Session transcript</section>
        <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />
      </div>,
    );

    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const plane = container.querySelector(
      ".response-board-plane",
    ) as HTMLElement | null;
    expect(plane).toBeTruthy();
    expect(plane?.style.width).toBe("552px");
    expect(plane?.style.height).toBe("572px");
    expect(plane?.style.minWidth).toBe("100%");
    expect(plane?.style.minHeight).toBe("100%");
  });

  it("suppresses selection only while an empty-canvas pan gesture is active", async () => {
    window.localStorage.setItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY, "0.5");
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
    const removeAllRanges = vi.fn();
    vi.spyOn(window, "getSelection").mockReturnValue({
      removeAllRanges,
    } as unknown as Selection);

    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    const cardBody = container.querySelector(".response-board-card-body") as HTMLElement;

    fireEvent.pointerDown(surface, {
      button: 0,
      pointerId: 11,
      clientX: 200,
      clientY: 180,
    });
    expect(removeAllRanges).toHaveBeenCalledOnce();
    expect(surface).toHaveClass("is-panning");
    fireEvent.pointerMove(surface, {
      pointerId: 11,
      clientX: 240,
      clientY: 200,
    });
    expect(readBoardTransform(plane)).toEqual({
      panX: 40,
      panY: 20,
      zoom: 0.5,
    });
    fireEvent.pointerUp(surface, { pointerId: 11 });
    expect(surface).not.toHaveClass("is-panning");

    fireEvent.pointerDown(cardBody, {
      button: 0,
      pointerId: 12,
      clientX: 220,
      clientY: 200,
    });
    expect(removeAllRanges).toHaveBeenCalledOnce();
    expect(surface).not.toHaveClass("is-panning");
  });

  it("keeps the logical point under the cursor stationary during ctrl-wheel zoom", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    mockSurfaceRect(surface);

    const zoomEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      ctrlKey: true,
      clientX: 300,
      clientY: 250,
      deltaY: 100,
    });
    act(() => {
      surface.dispatchEvent(zoomEvent);
    });

    const transform = readBoardTransform(plane);
    expect(zoomEvent.defaultPrevented).toBe(true);
    expect(transform.zoom).toBeLessThan(1);
    expect(transform.panX + 200 * transform.zoom).toBeCloseTo(200, 5);
    expect(transform.panY + 200 * transform.zoom).toBeCloseTo(200, 5);
    expect(Number(window.localStorage.getItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY))).toBeCloseTo(
      transform.zoom,
      5,
    );
  });

  it("supports Fn-wheel zoom without intercepting an unmodified wheel", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Server-owned immutable response")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    const plane = container.querySelector(".response-board-plane") as HTMLElement;
    mockSurfaceRect(surface);

    const ordinaryWheelEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
      deltaY: -100,
    });
    act(() => {
      surface.dispatchEvent(ordinaryWheelEvent);
    });
    expect(ordinaryWheelEvent.defaultPrevented).toBe(false);
    expect(readBoardTransform(plane)).toEqual({ panX: 0, panY: 0, zoom: 1 });

    const fnZoomEvent = new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      clientX: 300,
      clientY: 250,
      deltaY: -100,
    });
    Object.defineProperty(fnZoomEvent, "getModifierState", {
      value: (modifier: string) => modifier === "Fn",
    });
    act(() => {
      surface.dispatchEvent(fnZoomEvent);
    });

    expect(fnZoomEvent.defaultPrevented).toBe(true);
    expect(readBoardTransform(plane).zoom).toBeGreaterThan(1);
  });

  it("divides card drag deltas by the persisted view scale", async () => {
    vi.useFakeTimers();
    window.localStorage.setItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY, "0.5");
    try {
      vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [pinnedCard] });
      vi.mocked(updateResponseBoardCard).mockImplementation(
        async (_cardId, geometry) => ({ ...pinnedCard, ...geometry }),
      );
      const { container } = render(
        <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
      );
      await act(async () => Promise.resolve());

      const header = container.querySelector(".response-board-card-header") as HTMLElement;
      const card = container.querySelector(".response-board-card") as HTMLElement;
      fireEvent.pointerDown(header, {
        button: 0,
        pointerId: 13,
        clientX: 100,
        clientY: 100,
      });
      fireEvent.pointerMove(card, {
        pointerId: 13,
        clientX: 150,
        clientY: 125,
      });
      await act(async () => {
        vi.advanceTimersByTime(250);
        await Promise.resolve();
      });

      expect(updateResponseBoardCard).toHaveBeenCalledWith("card-1", {
        x: 220,
        y: 130,
        w: 360,
        h: 420,
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("supports keyboard and toolbar zoom controls with a 25%-200% range and reset", async () => {
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [] });
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Drop an agent response anywhere on the board.")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    mockSurfaceRect(surface);

    fireEvent.click(screen.getByRole("button", { name: "Zoom out" }));
    expect(screen.getByRole("button", { name: "Fit board in view" })).toHaveTextContent(
      "83%",
    );
    fireEvent.keyDown(surface, { ctrlKey: true, key: "+" });
    expect(screen.getByRole("button", { name: "Fit board in view" })).toHaveTextContent(
      "100%",
    );

    const zoomIn = screen.getByRole("button", { name: "Zoom in" });
    for (let index = 0; index < 20; index += 1) {
      fireEvent.click(zoomIn);
    }
    expect(screen.getByRole("button", { name: "Fit board in view" })).toHaveTextContent(
      "200%",
    );
    fireEvent.click(screen.getByRole("button", { name: "Fit board in view" }));
    expect(screen.getByRole("button", { name: "Fit board in view" })).toHaveTextContent(
      "100%",
    );
  });

  it("converts drops back into logical coordinates at the active zoom", async () => {
    window.localStorage.setItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY, "0.5");
    vi.mocked(fetchResponseBoard).mockResolvedValue({ cards: [] });
    vi.mocked(createResponseBoardCard).mockResolvedValue(pinnedCard);
    const dataTransfer = new MemoryDataTransfer();
    writeResponseBoardMessageDragData(
      dataTransfer as unknown as DataTransfer,
      { sessionId: "session-1", messageId: "message-1" },
    );
    const { container } = render(
      <ResponseBoardPanel refreshToken="refresh-1" onOpenSource={() => {}} />,
    );
    expect(await screen.findByText("Drop an agent response anywhere on the board.")).toBeTruthy();
    const surface = container.querySelector(".response-board-surface") as HTMLElement;
    mockSurfaceRect(surface);

    const dropEvent = new Event("drop", { bubbles: true, cancelable: true });
    Object.defineProperties(dropEvent, {
      clientX: { value: 300 },
      clientY: { value: 180 },
      dataTransfer: { value: dataTransfer },
    });
    act(() => {
      surface.dispatchEvent(dropEvent);
    });
    await waitFor(() =>
      expect(createResponseBoardCard).toHaveBeenCalledWith({
        sessionId: "session-1",
        messageId: "message-1",
        x: 220,
        y: 232,
      }),
    );
  });
});
