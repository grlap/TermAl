// Owns public types for the virtualized conversation list.
// Does not own rendering, measurement, or scroll orchestration.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import type {
  ApprovalDecision,
  JsonValue,
  McpElicitationAction,
  Message,
} from "../types";
import type { VirtualizedRange } from "./virtualized-conversation-measurement";

export type UserInputSubmitHandler = (
  sessionId: string,
  messageId: string,
  answers: Record<string, string[]>,
) => void;

export type BoundUserInputSubmitHandler = (
  messageId: string,
  answers: Record<string, string[]>,
) => void;

export type McpElicitationSubmitHandler = (
  sessionId: string,
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

export type BoundMcpElicitationSubmitHandler = (
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

export type CodexAppRequestSubmitHandler = (
  sessionId: string,
  messageId: string,
  result: JsonValue,
) => void;

export type BoundCodexAppRequestSubmitHandler = (
  messageId: string,
  result: JsonValue,
) => void;

export type RenderMessageCard = (
  message: Message,
  preferImmediateHeavyRender: boolean,
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  onUserInputSubmit: BoundUserInputSubmitHandler,
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler,
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler,
) => JSX.Element | null;

export type VirtualizedConversationJumpOptions = {
  // `center` is the default. `flush` is for callers that need the target DOM
  // mounted before the next browser paint.
  align?: "start" | "center" | "end";
  flush?: boolean;
};

export type VirtualizedConversationLayoutMessage = {
  messageId: string;
  messageIndex: number;
  pageIndex: number;
  type: Message["type"];
  author: Message["author"];
  // Estimated document-space top; per-message geometry is not measured.
  estimatedTopPx: number;
  estimatedHeightPx: number;
  // Page-level measured height copied onto each message in the page; null until
  // that page has reported its real DOM height.
  measuredPageHeightPx: number | null;
};

export type VirtualizedConversationLayoutSnapshot = {
  // A point-in-time snapshot for overview/marker navigation. Identity changes
  // as layout and scroll state changes; it is not a subscription object.
  sessionId: string;
  messageCount: number;
  estimatedTotalHeightPx: number;
  viewportTopPx: number;
  viewportHeightPx: number;
  viewportWidthPx: number;
  isActive: boolean;
  visiblePageRange: VirtualizedRange;
  mountedPageRange: VirtualizedRange;
  messages: VirtualizedConversationLayoutMessage[];
};

export type VirtualizedConversationViewportSnapshot = Omit<
  VirtualizedConversationLayoutSnapshot,
  "messages"
> & {
  // Identifies the loaded message window behind a viewport-only snapshot. This
  // prevents overview projections from reusing tail-window translations after
  // the virtualizer moves to a different same-size window.
  windowStartMessageId?: string | null;
  windowEndMessageId?: string | null;
};

export type VirtualizedConversationMessageListHandle = {
  // Stable for the lifetime of the mount. Methods read the latest layout
  // state internally, so consumers can keep the handle as an effect dependency
  // without retriggering on every virtualized layout update.
  getLayoutSnapshot: () => VirtualizedConversationLayoutSnapshot;
  getViewportSnapshot: () => VirtualizedConversationViewportSnapshot;
  // Returns false when the list is inactive, the scroll node is missing, or the
  // target cannot be resolved from the currently loaded transcript.
  jumpToMessageId: (
    messageId: string,
    options?: VirtualizedConversationJumpOptions,
  ) => boolean;
  jumpToMessageIndex: (
    messageIndex: number,
    options?: VirtualizedConversationJumpOptions,
  ) => boolean;
};

export type VirtualizedConversationMessageListHandleRef = {
  // Set while the list is mounted; cleared on unmount.
  current: VirtualizedConversationMessageListHandle | null;
};

export type UserScrollKind = "incremental" | "page_jump" | "seek" | null;

export type MessageWindowSnapshot = {
  ids: string[];
  sessionId: string;
};
