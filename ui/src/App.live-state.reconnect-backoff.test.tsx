// App.live-state.reconnect-backoff.test.tsx
//
// Owns: the /api/state reconnect-fallback backoff across EventSource
// recreation. A stream that never opens recreates its EventSource every few
// seconds; the fallback must settle at its cap instead of restarting at the
// fastest delay on every recreation, must keep polling rather than being
// dropped by each recreation, and must start fast again after a confirmed
// reopen. Also owns stream liveness: an OPEN stream that proved it sends
// server heartbeats and then falls silent is replaced; one that never sent a
// heartbeat (an older backend) is left alone.
//
// Does not own: the other reconnect and fallback flows
// (App.live-state.reconnect.test.tsx) or the watchdogs
// (App.live-state.watchdog.test.tsx).
//
// New file, not split from an existing one; the reconnect file is already
// large.
import { act, cleanup } from "@testing-library/react";
import { forwardRef } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import * as api from "./api";
import {
  RECONNECT_STATE_RESYNC_DELAY_MS,
  RECONNECT_STATE_RESYNC_MAX_DELAY_MS,
} from "./app-shell-internals";
import { setAppTestHooksForTests } from "./app-test-hooks";
import { LIVE_STREAM_SILENCE_RESTART_MS } from "./shared-live-events";
import {
  EventSourceMock,
  ResizeObserverMock,
  createScheduledAnimationFrameMocks,
  flushUiWork,
  jsonResponse,
  latestEventSource,
  makeStateResponse,
  makeWorkspaceLayoutResponse,
  renderApp,
  stubScrollIntoView,
} from "./app-test-harness";

vi.mock("./MonacoDiffEditor", () => ({
  MonacoDiffEditor: forwardRef(function EditorMock() { return <div />; }),
}));
vi.mock("./MonacoCodeEditor", () => ({
  MonacoCodeEditor: forwardRef(function EditorMock() { return <div />; }),
}));

const STEP_MS = 250;

describe("App live state — reconnect fallback backoff", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createScheduledAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    HTMLElement.prototype.scrollTo = vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    stubScrollIntoView();
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(makeWorkspaceLayoutResponse());
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    window.localStorage.clear();
    if (vi.isFakeTimers()) vi.useRealTimers();
    setAppTestHooksForTests(null);
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  /** Serves /api/state at revision 1 and records when each request was made. */
  function stubStateFetch() {
    const stateFetchTimes: number[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/state") {
          stateFetchTimes.push(Date.now());
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [],
              orchestrators: [],
              workspaces: [],
              sessions: [],
            }),
          );
        }
        throw new Error(`Unexpected fetch: ${url}`);
      }),
    );
    return stateFetchTimes;
  }

  async function renderHydrated() {
    await renderApp();
    const eventSource = latestEventSource();
    act(() => {
      eventSource.readyState = 1;
      eventSource.dispatchOpen();
      eventSource.dispatchNamedEvent("state", { revision: 1, projects: [], sessions: [] });
    });
  }

  /**
   * Keeps every EventSource the transport creates permanently CLOSED, as a
   * stream that cannot open does, while time advances.
   */
  async function runNeverOpeningOutage(durationMs: number) {
    const broken = new Set<EventSourceMock>();
    const breakNewSources = () => {
      for (const source of EventSourceMock.instances) {
        if (broken.has(source)) continue;
        broken.add(source);
        act(() => {
          source.readyState = 2;
          source.dispatchError();
        });
      }
    };
    breakNewSources();
    for (let elapsed = 0; elapsed < durationMs; elapsed += STEP_MS) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(STEP_MS);
      });
      breakNewSources();
    }
  }

  function gapsBetween(times: readonly number[]) {
    return times.slice(1).map((time, index) => time - times[index]);
  }

  it("settles the /api/state fallback at the cap across EventSource recreations and keeps polling", async () => {
    vi.useFakeTimers();
    const stateFetchTimes = stubStateFetch();
    await renderHydrated();
    const sourcesBefore = EventSourceMock.instances.length;
    const outageStart = Date.now();

    await runNeverOpeningOutage(40_000);

    // The stream was recreated many times while it stayed closed.
    expect(EventSourceMock.instances.length - sourcesBefore).toBeGreaterThanOrEqual(5);
    const lateFetches = stateFetchTimes.filter((time) => time - outageStart >= 20_000);
    // Still polling: recreation does not drop the pending fallback.
    expect(lateFetches.length).toBeGreaterThanOrEqual(3);
    // At the cap: recreation does not restart the backoff at its fastest
    // delay (which spaced fetches 0.4 to 1.6 s apart).
    for (const gap of gapsBetween(lateFetches)) {
      expect(gap).toBeGreaterThanOrEqual(RECONNECT_STATE_RESYNC_MAX_DELAY_MS - STEP_MS);
    }
  });

  it("resumes a pending fallback deadline across a recreation instead of starting a new delay", async () => {
    vi.useFakeTimers();
    const stateFetchTimes = stubStateFetch();
    await renderHydrated();
    const outageStart = Date.now();

    // The first fetch comes at the fastest delay; its success re-arms the next
    // one at the doubled delay. The EventSource is recreated (500 ms backoff)
    // before that deadline. A recreated transport that took a fresh delay
    // would fetch a full doubled delay after recreation instead.
    await runNeverOpeningOutage(2_500);

    expect(stateFetchTimes[0] - outageStart).toBe(RECONNECT_STATE_RESYNC_DELAY_MS);
    expect(stateFetchTimes[1] - outageStart).toBe(RECONNECT_STATE_RESYNC_DELAY_MS * 3);
  });

  it("resumes a pending fallback when the replacement stream stays CONNECTING without an error", async () => {
    vi.useFakeTimers();
    const stateFetchTimes = stubStateFetch();
    await renderHydrated();
    const outageStart = Date.now();
    const first = latestEventSource();

    // Only the first stream fails. Its replacement stays CONNECTING and never
    // reports an error, so nothing else would re-arm the owed fallback.
    act(() => {
      first.readyState = 2;
      first.dispatchError();
    });
    for (let elapsed = 0; elapsed < 2_500; elapsed += STEP_MS) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(STEP_MS);
      });
      for (const source of EventSourceMock.instances) {
        if (source !== first) source.readyState = 0;
      }
    }

    expect(EventSourceMock.instances.length).toBeGreaterThan(1);
    expect(stateFetchTimes[0] - outageStart).toBe(RECONNECT_STATE_RESYNC_DELAY_MS);
    expect(stateFetchTimes[1] - outageStart).toBe(RECONNECT_STATE_RESYNC_DELAY_MS * 3);
  });

  it("restarts the fallback at the fastest delay after a confirmed reopen", async () => {
    vi.useFakeTimers();
    const stateFetchTimes = stubStateFetch();
    await renderHydrated();
    await runNeverOpeningOutage(20_000);

    // The stream comes back and live data confirms it.
    const recovered = latestEventSource();
    act(() => {
      recovered.readyState = 1;
      recovered.dispatchOpen();
      recovered.dispatchNamedEvent("state", { revision: 2, projects: [], sessions: [] });
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_MAX_DELAY_MS * 2);
    });

    // A bad live event arms the fallback with the current backoff without
    // resetting it, so only the reset at confirmation makes it fast again.
    const fetchesBefore = stateFetchTimes.length;
    const armedAt = Date.now();
    act(() => {
      recovered.dispatchNamedEvent("state", "not json");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(RECONNECT_STATE_RESYNC_DELAY_MS + STEP_MS);
    });
    const next = stateFetchTimes[fetchesBefore];
    expect(next).toBeDefined();
    expect(next - armedAt).toBe(RECONNECT_STATE_RESYNC_DELAY_MS);
  });
});

describe("App live state — stream liveness", () => {
  const originalScrollTo = HTMLElement.prototype.scrollTo;

  beforeEach(() => {
    const { cancelAnimationFrameMock, requestAnimationFrameMock } =
      createScheduledAnimationFrameMocks();
    vi.stubGlobal("requestAnimationFrame", requestAnimationFrameMock);
    vi.stubGlobal("cancelAnimationFrame", cancelAnimationFrameMock);
    vi.stubGlobal("EventSource", EventSourceMock as unknown as typeof EventSource);
    vi.stubGlobal("ResizeObserver", ResizeObserverMock as unknown as typeof ResizeObserver);
    HTMLElement.prototype.scrollTo = vi.fn() as unknown as typeof HTMLElement.prototype.scrollTo;
    EventSourceMock.instances = [];
    stubScrollIntoView();
    vi.spyOn(api, "fetchWorkspaceLayout").mockResolvedValue(null);
    vi.spyOn(api, "fetchWorkspaceLayouts").mockResolvedValue({ workspaces: [] });
    vi.spyOn(api, "saveWorkspaceLayout").mockResolvedValue(makeWorkspaceLayoutResponse());
  });

  afterEach(async () => {
    await act(async () => {
      cleanup();
      await flushUiWork();
    });
    HTMLElement.prototype.scrollTo = originalScrollTo;
    window.localStorage.clear();
    if (vi.isFakeTimers()) vi.useRealTimers();
    setAppTestHooksForTests(null);
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  async function renderOpenStream() {
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      throw new Error(`Unexpected fetch: ${String(input)}`);
    }));
    await renderApp();
    const eventSource = latestEventSource();
    act(() => {
      eventSource.readyState = 1;
      eventSource.dispatchOpen();
      eventSource.dispatchNamedEvent("state", { revision: 1, projects: [], sessions: [] });
    });
    return eventSource;
  }

  it("replaces an OPEN stream that sent heartbeats and then fell silent", async () => {
    vi.useFakeTimers();
    const eventSource = await renderOpenStream();
    act(() => {
      eventSource.dispatchNamedEvent("heartbeat", "1");
    });
    const sourcesBefore = EventSourceMock.instances.length;

    // Still OPEN, but nothing arrives for longer than three missed beats.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LIVE_STREAM_SILENCE_RESTART_MS + 15_000);
    });

    expect(EventSourceMock.instances.length).toBeGreaterThan(sourcesBefore);
  });

  it("keeps a quiet OPEN stream from a server that never sent a heartbeat", async () => {
    vi.useFakeTimers();
    await renderOpenStream();
    const sourcesBefore = EventSourceMock.instances.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(LIVE_STREAM_SILENCE_RESTART_MS * 3);
    });

    expect(EventSourceMock.instances.length).toBe(sourcesBefore);
  });

  it("keeps a stream that delivers again before its silence restart fires", async () => {
    vi.useFakeTimers();
    // Record when the 5 s stream-health watchdog starts, so the test can land
    // a heartbeat between its silence tick and the delayed restart.
    const watchdogStarts: number[] = [];
    const setIntervalReal = window.setInterval.bind(window);
    vi.spyOn(window, "setInterval").mockImplementation(((handler: TimerHandler, delay?: number, ...args: unknown[]) => {
      if (delay === 5000) watchdogStarts.push(Date.now());
      return setIntervalReal(handler, delay, ...args);
    }) as typeof window.setInterval);
    const eventSource = await renderOpenStream();
    act(() => {
      eventSource.dispatchNamedEvent("heartbeat", "1");
    });
    const heartbeatAt = Date.now();
    const sourcesBefore = EventSourceMock.instances.length;
    expect(watchdogStarts).toHaveLength(1);
    const start = watchdogStarts[0];
    const silenceTick = start +
      Math.ceil((heartbeatAt + LIVE_STREAM_SILENCE_RESTART_MS - start) / 5000) * 5000;

    // The 5 s watchdog sees the silence and schedules the delayed restart...
    await act(async () => {
      await vi.advanceTimersByTimeAsync(silenceTick - Date.now());
    });
    // ...and a heartbeat arrives before that restart fires.
    act(() => {
      eventSource.dispatchNamedEvent("heartbeat", "1");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4_000);
    });

    expect(EventSourceMock.instances.length).toBe(sourcesBefore);
  });

  it("still restarts a stream that closed while a silence restart was pending", async () => {
    vi.useFakeTimers();
    const watchdogStarts: number[] = [];
    const setIntervalReal = window.setInterval.bind(window);
    vi.spyOn(window, "setInterval").mockImplementation(((handler: TimerHandler, delay?: number, ...args: unknown[]) => {
      if (delay === 5000) watchdogStarts.push(Date.now());
      return setIntervalReal(handler, delay, ...args);
    }) as typeof window.setInterval);
    const eventSource = await renderOpenStream();
    act(() => {
      eventSource.dispatchNamedEvent("heartbeat", "1");
    });
    const heartbeatAt = Date.now();
    const sourcesBefore = EventSourceMock.instances.length;
    const start = watchdogStarts[0];
    const silenceTick = start +
      Math.ceil((heartbeatAt + LIVE_STREAM_SILENCE_RESTART_MS - start) / 5000) * 5000;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(silenceTick - Date.now());
    });
    // Inside the pending window: one more event, then the stream closes. The
    // CLOSED error's own recovery is absorbed by the pending silence restart.
    act(() => {
      eventSource.dispatchNamedEvent("heartbeat", "1");
      eventSource.readyState = 2;
      eventSource.dispatchError();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(4_000);
    });

    expect(EventSourceMock.instances.length).toBeGreaterThan(sourcesBefore);
  });

  it("keeps an OPEN stream whose heartbeats keep arriving", async () => {
    vi.useFakeTimers();
    const eventSource = await renderOpenStream();
    const sourcesBefore = EventSourceMock.instances.length;

    for (let elapsed = 0; elapsed < LIVE_STREAM_SILENCE_RESTART_MS * 3; elapsed += 15_000) {
      act(() => {
        eventSource.dispatchNamedEvent("heartbeat", "1");
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(15_000);
      });
    }

    expect(EventSourceMock.instances.length).toBe(sourcesBefore);
  });
});
