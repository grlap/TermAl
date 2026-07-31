// Shared transcript paging policy. The browser starts with a small recent tail,
// then fetches one modest older page per explicit scroll/navigation demand.
//
// Backend `/api/sessions/{id}?tail=N` rejects requests above
// `SESSION_TAIL_HYDRATION_MAX_MESSAGES` in `src/state_accessors.rs`; keep this
// window below that cap unless the API contract changes.
export const SESSION_TAIL_WINDOW_MESSAGE_COUNT = 20;
export const SESSION_HISTORY_PAGE_MESSAGE_COUNT = 64;
