import { afterEach, describe, expect, it, vi } from "vitest";

import {
  cancelDelegationCommand,
  delegationCommands,
  getDelegationResultCommand,
  getDelegationStatusCommand,
  spawnDelegationCommand,
  waitDelegationCommand,
  waitDelegationsCommand,
  MAX_DELEGATION_WAIT_IDS,
  MAX_DELEGATION_WAIT_TIMEOUT_MS,
  MIN_DELEGATION_WAIT_INTERVAL_MS,
} from "./delegation-commands";
import type { DelegationRecord, DelegationResult, Session } from "./types";

function makeDelegation(
  overrides: Partial<DelegationRecord> = {},
): DelegationRecord {
  return {
    id: "delegation-1",
    parentSessionId: "parent-1",
    childSessionId: "child-1",
    mode: "reviewer",
    status: "running",
    title: "Review",
    prompt: "Review the diff.",
    cwd: "C:/workspace",
    agent: "Codex",
    model: "codex",
    writePolicy: { kind: "readOnly" },
    createdAt: "2026-05-02 10:00:00",
    startedAt: "2026-05-02 10:00:00",
    completedAt: null,
    result: null,
    ...overrides,
  };
}

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: "child-1",
    name: "Review",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace",
    model: "codex",
    status: "active",
    preview: "",
    messages: [],
    messagesLoaded: false,
    ...overrides,
  };
}

function makeResult(overrides: Partial<DelegationResult> = {}): DelegationResult {
  return {
    delegationId: "delegation-1",
    childSessionId: "child-1",
    status: "completed",
    summary: "No issues found.",
    findings: [],
    changedFiles: [],
    commandsRun: [{ command: "npx tsc --noEmit", status: "passed" }],
    notes: ["Reviewed staged changes."],
    ...overrides,
  };
}

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: {
      "Content-Type": "application/json",
    },
  });
}

function stubFetchResponses(...bodies: unknown[]) {
  const queue = [...bodies];
  const fetchMock = vi.fn<typeof fetch>(async () => {
    if (queue.length === 0) {
      throw new Error("unexpected fetch call");
    }
    const body = queue.shift();
    return jsonResponse(body);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("delegation command surface", () => {
  it("exports tool-style command names for MCP wrappers", () => {
    expect(delegationCommands).toMatchObject({
      spawn_delegation: spawnDelegationCommand,
      get_delegation_status: getDelegationStatusCommand,
      get_delegation_result: getDelegationResultCommand,
      cancel_delegation: cancelDelegationCommand,
    });
    expect(delegationCommands.wait_delegation).toBeTypeOf("function");
    expect(delegationCommands.wait_delegations).toBeTypeOf("function");
  });

  it("spawns a delegation and returns compact child/delegation ids with the child session", async () => {
    const delegation = makeDelegation();
    const childSession = makeSession();
    const fetchMock = stubFetchResponses({
      revision: 2,
      serverInstanceId: "server-a",
      delegation,
      childSession,
    });

    const result = await spawnDelegationCommand("parent-1", {
      prompt: "Review this change.",
      title: "Review",
    });

    expect(result).toMatchObject({
      delegationId: "delegation-1",
      childSessionId: "child-1",
      childSession,
      revision: 2,
      serverInstanceId: "server-a",
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions/parent-1/delegations",
      expect.objectContaining({
        body: JSON.stringify({
          prompt: "Review this change.",
          title: "Review",
        }),
        method: "POST",
      }),
    );
  });

  it("returns status, result, and cancel command packets", async () => {
    const completed = makeDelegation({
      status: "completed",
      completedAt: "2026-05-02 10:10:00",
      result: makeResult(),
    });
    const fetchMock = stubFetchResponses(
      {
        revision: 3,
        serverInstanceId: "server-a",
        delegation: completed,
      },
      {
        revision: 4,
        serverInstanceId: "server-a",
        result: makeResult(),
      },
      {
        revision: 5,
        serverInstanceId: "server-a",
        delegation: completed,
      },
    );

    await expect(
      getDelegationStatusCommand("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      delegationId: "delegation-1",
      childSessionId: "child-1",
      status: "completed",
      revision: 3,
    });
    await expect(
      getDelegationResultCommand("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      delegationId: "delegation-1",
      childSessionId: "child-1",
      status: "completed",
      summary: "No issues found.",
      findings: [],
      changedFiles: [],
      commandsRun: [{ command: "npx tsc --noEmit", status: "passed" }],
      failedChecks: [],
      notes: ["Reviewed staged changes."],
      revision: 4,
    });
    await expect(
      cancelDelegationCommand("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      delegationId: "delegation-1",
      status: "completed",
      revision: 5,
    });

    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "/api/sessions/parent-1/delegations/delegation-1/result",
      expect.anything(),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      3,
      "/api/sessions/parent-1/delegations/delegation-1/cancel",
      expect.objectContaining({ method: "POST" }),
    );
  });

  it("normalizes compact result packets and derives failed checks", async () => {
    stubFetchResponses({
      revision: 4,
      serverInstanceId: "server-a",
      result: makeResult({
        findings: undefined,
        changedFiles: undefined,
        commandsRun: [
          { command: "cargo check", status: "passed" },
          { command: "npx vitest run", status: "failed" },
        ],
        notes: undefined,
      }),
    });

    await expect(
      getDelegationResultCommand("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      findings: [],
      changedFiles: [],
      commandsRun: [
        { command: "cargo check", status: "passed" },
        { command: "npx vitest run", status: "failed" },
      ],
      failedChecks: [{ command: "npx vitest run", status: "failed" }],
      notes: [],
      revision: 4,
    });
  });

  it("waits for multiple delegations and returns when all are terminal", async () => {
    vi.useFakeTimers();
    stubFetchResponses(
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ id: "delegation-1", status: "running" }),
      },
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "running",
        }),
      },
      {
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-1",
          status: "completed",
          result: makeResult(),
        }),
      },
      {
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "running",
        }),
      },
      {
        revision: 4,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "canceled",
        }),
      },
    );

    const wait = waitDelegationsCommand(
      "parent-1",
      [" delegation-1 ", "delegation-2", "delegation-2"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 10,
      },
    );
    const assertion = expect(wait).resolves.toMatchObject({
      outcome: "completed",
      revision: 4,
      serverInstanceId: "server-a",
      completed: [
        { id: "delegation-1", status: "completed" },
        { id: "delegation-2", status: "canceled" },
      ],
      pending: [],
    });

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS * 5);

    await assertion;
  });

  it("waits for one delegation through the single-id wrapper", async () => {
    stubFetchResponses({
      revision: 2,
      serverInstanceId: "server-a",
      delegation: makeDelegation({ status: "completed" }),
    });

    await expect(
      waitDelegationCommand("parent-1", "delegation-1", {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).resolves.toMatchObject({
      outcome: "completed",
      completed: [{ id: "delegation-1", status: "completed" }],
      pending: [],
    });
  });

  it("treats failed delegations as terminal", async () => {
    stubFetchResponses({
      revision: 2,
      serverInstanceId: "server-a",
      delegation: makeDelegation({ status: "failed" }),
    });

    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).resolves.toMatchObject({
      outcome: "completed",
      completed: [{ id: "delegation-1", status: "failed" }],
      pending: [],
      revision: 2,
    });
  });

  it("reports the highest revision observed in one poll batch", async () => {
    stubFetchResponses(
      {
        revision: 10,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-1",
          status: "completed",
        }),
      },
      {
        revision: 8,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "completed",
        }),
      },
    );

    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1", "delegation-2"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).resolves.toMatchObject({
      outcome: "completed",
      revision: 10,
      serverInstanceId: "server-a",
    });
  });

  it("times out without canceling pending delegations", async () => {
    vi.useFakeTimers();
    const fetchMock = stubFetchResponses(
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ status: "running" }),
      },
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ status: "running" }),
      },
    );

    const wait = waitDelegationsCommand("parent-1", ["delegation-1"], {
      pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
      timeoutMs: 1,
    });

    const assertion = expect(wait).resolves.toMatchObject({
      outcome: "timeout",
      completed: [],
      pending: [{ id: "delegation-1", status: "running" }],
    });

    await vi.advanceTimersByTimeAsync(1);

    await assertion;
    const requestedUrls = fetchMock.mock.calls.map(([url]) => String(url));
    expect(requestedUrls.every((url) => !url.endsWith("/cancel"))).toBe(true);
  });

  it("rejects zero polling intervals instead of busy-looping", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        pollIntervalMs: 0,
      }),
    ).rejects.toThrow("pollIntervalMs must be a finite positive duration");
  });

  it("rejects timeout zero with the same positive-duration contract", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        timeoutMs: 0,
      }),
    ).rejects.toThrow("timeoutMs must be a finite positive duration");
  });

  it("rejects polling below the minimum interval", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS - 1,
      }),
    ).rejects.toThrow(
      `pollIntervalMs must be at least ${MIN_DELEGATION_WAIT_INTERVAL_MS}ms`,
    );
  });

  it("rejects excessive wait timeouts and delegation-id batches", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        timeoutMs: MAX_DELEGATION_WAIT_TIMEOUT_MS + 1,
      }),
    ).rejects.toThrow(
      `timeoutMs must be no greater than ${MAX_DELEGATION_WAIT_TIMEOUT_MS}ms`,
    );

    await expect(
      waitDelegationsCommand(
        "parent-1",
        Array.from(
          { length: MAX_DELEGATION_WAIT_IDS + 1 },
          (_, index) => `delegation-${index + 1}`,
        ),
      ),
    ).rejects.toThrow(
      `wait_delegations accepts at most ${MAX_DELEGATION_WAIT_IDS} ids`,
    );
  });

  it("rejects mismatched delegation ids instead of waiting until timeout", async () => {
    stubFetchResponses({
      revision: 2,
      serverInstanceId: "server-a",
      delegation: makeDelegation({ id: "other-delegation" }),
    });

    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).rejects.toThrow(
      "delegation status id mismatch: requested delegation-1, received other-delegation",
    );
  });

  it("rejects mixed server-instance status batches", async () => {
    stubFetchResponses(
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-1",
          status: "completed",
        }),
      },
      {
        revision: 3,
        serverInstanceId: "server-b",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "completed",
        }),
      },
    );

    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1", "delegation-2"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).rejects.toThrow(
      "delegation status server instance changed during wait: server-a -> server-b",
    );
  });

  it("uses global timers rather than the browser window object", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("window", undefined);
    stubFetchResponses(
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ status: "running" }),
      },
      {
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ status: "completed" }),
      },
    );

    const wait = waitDelegationsCommand("parent-1", ["delegation-1"], {
      pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
      timeoutMs: 100,
    });

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await expect(wait).resolves.toMatchObject({
      outcome: "completed",
      revision: 3,
    });
  });

  it("rejects status fetch failures and leaves partial-state handling to callers", async () => {
    vi.useFakeTimers();
    const fetchMock = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(
        jsonResponse({
          revision: 2,
          serverInstanceId: "server-a",
          delegation: makeDelegation({ status: "running" }),
        }),
      )
      .mockRejectedValueOnce(new Error("network down"));
    vi.stubGlobal("fetch", fetchMock);

    const wait = waitDelegationsCommand("parent-1", ["delegation-1"], {
      pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
      timeoutMs: 100,
    });
    const rejection = expect(wait).rejects.toThrow(
      "The TermAl backend is unavailable.",
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await rejection;
  });
});
