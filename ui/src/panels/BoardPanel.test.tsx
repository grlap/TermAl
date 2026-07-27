import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ApiRequestError, fetchCoordinationBoard } from "../api";
import type { BoardEntry, BoardListPage } from "../types";
import { BoardPanel } from "./BoardPanel";

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
    expect(screen.getByText("manual refresh")).toBeInTheDocument();
    expect(screen.getByText("gate-round-5")).toBeInTheDocument();
  });

  it("shows a loading state, not the empty state, before the first response", async () => {
    const first = deferred<BoardListPage>();
    fetchCoordinationBoardMock.mockReturnValue(first.promise);

    render(<BoardPanel sessionId="session-2098" />);

    expect(screen.getByText("Loading board…")).toBeInTheDocument();
    expect(screen.queryByText(/No board entries/)).not.toBeInTheDocument();
    expect(screen.queryByRole("list")).not.toBeInTheDocument();

    await act(async () => {
      first.resolve(boardPage([], 0));
    });
    expect(screen.getByText(/No board entries/)).toBeInTheDocument();
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
      { afterKey: "a.first", snapshotGeneration: 7 },
    );
    expect(fetchCoordinationBoardMock).toHaveBeenNthCalledWith(
      3,
      "session-2098",
      { afterKey: "b.second", snapshotGeneration: 7 },
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
      { knownGeneration: 5 },
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
      nextProject.resolve(
        boardPage([boardEntry("new.project", "fresh")], 3),
      );
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
