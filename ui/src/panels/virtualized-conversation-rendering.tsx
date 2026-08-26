// Owns page-band rendering and height measurement for the virtualized
// conversation list.
// Does not own scroll state, mounted-range scheduling, or layout snapshots.
// Split from: ui/src/panels/VirtualizedConversationMessageList.tsx.
import { memo, useLayoutEffect, useRef } from "react";
import { DeferredHeavyContentActivationProvider } from "../deferred-heavy-content-activation";
import type { ApprovalDecision, JsonValue, McpElicitationAction } from "../types";
import { VIRTUALIZED_MESSAGE_GAP_PX } from "./conversation-virtualization";
import { MessageSlot } from "./session-message-leaves";
import type { MessagePage } from "./virtualized-conversation-measurement";
import type {
  BoundCodexAppRequestSubmitHandler,
  BoundMcpElicitationSubmitHandler,
  BoundUserInputSubmitHandler,
  RenderMessageCard,
} from "./virtualized-conversation-types";

export const MeasuredPageBand = memo(function MeasuredPageBand({
  isActive,
  page,
  preferImmediateHeavyRender,
  deferMeasurementUntilNextFrame,
  allowDeferredHeavyActivation,
  renderMessageCard,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onSearchItemMount,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onHeightChange,
}: {
  isActive: boolean;
  page: MessagePage;
  preferImmediateHeavyRender: boolean;
  deferMeasurementUntilNextFrame: boolean;
  allowDeferredHeavyActivation: boolean;
  renderMessageCard: RenderMessageCard;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey?: string | null;
  onSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: BoundUserInputSubmitHandler;
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler;
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler;
  onHeightChange: (
    pageKey: string,
    pageIndex: number,
    nextHeight: number,
    pageNode?: HTMLElement | null,
    flushLayout?: boolean,
  ) => void;
}) {
  const pageRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = pageRef.current;
    if (!node) {
      return;
    }

    let frameId = 0;
    let pendingFlushLayout = false;
    const measure = (flushLayout = false) => {
      frameId = 0;
      const slotNodes = Array.from(
        node.querySelectorAll<HTMLElement>(".virtualized-message-slot"),
      );
      let totalHeight = 0;
      let measuredSlotCount = 0;
      slotNodes.forEach((slotNode, index) => {
        const slotHeight = Math.max(slotNode.getBoundingClientRect().height, 0);
        totalHeight += slotHeight;
        if (slotHeight > 0) {
          measuredSlotCount += 1;
        }
        if (index < slotNodes.length - 1) {
          totalHeight += VIRTUALIZED_MESSAGE_GAP_PX;
        }
      });
      if (page.hasTrailingGap) {
        totalHeight += VIRTUALIZED_MESSAGE_GAP_PX;
      }
      // Detached / not-yet-laid-out test environments can report zero-height
      // slots while still giving the page its fixed gap total. Treat that as
      // "not measured yet" rather than replacing realistic estimates with a
      // tiny gap-only page height that collapses the whole virtual layout.
      if (measuredSlotCount === 0) {
        return;
      }
      onHeightChange(
        page.key,
        page.pageIndex,
        totalHeight,
        node,
        flushLayout,
      );
    };

    const scheduleMeasure = (
      flushLayout = false,
      waitForPriorPaint = false,
    ) => {
      pendingFlushLayout ||= flushLayout;
      if (frameId !== 0) {
        return;
      }
      frameId = window.requestAnimationFrame(() => {
        frameId = 0;
        if (waitForPriorPaint) {
          // A callback registered from a layout effect runs before the next
          // paint. Queueing the actual measure from that callback guarantees
          // the estimated cold viewport receives one paint first.
          scheduleMeasure();
          return;
        }
        const flushPendingLayout = pendingFlushLayout;
        pendingFlushLayout = false;
        measure(flushPendingLayout);
      });
    };

    if (deferMeasurementUntilNextFrame) {
      // Cold activation already has an estimated bottom viewport. Measuring
      // every mounted card in this layout effect forces the browser to finish
      // the entire transcript layout before the click can paint. Cross one
      // paint boundary, then synchronously commit the refined spacer and
      // anchored bottom in the following frame. The estimated-bottom mount
      // path uses zero page buffers, and the page-height owner flushes only
      // visible/intersecting bands, so this synchronous correction stays
      // bounded to the cold viewport rather than the full transcript.
      scheduleMeasure(true, true);
    } else {
      measure();
    }
    const ResizeObserverCtor = globalThis.ResizeObserver;
    const resizeObserver =
      typeof ResizeObserverCtor === "function"
        ? new ResizeObserverCtor(() => {
            // Page bands report geometry only. The page-height owner resolves
            // bottom-follow, detached anchors, and the user-scroll cooldown
            // together after the coalesced measurement; dispatching a pane
            // repin here would bypass those authority guards.
            scheduleMeasure(true);
          })
        : null;
    resizeObserver?.observe(node);
    Array.from(node.querySelectorAll(".virtualized-message-slot")).forEach((slotNode) => {
      resizeObserver?.observe(slotNode);
    });

    return () => {
      resizeObserver?.disconnect();
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [
    isActive,
    onHeightChange,
    page.hasTrailingGap,
    page.key,
    page.pageIndex,
    deferMeasurementUntilNextFrame,
  ]);

  return (
    <div ref={pageRef} className="virtualized-message-page" data-page-key={page.key}>
      <div className="virtualized-message-range">
        {page.messages.map((message) => (
          <div
            key={message.id}
            className="virtualized-message-slot"
            data-message-id={message.id}
          >
            <MessageSlot
              itemKey={isActive ? `message:${message.id}` : undefined}
              isSearchMatch={conversationSearchMatchedItemKeys.has(`message:${message.id}`)}
              isSearchActive={conversationSearchActiveItemKey === `message:${message.id}`}
              onSearchItemMount={onSearchItemMount}
            >
              <DeferredHeavyContentActivationProvider
                allowActivation={allowDeferredHeavyActivation}
              >
                {renderMessageCard(
                  message,
                  preferImmediateHeavyRender,
                  onApprovalDecision,
                  onUserInputSubmit,
                  onMcpElicitationSubmit,
                  onCodexAppRequestSubmit,
                )}
              </DeferredHeavyContentActivationProvider>
            </MessageSlot>
          </div>
        ))}
      </div>
      {page.hasTrailingGap ? (
        <div
          className="virtualized-message-page-gap"
          style={{ height: VIRTUALIZED_MESSAGE_GAP_PX }}
        />
      ) : null}
    </div>
  );
});
