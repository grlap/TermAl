import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  createResponseBoardCard,
  deleteResponseBoardCard,
  fetchResponseBoard,
  updateResponseBoardCard,
  type ResponseBoardCard,
} from "../api";
import { getErrorMessage } from "../app-utils";
import { MessageCard } from "../message-cards";
import { readResponseBoardMessageDragData } from "../response-board";

const BOARD_PADDING = 72;
const CARD_PATCH_DEBOUNCE_MS = 250;
export const RESPONSE_BOARD_ZOOM_STORAGE_KEY = "termal.response-board.zoom.v1";
const MIN_BOARD_ZOOM = 0.25;
const MAX_BOARD_ZOOM = 2;
const BOARD_ZOOM_BUTTON_FACTOR = 1.2;
const BOARD_WHEEL_ZOOM_SENSITIVITY = 0.0015;

function wheelRequestsBoardZoom(event: WheelEvent) {
  return event.ctrlKey || event.getModifierState("Fn");
}

type BoardView = {
  panX: number;
  panY: number;
  zoom: number;
};

type CardGesture = {
  kind: "move" | "resize";
  cardId: string;
  pointerId: number;
  clientX: number;
  clientY: number;
  x: number;
  y: number;
  w: number;
  h: number;
  zoom: number;
};

type PanGesture = {
  pointerId: number;
  clientX: number;
  clientY: number;
  panX: number;
  panY: number;
};

function clampBoardZoom(value: number) {
  return Math.min(MAX_BOARD_ZOOM, Math.max(MIN_BOARD_ZOOM, value));
}

function readStoredBoardZoom() {
  try {
    const stored = Number(window.localStorage.getItem(RESPONSE_BOARD_ZOOM_STORAGE_KEY));
    return Number.isFinite(stored) && stored > 0 ? clampBoardZoom(stored) : 1;
  } catch {
    return 1;
  }
}

function zoomBoardViewAtPoint(
  view: BoardView,
  requestedZoom: number,
  surfaceX: number,
  surfaceY: number,
): BoardView {
  const zoom = clampBoardZoom(requestedZoom);
  if (zoom === view.zoom) {
    return view;
  }
  const logicalX = (surfaceX - view.panX) / view.zoom;
  const logicalY = (surfaceY - view.panY) / view.zoom;
  return {
    zoom,
    panX: surfaceX - logicalX * zoom,
    panY: surfaceY - logicalY * zoom,
  };
}

export function ResponseBoardPanel({
  refreshToken,
  onOpenSource,
}: {
  refreshToken: string;
  onOpenSource: (card: ResponseBoardCard) => void;
}) {
  const [cards, setCards] = useState<ResponseBoardCard[]>([]);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<BoardView>(() => ({
    panX: 0,
    panY: 0,
    zoom: readStoredBoardZoom(),
  }));
  const [isPanning, setIsPanning] = useState(false);
  const cardsRef = useRef<ResponseBoardCard[]>([]);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const cardGestureRef = useRef<CardGesture | null>(null);
  const panGestureRef = useRef<PanGesture | null>(null);
  const patchTimersRef = useRef(new Map<string, number>());
  const patchVersionsRef = useRef(new Map<string, number>());
  const replaceCards = useCallback((nextCards: ResponseBoardCard[]) => {
    cardsRef.current = nextCards;
    setCards(nextCards);
  }, []);

  useEffect(() => {
    let active = true;
    setIsLoading(true);
    setError(null);
    void fetchResponseBoard().then(
      (board) => {
        if (!active) {
          return;
        }
        replaceCards(board.cards);
        setIsLoading(false);
      },
      (reason) => {
        if (!active) {
          return;
        }
        setError(getErrorMessage(reason));
        setIsLoading(false);
      },
    );
    return () => {
      active = false;
    };
  }, [refreshToken, replaceCards]);

  useEffect(
    () => () => {
      for (const timer of patchTimersRef.current.values()) {
        window.clearTimeout(timer);
      }
      patchTimersRef.current.clear();
    },
    [],
  );

  useEffect(() => {
    try {
      window.localStorage.setItem(
        RESPONSE_BOARD_ZOOM_STORAGE_KEY,
        String(view.zoom),
      );
    } catch {
      // View persistence is best-effort; board data remains server-owned.
    }
  }, [view.zoom]);

  const scheduleCardPatch = useCallback((card: ResponseBoardCard) => {
    const currentTimer = patchTimersRef.current.get(card.id);
    if (currentTimer !== undefined) {
      window.clearTimeout(currentTimer);
    }
    const version = (patchVersionsRef.current.get(card.id) ?? 0) + 1;
    patchVersionsRef.current.set(card.id, version);
    const timer = window.setTimeout(() => {
      patchTimersRef.current.delete(card.id);
      void updateResponseBoardCard(card.id, {
        x: card.x,
        y: card.y,
        w: card.w,
        h: card.h,
      }).then(
        (persisted) => {
          if (patchVersionsRef.current.get(card.id) !== version) {
            return;
          }
          replaceCards(
            cardsRef.current.map((candidate) =>
              candidate.id === persisted.id ? persisted : candidate,
            ),
          );
        },
        (reason) => {
          if (patchVersionsRef.current.get(card.id) === version) {
            setError(getErrorMessage(reason));
          }
        },
      );
    }, CARD_PATCH_DEBOUNCE_MS);
    patchTimersRef.current.set(card.id, timer);
  }, [replaceCards]);

  const updateCardGeometry = useCallback(
    (cardId: string, update: (card: ResponseBoardCard) => ResponseBoardCard) => {
      const currentCard = cardsRef.current.find((card) => card.id === cardId);
      if (!currentCard) {
        return;
      }
      const changedCard = update(currentCard);
      replaceCards(
        cardsRef.current.map((card) =>
          card.id === cardId ? changedCard : card,
        ),
      );
      scheduleCardPatch(changedCard);
    },
    [replaceCards, scheduleCardPatch],
  );

  const handleCardPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLElement>) => {
      const gesture = cardGestureRef.current;
      if (!gesture || gesture.pointerId !== event.pointerId) {
        return;
      }
      const deltaX = (event.clientX - gesture.clientX) / gesture.zoom;
      const deltaY = (event.clientY - gesture.clientY) / gesture.zoom;
      updateCardGeometry(gesture.cardId, (card) =>
        gesture.kind === "move"
          ? {
              ...card,
              x: Math.max(0, gesture.x + deltaX),
              y: Math.max(0, gesture.y + deltaY),
            }
          : {
              ...card,
              w: Math.min(1_600, Math.max(240, gesture.w + deltaX)),
              h: Math.min(1_600, Math.max(160, gesture.h + deltaY)),
            },
      );
    },
    [updateCardGeometry],
  );

  const finishCardGesture = useCallback((event: ReactPointerEvent<HTMLElement>) => {
    if (cardGestureRef.current?.pointerId !== event.pointerId) {
      return;
    }
    cardGestureRef.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  }, []);

  const beginCardGesture = useCallback(
    (
      event: ReactPointerEvent<HTMLElement>,
      card: ResponseBoardCard,
      kind: CardGesture["kind"],
    ) => {
      if (event.button !== 0) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      const cardNode = event.currentTarget.closest(
        ".response-board-card",
      ) as HTMLElement | null;
      cardNode?.setPointerCapture?.(event.pointerId);
      cardGestureRef.current = {
        kind,
        cardId: card.id,
        pointerId: event.pointerId,
        clientX: event.clientX,
        clientY: event.clientY,
        x: card.x,
        y: card.y,
        w: card.w,
        h: card.h,
        zoom: view.zoom,
      };
    },
    [view.zoom],
  );

  const handleRemove = useCallback((cardId: string) => {
    const timer = patchTimersRef.current.get(cardId);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      patchTimersRef.current.delete(cardId);
    }
    patchVersionsRef.current.set(cardId, (patchVersionsRef.current.get(cardId) ?? 0) + 1);
    void deleteResponseBoardCard(cardId).then(
      () =>
        replaceCards(cardsRef.current.filter((card) => card.id !== cardId)),
      (reason) => setError(getErrorMessage(reason)),
    );
  }, [replaceCards]);

  const handleDrop = useCallback((event: React.DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    const source = readResponseBoardMessageDragData(event.dataTransfer);
    const surface = surfaceRef.current;
    if (!source || !surface) {
      return;
    }
    const rect = surface.getBoundingClientRect();
    const clientX = Number.isFinite(event.clientX)
      ? event.clientX
      : rect.left + surface.clientWidth / 2;
    const clientY = Number.isFinite(event.clientY)
      ? event.clientY
      : rect.top + surface.clientHeight / 2;
    const x = Math.max(
      0,
      (clientX - rect.left - view.panX) / view.zoom - 180,
    );
    const y = Math.max(
      0,
      (clientY - rect.top - view.panY) / view.zoom - 28,
    );
    setError(null);
    void createResponseBoardCard({ ...source, x, y }).then(
      (card) => replaceCards([...cardsRef.current, card]),
      (reason) => setError(getErrorMessage(reason)),
    );
  }, [replaceCards, view.panX, view.panY, view.zoom]);

  const zoomAtSurfaceCenter = useCallback(
    (resolveZoom: (currentZoom: number) => number) => {
      const surface = surfaceRef.current;
      if (!surface) {
        return;
      }
      const rect = surface.getBoundingClientRect();
      setView((current) =>
        zoomBoardViewAtPoint(
          current,
          resolveZoom(current.zoom),
          rect.width / 2,
          rect.height / 2,
        ),
      );
    },
    [],
  );

  const handleBoardWheel = useCallback((event: WheelEvent) => {
    const surface = surfaceRef.current;
    if (!wheelRequestsBoardZoom(event) || !surface) {
      return;
    }
    event.preventDefault();
    const rect = surface.getBoundingClientRect();
    const surfaceX = event.clientX - rect.left;
    const surfaceY = event.clientY - rect.top;
    setView((current) =>
      zoomBoardViewAtPoint(
        current,
        current.zoom * Math.exp(-event.deltaY * BOARD_WHEEL_ZOOM_SENSITIVITY),
        surfaceX,
        surfaceY,
      ),
    );
  }, []);

  useEffect(() => {
    const surface = surfaceRef.current;
    if (!surface) {
      return;
    }
    surface.addEventListener("wheel", handleBoardWheel, { passive: false });
    return () => surface.removeEventListener("wheel", handleBoardWheel);
  }, [handleBoardWheel]);

  const handleBoardKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      if (!(event.ctrlKey || event.metaKey)) {
        return;
      }
      if (event.key === "+" || event.key === "=") {
        event.preventDefault();
        zoomAtSurfaceCenter((zoom) => zoom * BOARD_ZOOM_BUTTON_FACTOR);
      } else if (event.key === "-" || event.key === "_") {
        event.preventDefault();
        zoomAtSurfaceCenter((zoom) => zoom / BOARD_ZOOM_BUTTON_FACTOR);
      } else if (event.key === "0") {
        event.preventDefault();
        zoomAtSurfaceCenter(() => 1);
      }
    },
    [zoomAtSurfaceCenter],
  );

  const handlePanPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const target = event.target as Element;
      if (event.button !== 0 || target.closest(".response-board-card")) {
        return;
      }
      event.preventDefault();
      window.getSelection()?.removeAllRanges();
      event.currentTarget.focus({ preventScroll: true });
      event.currentTarget.setPointerCapture?.(event.pointerId);
      setIsPanning(true);
      panGestureRef.current = {
        pointerId: event.pointerId,
        clientX: event.clientX,
        clientY: event.clientY,
        panX: view.panX,
        panY: view.panY,
      };
    },
    [view.panX, view.panY],
  );

  const handlePanPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const gesture = panGestureRef.current;
      if (!gesture || gesture.pointerId !== event.pointerId) {
        return;
      }
      setView((current) => ({
        ...current,
        panX: gesture.panX + (event.clientX - gesture.clientX),
        panY: gesture.panY + (event.clientY - gesture.clientY),
      }));
    },
    [],
  );

  const finishPan = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (panGestureRef.current?.pointerId !== event.pointerId) {
      return;
    }
    panGestureRef.current = null;
    setIsPanning(false);
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  }, []);

  const planeSize = useMemo(
    () => ({
      width: Math.max(
        0,
        ...cards.map((card) => card.x + card.w + BOARD_PADDING),
      ),
      height: Math.max(
        0,
        ...cards.map((card) => card.y + card.h + BOARD_PADDING),
      ),
    }),
    [cards],
  );

  return (
    <section className="response-board-panel" aria-label="Response board">
      <header className="response-board-toolbar">
        <div className="response-board-toolbar-copy">
          <strong>Response board</strong>
          <span>Drag any transcript message here or use Pin to board.</span>
        </div>
        <div className="response-board-toolbar-actions">
          <div className="response-board-zoom-controls" aria-label="Board zoom controls">
            <button
              type="button"
              aria-label="Zoom out"
              onClick={() =>
                zoomAtSurfaceCenter((zoom) => zoom / BOARD_ZOOM_BUTTON_FACTOR)
              }
            >
              −
            </button>
            <button
              type="button"
              className="response-board-zoom-reset"
              aria-label="Reset board zoom to 100%"
              onClick={() => zoomAtSurfaceCenter(() => 1)}
            >
              {Math.round(view.zoom * 100)}%
            </button>
            <button
              type="button"
              aria-label="Zoom in"
              onClick={() =>
                zoomAtSurfaceCenter((zoom) => zoom * BOARD_ZOOM_BUTTON_FACTOR)
              }
            >
              +
            </button>
          </div>
          <span className="response-board-card-count">{cards.length} cards</span>
        </div>
      </header>
      {error ? (
        <div className="response-board-error" role="alert">
          {error}
          <button type="button" onClick={() => setError(null)} aria-label="Dismiss error">
            ×
          </button>
        </div>
      ) : null}
      <div
        ref={surfaceRef}
        className={`response-board-surface${isPanning ? " is-panning" : ""}`}
        tabIndex={0}
        aria-label="Response board canvas"
        aria-keyshortcuts="Control+= Meta+= Control+- Meta+- Control+0 Meta+0"
        onDragOver={(event) => {
          event.preventDefault();
          event.dataTransfer.dropEffect = "copy";
        }}
        onDrop={handleDrop}
        onPointerDown={handlePanPointerDown}
        onPointerMove={handlePanPointerMove}
        onPointerUp={finishPan}
        onPointerCancel={finishPan}
        onLostPointerCapture={finishPan}
        onKeyDown={handleBoardKeyDown}
      >
        <div
          className="response-board-plane"
          style={{
            width: planeSize.width,
            height: planeSize.height,
            minWidth: `${100 / view.zoom}%`,
            minHeight: `${100 / view.zoom}%`,
            transform: `translate(${view.panX}px, ${view.panY}px) scale(${view.zoom})`,
          }}
        >
          {isLoading ? (
            <div className="response-board-empty">Loading response board…</div>
          ) : cards.length === 0 ? (
            <div className="response-board-empty">
              Drop an agent response anywhere on the board.
            </div>
          ) : null}
          {cards.map((card) => (
            <article
              key={card.id}
              className="response-board-card"
              style={{
                left: card.x,
                top: card.y,
                width: card.w,
                height: card.h,
              }}
              onPointerMove={handleCardPointerMove}
              onPointerUp={finishCardGesture}
              onPointerCancel={finishCardGesture}
            >
              <header
                className="response-board-card-header"
                onPointerDown={(event) => beginCardGesture(event, card, "move")}
              >
                <span
                  className={`response-board-agent-dot is-${card.sourceAgent.toLowerCase()}`}
                  aria-hidden="true"
                />
                <span className="response-board-card-title">
                  {card.sourceSessionName}
                </span>
                <time>{card.snapshot.timestamp}</time>
                <button
                  type="button"
                  className="response-board-remove"
                  aria-label={`Remove response from ${card.sourceSessionName}`}
                  onPointerDown={(event) => event.stopPropagation()}
                  onClick={() => handleRemove(card.id)}
                >
                  ×
                </button>
              </header>
              <div className="response-board-card-body">
                <div className="response-board-card-snapshot">
                  <MessageCard
                    message={card.snapshot}
                    approvalActionsEnabled={false}
                    parallelAgentActionsEnabled={false}
                    preferImmediateHeavyRender
                    onApprovalDecision={() => {}}
                    onUserInputSubmit={() => {}}
                  />
                </div>
              </div>
              <footer className="response-board-card-footer">
                <button type="button" onClick={() => onOpenSource(card)}>
                  Open in session
                </button>
              </footer>
              <button
                type="button"
                className="response-board-resize-handle"
                aria-label={`Resize response from ${card.sourceSessionName}`}
                onPointerDown={(event) => beginCardGesture(event, card, "resize")}
              />
            </article>
          ))}
        </div>
      </div>
    </section>
  );
}
