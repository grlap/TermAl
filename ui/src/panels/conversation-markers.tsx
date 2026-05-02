// Owns marker grouping, ordering, mounted-slot lookup, and marker chip/nav
// rendering for AgentSessionPanel conversations. Does not own marker fetching,
// mutation requests, overview-rail projection, or transcript virtualization.
// Split out of AgentSessionPanel.tsx during the round-39 marker extraction.

import type { CSSProperties } from "react";

import { DiffNavArrow } from "./DiffPanelIcons";
import type { ConversationMarker, Message } from "../types";

export function groupConversationMarkersByMessageId(
  markers: readonly ConversationMarker[],
) {
  const byMessageId = new Map<string, ConversationMarker[]>();
  markers.forEach((marker) => {
    const bucket = byMessageId.get(marker.messageId);
    if (bucket) {
      bucket.push(marker);
    } else {
      byMessageId.set(marker.messageId, [marker]);
    }
  });
  return byMessageId;
}

export function findMountedConversationMessageSlot(
  messageId: string,
  root: ParentNode = document,
) {
  const expectedItemKey = `message:${messageId}`;
  const candidates = root.querySelectorAll<HTMLElement>(
    "[data-session-search-item-key]",
  );
  for (const candidate of candidates) {
    if (candidate.dataset.sessionSearchItemKey === expectedItemKey) {
      return candidate;
    }
  }
  return null;
}

export function sortConversationMarkersForNavigation(
  markers: readonly ConversationMarker[],
  messages: readonly Message[],
) {
  if (markers.length === 0) {
    return [];
  }
  const messageIndexById = new Map<string, number>();
  messages.forEach((message, index) => {
    messageIndexById.set(message.id, index);
  });
  return [...markers].sort((left, right) => {
    const leftIndex =
      messageIndexById.get(left.messageId) ??
      left.messageIndexHint ??
      Number.MAX_SAFE_INTEGER;
    const rightIndex =
      messageIndexById.get(right.messageId) ??
      right.messageIndexHint ??
      Number.MAX_SAFE_INTEGER;
    if (leftIndex !== rightIndex) {
      return leftIndex - rightIndex;
    }
    const createdOrder = left.createdAt.localeCompare(right.createdAt);
    return createdOrder === 0 ? left.id.localeCompare(right.id) : createdOrder;
  });
}

export function MarkerPlusIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path
        d="M7.25 2.5h1.5v4.75h4.75v1.5H8.75v4.75h-1.5V8.75H2.5v-1.5h4.75Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function ConversationMarkerNavigator({
  markers,
  activeMarkerId,
  onJump,
  onNavigatePrevious,
  onNavigateNext,
}: {
  markers: readonly ConversationMarker[];
  activeMarkerId: string | null;
  onJump: (marker: ConversationMarker) => void;
  onNavigatePrevious: () => void;
  onNavigateNext: () => void;
}) {
  return (
    <nav className="conversation-marker-navigator" aria-label="Conversation markers">
      <div className="conversation-marker-navigator-copy">
        <span className="card-label">Markers</span>
        <span className="conversation-marker-count">{markers.length}</span>
      </div>
      <div className="conversation-marker-nav-controls">
        <button
          type="button"
          className="ghost-button conversation-marker-nav-button"
          aria-label="Previous marker"
          title="Previous marker"
          onClick={onNavigatePrevious}
        >
          <DiffNavArrow direction="up" />
        </button>
        <button
          type="button"
          className="ghost-button conversation-marker-nav-button"
          aria-label="Next marker"
          title="Next marker"
          onClick={onNavigateNext}
        >
          <DiffNavArrow direction="down" />
        </button>
      </div>
      <div className="conversation-marker-list">
        {markers.map((marker) => (
          <ConversationMarkerChip
            key={marker.id}
            marker={marker}
            isActive={marker.id === activeMarkerId}
            onClick={() => onJump(marker)}
          />
        ))}
      </div>
    </nav>
  );
}

export function ConversationMessageMarkers({
  markers,
  activeMarkerId,
  onMarkerClick,
}: {
  markers: readonly ConversationMarker[];
  activeMarkerId: string | null;
  onMarkerClick: (marker: ConversationMarker) => void;
}) {
  return (
    <div className="conversation-message-markers">
      {markers.map((marker) => (
        <ConversationMarkerChip
          key={marker.id}
          marker={marker}
          isActive={marker.id === activeMarkerId}
          onClick={() => onMarkerClick(marker)}
        />
      ))}
    </div>
  );
}

function ConversationMarkerChip({
  marker,
  isActive,
  onClick,
}: {
  marker: ConversationMarker;
  isActive: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`conversation-marker-chip${isActive ? " is-active" : ""}`}
      style={{
        "--conversation-marker-color": marker.color,
      } as CSSProperties}
      title={marker.body ?? marker.name}
      aria-label={`Jump to ${formatConversationMarkerKind(marker.kind)} marker ${marker.name}`}
      onClick={onClick}
    >
      <span className="conversation-marker-chip-swatch" aria-hidden="true" />
      <span className="conversation-marker-chip-name">{marker.name}</span>
      <span className="conversation-marker-chip-kind">
        {formatConversationMarkerKind(marker.kind)}
      </span>
    </button>
  );
}

function formatConversationMarkerKind(kind: ConversationMarker["kind"]) {
  switch (kind) {
    case "checkpoint":
      return "Checkpoint";
    case "decision":
      return "Decision";
    case "review":
      return "Review";
    case "bug":
      return "Bug";
    case "question":
      return "Question";
    case "handoff":
      return "Handoff";
    case "custom":
    default:
      return "Marker";
  }
}
