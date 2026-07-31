import { describe, expect, it, vi } from "vitest";

import {
  addSessionHistoryPageDemandListener,
  completeSessionHistoryPageDemand,
  requestSessionHistoryAroundPage,
  requestSessionHistoryOlderPage,
  requestSessionHistoryPage,
  requestSessionHistoryStartPage,
  requestSessionHistoryTailPage,
} from "./session-history-demand";

describe("session history page demand bridge", () => {
  it("fails completable demand immediately when no owner is mounted", async () => {
    await expect(
      requestSessionHistoryStartPage("session-unmounted"),
    ).resolves.toBe(false);
  });

  it("replays demand emitted before a listener is registered", () => {
    requestSessionHistoryPage("session-1");

    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-1",
      direction: "older",
    });

    removeListener();
  });

  it("dedupes pending demand for the same session", () => {
    requestSessionHistoryPage("session-2");
    requestSessionHistoryPage("session-2");

    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    expect(listener).toHaveBeenCalledTimes(1);
    expect(listener).toHaveBeenCalledWith({
      sessionId: "session-2",
      direction: "older",
    });

    removeListener();
  });

  it("resolves bounded start-page demand after the listener applies it", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryStartPage("session-start");
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-start",
      direction: "start",
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("emits a completable older-page demand for prompt navigation", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryOlderPage("session-older");
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-older",
      direction: "older",
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("emits a completable live-tail reattachment demand", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryTailPage("session-history");
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-history",
      direction: "tail",
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("emits a completable around-position demand", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryAroundPage("session-around", 420);
    const demand = listener.mock.calls[0]?.[0];
    expect(demand).toMatchObject({
      sessionId: "session-around",
      direction: "around",
      position: 420,
    });

    completeSessionHistoryPageDemand(demand?.requestId, true);
    await expect(applied).resolves.toBe(true);

    removeListener();
  });

  it("settles accepted demand when its owner unmounts before completion", async () => {
    const listener = vi.fn();
    const removeListener = addSessionHistoryPageDemandListener(listener);

    const applied = requestSessionHistoryTailPage("session-unmounted-late");
    expect(listener).toHaveBeenCalledTimes(1);

    removeListener();
    await expect(applied).resolves.toBe(false);
  });
});
