// Owns the rendered assistant connection-retry notice card.
// Does not own retry parsing/classification or general message metadata layout.
// Split from ui/src/message-cards.tsx.

import type { ReactNode } from "react";
import {
  connectionRetryPresentationFor,
  type ConnectionRetryDisplayState,
  type ConnectionRetryNotice,
} from "./connection-retry";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "./search-highlight";

export function ConnectionRetryCard({
  meta,
  notice,
  searchQuery,
  searchHighlightTone,
  displayState,
}: {
  meta: ReactNode;
  notice: ConnectionRetryNotice;
  searchQuery: string;
  searchHighlightTone: SearchHighlightTone;
  displayState: ConnectionRetryDisplayState;
}) {
  const {
    ariaLive,
    cardClassName,
    chipClassName,
    detail,
    heading,
    showSpinner,
  } = connectionRetryPresentationFor(notice, displayState);
  return (
    <article
      className={cardClassName}
      role="status"
      aria-live={ariaLive}
    >
      {meta}
      <div className="connection-notice-body">
        {showSpinner ? (
          <div
            className="activity-spinner connection-notice-spinner"
            aria-hidden="true"
          />
        ) : null}
        <div className="connection-notice-copy">
          <div className="card-label">Connection</div>
          <div className="connection-notice-heading">
            <h3>{heading}</h3>
            {notice.attemptLabel ? (
              <span className={chipClassName}>{notice.attemptLabel}</span>
            ) : null}
          </div>
          <p className="connection-notice-detail">
            {renderHighlightedText(detail, searchQuery, searchHighlightTone)}
          </p>
        </div>
      </div>
    </article>
  );
}
