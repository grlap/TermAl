// Owns: deciding which queued prompt rows can still be dispatched, from the
// canonical delivery receipts the backend sends with a session. A row whose
// delivery reached the provider (accepted), may have reached it (ambiguous),
// or provably never will (rejected) is never sent again, so it is delivery
// history rather than queued work. A row with no receipt, or one that is
// pending or still handing off, can still run.
// Does not own: rendering or dismissing rows. It mirrors the backend's
// delivery-history rule (src/prompt_delivery.rs,
// queued_prompt_is_delivery_history and session_has_runnable_queued_prompt),
// which deliberately counts more rows as work than queue selection does.
// New module; not split out of an existing file.
import type { PendingPrompt, PromptDeliveryView } from "./types";

type QueuedPromptSource = {
  pendingPrompts?: PendingPrompt[];
  promptDeliveries?: PromptDeliveryView[];
};

export function queuedPromptCanRun(
  promptId: string,
  deliveries: readonly PromptDeliveryView[] | undefined,
): boolean {
  const disposition = deliveries?.find((delivery) => delivery.promptId === promptId)?.disposition;
  return disposition === undefined || disposition === "pending" || disposition === "handingOff";
}

export function runnableQueuedPrompts(session: QueuedPromptSource): PendingPrompt[] {
  return (session.pendingPrompts ?? []).filter((prompt) =>
    queuedPromptCanRun(prompt.id, session.promptDeliveries),
  );
}
