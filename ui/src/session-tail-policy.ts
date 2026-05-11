// Shared transcript tail-window policy. Keep hydration and initial active-pane
// rendering aligned so partial hydration and visible transcript slicing do not
// disagree about how many recent messages are immediately available.
//
// Backend `/api/sessions/{id}/tail` currently caps requests at
// `SESSION_TAIL_HYDRATION_MAX_MESSAGES` in `src/state_accessors.rs`; keep this
// window below that cap unless the API starts reporting the effective limit.
export const SESSION_TAIL_WINDOW_MESSAGE_COUNT = 20;

// Tail-first hydration adds a round trip before the full transcript fetch, so
// use it only for larger transcripts rather than immediately above the
// `SESSION_TAIL_WINDOW_MESSAGE_COUNT` boundary.
export const SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES = 101;

// The active transcript only starts in render-window mode for substantially
// larger histories. Sessions between the hydration threshold and this render
// threshold still hydrate a small tail first, then render the full visible
// transcript normally once the full session response arrives.
export const SESSION_TAIL_RENDER_MIN_MESSAGES = 512;
