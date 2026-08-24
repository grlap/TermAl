// Owns the shared staging tray and staged-card preview presentation.
// Deliberately does not own card persistence, tab selection, or canvas gestures.
// Split from ResponseBoardPanel.tsx.

import type { ResponseBoardCard } from "../api";
import { MessageCard } from "../message-cards";

export const RESPONSE_BOARD_STAGED_CARD_MIME =
  "application/x-termal-response-board-staged-card";

export function ResponseBoardStagingTray({
  cards,
  previewCardId,
  onPreview,
}: {
  cards: ResponseBoardCard[];
  previewCardId: string | null;
  onPreview: (cardId: string) => void;
}) {
  return (
    <section className="response-board-staging" aria-label="Staging tray">
      <header>
        <strong>Staging</strong>
        <span>{cards.length} waiting</span>
      </header>
      <ul className="response-board-staging-list">
        {cards.length === 0 ? (
          <li className="response-board-staging-empty">
            Pin responses here, then place them on any board.
          </li>
        ) : (
          cards.map((card) => (
            <li key={card.id} className="response-board-staged-card-item">
              <button
                type="button"
                draggable
                className={`response-board-staged-card${previewCardId === card.id ? " is-previewing" : ""}`}
                onDragStart={(event) => {
                  event.dataTransfer.effectAllowed = "move";
                  event.dataTransfer.setData(
                    RESPONSE_BOARD_STAGED_CARD_MIME,
                    card.id,
                  );
                }}
                onClick={() => onPreview(card.id)}
              >
                <span
                  className={`response-board-agent-dot is-${card.sourceAgent.toLowerCase()}`}
                  aria-hidden="true"
                />
                <span>{card.sourceSessionName}</span>
                <small>{card.snapshot.timestamp}</small>
              </button>
            </li>
          ))
        )}
      </ul>
    </section>
  );
}

export function ResponseBoardPreview({
  card,
  selectedTabName,
  onPlace,
  onOpenSource,
  onDelete,
  onClose,
}: {
  card: ResponseBoardCard;
  selectedTabName: string | null;
  onPlace: () => void;
  onOpenSource: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  return (
    <section
      className="response-board-preview"
      aria-label="Staged card preview"
      onPointerDown={(event) => event.stopPropagation()}
    >
      <div className="response-board-preview-content">
        <MessageCard
          message={card.snapshot}
          approvalActionsEnabled={false}
          parallelAgentActionsEnabled={false}
          preferImmediateHeavyRender
          onApprovalDecision={() => {}}
          onUserInputSubmit={() => {}}
        />
      </div>
      <div className="response-board-preview-actions">
        <button type="button" onClick={onPlace}>
          Place on {selectedTabName ?? "board"}
        </button>
        <button type="button" onClick={onOpenSource}>
          Open source
        </button>
        <button type="button" className="is-danger" onClick={onDelete}>
          Delete
        </button>
        <button
          type="button"
          aria-label="Close staged card preview"
          onClick={onClose}
        >
          ×
        </button>
      </div>
    </section>
  );
}
