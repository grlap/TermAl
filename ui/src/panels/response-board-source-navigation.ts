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

        let virtualizerHandle = virtualizerHandleRef.current;
        if (request.sessionId !== sessionId) {
          return;
        }
        let userScrollGeneration =
          virtualizerHandle?.beginUserScrollNavigation() ?? null;
        const tryJoinFreshVirtualizer = () => {
          const currentVirtualizerHandle = virtualizerHandleRef.current;
          if (virtualizerHandle === null && currentVirtualizerHandle !== null) {
            // A short DOM transcript may become virtualized after history is
            // adopted. Join its generation guard only if no scroll/navigation
            // has happened since that fresh handle mounted.
            if (currentVirtualizerHandle.getUserScrollGeneration() !== 0) {
              return false;
            }
            virtualizerHandle = currentVirtualizerHandle;
            userScrollGeneration =
              currentVirtualizerHandle.beginUserScrollNavigation();
          }
          return true;
        };
        const requestIsCurrent = () => {
          if (
            disposed ||
            activeRequestToken !== requestToken ||
            !tryJoinFreshVirtualizer()
          ) {
            return false;
          }
          const currentVirtualizerHandle = virtualizerHandleRef.current;
          if (virtualizerHandle === null) {
            return currentVirtualizerHandle === null;
          }
          return (
            currentVirtualizerHandle === virtualizerHandle &&
            virtualizerHandle.getUserScrollGeneration() ===
              userScrollGeneration
          );
        };

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
          (error) => {
            if (!disposed && activeRequestToken === requestToken) {
              console.warn(
                "Response-board source navigation failed to load history.",
                {
                  error,
                  messageId: request.messageId,
                  sessionId: request.sessionId,
                },
              );
            }
          },
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
