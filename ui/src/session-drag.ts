export const SESSION_DRAG_MIME_TYPE = "application/x-termal-session";

export type DraggedSessionPayload = {
  sessionId: string;
};

export function attachSessionDragData(
  dataTransfer: Pick<DataTransfer, "setData">,
  sessionId: string,
  label: string,
) {
  const payload = {
    sessionId,
  } satisfies DraggedSessionPayload;

  dataTransfer.setData(SESSION_DRAG_MIME_TYPE, JSON.stringify(payload));
  dataTransfer.setData("text/plain", `TermAl session ${label}`);
}

export function dataTransferHasSessionDragType(
  dataTransfer: Pick<DataTransfer, "types"> | null,
) {
  return Array.from(dataTransfer?.types ?? []).includes(SESSION_DRAG_MIME_TYPE);
}

export function readSessionDragData(
  dataTransfer: Pick<DataTransfer, "getData"> | null,
): DraggedSessionPayload | null {
  const raw = dataTransfer?.getData(SESSION_DRAG_MIME_TYPE) ?? "";
  if (!raw) {
    return null;
  }

  try {
    const parsed: unknown = JSON.parse(raw);
    return isDraggedSessionPayload(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function isDraggedSessionPayload(value: unknown): value is DraggedSessionPayload {
  if (typeof value !== "object" || value === null) {
    return false;
  }

  const sessionId = (value as { sessionId?: unknown }).sessionId;
  return typeof sessionId === "string" && sessionId.trim().length > 0;
}
