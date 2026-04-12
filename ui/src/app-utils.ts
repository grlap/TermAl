import type { WheelEvent as ReactWheelEvent } from "react";
import type {
  AgentCommand,
  ApprovalDecision,
  CommandMessage,
  DiffMessage,
  ImageAttachment,
  Message,
  PendingPrompt,
  Session,
} from "./types";
import type { PaneViewMode, TabDropPlacement } from "./workspace";
import type { GitDiffRequestPayload, GitDiffSection } from "./api";
import { formatUserFacingError } from "./error-messages";

export type SessionFlagMap = Record<string, true | undefined>;

export type SessionAgentCommandMap = Record<string, AgentCommand[] | undefined>;

export type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

export const SUPPORTED_PASTED_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
export const MAX_PASTED_IMAGE_BYTES = 5 * 1024 * 1024;
export const DEFERRED_PREVIEW_LINE_LIMIT = 12;
export const DEFERRED_PREVIEW_CHARACTER_LIMIT = 720;
export const MAX_DEFERRED_PLACEHOLDER_HEIGHT = 960;

export function renderDecision(decision: Exclude<ApprovalDecision, "pending">) {
  switch (decision) {
    case "accepted":
      return "approved";
    case "acceptedForSession":
      return "approved for this session";
    case "canceled":
      return "canceled";
    case "rejected":
      return "rejected";
    case "interrupted":
      return "expired after restart";
  }
}

export function labelForStatus(status: Session["status"]) {
  switch (status) {
    case "active":
      return "Active";
    case "idle":
      return "Idle";
    case "approval":
      return "Awaiting approval";
    case "error":
      return "Error";
  }
}

export function labelForPaneViewMode(viewMode: PaneViewMode) {
  switch (viewMode) {
    case "session":
      return "Session";
    case "prompt":
      return "Prompt";
    case "commands":
      return "Commands";
    case "diffs":
      return "Diffs";
    case "canvas":
      return "Canvas";
    case "controlPanel":
      return "Control panel";
    case "orchestratorList":
      return "Orchestrators";
    case "orchestratorCanvas":
      return "Orchestration";
    case "sessionList":
      return "Sessions";
    case "projectList":
      return "Projects";
    case "source":
      return "Source";
    case "filesystem":
      return "Files";
    case "gitStatus":
      return "Git status";
    case "terminal":
      return "Terminal";
    case "instructionDebugger":
      return "Instructions";
    case "diffPreview":
      return "Diff preview";
  }
}

export function resolvePaneDropPlacementFromPointer(
  rect: DOMRect,
  clientX: number,
  clientY: number,
  allowedPlacements: readonly Exclude<TabDropPlacement, "tabs">[],
): Exclude<TabDropPlacement, "tabs"> {
  if (allowedPlacements.length <= 1) {
    return allowedPlacements[0] ?? "right";
  }

  if (
    allowedPlacements.length === 2 &&
    allowedPlacements.includes("left") &&
    allowedPlacements.includes("right")
  ) {
    return clientX <= rect.left + rect.width / 2 ? "left" : "right";
  }

  const distances: Array<readonly [Exclude<TabDropPlacement, "tabs">, number]> = [
    ["left", Math.max(clientX - rect.left, 0)],
    ["right", Math.max(rect.right - clientX, 0)],
    ["top", Math.max(clientY - rect.top, 0)],
    ["bottom", Math.max(rect.bottom - clientY, 0)],
  ];

  return distances
    .filter(([placement]) => allowedPlacements.includes(placement))
    .sort((left, right) => left[1] - right[1])[0]?.[0] ?? allowedPlacements[0] ?? "right";
}

export function isPointerWithinPaneTopArea(node: HTMLDivElement | null, clientY: number) {
  if (!node) {
    return false;
  }

  const rect = node.getBoundingClientRect();
  return clientY >= rect.top && clientY <= rect.bottom;
}

export function isHexColorDark(value: string) {
  const hex = value.trim().replace(/^#/, "");
  if (hex.length !== 6) {
    return false;
  }

  const red = Number.parseInt(hex.slice(0, 2), 16);
  const green = Number.parseInt(hex.slice(2, 4), 16);
  const blue = Number.parseInt(hex.slice(4, 6), 16);
  const luminance = (red * 299 + green * 587 + blue * 114) / 1000;
  return luminance < 148;
}

export function mapCommandStatus(status: CommandMessage["status"]): Session["status"] {
  switch (status) {
    case "success":
      return "idle";
    case "running":
      return "active";
    case "error":
      return "error";
  }
}

export function buildGitDiffPreviewRequestKey(
  paneId: string,
  request: GitDiffRequestPayload,
  openInNewTab: boolean,
) {
  const baseKey = [
    "git-preview",
    paneId,
    request.workdir,
    request.sectionId,
    request.originalPath ?? "",
    request.path,
  ].join(":");
  return openInNewTab ? `${baseKey}:${crypto.randomUUID()}` : baseKey;
}

export function pendingGitDiffPreviewChangeType(statusCode: string | null | undefined): DiffMessage["changeType"] {
  const normalizedStatusCode = statusCode?.trim().charAt(0) ?? "";
  return normalizedStatusCode === "A" || normalizedStatusCode === "?" ? "create" : "edit";
}

export function pendingGitDiffPreviewSummary(sectionId: GitDiffSection, path: string) {
  return `Loading ${sectionId} changes in ${path}`;
}

export function getErrorMessage(error: unknown) {
  return formatUserFacingError(error);
}

export function readNavigatorOnline() {
  if (typeof navigator === "undefined") {
    return true;
  }

  return navigator.onLine !== false;
}

export function primaryModifierLabel() {
  if (typeof navigator === "undefined") {
    return "Ctrl";
  }

  return navigator.platform.toLowerCase().includes("mac") ? "Cmd" : "Ctrl";
}

export type ConnectionRetryNotice = {
  attemptLabel: string | null;
  detail: string;
};

export function parseConnectionRetryNotice(text: string): ConnectionRetryNotice | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("Connection dropped before the response finished.")) {
    return null;
  }

  const attemptMatch = trimmed.match(/Retrying automatically \(attempt (\d+) of (\d+)\)\.?$/);
  const attemptLabel = attemptMatch ? `Attempt ${attemptMatch[1]} of ${attemptMatch[2]}` : null;

  return {
    attemptLabel,
    detail: trimmed,
  };
}

export function buildMessageListSignature(messages: Message[]) {
  const lastMessage = messages[messages.length - 1];

  return [
    messages.length.toString(),
    lastMessage?.id ?? "no-message",
    lastMessage ? messageChangeMarker(lastMessage) : "empty",
  ].join("|");
}

export function buildSessionConversationSignature(session: Session) {
  const messages = session.messages;
  const pendingPrompts = session.pendingPrompts ?? [];
  const lastMessage = messages[messages.length - 1];
  const lastPendingPrompt = pendingPrompts[pendingPrompts.length - 1];

  return [
    (messages.length + pendingPrompts.length).toString(),
    lastMessage?.id ?? "no-message",
    lastMessage ? messageChangeMarker(lastMessage) : "empty",
    lastPendingPrompt?.id ?? "no-pending-prompt",
    lastPendingPrompt ? pendingPromptChangeMarker(lastPendingPrompt) : "empty",
  ].join("|");
}

export function messageChangeMarker(message: Message) {
  switch (message.type) {
    case "text":
      return `${message.type}:${message.text.length}:${message.attachments?.length ?? 0}`;
    case "thinking":
      return `${message.type}:${message.lines.length}:${message.title.length}`;
    case "command":
      return `${message.type}:${message.status}:${message.output.length}`;
    case "diff":
      return `${message.type}:${message.filePath}:${message.diff.length}`;
    case "markdown":
      return `${message.type}:${message.title.length}:${message.markdown.length}`;
    case "parallelAgents":
      return `${message.type}:${message.agents.length}:${message.agents
        .map((agent) => `${agent.id}:${agent.status}:${agent.detail?.length ?? 0}`)
        .join("|")}`;
    case "fileChanges":
      return `${message.type}:${message.files.length}:${message.files
        .map((file) => `${file.kind}:${file.path}`)
        .join("|")}`;
    case "subagentResult":
      return `${message.type}:${message.title.length}:${message.summary.length}`;
    case "approval":
      return `${message.type}:${message.decision}:${message.command.length}`;
  }
}

export function pendingPromptChangeMarker(prompt: PendingPrompt) {
  return `${prompt.text.length}:${prompt.attachments?.length ?? 0}`;
}

export function collectCandidateSourcePaths(session: Session) {
  const paths = session.messages
    .filter((message): message is DiffMessage => message.type === "diff")
    .map((message) => message.filePath);

  return Array.from(new Set(paths));
}

export function findLastUserPrompt(session: Session) {
  for (let index = session.messages.length - 1; index >= 0; index -= 1) {
    const message = session.messages[index];
    if (message.type === "text" && message.author === "you") {
      const prompt = message.text.trim();
      if (prompt) {
        return prompt;
      }
    }
  }

  return null;
}

export function setSessionFlag(current: SessionFlagMap, sessionId: string, value: boolean): SessionFlagMap {
  if (value) {
    return {
      ...current,
      [sessionId]: true,
    };
  }

  if (!current[sessionId]) {
    return current;
  }

  const next = { ...current };
  delete next[sessionId];
  return next;
}

export function pruneSessionValues<T>(
  current: Record<string, T>,
  availableSessionIds: Set<string>,
  invalidSessionIds?: Set<string>,
): Record<string, T> {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    return availableSessionIds.has(sessionId) && !invalidSessionIds?.has(sessionId);
  });
  return Object.fromEntries(nextEntries) as Record<string, T>;
}

export function pruneSessionCommandValues(
  current: SessionAgentCommandMap,
  availableSessionIds: Set<string>,
  invalidSessionIds: Set<string>,
): SessionAgentCommandMap {
  return pruneSessionValues(current, availableSessionIds, invalidSessionIds);
}

export function pruneSessionAttachmentValues(
  current: Record<string, DraftImageAttachment[]>,
  availableSessionIds: Set<string>,
): Record<string, DraftImageAttachment[]> {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    const keep = availableSessionIds.has(sessionId);
    if (!keep) {
      releaseDraftAttachments(current[sessionId] ?? []);
    }
    return keep;
  });
  return Object.fromEntries(nextEntries);
}

export function pruneSessionFlags(current: SessionFlagMap, availableSessionIds: Set<string>): SessionFlagMap {
  const nextEntries = Object.entries(current).filter(([sessionId]) => availableSessionIds.has(sessionId));
  return Object.fromEntries(nextEntries);
}

export function pruneSessionFlagsWithInvalidation(
  current: SessionFlagMap,
  availableSessionIds: Set<string>,
  invalidSessionIds: Set<string>,
): SessionFlagMap {
  const nextEntries = Object.entries(current).filter(([sessionId]) => {
    return availableSessionIds.has(sessionId) && !invalidSessionIds.has(sessionId);
  });
  return Object.fromEntries(nextEntries);
}

export function removeQueuedPromptFromSessions(
  sessions: Session[],
  sessionId: string,
  promptId: string,
): Session[] {
  return sessions.map((session) => {
    if (session.id !== sessionId || !session.pendingPrompts?.length) {
      return session;
    }

    const pendingPrompts = session.pendingPrompts.filter((prompt) => prompt.id !== promptId);
    if (pendingPrompts.length === session.pendingPrompts.length) {
      return session;
    }

    if (pendingPrompts.length === 0) {
      const { pendingPrompts: _discard, ...rest } = session;
      return rest;
    }

    return {
      ...session,
      pendingPrompts,
    };
  });
}

export function releaseDraftAttachments(attachments: DraftImageAttachment[]) {
  for (const attachment of attachments) {
    URL.revokeObjectURL(attachment.previewUrl);
  }
}

export function collectClipboardImageFiles(clipboardData: DataTransfer | null): File[] {
  if (!clipboardData) {
    return [];
  }

  return Array.from(clipboardData.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null && file.type.startsWith("image/"));
}

export async function createDraftAttachmentsFromFiles(files: File[]) {
  const attachments: DraftImageAttachment[] = [];
  const errors: string[] = [];

  for (const [index, file] of files.entries()) {
    try {
      attachments.push(await createDraftAttachment(file, index));
    } catch (error) {
      errors.push(getErrorMessage(error));
    }
  }

  return { attachments, errors };
}

export async function createDraftAttachment(file: File, index: number): Promise<DraftImageAttachment> {
  const mediaType = file.type.trim().toLowerCase();
  if (!SUPPORTED_PASTED_IMAGE_TYPES.has(mediaType)) {
    throw new Error(`Unsupported pasted image type \`${mediaType || "unknown"}\`.`);
  }

  if (file.size > MAX_PASTED_IMAGE_BYTES) {
    throw new Error(
      `Pasted image exceeds the ${Math.round(MAX_PASTED_IMAGE_BYTES / (1024 * 1024))} MB limit.`,
    );
  }

  const dataUrl = await readFileAsDataUrl(file);
  const [, base64Data = ""] = dataUrl.split(",", 2);
  if (!base64Data) {
    throw new Error("Failed to read pasted image data.");
  }

  const fileName = file.name.trim() || defaultDraftAttachmentFileName(index, mediaType);

  return {
    id: crypto.randomUUID(),
    previewUrl: URL.createObjectURL(file),
    base64Data,
    byteSize: file.size,
    fileName,
    mediaType,
  };
}

export function readFileAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => {
      reject(new Error(`Failed to read pasted image \`${file.name || "image"}\`.`));
    };
    reader.onload = () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error(`Failed to decode pasted image \`${file.name || "image"}\`.`));
      }
    };
    reader.readAsDataURL(file);
  });
}

export function defaultDraftAttachmentFileName(index: number, mediaType: string) {
  const extension = mediaType === "image/png"
    ? "png"
    : mediaType === "image/jpeg"
      ? "jpg"
      : mediaType === "image/gif"
        ? "gif"
        : mediaType === "image/webp"
          ? "webp"
          : "img";

  return `pasted-image-${index + 1}.${extension}`;
}

export function imageAttachmentSummaryLabel(count: number) {
  return count === 1 ? "1 image attached" : `${count} images attached`;
}

export function formatByteSize(byteSize: number) {
  if (byteSize < 1024) {
    return `${byteSize} B`;
  }

  if (byteSize < 1024 * 1024) {
    return `${(byteSize / 1024).toFixed(1)} KB`;
  }

  return `${(byteSize / (1024 * 1024)).toFixed(1)} MB`;
}

export function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}

export function normalizeWheelDelta(
  event: Pick<ReactWheelEvent<HTMLElement>, "deltaMode" | "deltaY">,
  container: HTMLElement,
) {
  if (event.deltaMode === 1) {
    const computedLineHeight = Number.parseFloat(window.getComputedStyle(container).lineHeight);
    const lineHeight = Number.isFinite(computedLineHeight) ? computedLineHeight : 16;
    return event.deltaY * lineHeight;
  }

  if (event.deltaMode === 2) {
    return event.deltaY * Math.max(container.clientHeight, 1);
  }

  return event.deltaY;
}

export function canNestedScrollableConsumeWheel(
  target: EventTarget | null,
  container: HTMLElement,
  deltaY: number,
) {
  let current = target instanceof HTMLElement ? target : null;

  while (current && current !== container) {
    const style = window.getComputedStyle(current);
    const overflowY = style.overflowY;
    const canScrollY =
      (overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay") &&
      current.scrollHeight > current.clientHeight + 1;

    if (canScrollY) {
      const maxScrollTop = current.scrollHeight - current.clientHeight;
      if (deltaY < 0 && current.scrollTop > 0) {
        return true;
      }
      if (deltaY > 0 && current.scrollTop < maxScrollTop) {
        return true;
      }
    }

    current = current.parentElement;
  }

  return false;
}

export function dropLabelForPlacement(placement: TabDropPlacement) {
  switch (placement) {
    case "tabs":
      return "Tabs";
    case "left":
      return "Left";
    case "right":
      return "Right";
    case "top":
      return "Top";
    case "bottom":
      return "Bottom";
  }
}
