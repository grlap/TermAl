import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ApiRequestError, fetchCoordinationBoard } from "../api";
import type { BoardEntry, BoardListPage } from "../types";
import { BoardPanel, formatBoardRelativeTime } from "./BoardPanel";
import {
  BOARD_BACKGROUND_PAGINATION_RESTART_LIMIT,
  BOARD_BACKGROUND_RETRY_BASE_MS,
  BOARD_BACKGROUND_RETRY_MAX_MS,
  BOARD_CHANGE_HIGHLIGHT_MS,
  BOARD_LIVE_POLL_INTERVAL_MS,
  BOARD_STALE_CLAIM_MAX_MS,
  BOARD_STALE_CLAIM_MS,
  boardBackgroundRetryDelay,
  boardStaleClaimWindow,
} from "./board-panel-polling";

vi.mock("../api", async () => {
  const { ApiRequestError } = await import("../api-request");
  return {
    ApiRequestError,
    fetchCoordinationBoard: vi.fn(),
  };
});

const fetchCoordinationBoardMock = vi.mocked(fetchCoordinationBoard);

function boardEntry(key: string, value: unknown, revision = 1): BoardEntry {
  return {
    key,
    revision,
    updatedAtGeneration: revision,
    value,
    deleted: false,
    authorSessionId: "session-630",
    authorName: "Termal::Codex",
    updatedAt: "2026-07-26T15:00:00.000Z",
    stateStamp: "gate-round-5",
  };
}

function boardPage(
  entries: BoardEntry[],
  generation: number,
  options?: { unchanged?: boolean; nextAfterKey?: string | null },
): BoardListPage {
  return {
    generation,
    entries,
    nextAfterKey: options?.nextAfterKey ?? null,
    unchanged: options?.unchanged ?? false,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

beforeEach(() => {
  fetchCoordinationBoardMock.mockReset();
});

describe("BoardPanel", () => {
  it("renders fetched entries with key, value, revision, and author", async () => {
    fetchCoordinationBoardMock.mockResolvedValue(
      boardPage([boardEntry("activity.rust-suite", { holder: "Fable" }, 4)], 9),
    );

    render(<BoardPanel sessionId="session-2098" />);

    await waitFor(() => {
      expect(screen.getByText("activity.rust-suite")).toBeInTheDocument();
    });
    expect(fetchCoordinationBoardMock).toHaveBeenCalledWith("session-2098", {
      knownGeneration: undefined,
      signal: expect.any(AbortSignal),
    });
    expect(screen.getByText(/"holder": "Fable"/)).toBeInTheDocument();
    expect(screen.getByText("r4")).toBeInTheDocument();
    expect(
      screen.getByLabelText(
        "Per-key revision 4; CAS token for expectedRevision",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("Termal::Codex")).toBeInTheDocument();
    expect(screen.getByText("gen 9")).toBeInTheDocument();
    expect(screen.getByText("live")).toBeInTheDocument();
    expect(screen.getByText("gate-round-5")).toBeInTheDocument();
  });

  it("shows a loading state, not the empty state, before the first response", async () => {
    const first = deferred<BoardListPage>();
    fetchCoordinationBoardMock.mockReturnValue(first.promise);

    render(<BoardPanel sessionId="session-2098" />);

    expect(screen.getByText("Loading board…")).toBeInTheDocument();
    expect(screen.queryByText(/Nothing published yet/)).not.toBeInTheDocument();
    expect(screen.queryByRole("list")).not.toBeInTheDocument();

    await act(async () => {
      first.resolve(boardPage([], 0));
    });
    expect(screen.getByText(/Nothing published yet/)).toBeInTheDocument();
    expect(screen.getByText(/never trigger wake-ups/)).toBeInTheDocument();
    expect(screen.queryByRole("list")).not.toBeInTheDocument();
  });

  it("fetches every page under one snapshot generation — no silent truncation", async () => {
    fetchCoordinationBoardMock
      .mockResolvedValueOnce(
        boardPage([boardEntry("a.first", 1)], 7, { nextAfterKey: "a.first" }),
      )
      .mockResolvedValueOnce(
        boardPage([boardEntry("b.second", 2)], 7, { nextAfterKey: "b.second" }),
      )
      .mockResolvedValueOnce(boardPage([boardEntry("c.third", 3)], 7));

    render(<BoardPanel sessionId="session-2098" />);

    await waitFor(() => {
      expect(screen.getByText("c.third")).toBeInTheDocument();
    });
    expect(screen.getByText("a.first")).toBeInTheDocument();
    expect(screen.getByText("b.second")).toBeInTheDocument();
    expect(fetchCoordinationBoardMock).toHaveBeenNthCalledWith(
      2,
      "session-2098",
      {
        afterKey: "a.first",
        snapshotGeneration: 7,
        signal: expect.any(AbortSignal),
      },
    );
    expect(fetchCoordinationBoardMock).toHaveBeenNthCalledWith(
      3,
      "session-2098",
      {
        afterKey: "b.second",
        snapshotGeneration: 7,
        signal: expect.any(AbortSignal),
      },
    );
  });

  it("restarts the listing when a write lands between pages", async () => {
    fetchCoordinationBoardMock
      .mockResolvedValueOnce(
        boardPage([boardEntry("a.first", 1)], 7, { nextAfterKey: "a.first" }),
      )
      .mockRejectedValueOnce(
        new ApiRequestError(
          "request-failed",
          "coordination board scope changed during pagination",
          { status: 409 },
        ),
      )
      .mockResolvedValueOnce(
        boardPage([boardEntry("a.first", 1, 2)], 8, {
          nextAfterKey: "a.first",
        }),
      )
      .mockResolvedValueOnce(boardPage([boardEntry("z.last", 9)], 8));

    render(<BoardPanel sessionId="session-2098" />);

    await waitFor(() => {
      expect(screen.getByText("z.last")).toBeInTheDocument();
    });
    expect(screen.getByText("gen 8")).toBeInTheDocument();
    // Restart re-listed from the first page rather than surfacing an error.
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("surfaces a bounded error after pagination restart exhaustion", async () => {
    fetchCoordinationBoardMock.mockImplementation(
      async (_sessionId, options) => {
        if (options?.afterKey) {
          throw new ApiRequestError(
            "request-failed",
            "coordination board scope changed during pagination",
            { status: 409 },
          );
        }
        return boardPage([boardEntry("a.first", 1)], 7, {
          nextAfterKey: "a.first",
        });
      },
    );

    render(<BoardPanel sessionId="session-2098" />);

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(
        "The board kept changing while it was read. Refresh again when writes settle.",
      );
    });
    expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(12);
  });

  it("passes the rendered generation as knownGeneration and keeps entries on unchanged", async () => {
    fetchCoordinationBoardMock.mockResolvedValueOnce(
      boardPage([boardEntry("freeze.fingerprint", "abc123")], 5),
    );

    render(<BoardPanel sessionId="session-2098" />);
    await waitFor(() => {
      expect(screen.getByText("freeze.fingerprint")).toBeInTheDocument();
    });

    fetchCoordinationBoardMock.mockResolvedValueOnce(
      boardPage([], 5, { unchanged: true }),
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
    });

    expect(fetchCoordinationBoardMock).toHaveBeenLastCalledWith(
      "session-2098",
      {
        knownGeneration: 5,
        signal: expect.any(AbortSignal),
      },
    );
    // unchanged short-circuit must NOT clear the rendered entries.
    expect(screen.getByText("freeze.fingerprint")).toBeInTheDocument();
  });

  it("ignores a stale response after sessionId changes", async () => {
    const stale = deferred<BoardListPage>();
    fetchCoordinationBoardMock.mockReturnValueOnce(stale.promise);

    const { rerender } = render(<BoardPanel sessionId="session-old" />);

    fetchCoordinationBoardMock.mockResolvedValueOnce(
      boardPage([boardEntry("new.project", "fresh")], 3),
    );
    rerender(<BoardPanel sessionId="session-new" />);

    await waitFor(() => {
      expect(screen.getByText("new.project")).toBeInTheDocument();
    });

    // The old session's response arrives late and must be discarded.
    await act(async () => {
      stale.resolve(boardPage([boardEntry("old.project", "stale")], 99));
    });
    expect(screen.queryByText("old.project")).not.toBeInTheDocument();
    expect(screen.getByText("new.project")).toBeInTheDocument();
    expect(screen.getByText("gen 3")).toBeInTheDocument();
  });

  it("hides the previous project's entries on the first render after a session change", async () => {
    fetchCoordinationBoardMock.mockResolvedValueOnce(
      boardPage([boardEntry("old.project", "stale")], 2),
    );
    const { rerender } = render(<BoardPanel sessionId="session-old" />);
    await waitFor(() => {
      expect(screen.getByText("old.project")).toBeInTheDocument();
    });

    const nextProject = deferred<BoardListPage>();
    fetchCoordinationBoardMock.mockReturnValueOnce(nextProject.promise);
    rerender(<BoardPanel sessionId="session-new" />);

    expect(screen.queryByText("old.project")).not.toBeInTheDocument();
    expect(screen.queryByText("gen 2")).not.toBeInTheDocument();
    expect(screen.getByText("Loading board…")).toBeInTheDocument();

    await act(async () => {
      nextProject.resolve(boardPage([boardEntry("new.project", "fresh")], 3));
    });
    expect(screen.getByText("new.project")).toBeInTheDocument();
    expect(screen.queryByText("old.project")).not.toBeInTheDocument();
  });

  it("surfaces fetch failures as an alert without crashing", async () => {
    fetchCoordinationBoardMock.mockRejectedValue(
      new Error("session must be a local root session"),
    );

    render(<BoardPanel sessionId="session-child" />);

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(
        "session must be a local root session",
      );
    });
  });

  it("shows the backend's safe-retry guidance for transient board contention", async () => {
    fetchCoordinationBoardMock.mockRejectedValue(
      new ApiRequestError(
        "request-failed",
        "coordination board storage is temporarily busy; no mutation was attempted by this read operation, so retry the same request",
        { status: 503 },
      ),
    );

    render(<BoardPanel sessionId="session-root" />);

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(
        "no mutation was attempted by this read operation, so retry the same request",
      );
    });
    expect(screen.getByRole("alert")).not.toHaveTextContent(
      "The TermAl backend is unavailable.",
    );
  });
});

describe("BoardPanel visibility", () => {
  it("live-polls on the cheap unchanged probe while mounted", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([boardEntry("gates.union", "green", 2)], 4),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 4, { unchanged: true }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(fetchCoordinationBoardMock).toHaveBeenLastCalledWith(
        "session-2098",
        {
          knownGeneration: 4,
          signal: expect.any(AbortSignal),
        },
      );
      // The unchanged tick keeps the rendered facts.
      expect(screen.getByText("gates.union")).toBeInTheDocument();

      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("highlights entries that changed in the newest observed writes, then clears", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage(
          [
            { ...boardEntry("stable.key", "old", 1), updatedAtGeneration: 2 },
            { ...boardEntry("moving.key", "v1", 1), updatedAtGeneration: 3 },
          ],
          3,
        ),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      // First load is not "change" — nothing highlighted.
      expect(screen.queryByText("updated")).not.toBeInTheDocument();

      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage(
          [
            { ...boardEntry("stable.key", "old", 1), updatedAtGeneration: 2 },
            { ...boardEntry("moving.key", "v2", 2), updatedAtGeneration: 5 },
          ],
          5,
        ),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      // Only the entry whose updatedAtGeneration moved past the previous
      // rendered generation carries the badge.
      const updatedBadge = screen.getByText("updated");
      expect(updatedBadge).toBeInTheDocument();
      expect(updatedBadge.closest("li")).toHaveTextContent(
        "moving.key",
      );

      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 5, { unchanged: true }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_CHANGE_HIGHLIGHT_MS + 50);
      });
      expect(screen.queryByText("updated")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("tells an active-but-empty board apart from a never-used one", async () => {
    fetchCoordinationBoardMock.mockResolvedValue(boardPage([], 69));

    render(<BoardPanel sessionId="session-2098" />);

    await waitFor(() => {
      expect(screen.getByText(/board has seen 69 writes/)).toBeInTheDocument();
    });
    expect(screen.queryByText(/Nothing published yet/)).not.toBeInTheDocument();
  });
});

describe("BoardPanel polling discipline", () => {
  it("coalesces poll ticks behind a slow in-flight request instead of starving it", async () => {
    vi.useFakeTimers();
    try {
      const slow = deferred<BoardListPage>();
      fetchCoordinationBoardMock.mockReturnValue(slow.promise);
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // A poll interval passes while the first request is still in flight
      // and still inside the stale-claim window: the tick must coalesce,
      // not invalidate. (Past BOARD_STALE_CLAIM_MS supersession takes over —
      // covered by the liveness test.)
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // The slow response still APPLIES — the panel was never starved.
      await act(async () => {
        slow.resolve(boardPage([boardEntry("slow.fact", "arrived", 1)], 12));
      });
      expect(screen.getByText("slow.fact")).toBeInTheDocument();
      expect(screen.getByText("gen 12")).toBeInTheDocument();

      // With the guard released, the next tick polls again.
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 12, { unchanged: true }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps background polls silent: no loading flash, empty state stays honest", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValue(boardPage([], 69));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText(/board has seen 69 writes/)).toBeInTheDocument();

      // A slow BACKGROUND probe must not flip the UI into loading or flicker
      // the manual button into a foreground-pending state.
      const slowProbe = deferred<BoardListPage>();
      fetchCoordinationBoardMock.mockReturnValue(slowProbe.promise);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(screen.getByText(/board has seen 69 writes/)).toBeInTheDocument();
      expect(screen.queryByText("Loading board…")).not.toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();

      await act(async () => {
        slowProbe.resolve(boardPage([], 69, { unchanged: true }));
      });
      expect(screen.getByText(/board has seen 69 writes/)).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("skips poll ticks entirely while the document is hidden", async () => {
    vi.useFakeTimers();
    let hidden = false;
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => hidden,
    });
    try {
      fetchCoordinationBoardMock.mockResolvedValue(boardPage([], 3));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      hidden = true;
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      hidden = false;
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 3, { unchanged: true }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
    } finally {
      // The own-property delete restores the prototype getter; there is no
      // prototype mutation to undo.
      Reflect.deleteProperty(document, "hidden");
      vi.useRealTimers();
    }
  });
});

describe("BoardPanel guard ownership", () => {
  it("a superseded session's late settle cannot release the new session's claim", async () => {
    vi.useFakeTimers();
    try {
      const slowA = deferred<BoardListPage>();
      fetchCoordinationBoardMock.mockReturnValueOnce(slowA.promise);
      const { rerender } = render(<BoardPanel sessionId="session-a" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // Switch to B while A is still pending: B's initial load starts.
      const slowB = deferred<BoardListPage>();
      fetchCoordinationBoardMock.mockReturnValueOnce(slowB.promise);
      rerender(<BoardPanel sessionId="session-b" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      // A settles LATE. Its finally must not free B's claim...
      await act(async () => {
        slowA.resolve(boardPage([boardEntry("a.stale", "old", 1)], 99));
      });
      // ...so a poll tick while B is still pending must NOT start a third
      // request.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      // And A's stale payload never rendered.
      expect(screen.queryByText("a.stale")).not.toBeInTheDocument();

      // B settles and applies; the guard is properly released, so the next
      // tick polls again.
      await act(async () => {
        slowB.resolve(boardPage([boardEntry("b.fresh", "new", 1)], 5));
      });
      expect(screen.getByText("b.fresh")).toBeInTheDocument();
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 5, { unchanged: true }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("clears background stale state when the session scope changes", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock
        .mockResolvedValueOnce(boardPage([], 2))
        .mockRejectedValueOnce(new Error("session A unavailable"))
        .mockResolvedValueOnce(
          boardPage([boardEntry("session-b.fact", "fresh", 1)], 1),
        );
      const { rerender } = render(<BoardPanel sessionId="session-a" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(screen.getByText("stale")).toBeInTheDocument();

      rerender(<BoardPanel sessionId="session-b" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      expect(screen.getByText("session-b.fact")).toBeInTheDocument();
      expect(screen.getByText("live")).toBeInTheDocument();
      expect(screen.queryByText("stale")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("formatBoardRelativeTime", () => {
  const now = Date.parse("2026-07-27T12:00:00.000Z");

  it("covers every boundary against an injected clock", () => {
    expect(formatBoardRelativeTime("2026-07-27T11:59:00.001Z", now)).toBe(
      "just now",
    );
    expect(formatBoardRelativeTime("2026-07-27T11:59:00.000Z", now)).toBe(
      "1m ago",
    );
    expect(formatBoardRelativeTime("2026-07-27T11:01:00.000Z", now)).toBe(
      "59m ago",
    );
    expect(formatBoardRelativeTime("2026-07-27T11:00:00.000Z", now)).toBe(
      "1h ago",
    );
    expect(formatBoardRelativeTime("2026-07-26T12:00:00.001Z", now)).toBe(
      "23h ago",
    );
    const oneDayAgo = "2026-07-26T12:00:00.000Z";
    expect(formatBoardRelativeTime(oneDayAgo, now)).toBe(
      new Date(oneDayAgo).toLocaleDateString(),
    );
  });

  it("passes unparseable input through verbatim", () => {
    expect(formatBoardRelativeTime("not-a-date", now)).toBe("not-a-date");
  });
});

describe("BoardPanel liveness", () => {
  it("uses monotonic request age when the wall clock jumps", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-27T12:00:00.000Z"));
    try {
      const slow = deferred<BoardListPage>();
      fetchCoordinationBoardMock.mockReturnValueOnce(slow.promise);
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      // A wall-clock correction must not make an 8-second request look older
      // than the 16-second stale threshold.
      vi.setSystemTime(new Date("2026-07-28T12:00:00.000Z"));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      await act(async () => {
        slow.resolve(boardPage([], 1));
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("relative ages keep aging across unchanged polls on an idle board", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-27T12:00:00.000Z"));
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage(
          [
            {
              ...boardEntry("quiet.fact", "still", 1),
              updatedAt: "2026-07-27T11:59:55.000Z",
            },
          ],
          4,
        ),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText("just now")).toBeInTheDocument();

      // Nine unchanged polls carry us ~72s past the write. Without the
      // per-probe re-render tick, React's same-value bailout would freeze
      // the label at "just now" forever (review 2026-07-27, Medium).
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 4, { unchanged: true }),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(
          BOARD_LIVE_POLL_INTERVAL_MS * 9 + 100,
        );
      });
      expect(screen.queryByText("just now")).not.toBeInTheDocument();
      expect(screen.getAllByText("1m ago").length).toBeGreaterThan(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps relative ages moving while a slow background probe coalesces ticks", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-27T12:00:00.000Z"));
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage(
          [
            {
              ...boardEntry("slow.clock", "still reading", 1),
              updatedAt: "2026-07-27T11:59:11.000Z",
            },
          ],
          4,
        ),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText("just now")).toBeInTheDocument();

      const slowProbe = deferred<BoardListPage>();
      fetchCoordinationBoardMock.mockReturnValueOnce(slowProbe.promise);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.getByText("just now")).toBeInTheDocument();

      // The next interval coalesces behind the progressing request, but still
      // owns the render clock and crosses the one-minute display boundary.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.queryByText("just now")).not.toBeInTheDocument();
      expect(screen.getAllByText("1m ago").length).toBeGreaterThan(0);

      await act(async () => {
        slowProbe.resolve(boardPage([], 4, { unchanged: true }));
      });
    } finally {
      vi.useRealTimers();
    }
  });

  it("a hung request is superseded after the stale-claim window instead of wedging polling", async () => {
    vi.useFakeTimers();
    try {
      const neverSettles = deferred<BoardListPage>();
      let initialSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock.mockImplementationOnce(
        (_sessionId, options) => {
          initialSignal = options?.signal;
          return neverSettles.promise;
        },
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // First poll tick: claim is younger than the stale window — coalesce.
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([boardEntry("revived.fact", "alive", 1)], 6),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // Second tick crosses BOARD_STALE_CLAIM_MS: the hung claim is
      // superseded, a fresh request fires and applies.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.getByText("revived.fact")).toBeInTheDocument();
      expect(initialSignal?.aborted).toBe(true);
      expect(screen.queryByText("Loading board…")).not.toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("marks a hung-request takeover stale and applies background backoff", async () => {
    vi.useFakeTimers();
    try {
      const hungInitial = deferred<BoardListPage>();
      const hungTakeover = deferred<BoardListPage>();
      fetchCoordinationBoardMock
        .mockReturnValueOnce(hungInitial.promise)
        .mockReturnValueOnce(hungTakeover.promise);

      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });

      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(
        screen.getByRole("status", {
          name: "Stale; live board polling failed and will retry automatically",
        }),
      ).toHaveTextContent("stale");
      expect(screen.getByRole("alert")).toHaveTextContent(
        "The board refresh stopped making progress",
      );

      // The next ordinary interval is inside the stale-claim retry delay, so
      // another takeover is not started immediately.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      await act(async () => {
        hungTakeover.resolve(boardPage([], 1));
      });
      expect(screen.getByText("live")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports a failed stale takeover instead of rendering a false empty board", async () => {
    vi.useFakeTimers();
    try {
      const neverSettles = deferred<BoardListPage>();
      fetchCoordinationBoardMock
        .mockReturnValueOnce(neverSettles.promise)
        .mockRejectedValueOnce(new Error("takeover unavailable"));

      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(
          BOARD_STALE_CLAIM_MS + BOARD_LIVE_POLL_INTERVAL_MS / 4,
        );
      });

      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.queryByText("Loading board…")).not.toBeInTheDocument();
      expect(
        screen.queryByText(/Nothing published yet/),
      ).not.toBeInTheDocument();
      expect(screen.getByRole("alert")).toHaveTextContent(
        "takeover unavailable",
      );
      expect(screen.getByText("stale")).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not treat foreground error ownership as a successful snapshot", async () => {
    vi.useFakeTimers();
    try {
      const hungRetry = deferred<BoardListPage>();
      fetchCoordinationBoardMock
        .mockRejectedValueOnce(new Error("initial unavailable"))
        .mockReturnValueOnce(hungRetry.promise)
        .mockRejectedValueOnce(new Error("takeover unavailable"));

      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByRole("alert")).toHaveTextContent(
        "initial unavailable",
      );

      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
      });
      expect(
        screen.getByRole("button", { name: "Refreshing…" }),
      ).toBeDisabled();

      // The retry hangs, then its stale automatic takeover fails. The initial
      // error owned this session's display but never established a snapshot.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      expect(
        screen.queryByText(/Nothing published yet/),
      ).not.toBeInTheDocument();
      expect(screen.getByRole("alert")).toHaveTextContent(
        "takeover unavailable",
      );
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("renews a foreground claim when a slow multi-page read makes progress", async () => {
    vi.useFakeTimers();
    try {
      const firstPage = deferred<BoardListPage>();
      const secondPage = deferred<BoardListPage>();
      let foregroundSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock
        .mockImplementationOnce((_sessionId, options) => {
          foregroundSignal = options?.signal;
          return firstPage.promise;
        })
        .mockReturnValueOnce(secondPage.promise);

      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // Finish the first page just before the original stale deadline. The
      // continuation proves progress and renews the claim.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(
          BOARD_STALE_CLAIM_MS - BOARD_LIVE_POLL_INTERVAL_MS - 1_050,
        );
        firstPage.resolve(
          boardPage([boardEntry("a.first", 1)], 7, {
            nextAfterKey: "a.first",
          }),
        );
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      // The next interval crosses 16 s since request start, but only ~1 s
      // since page progress. It must coalesce rather than abort/restart.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(1_100);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(foregroundSignal?.aborted).toBe(false);

      await act(async () => {
        secondPage.resolve(boardPage([boardEntry("z.last", 2)], 7));
      });
      expect(screen.getByText("a.first")).toBeInTheDocument();
      expect(screen.getByText("z.last")).toBeInTheDocument();
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("gives a slow replacement probe a growing stale-claim window", async () => {
    vi.useFakeTimers();
    try {
      const hungInitial = deferred<BoardListPage>();
      const slowTakeover = deferred<BoardListPage>();
      let takeoverSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock
        .mockReturnValueOnce(hungInitial.promise)
        .mockImplementationOnce((_sessionId, options) => {
          takeoverSignal = options?.signal;
          return slowTakeover.promise;
        });

      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      // The replacement has already consumed the original 16 s window, but
      // the prior takeover grants it 32 s before another abort. This lets a
      // slow-but-finite TTFB complete instead of entering a fixed timeout loop.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(takeoverSignal?.aborted).toBe(false);

      await act(async () => {
        slowTakeover.resolve(
          boardPage([boardEntry("slow.takeover", "arrived", 1)], 8),
        );
      });
      expect(screen.getByText("slow.takeover")).toBeInTheDocument();
      expect(screen.getByText("live")).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps a hung manual Refresh actionable while polling takes over", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage([boardEntry("steady.fact", "kept", 1)], 4),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      const hungManual = deferred<BoardListPage>();
      const takeover = deferred<BoardListPage>();
      fetchCoordinationBoardMock
        .mockReturnValueOnce(hungManual.promise)
        .mockReturnValueOnce(takeover.promise);
      fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });

      expect(screen.getByText("steady.fact")).toBeInTheDocument();
      expect(screen.getByRole("alert")).toHaveTextContent(
        "The board refresh stopped making progress",
      );
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();

      await act(async () => {
        takeover.resolve(boardPage([], 4, { unchanged: true }));
      });
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("aborts, invalidates, and stops polling when the panel unmounts", async () => {
    vi.useFakeTimers();
    try {
      const pending = deferred<BoardListPage>();
      let requestSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock.mockImplementationOnce(
        (_sessionId, options) => {
          requestSignal = options?.signal;
          return pending.promise;
        },
      );

      const { unmount } = render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      unmount();
      expect(requestSignal?.aborted).toBe(true);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS * 4);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      // A late mock settlement after cleanup must be inert.
      await act(async () => {
        pending.resolve(boardPage([boardEntry("late.fact", "ignored", 1)], 1));
      });
      expect(screen.queryByText("late.fact")).not.toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("BoardPanel review follow-ups", () => {
  it("renders the last-write header chip and singular write pluralisation", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-27T12:00:00.000Z"));
    try {
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage(
          [
            {
              ...boardEntry("one.fact", "v", 1),
              updatedAt: "2026-07-27T11:55:00.000Z",
            },
          ],
          1,
        ),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText("one.fact")).toBeInTheDocument();
      expect(screen.getByText(/last write 5m ago/)).toBeInTheDocument();

      cleanup();
      fetchCoordinationBoardMock.mockResolvedValue(boardPage([], 1));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText(/board has seen 1 write\b/)).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("reports a deletion-only generation as an observed last change", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-27T12:00:00.000Z"));
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage([boardEntry("temporary.fact", "present", 1)], 1),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText("temporary.fact")).toBeInTheDocument();

      fetchCoordinationBoardMock.mockResolvedValueOnce(boardPage([], 2));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });

      expect(screen.queryByText("temporary.fact")).not.toBeInTheDocument();
      expect(screen.getByText("gen 2")).toBeInTheDocument();
      expect(screen.getByText("last change just now")).toBeInTheDocument();
      expect(
        screen.getByText("Board entries removed at scope generation 2"),
      ).toBeInTheDocument();
      expect(
        screen.getByTitle(
          /Deletion-only board generation observed at .*; deleted rows do not expose a write timestamp/,
        ),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("background probe failure degrades to stale, backs off, and recovers silently", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage([boardEntry("healthy.fact", "ok", 1)], 2),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();

      fetchCoordinationBoardMock.mockRejectedValueOnce(
        new Error("backend unreachable"),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(
        screen.getByRole("status", {
          name: "Stale; live board polling failed and will retry automatically",
        }),
      ).toHaveTextContent("stale");
      expect(
        screen.getByTitle(
          "Live polling is temporarily stale. Retrying automatically; use Refresh to retry now.",
        ),
      ).toBeInTheDocument();
      // Facts stay rendered while the automatic probe is stale.
      expect(screen.getByText("healthy.fact")).toBeInTheDocument();
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 2, { unchanged: true }),
      );
      // The first ordinary tick is inside the 16 s retry window and is skipped.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      // The next tick crosses the retry deadline, succeeds, and returns live.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(
        screen.getByRole("status", {
          name: "Live; the board re-checks automatically every few seconds",
        }),
      ).toHaveTextContent("live");
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds pagination restarts for a busy background probe", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage([boardEntry("steady.fact", "ok", 1)], 1),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      const conflict = new ApiRequestError(
        "request-failed",
        "coordination board scope changed during pagination",
        { status: 409 },
      );
      fetchCoordinationBoardMock
        .mockResolvedValueOnce(
          boardPage([boardEntry("a.first", 1)], 2, {
            nextAfterKey: "a.first",
          }),
        )
        .mockRejectedValueOnce(conflict)
        .mockResolvedValueOnce(
          boardPage([boardEntry("a.first", 2)], 3, {
            nextAfterKey: "a.first",
          }),
        )
        .mockRejectedValueOnce(conflict);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });

      // Initial load plus two background listing attempts (first page + 409).
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(5);
      expect(BOARD_BACKGROUND_PAGINATION_RESTART_LIMIT).toBe(1);
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(screen.getByText("steady.fact")).toBeInTheDocument();
      expect(
        screen.getByRole("status", {
          name: "Stale; live board polling failed and will retry automatically",
        }),
      ).toHaveTextContent("stale");
    } finally {
      vi.useRealTimers();
    }
  });

  it("manual Refresh bypasses background backoff and reports its error", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage([boardEntry("steady.fact", "ok", 1)], 2),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      fetchCoordinationBoardMock.mockRejectedValueOnce(
        new Error("background unavailable"),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();

      fetchCoordinationBoardMock.mockRejectedValueOnce(
        new Error("manual refresh failed"),
      );
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
      });

      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      expect(screen.getByRole("alert")).toHaveTextContent(
        "manual refresh failed",
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("caps exponential background retry delay", () => {
    expect(boardBackgroundRetryDelay(1)).toBe(BOARD_BACKGROUND_RETRY_BASE_MS);
    expect(boardBackgroundRetryDelay(2)).toBe(
      BOARD_BACKGROUND_RETRY_BASE_MS * 2,
    );
    expect(boardBackgroundRetryDelay(3)).toBe(BOARD_BACKGROUND_RETRY_MAX_MS);
    expect(boardBackgroundRetryDelay(20)).toBe(BOARD_BACKGROUND_RETRY_MAX_MS);
  });

  it("pins the board polling timing policy", () => {
    expect(BOARD_STALE_CLAIM_MS).toBe(BOARD_LIVE_POLL_INTERVAL_MS * 2);
    expect(BOARD_BACKGROUND_RETRY_BASE_MS).toBe(
      BOARD_LIVE_POLL_INTERVAL_MS * 2,
    );
    expect(boardStaleClaimWindow(1)).toBe(BOARD_STALE_CLAIM_MS * 2);
    expect(boardStaleClaimWindow(3)).toBe(BOARD_STALE_CLAIM_MAX_MS);
    expect(boardStaleClaimWindow(30)).toBe(BOARD_STALE_CLAIM_MAX_MS);
  });

  it("keeps relative timestamps aging while network probes back off", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-27T12:00:00.000Z"));
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage(
          [
            {
              ...boardEntry("aging.fact", "ok", 1),
              updatedAt: "2026-07-27T11:59:13.000Z",
            },
          ],
          2,
        ),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(screen.getByText("just now")).toBeInTheDocument();

      fetchCoordinationBoardMock.mockRejectedValueOnce(
        new Error("background unavailable"),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(screen.getByText("just now")).toBeInTheDocument();

      // This tick is skipped by the 16 s network backoff, but still advances
      // the render clock past the one-minute relative-time boundary.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.queryByText("just now")).not.toBeInTheDocument();
      expect(screen.getAllByText("1m ago").length).toBeGreaterThan(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a manual click supersedes a background probe and keeps foreground error semantics", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage([boardEntry("steady.fact", "v", 1)], 3),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      const slowProbe = deferred<BoardListPage>();
      let backgroundSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock
        .mockImplementationOnce((_sessionId, options) => {
          backgroundSignal = options?.signal;
          return slowProbe.promise;
        })
        .mockRejectedValueOnce(new Error("manual refresh unavailable"));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      // The automatic probe itself stays visually quiet. A manual click aborts
      // it and starts a true foreground request instead of inheriting the
      // silent-error/backoff semantics of the background request.
      const button = screen.getByRole("button", { name: "Refresh" });
      expect(button).not.toBeDisabled();
      await act(async () => {
        fireEvent.click(button);
      });
      expect(backgroundSignal?.aborted).toBe(true);
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      expect(screen.getByRole("alert")).toHaveTextContent(
        "manual refresh unavailable",
      );
      expect(
        screen.getByRole("button", { name: "Refresh" }),
      ).not.toBeDisabled();
    } finally {
      vi.useRealTimers();
    }
  });

  it("announces updated entries through a polite live region", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage(
          [{ ...boardEntry("quiet.fact", "v1", 1), updatedAtGeneration: 1 }],
          1,
        ),
      );
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage(
          [{ ...boardEntry("quiet.fact", "v2", 2), updatedAtGeneration: 4 }],
          4,
        ),
      );
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(
        screen.getByText("1 board entry updated at scope generation 4"),
      ).toBeInTheDocument();

      // A second single-entry update lands before the first highlight clears.
      // The generation makes the live-region text distinct, so assistive
      // technology receives another DOM mutation instead of identical text.
      fetchCoordinationBoardMock.mockResolvedValueOnce(
        boardPage(
          [{ ...boardEntry("quiet.fact", "v3", 3), updatedAtGeneration: 5 }],
          5,
        ),
      );
      await act(async () => {
        fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
      });
      expect(
        screen.getByText("1 board entry updated at scope generation 5"),
      ).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("refreshes once when the tab becomes visible again", async () => {
    vi.useFakeTimers();
    let hidden = false;
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => hidden,
    });
    try {
      fetchCoordinationBoardMock.mockResolvedValue(boardPage([], 2));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      hidden = true;
      document.dispatchEvent(new Event("visibilitychange"));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(1);

      hidden = false;
      fetchCoordinationBoardMock.mockResolvedValue(
        boardPage([], 2, { unchanged: true }),
      );
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
        await vi.advanceTimersByTimeAsync(50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
    } finally {
      Reflect.deleteProperty(document, "hidden");
      vi.useRealTimers();
    }
  });

  it("refreshes immediately on visibility return even during background backoff", async () => {
    vi.useFakeTimers();
    let hidden = false;
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => hidden,
    });
    try {
      fetchCoordinationBoardMock
        .mockResolvedValueOnce(boardPage([], 2))
        .mockRejectedValueOnce(new Error("temporary outage"))
        .mockResolvedValueOnce(boardPage([], 2, { unchanged: true }));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);
      expect(screen.getByText("stale")).toBeInTheDocument();

      hidden = true;
      document.dispatchEvent(new Event("visibilitychange"));
      hidden = false;
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
        await vi.advanceTimersByTimeAsync(0);
      });

      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      expect(screen.getByText("live")).toBeInTheDocument();
    } finally {
      Reflect.deleteProperty(document, "hidden");
      vi.useRealTimers();
    }
  });

  it("visibility return replaces a pending background probe", async () => {
    vi.useFakeTimers();
    let hidden = false;
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => hidden,
    });
    try {
      fetchCoordinationBoardMock.mockResolvedValueOnce(boardPage([], 2));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });

      const pendingProbe = deferred<BoardListPage>();
      let pendingSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock
        .mockImplementationOnce((_sessionId, options) => {
          pendingSignal = options?.signal;
          return pendingProbe.promise;
        })
        .mockResolvedValueOnce(boardPage([], 2, { unchanged: true }));
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      hidden = true;
      document.dispatchEvent(new Event("visibilitychange"));
      hidden = false;
      await act(async () => {
        document.dispatchEvent(new Event("visibilitychange"));
        await vi.advanceTimersByTimeAsync(0);
      });

      expect(pendingSignal?.aborted).toBe(true);
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      expect(screen.getByText("live")).toBeInTheDocument();
    } finally {
      Reflect.deleteProperty(document, "hidden");
      vi.useRealTimers();
    }
  });

  it("does not let network failures inflate a later stale-claim window", async () => {
    vi.useFakeTimers();
    try {
      fetchCoordinationBoardMock
        .mockResolvedValueOnce(boardPage([], 2))
        .mockRejectedValueOnce(new Error("fast network failure"));
      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS + 50);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      const hungAfterFailure = deferred<BoardListPage>();
      let hungSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock
        .mockImplementationOnce((_sessionId, options) => {
          hungSignal = options?.signal;
          return hungAfterFailure.promise;
        })
        .mockResolvedValueOnce(boardPage([], 2, { unchanged: true }));

      // The ordinary network error backs polling off for 16 s. Once a new
      // request starts, however, its no-progress window remains the base 16 s
      // because no stale takeover has occurred in this chain.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_BACKGROUND_RETRY_BASE_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });

      expect(hungSignal?.aborted).toBe(true);
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it("inspects an active stale claim before applying idle retry backoff", async () => {
    vi.useFakeTimers();
    try {
      const firstHungClaim = deferred<BoardListPage>();
      const replacementHungClaim = deferred<BoardListPage>();
      let firstHungSignal: AbortSignal | undefined;
      let replacementHungSignal: AbortSignal | undefined;
      fetchCoordinationBoardMock
        .mockResolvedValueOnce(boardPage([], 2))
        .mockRejectedValueOnce(new Error("fast failure one"))
        .mockRejectedValueOnce(new Error("fast failure two"))
        .mockImplementationOnce((_sessionId, options) => {
          firstHungSignal = options?.signal;
          return firstHungClaim.promise;
        })
        .mockImplementationOnce((_sessionId, options) => {
          replacementHungSignal = options?.signal;
          return replacementHungClaim.promise;
        })
        .mockResolvedValueOnce(boardPage([], 2, { unchanged: true }));

      render(<BoardPanel sessionId="session-2098" />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_LIVE_POLL_INTERVAL_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(2);

      // Failure one backs off 16 s; failure two then backs off 32 s.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_BACKGROUND_RETRY_BASE_MS);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(3);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_BACKGROUND_RETRY_BASE_MS * 2);
      });
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(4);

      // The first hung claim expires after the base 16 s window. Its
      // replacement receives a 32 s stale window while retry backoff grows to
      // 64 s. Active-claim inspection must still run at 32 s rather than wait
      // for that longer idle-probe deadline.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(BOARD_STALE_CLAIM_MS);
      });
      expect(firstHungSignal?.aborted).toBe(true);
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(5);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(boardStaleClaimWindow(1));
      });
      expect(replacementHungSignal?.aborted).toBe(true);
      expect(fetchCoordinationBoardMock).toHaveBeenCalledTimes(6);
    } finally {
      vi.useRealTimers();
    }
  });
});
