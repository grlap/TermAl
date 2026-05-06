import { afterEach, describe, expect, expectTypeOf, it, vi } from "vitest";

import { ApiRequestError } from "./api";
import {
  cancelDelegationCommand,
  createDelegationCommands,
  delegationCommands,
  getDelegationResultCommand,
  getDelegationStatusCommand,
  MAX_REVIEWER_BATCH_SIZE,
  spawnDelegationCommand,
  waitDelegationCommand,
  waitDelegationsCommand,
  MAX_DELEGATION_MODEL_CHARS,
  MAX_DELEGATION_TITLE_CHARS,
  MAX_DELEGATION_PROMPT_BYTES,
  MAX_DELEGATION_WAIT_IDS,
  MAX_DELEGATION_WAIT_TIMEOUT_MS,
  MIN_DELEGATION_WAIT_INTERVAL_MS,
  type DelegationCommandTransport,
  type WaitDelegationErrorPacket,
  type WaitDelegationsResult,
} from "./delegation-commands";
import type { DelegationRecord, DelegationResult, Session } from "./types";

type CreateDelegationTransportResponse = Awaited<
  ReturnType<DelegationCommandTransport["createDelegation"]>
>;

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
    commandsRun: [{ command: "npx tsc --noEmit", status: "success" }],
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function expectRedactedDelegationSummary(delegation: object) {
  const record = delegation as Record<string, unknown>;
  expect(delegation).toMatchObject({
    id: expect.any(String),
    parentSessionId: expect.any(String),
    childSessionId: expect.any(String),
    mode: expect.any(String),
    status: expect.any(String),
    title: expect.any(String),
    agent: expect.any(String),
    writePolicy: expect.any(Object),
    createdAt: expect.any(String),
  });
  expectOwnNullableModelProperty(record);
  expectOwnNullableTimestampProperty(record, "startedAt");
  expectOwnNullableTimestampProperty(record, "completedAt");
  expect(delegation).not.toHaveProperty("prompt");
  expect(delegation).not.toHaveProperty("cwd");
  const maybeResult = (delegation as { result?: object | null }).result;
  if (maybeResult) {
    expect(maybeResult).not.toHaveProperty("findings");
    expect(maybeResult).not.toHaveProperty("changedFiles");
    expect(maybeResult).not.toHaveProperty("commandsRun");
    expect(maybeResult).not.toHaveProperty("notes");
  }
}

function expectOwnNullableModelProperty(record: Record<string, unknown>) {
  expect(Object.prototype.hasOwnProperty.call(record, "model")).toBe(true);
  const value = record.model;
  expect(value === null || (typeof value === "string" && value.length > 0)).toBe(
    true,
  );
}

function expectOwnNullableTimestampProperty(
  record: Record<string, unknown>,
  property: "startedAt" | "completedAt",
) {
  expect(Object.prototype.hasOwnProperty.call(record, property)).toBe(true);
  const value = record[property];
  expect(
    value === null ||
      (typeof value === "string" &&
        /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(value)),
  ).toBe(true);
}

function expectRedactedChildSessionSummary(childSession: object) {
  expect(Object.keys(childSession).sort()).toEqual([
    "agent",
    "emoji",
    "id",
    "model",
    "name",
    "parentDelegationId",
    "status",
  ]);
  expect(childSession).toMatchObject({
    id: expect.any(String),
    name: expect.any(String),
    emoji: expect.any(String),
    agent: expect.any(String),
    model: expect.any(String),
    status: expect.any(String),
  });
  expect(childSession).toHaveProperty("parentDelegationId");
  expect(childSession).not.toHaveProperty("workdir");
  expect(childSession).not.toHaveProperty("messages");
  expect(childSession).not.toHaveProperty("pendingPrompts");
  expect(childSession).not.toHaveProperty("preview");
}

function expectCapturedAbortSignal(
  signal: AbortSignal | null,
): asserts signal is AbortSignal {
  expect(signal).not.toBeNull();
  expect(signal).toBeInstanceOf(AbortSignal);
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("delegation command surface", () => {
  it("pins public wait limit constants", () => {
    expect(MIN_DELEGATION_WAIT_INTERVAL_MS).toBe(500);
    expect(MAX_DELEGATION_WAIT_IDS).toBe(10);
    expect(MAX_DELEGATION_PROMPT_BYTES).toBe(64 * 1024);
    expect(MAX_DELEGATION_TITLE_CHARS).toBe(200);
    expect(MAX_DELEGATION_MODEL_CHARS).toBe(200);
    expect(MAX_REVIEWER_BATCH_SIZE).toBe(4);
  });

  it("exports tool-style command names for MCP wrappers", () => {
    expect(delegationCommands.spawn_delegation).toBeTypeOf("function");
    expect(delegationCommands.spawn_reviewer_batch).toBeTypeOf("function");
    expect(delegationCommands.get_delegation_status).toBeTypeOf("function");
    expect(delegationCommands.get_delegation_result).toBeTypeOf("function");
    expect(delegationCommands.cancel_delegation).toBeTypeOf("function");
    expect(delegationCommands.wait_delegations).toBeTypeOf("function");
    expect(delegationCommands).not.toHaveProperty("wait_delegation");
  });

  it("keeps wait results discriminated by outcome", () => {
    type ErrorResult = Extract<WaitDelegationsResult, { outcome: "error" }>;
    type NonErrorResult = Extract<
      WaitDelegationsResult,
      { outcome: "completed" | "timeout" }
    >;

    expectTypeOf<ErrorResult>().toMatchTypeOf<{
      outcome: "error";
      error: WaitDelegationErrorPacket;
    }>();
    expectTypeOf<NonErrorResult>().toMatchTypeOf<{
      outcome: "completed" | "timeout";
      error?: never;
    }>();
  });

  it("spawns a delegation and returns redacted child/delegation summaries", async () => {
    const delegation = makeDelegation();
    const childSession = makeSession({
      parentDelegationId: "delegation-1",
      projectId: "project-1",
      externalSessionId: "external-child-1",
      modelOptions: [{ label: "Codex", value: "codex" }],
      approvalPolicy: "on-request",
      agentCommandsRevision: 7,
      preview: "Review this change.",
      messages: [{ text: "Review this change." } as never],
      messageCount: 1,
      pendingPrompts: [{ prompt: "Review this change." } as never],
      sessionMutationStamp: 123,
    });
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
      childSession: {
        id: "child-1",
        name: "Review",
        emoji: "AI",
        agent: "Codex",
        model: "codex",
        status: "active",
        parentDelegationId: "delegation-1",
      },
      revision: 2,
      serverInstanceId: "server-a",
    });
    expectRedactedDelegationSummary(result.delegation);
    expect(result.delegation).not.toHaveProperty("result");
    expectRedactedChildSessionSummary(result.childSession);
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

  it("trims spawn prompts and parent ids before dispatch", async () => {
    const delegation = makeDelegation();
    const childSession = makeSession();
    const fetchMock = stubFetchResponses({
      revision: 2,
      serverInstanceId: "server-a",
      delegation,
      childSession,
    });

    await spawnDelegationCommand(" parent-1 ", {
      prompt: "  Review this change.  ",
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions/parent-1/delegations",
      expect.objectContaining({
        body: JSON.stringify({
          prompt: "Review this change.",
        }),
      }),
    );
  });

  it("rejects invalid spawn prompt and null optional fields before dispatch", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      spawnDelegationCommand("parent-1", { prompt: "   " }),
    ).rejects.toThrow(/^prompt must be non-empty/);
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "x".repeat(MAX_DELEGATION_PROMPT_BYTES + 1),
      }),
    ).rejects.toThrow(
      new RegExp(
        `^prompt must be no larger than ${MAX_DELEGATION_PROMPT_BYTES} bytes`,
      ),
    );
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "界".repeat(Math.floor(MAX_DELEGATION_PROMPT_BYTES / 2)),
      }),
    ).rejects.toThrow(
      new RegExp(
        `^prompt must be no larger than ${MAX_DELEGATION_PROMPT_BYTES} bytes`,
      ),
    );
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        title: "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1),
      }),
    ).rejects.toThrow(
      new RegExp(
        `^title must be no longer than ${MAX_DELEGATION_TITLE_CHARS} characters`,
      ),
    );
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        model: "x".repeat(MAX_DELEGATION_MODEL_CHARS + 1),
      }),
    ).rejects.toThrow(
      new RegExp(
        `^model must be no longer than ${MAX_DELEGATION_MODEL_CHARS} characters`,
      ),
    );
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        title: null as never,
      }),
    ).rejects.toThrow(/^title must be omitted instead of null/);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("accepts spawn metadata exactly at the trimmed character cap", async () => {
    const delegation = makeDelegation();
    const childSession = makeSession();
    const fetchMock = stubFetchResponses({
      revision: 2,
      serverInstanceId: "server-a",
      delegation,
      childSession,
    });
    const title = `  ${"界".repeat(MAX_DELEGATION_TITLE_CHARS)}  `;
    const model = `  ${"界".repeat(MAX_DELEGATION_MODEL_CHARS)}  `;

    await spawnDelegationCommand("parent-1", {
      prompt: "Review this change.",
      title,
      model,
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/sessions/parent-1/delegations",
      expect.objectContaining({
        body: JSON.stringify({
          prompt: "Review this change.",
          title,
          model,
        }),
      }),
    );
  });

  it("spawns reviewer batches in parallel and forces read-only reviewer requests", async () => {
    const first = deferred<CreateDelegationTransportResponse>();
    const second = deferred<CreateDelegationTransportResponse>();
    const pending = [first.promise, second.promise];
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn((_parentSessionId, _request) => {
        const promise = pending.shift();
        if (!promise) {
          throw new Error("unexpected createDelegation call");
        }
        return promise;
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };
    const commands = createDelegationCommands(transport);

    const batch = commands.spawn_reviewer_batch(" parent-1 ", [
      {
        prompt: "  Review React changes.  ",
        title: " React review ",
      },
      {
        prompt: "Review Rust changes.",
        title: "Rust review",
        mode: "worker",
        writePolicy: { kind: "sharedWorktree", ownedPaths: ["src"] },
      } as never,
    ]);

    await Promise.resolve();
    expect(transport.createDelegation).toHaveBeenCalledTimes(2);
    expect(transport.createDelegation).toHaveBeenNthCalledWith(1, "parent-1", {
      prompt: "Review React changes.",
      title: "React review",
      mode: "reviewer",
      writePolicy: { kind: "readOnly" },
    });
    expect(transport.createDelegation).toHaveBeenNthCalledWith(2, "parent-1", {
      prompt: "Review Rust changes.",
      title: "Rust review",
      mode: "reviewer",
      writePolicy: { kind: "readOnly" },
    });

    second.resolve({
      revision: 4,
      serverInstanceId: "server-a",
      delegation: makeDelegation({
        id: "delegation-2",
        childSessionId: "child-2",
        title: "Rust review",
      }),
      childSession: makeSession({
        id: "child-2",
        name: "Rust review",
        parentDelegationId: "delegation-2",
      }),
    });
    first.resolve({
      revision: 3,
      serverInstanceId: "server-a",
      delegation: makeDelegation({
        id: "delegation-1",
        childSessionId: "child-1",
        title: "React review",
      }),
      childSession: makeSession({
        id: "child-1",
        name: "React review",
        parentDelegationId: "delegation-1",
      }),
    });

    await expect(batch).resolves.toMatchObject({
      outcome: "completed",
      delegationIds: ["delegation-1", "delegation-2"],
      childSessionIds: ["child-1", "child-2"],
      spawned: [
        { delegationId: "delegation-1", childSessionId: "child-1" },
        { delegationId: "delegation-2", childSessionId: "child-2" },
      ],
      failed: [],
      revision: 4,
      serverInstanceId: "server-a",
      error: null,
    });
  });

  it("returns partial reviewer batch failures without hiding successful spawns", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => {
        if (request.title === "Rust review") {
          throw new ApiRequestError(
            "request-failed",
            "parent session already has 4 active delegations",
            { status: 409 },
          );
        }
        return {
          revision: 3,
          serverInstanceId: "server-a",
          delegation: makeDelegation({
            id: "delegation-1",
            childSessionId: "child-1",
            title: "React review",
          }),
          childSession: makeSession({
            id: "child-1",
            name: "React review",
            parentDelegationId: "delegation-1",
          }),
        };
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch("parent-1", [
        { prompt: "Review React.", title: "React review" },
        { prompt: "Review Rust.", title: "Rust review" },
      ]),
    ).resolves.toMatchObject({
      outcome: "partial",
      delegationIds: ["delegation-1"],
      failed: [
        {
          index: 1,
          title: "Rust review",
          message: "parent session already has 4 active delegations",
          name: "ApiRequestError",
          apiErrorKind: "request-failed",
          status: 409,
          restartRequired: false,
        },
      ],
      revision: 3,
      serverInstanceId: "server-a",
      error: null,
    });
  });

  it("redacts unsafe reviewer batch failure messages while preserving metadata", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async () => {
        throw new ApiRequestError(
          "request-failed",
          "failed to persist delegation: C:/Users/example/.termal/state.sqlite",
          { status: 500 },
        );
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch("parent-1", [
        { prompt: "Review React.", title: "React review" },
      ]),
    ).resolves.toMatchObject({
      outcome: "error",
      spawned: [],
      failed: [
        {
          index: 0,
          title: "React review",
          name: "ApiRequestError",
          message: "Spawn delegation failed.",
          apiErrorKind: "request-failed",
          status: 500,
          restartRequired: false,
        },
      ],
      revision: null,
      serverInstanceId: null,
      error: null,
    });
  });

  it("returns a batch error when reviewer spawns cross backend instances", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => {
        const isRust = request.title === "Rust review";
        return {
          revision: isRust ? 1 : 8,
          serverInstanceId: isRust ? "server-b" : "server-a",
          delegation: makeDelegation({
            id: isRust ? "delegation-2" : "delegation-1",
            childSessionId: isRust ? "child-2" : "child-1",
            title: request.title,
          }),
          childSession: makeSession({
            id: isRust ? "child-2" : "child-1",
            name: request.title,
            parentDelegationId: isRust ? "delegation-2" : "delegation-1",
          }),
        };
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch("parent-1", [
        { prompt: "Review React.", title: "React review" },
        { prompt: "Review Rust.", title: "Rust review" },
      ]),
    ).resolves.toMatchObject({
      outcome: "error",
      delegationIds: ["delegation-1", "delegation-2"],
      failed: [],
      revision: null,
      serverInstanceId: null,
      error: {
        kind: "mixed-server-instance",
        serverInstanceIds: ["server-a", "server-b"],
      },
    });
  });

  it("rejects invalid reviewer batches before dispatch", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };
    const commands = createDelegationCommands(transport);

    await expect(commands.spawn_reviewer_batch("parent-1", [])).rejects.toThrow(
      /^spawn_reviewer_batch requires at least one reviewer/,
    );
    await expect(
      commands.spawn_reviewer_batch(
        "parent-1",
        Array.from({ length: MAX_REVIEWER_BATCH_SIZE + 1 }, (_, index) => ({
          prompt: `Review ${index + 1}.`,
          title: `Reviewer ${index + 1}`,
        })),
      ),
    ).rejects.toThrow(
      new RegExp(
        `^spawn_reviewer_batch accepts at most ${MAX_REVIEWER_BATCH_SIZE} reviewers`,
      ),
    );
    await expect(
      commands.spawn_reviewer_batch("parent-1", [
        { prompt: "   ", title: "Empty prompt" },
      ]),
    ).rejects.toThrow(/^prompt must be non-empty/);
    expect(transport.createDelegation).not.toHaveBeenCalled();
  });

  it("can run through an injected transport without browser-relative fetch", async () => {
    const delegation = makeDelegation();
    const childSession = makeSession();
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => ({
        revision: 2,
        serverInstanceId: "server-a",
        delegation,
        childSession,
      })),
      fetchDelegationStatus: vi.fn(async () => ({
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ status: "completed" }),
      })),
      fetchDelegationResult: vi.fn(async () => ({
        revision: 4,
        serverInstanceId: "server-a",
        result: makeResult(),
      })),
      cancelDelegation: vi.fn(async () => ({
        revision: 5,
        serverInstanceId: "server-a",
        delegation,
      })),
    };
    const commands = createDelegationCommands(transport);

    const spawnResult = await commands.spawn_delegation("parent-1", {
      prompt: "Review this change.",
      title: "Review",
    });
    expect(spawnResult).toMatchObject({
      delegationId: "delegation-1",
      childSession: {
        id: "child-1",
        agent: "Codex",
        model: "codex",
      },
    });
    expectRedactedChildSessionSummary(spawnResult.childSession);
    expect(transport.createDelegation).toHaveBeenCalledWith("parent-1", {
      prompt: "Review this change.",
      title: "Review",
    });
    await expect(
      commands.wait_delegations("parent-1", ["delegation-1"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
      }),
    ).resolves.toMatchObject({
      outcome: "completed",
      completed: [{ id: "delegation-1" }],
    });
    await expect(
      commands.get_delegation_status("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      delegationId: "delegation-1",
      status: "completed",
    });
    await expect(
      commands.get_delegation_result("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      delegationId: "delegation-1",
      summary: "No issues found.",
    });
    await expect(
      commands.cancel_delegation("parent-1", "delegation-1"),
    ).resolves.toMatchObject({
      delegationId: "delegation-1",
      status: "running",
    });
    expect(transport.fetchDelegationStatus).toHaveBeenCalledTimes(2);
    expect(transport.fetchDelegationResult).toHaveBeenCalledTimes(1);
    expect(transport.cancelDelegation).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
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

    const statusResult = await getDelegationStatusCommand(
      "parent-1",
      "delegation-1",
    );
    expect(statusResult).toMatchObject({
      delegationId: "delegation-1",
      childSessionId: "child-1",
      status: "completed",
      delegation: {
        result: {
          delegationId: "delegation-1",
          childSessionId: "child-1",
          status: "completed",
          summary: "No issues found.",
        },
      },
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
      commandsRun: [{ command: "npx tsc --noEmit", status: "success" }],
      notes: ["Reviewed staged changes."],
      revision: 4,
    });
    const cancelResult = await cancelDelegationCommand(
      "parent-1",
      "delegation-1",
    );
    expect(cancelResult).toMatchObject({
      delegationId: "delegation-1",
      status: "completed",
      revision: 5,
    });
    expectRedactedDelegationSummary(statusResult.delegation);
    expectRedactedDelegationSummary(cancelResult.delegation);

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

  it("normalizes compact result packets without deriving failed checks", async () => {
    stubFetchResponses({
      revision: 4,
      serverInstanceId: "server-a",
      result: makeResult({
        findings: undefined,
        changedFiles: undefined,
        commandsRun: [
          { command: "cargo check", status: "success" },
          { command: "npx vitest run", status: "error" },
          { command: "slow smoke test", status: "running" },
        ],
        notes: undefined,
      }),
    });

    const result = await getDelegationResultCommand("parent-1", "delegation-1");
    expect(result).toMatchObject({
      findings: [],
      changedFiles: [],
      commandsRun: [
        { command: "cargo check", status: "success" },
        { command: "npx vitest run", status: "error" },
        { command: "slow smoke test", status: "running" },
      ],
      notes: [],
      revision: 4,
    });
    expect(result).not.toHaveProperty("failedChecks");
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
    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS * 5);

    const result = await wait;
    expect(result).toMatchObject({
      outcome: "completed",
      revision: 4,
      serverInstanceId: "server-a",
      completed: [
        { id: "delegation-1", status: "completed" },
        { id: "delegation-2", status: "canceled" },
      ],
      pending: [],
    });
    result.delegations.forEach(expectRedactedDelegationSummary);
    result.completed.forEach(expectRedactedDelegationSummary);
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
    const timeoutMs = MIN_DELEGATION_WAIT_INTERVAL_MS * 3;
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
      timeoutMs,
    });

    const assertion = expect(wait).resolves.toMatchObject({
      outcome: "timeout",
      completed: [],
      pending: [{ id: "delegation-1", status: "running" }],
    });

    await vi.advanceTimersByTimeAsync(timeoutMs);

    await assertion;
    const requestedUrls = fetchMock.mock.calls.map(([url]) => String(url));
    expect(requestedUrls.every((url) => !url.endsWith("/cancel"))).toBe(true);
  });

  it("rejects zero polling intervals instead of busy-looping", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        pollIntervalMs: 0,
      }),
    ).rejects.toThrow(/^pollIntervalMs must be a finite positive duration/);
  });

  it("rejects timeout zero with the same positive-duration contract", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        timeoutMs: 0,
      }),
    ).rejects.toThrow(/^timeoutMs must be a finite positive duration/);
  });

  it("rejects polling below the minimum interval", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS - 1,
      }),
    ).rejects.toThrow(
      new RegExp(
        `^pollIntervalMs must be at least ${MIN_DELEGATION_WAIT_INTERVAL_MS}ms`,
      ),
    );
  });

  it("rejects excessive wait timeouts and delegation-id batches", async () => {
    await expect(
      waitDelegationsCommand("parent-1", ["delegation-1"], {
        timeoutMs: MAX_DELEGATION_WAIT_TIMEOUT_MS + 1,
      }),
    ).rejects.toThrow(
      new RegExp(
        `^timeoutMs must be no greater than ${MAX_DELEGATION_WAIT_TIMEOUT_MS}ms`,
      ),
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
      new RegExp(
        `^wait_delegations accepts at most ${MAX_DELEGATION_WAIT_IDS} ids`,
      ),
    );
  });

  it("rejects blank delegation ids instead of returning an empty success", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      waitDelegationCommand("parent-1", " ", {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).rejects.toThrow(/^delegation id must be non-empty/);
    await expect(
      waitDelegationsCommand("parent-1", [" ", "\t"], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).rejects.toThrow(/^delegation id must be non-empty/);
    await expect(
      waitDelegationsCommand("parent-1", [], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).rejects.toThrow(/^wait_delegations requires at least one delegation id/);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    ["slash", "parent/1", "delegation-1"],
    ["question mark", "parent-1", "delegation?1"],
    ["fragment", "parent-1", "delegation#1"],
    ["control character", "parent-1", "delegation\u00011"],
    ["DEL character", "parent-1", "delegation\u007f1"],
  ])("rejects unsafe transport ids before dispatch: %s", async (
    _caseName,
    parentSessionId,
    delegationId,
  ) => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      getDelegationStatusCommand(parentSessionId, delegationId),
    ).rejects.toThrow(
      /must not contain \/, \?, #, or control characters/,
    );
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("returns an error outcome for mismatched delegation ids", async () => {
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
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "mismatched-delegation-id",
        name: "MismatchedDelegationIdError",
        requestedId: "delegation-1",
        receivedId: "other-delegation",
      },
      delegations: [],
      completed: [],
      pending: [],
    });
  });

  it("returns an error outcome for mixed server-instance status batches", async () => {
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
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "mixed-server-instance",
        name: "MixedDelegationServerInstanceError",
        serverInstanceIds: ["server-a", "server-b"],
      },
    });
  });

  it("times out at the deadline while a status fetch is still in flight", async () => {
    vi.useFakeTimers();
    const captured = { signal: null as AbortSignal | null };
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn((_parentSessionId, _delegationId, options) => {
        captured.signal = options?.signal ?? null;
        return new Promise<never>((_resolve, reject) => {
          captured.signal?.addEventListener("abort", () => {
            reject(new DOMException("Aborted", "AbortError"));
          });
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const commands = createDelegationCommands(transport);
    const wait = commands.wait_delegations(
      "parent-1",
      ["delegation-1"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 2 - 1,
      },
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS * 2 - 1);

    await expect(wait).resolves.toMatchObject({
      outcome: "timeout",
      delegations: [],
      completed: [],
      pending: [],
    });
    expectCapturedAbortSignal(captured.signal);
    expect(captured.signal.aborted).toBe(true);
  });

  it("continues polling after a per-batch status timeout when the overall wait has budget", async () => {
    vi.useFakeTimers();
    let callCount = 0;
    let firstBatchAbortedAt: number | null = null;
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn((_parentSessionId, _delegationId, options) => {
        callCount += 1;
        if (callCount === 1) {
          return new Promise<never>((_resolve, reject) => {
            options?.signal?.addEventListener("abort", () => {
              firstBatchAbortedAt = Date.now();
              reject(new DOMException("Aborted", "AbortError"));
            });
          });
        }
        return Promise.resolve({
          revision: 2,
          serverInstanceId: "server-a",
          delegation: makeDelegation({ status: "completed" }),
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const startedAt = Date.now();
    const wait = createDelegationCommands(transport).wait_delegations(
      "parent-1",
      ["delegation-1"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 5,
      },
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS * 2 - 1);
    expect(transport.fetchDelegationStatus).toHaveBeenCalledTimes(1);
    expect(firstBatchAbortedAt).toBeNull();

    await vi.advanceTimersByTimeAsync(1);
    expect(firstBatchAbortedAt).toBe(
      startedAt + MIN_DELEGATION_WAIT_INTERVAL_MS * 2,
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await expect(wait).resolves.toMatchObject({
      outcome: "completed",
      completed: [{ id: "delegation-1", status: "completed" }],
    });
    expect(transport.fetchDelegationStatus).toHaveBeenCalledTimes(2);
  });

  it("keeps fulfilled batch responses and aborts siblings when one status fetch fails", async () => {
    type DelegationStatusTransportResponse = Awaited<
      ReturnType<DelegationCommandTransport["fetchDelegationStatus"]>
    >;
    let resolveFirst:
      | ((response: DelegationStatusTransportResponse) => void)
      | undefined;
    let rejectSecond: ((error: unknown) => void) | undefined;
    let thirdSignal: AbortSignal | null = null;
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn<
        DelegationCommandTransport["fetchDelegationStatus"]
      >((_parentSessionId, delegationId, options) => {
        if (delegationId === "delegation-1") {
          return new Promise<DelegationStatusTransportResponse>((resolve) => {
            resolveFirst = resolve;
          });
        }
        if (delegationId === "delegation-2") {
          return new Promise<DelegationStatusTransportResponse>(
            (_resolve, reject) => {
              rejectSecond = reject;
            },
          );
        }
        thirdSignal = options?.signal ?? null;
        return new Promise<never>((_resolve, reject) => {
          thirdSignal?.addEventListener("abort", () => {
            reject(new DOMException("Aborted", "AbortError"));
          });
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const commands = createDelegationCommands(transport);
    const wait = commands.wait_delegations(
      "parent-1",
      ["delegation-1", "delegation-2", "delegation-3"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
      },
    );

    const resolveFirstStatusFetch = resolveFirst;
    expect(resolveFirstStatusFetch).toBeDefined();
    resolveFirstStatusFetch!({
      revision: 2,
      serverInstanceId: "server-a",
      delegation: makeDelegation({
        id: "delegation-1",
        status: "completed",
      }),
    });
    await Promise.resolve();
    const rejectSecondStatusFetch = rejectSecond;
    expect(rejectSecondStatusFetch).toBeDefined();
    rejectSecondStatusFetch!(
      new ApiRequestError("request-failed", "backend rejected delegation", {
        status: 409,
      }),
    );

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      completed: [{ id: "delegation-1", status: "completed" }],
      pending: [],
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message: "Delegation status fetch failed.",
        apiErrorKind: "request-failed",
        status: 409,
        restartRequired: false,
      },
    });
    const capturedThirdSignal = thirdSignal as AbortSignal | null;
    expectCapturedAbortSignal(capturedThirdSignal);
    expect(capturedThirdSignal.aborted).toBe(true);
  });

  it("prioritizes fetch failure while preserving same-instance batch responses", async () => {
    vi.useFakeTimers();
    type DelegationStatusTransportResponse = Awaited<
      ReturnType<DelegationCommandTransport["fetchDelegationStatus"]>
    >;
    let callCount = 0;
    let rejectThirdSecondBatch: ((error: unknown) => void) | undefined;
    const runningById = (delegationId: string): DelegationStatusTransportResponse => ({
      revision: 2,
      serverInstanceId: "server-a",
      delegation: makeDelegation({
        id: delegationId,
        childSessionId: delegationId.replace("delegation", "child"),
        status: "running",
      }),
    });
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async (_parentSessionId, delegationId) => {
        callCount += 1;
        if (callCount <= 3) {
          return runningById(delegationId);
        }
        if (delegationId === "delegation-1") {
          return {
            revision: 3,
            serverInstanceId: "server-a",
            delegation: makeDelegation({
              id: "delegation-1",
              status: "completed",
            }),
          };
        }
        if (delegationId === "delegation-2") {
          return {
            revision: 4,
            serverInstanceId: "server-b",
            delegation: makeDelegation({
              id: "delegation-2",
              childSessionId: "child-2",
              status: "completed",
            }),
          };
        }
        return new Promise<never>((_resolve, reject) => {
          rejectThirdSecondBatch = reject;
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const wait = createDelegationCommands(transport).wait_delegations(
      "parent-1",
      ["delegation-1", "delegation-2", "delegation-3"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 5,
      },
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);
    await Promise.resolve();
    const rejectThirdStatusFetch = rejectThirdSecondBatch;
    expect(rejectThirdStatusFetch).toBeDefined();
    rejectThirdStatusFetch!(
      new ApiRequestError("backend-unavailable", "The TermAl backend is unavailable."),
    );

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      completed: [{ id: "delegation-1", status: "completed" }],
      pending: [
        { id: "delegation-2", status: "running" },
        { id: "delegation-3", status: "running" },
      ],
      revision: 3,
      serverInstanceId: "server-a",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        apiErrorKind: "backend-unavailable",
      },
    });
  });

  it("redacts unknown transport failure messages from wait error packets", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async () => {
        throw new Error("token=secret C:/internal/backend.log");
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "Error",
        message: "Delegation status fetch failed.",
        apiErrorKind: null,
        status: null,
        restartRequired: null,
      },
    });
  });

  it.each([
    ["token assignment", "token=secret C:/internal/backend.log"],
    ["bearer token", "Authorization: Bearer secret-token"],
    ["env var", "OPENAI_API_KEY=secret-value"],
    ["raw token prefix", "sk-proj-secret-value"],
    ["JWT token", "Authorization: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.signature"],
    ["GitHub PAT", "X-GitHub-Token: ghp_someTokenValue123"],
    ["UNC path", "\\\\server\\share\\backend.log"],
    ["home path", "could not write /home/admin/.config/termal"],
    ["URL", "failed at https://internal.example/log"],
  ])(
    "redacts sensitive ApiRequestError messages from wait error packets: %s",
    async (_caseName, message) => {
      const transport: DelegationCommandTransport = {
        createDelegation: vi.fn(),
        fetchDelegationStatus: vi.fn(async () => {
          throw new ApiRequestError("request-failed", message, { status: 500 });
        }),
        fetchDelegationResult: vi.fn(),
        cancelDelegation: vi.fn(),
      };

      await expect(
        createDelegationCommands(transport).wait_delegations(
          "parent-1",
          ["delegation-1"],
          {
            pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
            timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
          },
        ),
      ).resolves.toMatchObject({
        outcome: "error",
        error: {
          kind: "status-fetch-failed",
          name: "ApiRequestError",
          message: "Delegation status fetch failed.",
          apiErrorKind: "request-failed",
          status: 500,
        },
      });
    },
  );

  it.each([
    ["backend unavailable", "The TermAl backend is unavailable."],
    ["generic status", "Request failed with status 500."],
  ])("passes through allowlisted ApiRequestError messages: %s", async (
    _caseName,
    message,
  ) => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async () => {
        throw new ApiRequestError("request-failed", message, { status: 500 });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message,
        apiErrorKind: "request-failed",
        status: 500,
      },
    });
  });

  it("passes through restart-required backend route diagnostics", async () => {
    const message =
      "The running backend does not expose /api/sessions/parent-1/delegations/delegation-1 (HTTP 404). Restart TermAl so the latest API routes are loaded.";
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async () => {
        throw new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message,
        apiErrorKind: "backend-unavailable",
        status: 404,
        restartRequired: true,
      },
    });
  });

  it("redacts backend-unavailable messages outside audited safe shapes", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async () => {
        throw new ApiRequestError(
          "backend-unavailable",
          "proxy failed at https://internal.example/path with token=secret",
          {
            status: 503,
            restartRequired: true,
          },
        );
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message: "Delegation status fetch failed.",
        apiErrorKind: "backend-unavailable",
        status: 503,
        restartRequired: true,
      },
    });
  });

  it("redacts route-shaped backend-unavailable messages for other delegation ids", async () => {
    const message =
      "The running backend does not expose /api/sessions/token=secret/delegations/delegation-1 (HTTP 404). Restart TermAl so the latest API routes are loaded.";
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async () => {
        throw new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message: "Delegation status fetch failed.",
        apiErrorKind: "backend-unavailable",
        status: 404,
        restartRequired: true,
      },
    });
  });

  it("redacts route-shaped backend-unavailable messages for wrong delegation ids", async () => {
    const message =
      "The running backend does not expose /api/sessions/parent-1/delegations/token=secret (HTTP 404). Restart TermAl so the latest API routes are loaded.";
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async () => {
        throw new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message: "Delegation status fetch failed.",
        apiErrorKind: "backend-unavailable",
        status: 404,
        restartRequired: true,
      },
    });
  });

  it("passes through backend route diagnostics for the failed delegation in a batch", async () => {
    const message =
      "The running backend does not expose /api/sessions/parent-1/delegations/delegation-2 (HTTP 404). Restart TermAl so the latest API routes are loaded.";
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async (_parentSessionId, delegationId) => {
        if (delegationId === "delegation-1") {
          return {
            revision: 2,
            serverInstanceId: "server-a",
            delegation: makeDelegation({ id: "delegation-1", status: "running" }),
          };
        }
        throw new ApiRequestError("backend-unavailable", message, {
          status: 404,
          restartRequired: true,
        });
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).wait_delegations(
        "parent-1",
        ["delegation-1", "delegation-2"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      pending: [{ id: "delegation-1", status: "running" }],
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message,
        apiErrorKind: "backend-unavailable",
        status: 404,
        restartRequired: true,
      },
    });
  });

  it("reports mixed server instances across polling cycles", async () => {
    vi.useFakeTimers();
    stubFetchResponses(
      {
        revision: 2,
        serverInstanceId: "server-a",
        delegation: makeDelegation({ status: "running" }),
      },
      {
        revision: 3,
        serverInstanceId: "server-b",
        delegation: makeDelegation({ status: "completed" }),
      },
    );

    const wait = waitDelegationsCommand("parent-1", ["delegation-1"], {
      pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
      timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
    });

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      pending: [{ id: "delegation-1", status: "running" }],
      error: {
        kind: "mixed-server-instance",
        serverInstanceIds: ["server-a", "server-b"],
      },
    });
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
      timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
    });

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await expect(wait).resolves.toMatchObject({
      outcome: "completed",
      revision: 3,
    });
  });

  it("returns partial state when a later status fetch fails", async () => {
    vi.useFakeTimers();
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi
        .fn<DelegationCommandTransport["fetchDelegationStatus"]>()
        .mockResolvedValueOnce({
          revision: 2,
          serverInstanceId: "server-a",
          delegation: makeDelegation({ status: "running" }),
        })
        .mockRejectedValueOnce(
          new ApiRequestError(
            "backend-unavailable",
            "The TermAl backend is unavailable.",
          ),
        ),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const wait = createDelegationCommands(transport).wait_delegations(
      "parent-1",
      ["delegation-1"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
      },
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "status-fetch-failed",
        name: "ApiRequestError",
        message: "The TermAl backend is unavailable.",
        apiErrorKind: "backend-unavailable",
        status: null,
        restartRequired: false,
      },
      pending: [{ id: "delegation-1", status: "running" }],
    });
    expect(transport.fetchDelegationStatus).toHaveBeenCalledTimes(2);
  });
});
