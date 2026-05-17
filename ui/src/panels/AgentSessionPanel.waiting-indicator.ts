// Owns: AgentSessionPanel's effective waiting-indicator visibility predicate.
// Does not own: waiting prompt text, pane-level send/delegation decisions, or card rendering.
// Split from: ui/src/panels/AgentSessionPanel.tsx.

import type { Message, Session } from "../types";
import {
  hasAgentOutputAfterLatestUserPrompt,
  hasTurnFinalizingOutputAfterLatestUserPrompt,
} from "../SessionPaneView.waiting-indicator";
import type { WaitingIndicatorKind } from "./AgentSessionPanel.types";

type ShouldShowAgentSessionWaitingIndicatorOptions = {
  showWaitingIndicator: boolean;
  waitingIndicatorKind: WaitingIndicatorKind;
  sessionStatus: Session["status"];
  visibleMessages: readonly Message[];
};

export function shouldShowAgentSessionWaitingIndicator({
  showWaitingIndicator,
  waitingIndicatorKind,
  sessionStatus,
  visibleMessages,
}: ShouldShowAgentSessionWaitingIndicatorOptions) {
  return (
    showWaitingIndicator &&
    (waitingIndicatorKind === "delegationWait" ||
      waitingIndicatorKind === "send" ||
      (sessionStatus === "active" &&
        !hasTurnFinalizingOutputAfterLatestUserPrompt(visibleMessages)) ||
      !hasAgentOutputAfterLatestUserPrompt(visibleMessages))
  );
}
