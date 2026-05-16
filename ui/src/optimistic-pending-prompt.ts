// Owns local-only pending prompt construction for optimistic sends.
// Does not own send orchestration, backend reconciliation, or queued-prompt
// cancellation. Split from app-session-actions.ts to keep action flow smaller.
import type { DraftImageAttachment } from "./app-utils";
import type { PendingPrompt } from "./types";

let nextOptimisticPromptSequence = 0;

export function createOptimisticPendingPrompt(
  sessionId: string,
  text: string,
  expandedText: string | null,
  attachments: DraftImageAttachment[],
  now: Date = new Date(),
): PendingPrompt {
  nextOptimisticPromptSequence += 1;
  return {
    id: [
      "optimistic-send",
      sessionId,
      now.getTime().toString(36),
      nextOptimisticPromptSequence.toString(36),
    ].join("-"),
    timestamp: now.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    }),
    text,
    expandedText,
    attachments: attachments.map((attachment) => ({
      byteSize: attachment.byteSize,
      fileName: attachment.fileName,
      mediaType: attachment.mediaType,
    })),
    localOnly: true,
  };
}
