// Owns the bounded retry policy for model-option refreshes deferred by the
// backend lifecycle fence. It does not own request state or UI rendering.

export const SESSION_MODEL_OPTIONS_DEFERRED_RETRY_LIMIT = 6;

const SESSION_MODEL_OPTIONS_DEFERRED_RETRY_BASE_MS = 500;
const SESSION_MODEL_OPTIONS_DEFERRED_RETRY_MAX_MS = 4_000;

export function sessionModelOptionsDeferredRetryDelay(attempt: number): number {
  return Math.min(
    SESSION_MODEL_OPTIONS_DEFERRED_RETRY_BASE_MS * 2 ** Math.max(0, attempt),
    SESSION_MODEL_OPTIONS_DEFERRED_RETRY_MAX_MS,
  );
}
