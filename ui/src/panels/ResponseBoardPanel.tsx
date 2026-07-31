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
};

type PanGesture = {
  pointerId: number;
  clientX: number;
  clientY: number;
  scrollLeft: number;
  scrollTop: number;
};

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
      const deltaX = event.clientX - gesture.clientX;
      const deltaY = event.clientY - gesture.clientY;
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
      };
    },
    [],
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
    const x = Math.max(0, clientX - rect.left + surface.scrollLeft - 180);
    const y = Math.max(0, clientY - rect.top + surface.scrollTop - 28);
    setError(null);
    void createResponseBoardCard({ ...source, x, y }).then(
      (card) => replaceCards([...cardsRef.current, card]),
      (reason) => setError(getErrorMessage(reason)),
    );
  }, [replaceCards]);

  const handlePanPointerDown = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const target = event.target as Element;
      if (event.button !== 0 || target.closest(".response-board-card")) {
        return;
      }
      event.currentTarget.setPointerCapture?.(event.pointerId);
      panGestureRef.current = {
        pointerId: event.pointerId,
        clientX: event.clientX,
        clientY: event.clientY,
        scrollLeft: event.currentTarget.scrollLeft,
        scrollTop: event.currentTarget.scrollTop,
      };
    },
    [],
  );

  const handlePanPointerMove = useCallback(
    (event: ReactPointerEvent<HTMLDivElement>) => {
      const gesture = panGestureRef.current;
      if (!gesture || gesture.pointerId !== event.pointerId) {
        return;
      }
      event.currentTarget.scrollLeft =
        gesture.scrollLeft - (event.clientX - gesture.clientX);
      event.currentTarget.scrollTop =
        gesture.scrollTop - (event.clientY - gesture.clientY);
    },
    [],
  );

  const finishPan = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (panGestureRef.current?.pointerId !== event.pointerId) {
      return;
    }
    panGestureRef.current = null;
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
        <div>
          <strong>Response board</strong>
          <span>Drag any transcript message here or use Pin to board.</span>
        </div>
        <span className="response-board-card-count">{cards.length} cards</span>
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
        className="response-board-surface"
        onDragOver={(event) => {
          event.preventDefault();
          event.dataTransfer.dropEffect = "copy";
        }}
        onDrop={handleDrop}
        onPointerDown={handlePanPointerDown}
        onPointerMove={handlePanPointerMove}
        onPointerUp={finishPan}
        onPointerCancel={finishPan}
      >
        <div
          className="response-board-plane"
          style={{
            width: planeSize.width,
            height: planeSize.height,
            minWidth: "100%",
            minHeight: "100%",
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
