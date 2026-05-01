// Owns pure conversation-overview projection from transcript messages, optional
// virtualizer layout snapshots, and caller-provided marker metadata. Does not
// own rendering, scroll writes, transcript hydration, marker fetching, or heavy
// Markdown/Mermaid/math parsing. New module for the overview-map foundation.

import type { Message } from "../types";
import { estimateConversationMessageHeight } from "./conversation-virtualization";
import type {
  VirtualizedConversationLayoutMessage,
  VirtualizedConversationLayoutSnapshot,
} from "./VirtualizedConversationMessageList";

export type ConversationOverviewItemKind =
  | "approval"
  | "assistant_text"
  | "command"
  | "diff"
  | "file_changes"
  | "input_request"
  | "parallel_agents"
  | "subagent_result"
  | "thinking"
  | "user_prompt";

export type ConversationOverviewItemStatus =
  | "approval"
  | "error"
  | "running"
  | "success"
  | null;

export type ConversationOverviewMarkerInput = {
  id: string;
  messageId: string;
  name: string;
  color?: string | null;
  kind?: string | null;
};

export type ConversationOverviewMarkerProjection = ConversationOverviewMarkerInput & {
  itemIndex: number;
  mapTopPx: number;
};

export type ConversationOverviewItem = {
  messageId: string;
  messageIndex: number;
  type: Message["type"];
  author: Message["author"];
  kind: ConversationOverviewItemKind;
  status: ConversationOverviewItemStatus;
  estimatedTopPx: number;
  estimatedHeightPx: number;
  measuredHeightPx: number | null;
  measuredPageHeightPx: number | null;
  documentTopPx: number;
  documentHeightPx: number;
  mapTopPx: number;
  mapHeightPx: number;
  markerIds: string[];
  markers: ConversationOverviewMarkerProjection[];
  textSample: string;
};

export type ConversationOverviewProjection = {
  items: ConversationOverviewItem[];
  markers: ConversationOverviewMarkerProjection[];
  sourceHeightPx: number;
  totalHeightPx: number;
  scale: number;
  viewportTopPx: number;
  viewportHeightPx: number;
};

export type BuildConversationOverviewProjectionOptions = {
  messages: readonly Message[];
  layoutSnapshot?: VirtualizedConversationLayoutSnapshot | null;
  markers?: readonly ConversationOverviewMarkerInput[];
  availableWidthPx?: number;
  maxHeightPx?: number;
  minItemHeightPx?: number;
  maxSampleLength?: number;
};

const DEFAULT_AVAILABLE_WIDTH_PX = 760;
const DEFAULT_MAX_HEIGHT_PX = 960;
const DEFAULT_MIN_ITEM_HEIGHT_PX = 2;
const DEFAULT_MAX_SAMPLE_LENGTH = 96;

export function buildConversationOverviewProjection({
  messages,
  layoutSnapshot = null,
  markers = [],
  availableWidthPx = DEFAULT_AVAILABLE_WIDTH_PX,
  maxHeightPx = DEFAULT_MAX_HEIGHT_PX,
  minItemHeightPx = DEFAULT_MIN_ITEM_HEIGHT_PX,
  maxSampleLength = DEFAULT_MAX_SAMPLE_LENGTH,
}: BuildConversationOverviewProjectionOptions): ConversationOverviewProjection {
  const layoutByMessageId = buildLayoutByMessageId(layoutSnapshot);
  const measuredHeightByMessageId = buildMeasuredHeightByMessageId(layoutSnapshot);
  const markersByMessageId = groupMarkersByMessageId(markers);
  const estimatedRows = buildEstimatedRows(messages, {
    availableWidthPx,
    layoutByMessageId,
    measuredHeightByMessageId,
  });
  const sourceHeightPx = Math.max(
    0,
    estimatedRows.reduce((total, row) => total + row.documentHeightPx, 0),
  );
  const boundedMaxHeightPx = Math.max(1, maxHeightPx);
  const scale =
    sourceHeightPx > boundedMaxHeightPx ? boundedMaxHeightPx / sourceHeightPx : 1;
  const totalHeightPx = sourceHeightPx === 0 ? 0 : Math.max(1, sourceHeightPx * scale);

  const projectedMarkers: ConversationOverviewMarkerProjection[] = [];
  const items = estimatedRows.map((row, itemIndex) => {
    const itemMarkers = (markersByMessageId.get(row.message.id) ?? []).map((marker) => {
      const projection = {
        ...marker,
        itemIndex,
        mapTopPx: row.documentTopPx * scale,
      };
      projectedMarkers.push(projection);
      return projection;
    });

    return {
      messageId: row.message.id,
      messageIndex: row.messageIndex,
      type: row.message.type,
      author: row.message.author,
      kind: classifyConversationOverviewItem(row.message),
      status: resolveConversationOverviewStatus(row.message),
      estimatedTopPx: row.estimatedTopPx,
      estimatedHeightPx: row.estimatedHeightPx,
      measuredHeightPx: row.measuredHeightPx,
      measuredPageHeightPx: row.measuredPageHeightPx,
      documentTopPx: row.documentTopPx,
      documentHeightPx: row.documentHeightPx,
      mapTopPx: row.documentTopPx * scale,
      mapHeightPx: Math.max(minItemHeightPx, row.documentHeightPx * scale),
      markerIds: itemMarkers.map((marker) => marker.id),
      markers: itemMarkers,
      textSample: summarizeConversationOverviewMessage(row.message, maxSampleLength),
    };
  });

  return {
    items,
    markers: projectedMarkers,
    sourceHeightPx,
    totalHeightPx,
    scale,
    viewportTopPx: projectViewportTop(layoutSnapshot, sourceHeightPx, scale),
    viewportHeightPx: projectViewportHeight(layoutSnapshot, sourceHeightPx, scale),
  };
}

export function findConversationOverviewItemAtY(
  projection: ConversationOverviewProjection,
  mapY: number,
): ConversationOverviewItem | null {
  if (projection.items.length === 0) {
    return null;
  }

  const boundedY = clamp(mapY, 0, projection.totalHeightPx);
  let nearestItem = projection.items[0];
  let nearestDistance = Number.POSITIVE_INFINITY;

  for (const item of projection.items) {
    const hitTop = item.documentTopPx * projection.scale;
    const hitHeight = item.documentHeightPx * projection.scale;
    const hitBottom = hitTop + hitHeight;
    if (boundedY >= hitTop && boundedY <= hitBottom) {
      return item;
    }

    const itemCenter = hitTop + hitHeight / 2;
    const distance = Math.abs(itemCenter - boundedY);
    if (distance < nearestDistance) {
      nearestDistance = distance;
      nearestItem = item;
    }
  }

  return nearestItem;
}

export function getConversationOverviewItemByMessageId(
  projection: ConversationOverviewProjection,
  messageId: string,
): ConversationOverviewItem | null {
  return projection.items.find((item) => item.messageId === messageId) ?? null;
}

function classifyConversationOverviewItem(
  message: Message,
): ConversationOverviewItemKind {
  switch (message.type) {
    case "text":
      return message.author === "you" ? "user_prompt" : "assistant_text";
    case "thinking":
      return "thinking";
    case "command":
      return "command";
    case "diff":
      return "diff";
    case "markdown":
      return "assistant_text";
    case "parallelAgents":
      return "parallel_agents";
    case "fileChanges":
      return "file_changes";
    case "subagentResult":
      return "subagent_result";
    case "approval":
      return "approval";
    case "userInputRequest":
    case "mcpElicitationRequest":
    case "codexAppRequest":
      return "input_request";
  }
}

function resolveConversationOverviewStatus(
  message: Message,
): ConversationOverviewItemStatus {
  switch (message.type) {
    case "command":
      return message.status;
    case "parallelAgents":
      if (message.agents.some((agent) => agent.status === "error")) {
        return "error";
      }
      if (
        message.agents.some(
          (agent) => agent.status === "initializing" || agent.status === "running",
        )
      ) {
        return "running";
      }
      return message.agents.length > 0 ? "success" : null;
    case "approval":
      switch (message.decision) {
        case "pending":
          return "approval";
        case "accepted":
        case "acceptedForSession":
          return "success";
        case "canceled":
        case "interrupted":
        case "rejected":
          return "error";
      }
      break;
    case "userInputRequest":
    case "mcpElicitationRequest":
    case "codexAppRequest":
      switch (message.state) {
        case "pending":
          return "approval";
        case "submitted":
          return "success";
        case "canceled":
        case "interrupted":
          return "error";
      }
      break;
    case "text":
    case "thinking":
    case "diff":
    case "markdown":
    case "fileChanges":
    case "subagentResult":
      return null;
  }

  return null;
}

function summarizeConversationOverviewMessage(
  message: Message,
  maxSampleLength: number,
): string {
  const sample = normalizeSample(resolveConversationOverviewSampleText(message));
  if (maxSampleLength <= 0 || sample.length <= maxSampleLength) {
    return sample;
  }
  return `${sample.slice(0, Math.max(0, maxSampleLength - 3)).trimEnd()}...`;
}

function resolveConversationOverviewSampleText(message: Message): string {
  switch (message.type) {
    case "text":
      return message.text;
    case "thinking":
      return [message.title, ...message.lines].join(" ");
    case "command":
      return message.command || message.output;
    case "diff":
      return [message.filePath, message.summary].filter(Boolean).join(" ");
    case "markdown":
      return [message.title, message.markdown].filter(Boolean).join(" ");
    case "parallelAgents":
      return message.agents.map((agent) => agent.title).join(" ");
    case "fileChanges":
      return [message.title, ...message.files.map((file) => file.path)].join(" ");
    case "subagentResult":
      return [message.title, message.summary].filter(Boolean).join(" ");
    case "approval":
      return [message.title, message.detail, message.command].filter(Boolean).join(" ");
    case "userInputRequest":
      return [message.title, message.detail].filter(Boolean).join(" ");
    case "mcpElicitationRequest":
      return [message.title, message.detail, message.request.message]
        .filter(Boolean)
        .join(" ");
    case "codexAppRequest":
      return [message.title, message.detail, message.method].filter(Boolean).join(" ");
  }
}

function normalizeSample(sample: string): string {
  return sample.replace(/\s+/g, " ").trim();
}

function buildLayoutByMessageId(
  layoutSnapshot: VirtualizedConversationLayoutSnapshot | null,
) {
  const layoutByMessageId = new Map<string, VirtualizedConversationLayoutMessage>();
  for (const layoutMessage of layoutSnapshot?.messages ?? []) {
    layoutByMessageId.set(layoutMessage.messageId, layoutMessage);
  }
  return layoutByMessageId;
}

function buildMeasuredHeightByMessageId(
  layoutSnapshot: VirtualizedConversationLayoutSnapshot | null,
) {
  const measuredHeightByMessageId = new Map<string, number>();
  const pageGroups = new Map<number, VirtualizedConversationLayoutMessage[]>();

  for (const layoutMessage of layoutSnapshot?.messages ?? []) {
    if (layoutMessage.measuredPageHeightPx === null) {
      continue;
    }
    const pageMessages = pageGroups.get(layoutMessage.pageIndex);
    if (pageMessages) {
      pageMessages.push(layoutMessage);
    } else {
      pageGroups.set(layoutMessage.pageIndex, [layoutMessage]);
    }
  }

  for (const pageMessages of pageGroups.values()) {
    const measuredPageHeightPx = pageMessages[0]?.measuredPageHeightPx;
    if (measuredPageHeightPx === null || measuredPageHeightPx === undefined) {
      continue;
    }
    const estimatedPageHeightPx = pageMessages.reduce(
      (total, message) => total + Math.max(1, message.estimatedHeightPx),
      0,
    );
    if (estimatedPageHeightPx <= 0 || measuredPageHeightPx <= 0) {
      continue;
    }

    for (const message of pageMessages) {
      measuredHeightByMessageId.set(
        message.messageId,
        (Math.max(1, message.estimatedHeightPx) / estimatedPageHeightPx) *
          measuredPageHeightPx,
      );
    }
  }

  return measuredHeightByMessageId;
}

function groupMarkersByMessageId(markers: readonly ConversationOverviewMarkerInput[]) {
  const markersByMessageId = new Map<string, ConversationOverviewMarkerInput[]>();
  for (const marker of markers) {
    const markerGroup = markersByMessageId.get(marker.messageId);
    if (markerGroup) {
      markerGroup.push(marker);
    } else {
      markersByMessageId.set(marker.messageId, [marker]);
    }
  }
  return markersByMessageId;
}

function buildEstimatedRows(
  messages: readonly Message[],
  options: {
    availableWidthPx: number;
    layoutByMessageId: Map<string, VirtualizedConversationLayoutMessage>;
    measuredHeightByMessageId: Map<string, number>;
  },
) {
  let estimatedTopPx = 0;
  let documentTopPx = 0;

  return messages.map((message, messageIndex) => {
    const layoutMessage = options.layoutByMessageId.get(message.id);
    const estimatedHeightPx =
      layoutMessage?.estimatedHeightPx ??
      estimateConversationMessageHeight(message, {
        availableWidthPx: options.availableWidthPx,
      });
    const measuredHeightPx = options.measuredHeightByMessageId.get(message.id) ?? null;
    const measuredPageHeightPx = layoutMessage?.measuredPageHeightPx ?? null;
    const rowEstimatedTopPx = layoutMessage?.estimatedTopPx ?? estimatedTopPx;
    const documentHeightPx = Math.max(1, measuredHeightPx ?? estimatedHeightPx);
    const row = {
      message,
      messageIndex,
      estimatedTopPx: rowEstimatedTopPx,
      estimatedHeightPx,
      measuredHeightPx,
      measuredPageHeightPx,
      documentTopPx,
      documentHeightPx,
    };

    estimatedTopPx = rowEstimatedTopPx + estimatedHeightPx;
    documentTopPx += documentHeightPx;
    return row;
  });
}

function projectViewportTop(
  layoutSnapshot: VirtualizedConversationLayoutSnapshot | null,
  sourceHeightPx: number,
  scale: number,
) {
  if (!layoutSnapshot || sourceHeightPx <= 0 || layoutSnapshot.estimatedTotalHeightPx <= 0) {
    return 0;
  }

  const sourceTopPx =
    (layoutSnapshot.viewportTopPx / layoutSnapshot.estimatedTotalHeightPx) *
    sourceHeightPx;
  return clamp(sourceTopPx, 0, sourceHeightPx) * scale;
}

function projectViewportHeight(
  layoutSnapshot: VirtualizedConversationLayoutSnapshot | null,
  sourceHeightPx: number,
  scale: number,
) {
  if (!layoutSnapshot || sourceHeightPx <= 0 || layoutSnapshot.estimatedTotalHeightPx <= 0) {
    return 0;
  }

  const sourceHeight =
    (layoutSnapshot.viewportHeightPx / layoutSnapshot.estimatedTotalHeightPx) *
    sourceHeightPx;
  return clamp(sourceHeight, 0, sourceHeightPx) * scale;
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max);
}
