import type { ResponseBoardCard } from "./api";

export const RESPONSE_BOARD_MESSAGE_MIME =
  "application/x-termal-response-board-message+json";

export type ResponseBoardInvalidationSource = object;

type ResponseBoardInvalidationListener = (
  source: ResponseBoardInvalidationSource | null,
) => void;

const responseBoardInvalidationListeners =
  new Set<ResponseBoardInvalidationListener>();

export function createResponseBoardInvalidationSource(): ResponseBoardInvalidationSource {
  return {};
}

export function notifyResponseBoardChanged(
  source: ResponseBoardInvalidationSource | null,
) {
  for (const listener of [...responseBoardInvalidationListeners]) {
    listener(source);
  }
}

export function subscribeResponseBoardInvalidation(
  listener: ResponseBoardInvalidationListener,
) {
  responseBoardInvalidationListeners.add(listener);
  return () => {
    responseBoardInvalidationListeners.delete(listener);
  };
}

export type ResponseBoardMessageDrag = {
  sessionId: string;
  messageId: string;
};

export function writeResponseBoardMessageDragData(
  dataTransfer: DataTransfer,
  source: ResponseBoardMessageDrag,
) {
  const normalized = normalizeResponseBoardMessageDrag(source);
  if (!normalized) {
    return false;
  }
  dataTransfer.effectAllowed = "copy";
  dataTransfer.setData(RESPONSE_BOARD_MESSAGE_MIME, JSON.stringify(normalized));
  return true;
}

export function readResponseBoardMessageDragData(
  dataTransfer: DataTransfer,
): ResponseBoardMessageDrag | null {
  const encoded = dataTransfer.getData(RESPONSE_BOARD_MESSAGE_MIME);
  if (!encoded) {
    return null;
  }
  try {
    const parsed: unknown = JSON.parse(encoded);
    if (!isRecord(parsed)) {
      return null;
    }
    const keys = Object.keys(parsed).sort();
    if (keys.length !== 2 || keys[0] !== "messageId" || keys[1] !== "sessionId") {
      return null;
    }
    return normalizeResponseBoardMessageDrag(parsed);
  } catch {
    return null;
  }
}

export function nextResponseBoardCardPosition(
  cards: readonly Pick<ResponseBoardCard, "x" | "y">[],
) {
  for (let index = 0; index <= cards.length; index += 1) {
    const candidate = {
      x: 64 + (index % 4) * 392,
      y: 64 + Math.floor(index / 4) * 452,
    };
    if (
      cards.every(
        (card) =>
          Math.abs(card.x - candidate.x) > 48 ||
          Math.abs(card.y - candidate.y) > 48,
      )
    ) {
      return candidate;
    }
  }
  return { x: 64, y: 64 + Math.ceil(cards.length / 4) * 452 };
}

function normalizeResponseBoardMessageDrag(
  value: unknown,
): ResponseBoardMessageDrag | null {
  if (!isRecord(value)) {
    return null;
  }
  const sessionId = normalizeIdentifier(value.sessionId);
  const messageId = normalizeIdentifier(value.messageId);
  return sessionId && messageId ? { sessionId, messageId } : null;
}

function normalizeIdentifier(value: unknown) {
  if (typeof value !== "string") {
    return null;
  }
  const normalized = value.trim();
  return normalized && normalized.length <= 512 ? normalized : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
