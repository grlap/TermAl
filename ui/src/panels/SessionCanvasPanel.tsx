import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent as ReactDragEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { AgentIcon } from "../agent-icon";
import {
  dataTransferHasSessionDragType,
  readSessionDragData,
} from "../session-drag";
import { TAB_DRAG_MIME_TYPE, type WorkspaceTabDrag } from "../tab-drag";
import type { Session } from "../types";
import {
  WORKSPACE_CANVAS_DEFAULT_ZOOM,
  normalizeWorkspaceCanvasZoom,
  type WorkspaceCanvasCard,
  type WorkspaceCanvasTab,
} from "../workspace";

const CANVAS_WIDTH_PX = 3600;
const CANVAS_HEIGHT_PX = 2400;
const CARD_WIDTH_PX = 336;
const CARD_HEIGHT_PX = 228;
const CARD_MARGIN_PX = 48;
const CARD_GRID_COLUMNS = 4;
const CARD_GRID_GAP_X = 372;
const CARD_GRID_GAP_Y = 276;
const CARD_GRID_OFFSET_X = 160;
const CARD_GRID_OFFSET_Y = 120;
const WHEEL_ZOOM_SENSITIVITY = 0.0015;
const PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX = 3;

type CardDragState = {
  deltaX: number;
  deltaY: number;
  originX: number;
  originY: number;
  pointerId: number;
  sessionId: string;
  startClientX: number;
  startClientY: number;
};

type ZoomAnchor = {
  canvasX: number;
  canvasY: number;
  clientOffsetX: number;
  clientOffsetY: number;
  scrollContainer: HTMLElement;
  scrollLeft: number;
  scrollTop: number;
};

type PanDragState = {
  hasMoved: boolean;
  originScrollLeft: number;
  originScrollTop: number;
  pointerId: number;
  scrollContainer: HTMLElement;
  startClientX: number;
  startClientY: number;
};

export function SessionCanvasPanel({
  tab,
  sessionLookup,
  draggedTab,
  onOpenSession,
  onRemoveCard,
  onSetZoom,
  onUpsertCard,
}: {
  tab: WorkspaceCanvasTab;
  sessionLookup: ReadonlyMap<string, Session>;
  draggedTab: WorkspaceTabDrag | null;
  onOpenSession: (sessionId: string) => void;
  onRemoveCard: (sessionId: string) => void;
  onSetZoom: (zoom: number) => void;
  onUpsertCard: (sessionId: string, position: Pick<WorkspaceCanvasCard, "x" | "y">) => void;
}) {
  const scaleFrameRef = useRef<HTMLDivElement | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const dragDepthRef = useRef(0);
  const panDragStateRef = useRef<PanDragState | null>(null);
  const suppressPanContextMenuRef = useRef(false);
  const zoomAnchorRef = useRef<ZoomAnchor | null>(null);
  const [isPanning, setIsPanning] = useState(false);
  const [isSessionDropActive, setIsSessionDropActive] = useState(false);
  const [cardDragState, setCardDragState] = useState<CardDragState | null>(null);

  const zoom = normalizeWorkspaceCanvasZoom(tab.zoom);
  const liveCards = useMemo(
    () =>
      tab.cards.flatMap((card) => {
        const session = sessionLookup.get(card.sessionId);
        return session ? [{ card, session }] : [];
      }),
    [sessionLookup, tab.cards],
  );

  useEffect(() => {
    const frame = scaleFrameRef.current;
    if (!frame) {
      return;
    }

    function handleCanvasWheel(event: WheelEvent) {
      if (event.defaultPrevented || !event.ctrlKey) {
        return;
      }

      const frameNode = event.currentTarget;
      if (!(frameNode instanceof HTMLDivElement)) {
        return;
      }

      const scrollContainer = getCanvasScrollContainer(frameNode);
      if (!scrollContainer) {
        return;
      }

      event.preventDefault();
      const nextZoom = normalizeWorkspaceCanvasZoom(
        zoom * Math.exp(-event.deltaY * WHEEL_ZOOM_SENSITIVITY),
      );
      if (nextZoom === zoom) {
        return;
      }

      const rect = frameNode.getBoundingClientRect();
      const clientOffsetX = event.clientX - rect.left;
      const clientOffsetY = event.clientY - rect.top;
      zoomAnchorRef.current = {
        canvasX: clientOffsetX / zoom,
        canvasY: clientOffsetY / zoom,
        clientOffsetX,
        clientOffsetY,
        scrollContainer,
        scrollLeft: scrollContainer.scrollLeft,
        scrollTop: scrollContainer.scrollTop,
      };
      onSetZoom(nextZoom);
    }

    frame.addEventListener("wheel", handleCanvasWheel, { passive: false });
    return () => {
      frame.removeEventListener("wheel", handleCanvasWheel);
    };
  }, [onSetZoom, zoom]);

  useLayoutEffect(() => {
    const anchor = zoomAnchorRef.current;
    if (!anchor) {
      return;
    }

    zoomAnchorRef.current = null;
    anchor.scrollContainer.scrollLeft = Math.max(
      anchor.scrollLeft + anchor.canvasX * zoom - anchor.clientOffsetX,
      0,
    );
    anchor.scrollContainer.scrollTop = Math.max(
      anchor.scrollTop + anchor.canvasY * zoom - anchor.clientOffsetY,
      0,
    );
  }, [zoom]);

  function clearDropState() {
    dragDepthRef.current = 0;
    setIsSessionDropActive(false);
  }

  function handleCanvasDragEnter(event: ReactDragEvent<HTMLElement>) {
    if (!canAcceptSessionDrop(event.dataTransfer, draggedTab)) {
      return;
    }

    event.preventDefault();
    dragDepthRef.current += 1;
    setIsSessionDropActive(true);
  }

  function handleCanvasDragOver(event: ReactDragEvent<HTMLElement>) {
    if (!canAcceptSessionDrop(event.dataTransfer, draggedTab)) {
      return;
    }

    event.preventDefault();
    event.dataTransfer.dropEffect = dataTransferHasSessionDragType(event.dataTransfer)
      ? "copy"
      : "move";
    if (!isSessionDropActive) {
      setIsSessionDropActive(true);
    }
  }

  function handleCanvasDragLeave(event: ReactDragEvent<HTMLElement>) {
    if (!canAcceptSessionDrop(event.dataTransfer, draggedTab)) {
      return;
    }

    const nextTarget = event.relatedTarget;
    if (nextTarget instanceof Node && event.currentTarget.contains(nextTarget)) {
      return;
    }

    dragDepthRef.current = Math.max(dragDepthRef.current - 1, 0);
    if (dragDepthRef.current === 0) {
      setIsSessionDropActive(false);
    }
  }

  function handleCanvasDrop(event: ReactDragEvent<HTMLElement>) {
    if (!canAcceptSessionDrop(event.dataTransfer, draggedTab)) {
      return;
    }

    event.preventDefault();
    const sessionId = readDroppedSessionId(event.dataTransfer, draggedTab);
    clearDropState();
    if (!sessionId) {
      return;
    }

    const existingIndex = tab.cards.findIndex((card) => card.sessionId === sessionId);
    onUpsertCard(
      sessionId,
      resolveDropPosition(
        surfaceRef.current,
        event.clientX,
        event.clientY,
        zoom,
        existingIndex >= 0 ? existingIndex : tab.cards.length,
      ),
    );
  }

  function startCanvasPan(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 2) {
      return;
    }

    const scrollContainer = getCanvasScrollContainer(event.currentTarget);
    if (!scrollContainer) {
      return;
    }

    event.preventDefault();
    suppressPanContextMenuRef.current = false;
    event.currentTarget.setPointerCapture(event.pointerId);
    panDragStateRef.current = {
      hasMoved: false,
      originScrollLeft: scrollContainer.scrollLeft,
      originScrollTop: scrollContainer.scrollTop,
      pointerId: event.pointerId,
      scrollContainer,
      startClientX: event.clientX,
      startClientY: event.clientY,
    };
    setIsPanning(true);
  }

  function updateCanvasPan(event: ReactPointerEvent<HTMLDivElement>) {
    const current = panDragStateRef.current;
    if (!current || current.pointerId !== event.pointerId) {
      return;
    }

    event.preventDefault();
    const deltaX = event.clientX - current.startClientX;
    const deltaY = event.clientY - current.startClientY;
    current.scrollContainer.scrollLeft = Math.max(current.originScrollLeft - deltaX, 0);
    current.scrollContainer.scrollTop = Math.max(current.originScrollTop - deltaY, 0);
    if (
      !current.hasMoved &&
      (Math.abs(deltaX) >= PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX ||
        Math.abs(deltaY) >= PAN_CONTEXT_MENU_SUPPRESS_THRESHOLD_PX)
    ) {
      current.hasMoved = true;
      suppressPanContextMenuRef.current = true;
    }
  }

  function finishCanvasPan(event: ReactPointerEvent<HTMLDivElement>, cancelled = false) {
    const current = panDragStateRef.current;
    if (!current || current.pointerId !== event.pointerId) {
      return;
    }

    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }

    if (!cancelled && current.hasMoved) {
      suppressPanContextMenuRef.current = true;
    }

    panDragStateRef.current = null;
    setIsPanning(false);
  }

  function handleCanvasContextMenu(event: ReactMouseEvent<HTMLDivElement>) {
    if (!isPanning && !suppressPanContextMenuRef.current) {
      return;
    }

    event.preventDefault();
    suppressPanContextMenuRef.current = false;
  }

  function startCardDrag(card: WorkspaceCanvasCard, event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) {
      return;
    }

    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    setCardDragState({
      deltaX: 0,
      deltaY: 0,
      originX: card.x,
      originY: card.y,
      pointerId: event.pointerId,
      sessionId: card.sessionId,
      startClientX: event.clientX,
      startClientY: event.clientY,
    });
  }

  function updateCardDrag(sessionId: string, event: ReactPointerEvent<HTMLDivElement>) {
    setCardDragState((current) => {
      if (!current || current.pointerId !== event.pointerId || current.sessionId !== sessionId) {
        return current;
      }

      return {
        ...current,
        deltaX: (event.clientX - current.startClientX) / zoom,
        deltaY: (event.clientY - current.startClientY) / zoom,
      };
    });
  }

  function finishCardDrag(
    sessionId: string,
    event: ReactPointerEvent<HTMLDivElement>,
    cancelled = false,
  ) {
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }

    setCardDragState((current) => {
      if (!current || current.pointerId !== event.pointerId || current.sessionId !== sessionId) {
        return current;
      }

      if (!cancelled) {
        const nextPosition = clampCanvasPosition({
          x: current.originX + current.deltaX,
          y: current.originY + current.deltaY,
        });
        if (nextPosition.x !== current.originX || nextPosition.y !== current.originY) {
          onUpsertCard(sessionId, nextPosition);
        }
      }

      return null;
    });
  }

  return (
    <section
      className={`session-canvas-panel${isSessionDropActive ? " drop-active" : ""}`}
      onDragEnter={handleCanvasDragEnter}
      onDragLeave={handleCanvasDragLeave}
      onDragOver={handleCanvasDragOver}
      onDrop={handleCanvasDrop}
    >
      <header className="session-canvas-toolbar">
        <div className="session-canvas-toolbar-copy">
          <div className="card-label">Canvas</div>
          <h2>Session board</h2>
          <p>
            Drag sessions in from the Sessions list or any session tab, rearrange the cards, use
            Ctrl + wheel to zoom, and right-drag to pan.
          </p>
        </div>
        <div className="session-canvas-toolbar-meta">
          <span className="session-canvas-toolbar-count">
            {liveCards.length} card{liveCards.length === 1 ? "" : "s"}
          </span>
          <button
            className="ghost-button session-canvas-toolbar-zoom"
            type="button"
            onClick={() => onSetZoom(WORKSPACE_CANVAS_DEFAULT_ZOOM)}
            disabled={zoom === WORKSPACE_CANVAS_DEFAULT_ZOOM}
            title="Reset zoom to 100%"
          >
            {Math.round(zoom * 100)}%
          </button>
        </div>
      </header>

      <div
        ref={scaleFrameRef}
        className={`session-canvas-scale-frame${isPanning ? " panning" : ""}`}
        style={{
          width: `${CANVAS_WIDTH_PX * zoom}px`,
          height: `${CANVAS_HEIGHT_PX * zoom}px`,
        }}
        onContextMenu={handleCanvasContextMenu}
        onPointerCancel={(event) => finishCanvasPan(event, true)}
        onPointerDown={startCanvasPan}
        onPointerMove={updateCanvasPan}
        onPointerUp={(event) => finishCanvasPan(event)}
      >
        <div
          ref={surfaceRef}
          className="session-canvas-surface"
          style={{
            width: `${CANVAS_WIDTH_PX}px`,
            height: `${CANVAS_HEIGHT_PX}px`,
            transform: `scale(${zoom})`,
          }}
        >
          {liveCards.length === 0 ? (
            <div className="session-canvas-empty panel">
              <div className="card-label">Start Here</div>
              <h3>Build a session canvas</h3>
              <p>Drop sessions here to pin them as cards. Each card stays live and opens the real session.</p>
              <p>Drag by a card header to reposition it.</p>
            </div>
          ) : null}

          {isSessionDropActive ? (
            <div className="session-canvas-drop-hint" aria-hidden="true">
              Drop to pin this session on the canvas
            </div>
          ) : null}

          {liveCards.map(({ card, session }) => {
            const isDragging = cardDragState?.sessionId === card.sessionId;
            const offsetX = isDragging ? cardDragState.deltaX : 0;
            const offsetY = isDragging ? cardDragState.deltaY : 0;

            return (
              <article
                key={card.sessionId}
                className={`session-canvas-card${isDragging ? " dragging" : ""}`}
                style={{
                  left: `${card.x + offsetX}px`,
                  top: `${card.y + offsetY}px`,
                  width: `${CARD_WIDTH_PX}px`,
                }}
                onDoubleClick={() => onOpenSession(session.id)}
              >
                <div
                  className="session-canvas-card-grab"
                  onPointerDown={(event) => startCardDrag(card, event)}
                  onPointerMove={(event) => updateCardDrag(card.sessionId, event)}
                  onPointerUp={(event) => finishCardDrag(card.sessionId, event)}
                  onPointerCancel={(event) => finishCardDrag(card.sessionId, event, true)}
                >
                  <div className="session-canvas-card-topline">
                    <span className={`session-canvas-card-status is-${statusTone(session.status)}`}>
                      <AgentIcon agent={session.agent} className="session-canvas-card-agent" />
                      <span>{statusLabel(session.status)}</span>
                    </span>
                    <button
                      className="session-canvas-card-remove"
                      type="button"
                      onPointerDown={(event) => event.stopPropagation()}
                      onClick={(event) => {
                        event.stopPropagation();
                        onRemoveCard(session.id);
                      }}
                      aria-label={`Remove ${session.name} from canvas`}
                      title="Remove from canvas"
                    >
                      &times;
                    </button>
                  </div>
                  <h3>{session.name}</h3>
                </div>

                <div className="session-canvas-card-body">
                  <div className="session-canvas-card-meta">
                    <span>{session.agent}</span>
                    <span>{session.model}</span>
                  </div>
                  <div className="session-canvas-card-workdir" title={session.workdir}>
                    {session.workdir}
                  </div>
                  <div className="session-canvas-card-preview" title={session.preview || "No preview yet."}>
                    {session.preview || "No preview yet."}
                  </div>
                </div>

                <div className="session-canvas-card-actions">
                  <button
                    className="ghost-button session-canvas-card-action"
                    type="button"
                    onClick={() => onOpenSession(session.id)}
                  >
                    Open session
                  </button>
                </div>
              </article>
            );
          })}
        </div>
      </div>
    </section>
  );
}

function canAcceptSessionDrop(
  dataTransfer: Pick<DataTransfer, "types"> | null,
  draggedTab: WorkspaceTabDrag | null,
) {
  return dataTransferHasSessionDragType(dataTransfer) || (
    Array.from(dataTransfer?.types ?? []).includes(TAB_DRAG_MIME_TYPE) &&
    draggedTab?.tab.kind === "session"
  );
}

function readDroppedSessionId(
  dataTransfer: Pick<DataTransfer, "getData" | "types"> | null,
  draggedTab: WorkspaceTabDrag | null,
) {
  const sessionDrag = readSessionDragData(dataTransfer);
  if (sessionDrag) {
    return sessionDrag.sessionId;
  }

  const raw = dataTransfer?.getData(TAB_DRAG_MIME_TYPE) ?? "";
  if (raw) {
    try {
      const parsed: unknown = JSON.parse(raw);
      const droppedTab = (parsed as { tab?: { kind?: string; sessionId?: string } }).tab;
      if (droppedTab?.kind === "session" && typeof droppedTab.sessionId === "string") {
        const sessionId = droppedTab.sessionId.trim();
        return sessionId || null;
      }
    } catch {
      return draggedTab?.tab.kind === "session" ? draggedTab.tab.sessionId : null;
    }
  }

  return draggedTab?.tab.kind === "session" ? draggedTab.tab.sessionId : null;
}

function resolveDropPosition(
  surface: HTMLDivElement | null,
  clientX: number,
  clientY: number,
  zoom: number,
  fallbackIndex: number,
) {
  if (!surface) {
    return defaultCardPosition(fallbackIndex);
  }

  const rect = surface.getBoundingClientRect();
  return clampCanvasPosition({
    x: (clientX - rect.left) / zoom - CARD_WIDTH_PX / 2,
    y: (clientY - rect.top) / zoom - 72,
  });
}

function defaultCardPosition(index: number) {
  const row = Math.floor(index / CARD_GRID_COLUMNS);
  const column = index % CARD_GRID_COLUMNS;
  return clampCanvasPosition({
    x: CARD_GRID_OFFSET_X + column * CARD_GRID_GAP_X,
    y: CARD_GRID_OFFSET_Y + row * CARD_GRID_GAP_Y,
  });
}

function clampCanvasPosition(position: Pick<WorkspaceCanvasCard, "x" | "y">) {
  return {
    x: clamp(position.x, CARD_MARGIN_PX, CANVAS_WIDTH_PX - CARD_WIDTH_PX - CARD_MARGIN_PX),
    y: clamp(position.y, CARD_MARGIN_PX, CANVAS_HEIGHT_PX - CARD_HEIGHT_PX - CARD_MARGIN_PX),
  };
}

function statusTone(status: Session["status"]) {
  if (status === "approval") {
    return "approval";
  }

  return status;
}

function statusLabel(status: Session["status"]) {
  switch (status) {
    case "active":
      return "Active";
    case "approval":
      return "Needs approval";
    case "error":
      return "Error";
    case "idle":
      return "Idle";
  }
}

function getCanvasScrollContainer(node: Element | null) {
  const container = node?.closest(".message-stack");
  return container instanceof HTMLElement ? container : null;
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(Math.round(value), min), max);
}
