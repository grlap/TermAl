// Shared transcript tail-window policy. Keep hydration and initial active-pane
// rendering aligned so partial hydration and visible transcript slicing do not
// disagree about how many recent messages are immediately available.
//
// Backend `/api/sessions/{id}?tail=N` rejects requests above
// `SESSION_TAIL_HYDRATION_MAX_MESSAGES` in `src/state_accessors.rs`; keep this
// window below that cap unless the API contract changes.
export const SESSION_TAIL_WINDOW_MESSAGE_COUNT = 20;

// The active transcript only starts in render-window mode for substantially
// larger histories.
export const SESSION_TAIL_RENDER_MIN_MESSAGES = 512;

// Tail-first hydration adds a round trip before the full transcript fetch, so
// use it only when the visible transcript will also render from a tail window.
export const SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES =
  SESSION_TAIL_RENDER_MIN_MESSAGES + 1;
