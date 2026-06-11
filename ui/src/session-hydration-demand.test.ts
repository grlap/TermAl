import { describe, expect, it, vi } from "vitest";

import {
  addSessionFullHydrationDemandListener,
  requestSessionFullHydration,
} from "./session-hydration-demand";

describe("session hydration demand bridge", () => {
  it("replays demand emitted before a listener is registered", () => {
    requestSessionFullHydration("session-1");

    const listener = vi.fn();
    const removeListener = addSessionFullHydrationDemandListener(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith({ sessionId: "session-1" });

    removeListener();
  });

  it("dedupes pending demand for the same session", () => {
    requestSessionFullHydration("session-2");
    requestSessionFullHydration("session-2");

    const listener = vi.fn();
    const removeListener = addSessionFullHydrationDemandListener(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith({ sessionId: "session-2" });

    removeListener();
  });
});
