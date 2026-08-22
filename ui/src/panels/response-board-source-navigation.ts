// Owns cancellation of delayed response-board source jumps after history adoption.
// Does not own board request retention, history fetching, or virtualizer scrolling.
// Split from: ui/src/panels/AgentSessionPanel.tsx.
import { useEffect } from "react";

import { subscribeResponseBoardSourceNavigation } from "../response-board-navigation";
import type { VirtualizedConversationMessageListHandleRef } from "./virtualized-conversation-types";

export function useResponseBoardSourceNavigation({
  jumpToMessageId,
  requestHistoryAround,
  sessionId,
  subscriberKey,
  virtualizerHandleRef,
}: {
  jumpToMessageId: (messageId: string) => void;
  requestHistoryAround: (position: number) => Promise<boolean>;
  sessionId: string;
  subscriberKey: object;
  virtualizerHandleRef: VirtualizedConversationMessageListHandleRef;
}) {
  useEffect(() => {
    let activeRequestToken = 0;
    let disposed = false;
    let retryFrame: number | null = null;

    const cancelRetryFrame = () => {
      if (retryFrame !== null) {
        window.cancelAnimationFrame(retryFrame);
        retryFrame = null;
      }
    };

    const unsubscribe = subscribeResponseBoardSourceNavigation(
      sessionId,
      (request) => {
        activeRequestToken += 1;
        const requestToken = activeRequestToken;
        cancelRetryFrame();

        const virtualizerHandle = virtualizerHandleRef.current;
        if (virtualizerHandle === null || request.sessionId !== sessionId) {
          return;
        }
        const userScrollGeneration =
          virtualizerHandle.beginUserScrollNavigation();
        const requestIsCurrent = () =>
          !disposed &&
          activeRequestToken === requestToken &&
          virtualizerHandleRef.current === virtualizerHandle &&
          virtualizerHandle.getUserScrollGeneration() === userScrollGeneration;

        void requestHistoryAround(request.messagePosition).then(
          (accepted) => {
            if (!accepted || !requestIsCurrent()) {
              return;
            }
            retryFrame = window.requestAnimationFrame(() => {
              retryFrame = null;
              if (requestIsCurrent()) {
                jumpToMessageId(request.messageId);
              }
            });
          },
          () => {},
        );
      },
      subscriberKey,
    );

    return () => {
      disposed = true;
      activeRequestToken += 1;
      cancelRetryFrame();
      unsubscribe();
    };
  }, [
    jumpToMessageId,
    requestHistoryAround,
    sessionId,
    subscriberKey,
    virtualizerHandleRef,
  ]);
}
