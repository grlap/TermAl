import { afterEach, describe, expect, expectTypeOf, it, vi } from "vitest";

import { ApiRequestError } from "./api";
import type { SpawnDelegationTransportFailurePacket } from "./delegation-error-packets";
import {
  cancelDelegationCommand,
  createComposerDelegationRequest,
  createDelegationCommands,
  DELEGATION_COMPOSER_TITLE_MAX_CHARS,
  delegationTitleFromPrompt,
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
  resolveComposerDelegationAvailability,
  type DelegationCommandTransport,
  type SpawnReviewerBatchFailure,
  type SpawnReviewerBatchCommandResult,
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

function makeResult(
  overrides: Partial<DelegationResult> = {},
): DelegationResult {
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
  expect(
    value === null || (typeof value === "string" && value.length > 0),
  ).toBe(true);
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
    expect(delegationCommands.resume_after_delegations).toBeTypeOf("function");
    expect(delegationCommands).not.toHaveProperty("wait_delegation");
  });

  it("schedules backend parent resume waits for delegation fan-in", async () => {
    const createDelegationWait = vi.fn<
      NonNullable<DelegationCommandTransport["createDelegationWait"]>
    >(async (parentSessionId, request) => ({
      revision: 42,
      serverInstanceId: "server-a",
      resumePromptQueued: true,
      resumeDispatchRequested: true,
      wait: {
        id: "delegation-wait-1",
        parentSessionId,
        delegationIds: request.delegationIds,
        mode: request.mode ?? "all",
        createdAt: "2026-05-09 10:00:00",
        title: request.title ?? null,
      },
    }));
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
      createDelegationWait,
    };

    await expect(
      createDelegationCommands(transport).resume_after_delegations(
        " parent-1 ",
        [" delegation-1 ", "delegation-1", "delegation-2"],
        { mode: "all", title: "Reviewer fan-in" },
      ),
    ).resolves.toMatchObject({
      revision: 42,
      resumePromptQueued: true,
      resumeDispatchRequested: true,
      wait: {
        parentSessionId: "parent-1",
        delegationIds: ["delegation-1", "delegation-2"],
        mode: "all",
        title: "Reviewer fan-in",
      },
    });
    expect(createDelegationWait).toHaveBeenCalledWith("parent-1", {
      delegationIds: ["delegation-1", "delegation-2"],
      mode: "all",
      title: "Reviewer fan-in",
    });
  });

  it("validates backend resume wait titles before scheduling", async () => {
    const createDelegationWait = vi.fn<
      NonNullable<DelegationCommandTransport["createDelegationWait"]>
    >();
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
      createDelegationWait,
    };

    await expect(
      createDelegationCommands(transport).resume_after_delegations(
        "parent-1",
        ["delegation-1"],
        { title: "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1) },
      ),
    ).rejects.toThrow(
      `title must be no longer than ${MAX_DELEGATION_TITLE_CHARS} characters`,
    );
    expect(createDelegationWait).not.toHaveBeenCalled();
  });

  it.each([
    ["blank prompt", "   \n\t  ", "Delegated review"],
    ["normalized whitespace", "  Review\n\nthis\tchange  ", "Review this change"],
    [
      "exact maximum length",
      "x".repeat(DELEGATION_COMPOSER_TITLE_MAX_CHARS),
      "x".repeat(DELEGATION_COMPOSER_TITLE_MAX_CHARS),
    ],
    [
      "truncated overlong prompt",
      "x".repeat(DELEGATION_COMPOSER_TITLE_MAX_CHARS + 1),
      `${"x".repeat(DELEGATION_COMPOSER_TITLE_MAX_CHARS - 3)}...`,
    ],
  ])("builds composer delegation title for %s", (_label, prompt, expected) => {
    expect(delegationTitleFromPrompt(prompt)).toBe(expected);
  });

  it("does not split surrogate pairs when truncating composer delegation titles", () => {
    const prompt = `${"a".repeat(DELEGATION_COMPOSER_TITLE_MAX_CHARS - 4)}🙂bbbb`;

    expect(delegationTitleFromPrompt(prompt)).toBe(
      `${"a".repeat(DELEGATION_COMPOSER_TITLE_MAX_CHARS - 4)}🙂...`,
    );
  });

  it("builds the default read-only reviewer request for composer delegation", () => {
    const request = createComposerDelegationRequest(
      makeSession({
        agent: "Claude",
        model: "claude-sonnet-4.5",
      }),
      "Review the current diff.",
    );

    expect(request).toEqual({
      title: "Review the current diff.",
      prompt: "Review the current diff.",
      agent: "Claude",
      model: "claude-sonnet-4.5",
      mode: "reviewer",
      writePolicy: { kind: "readOnly" },
    });
  });

  it("allows composer delegation callers to request isolated worktree review", () => {
    const request = createComposerDelegationRequest(
      makeSession({
        agent: "Codex",
        model: "gpt-5.5",
      }),
      "Run /review-local.",
      { writePolicy: { kind: "isolatedWorktree", ownedPaths: [] } },
    );

    expect(request).toMatchObject({
      prompt: "Run /review-local.",
      agent: "Codex",
      model: "gpt-5.5",
      mode: "reviewer",
      writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
    });
  });

  it("preserves composer delegation mode overrides from resolved commands", () => {
    const request = createComposerDelegationRequest(
      makeSession({
        agent: "Codex",
        model: "gpt-5.5",
      }),
      "Explore the command resolver.",
      { mode: "explorer", title: "Resolver exploration" },
    );

    expect(request).toMatchObject({
      title: "Resolver exploration",
      prompt: "Explore the command resolver.",
      model: "gpt-5.5",
      mode: "explorer",
      writePolicy: { kind: "readOnly" },
    });
  });

  it.each([
    [
      "local project session",
      makeSession({
        id: "parent-1",
        projectId: "project-1",
      }),
      { remoteId: "local" },
      { outcome: "available" },
    ],
    [
      "missing parent session",
      null,
      null,
      {
        outcome: "error",
        message: "Session is no longer available.",
      },
    ],
    [
      "missing loaded project",
      makeSession({
        id: "parent-1",
        projectId: "project-1",
      }),
      null,
      {
        outcome: "error",
        message: "Delegations are unavailable until the session project is loaded.",
      },
    ],
    [
      "remote project session",
      makeSession({
        id: "parent-1",
        projectId: "project-1",
      }),
      { remoteId: "ssh-lab" },
      {
        outcome: "error",
        message: "Delegations are available only for local sessions.",
      },
    ],
    [
      "remote session without project",
      makeSession({
        id: "remote-parent",
        projectId: null,
        remoteId: "ssh-lab",
      }),
      null,
      {
        outcome: "error",
        message: "Delegations are available only for local sessions.",
      },
    ],
  ] satisfies Array<
    [
      string,
      Session | null,
      { remoteId: string } | null,
      ReturnType<typeof resolveComposerDelegationAvailability>,
    ]
  >)("resolves composer delegation availability for %s", (
    _label,
    parentSession,
    parentProject,
    expected,
  ) => {
    expect(
      resolveComposerDelegationAvailability(parentSession, parentProject),
    ).toEqual(expected);
  });

  it("uses caller-held parent session when composer delegation is available", () => {
    const localSession = makeSession({
      id: "parent-1",
      projectId: "project-1",
      agent: "Claude",
      model: "claude-sonnet-4.5",
    });
    const availability = resolveComposerDelegationAvailability(localSession, {
      remoteId: "local",
    });

    expect(availability).toEqual({ outcome: "available" });
    expect(
      createComposerDelegationRequest(localSession, "Review the current diff."),
    ).toMatchObject({
      agent: "Claude",
      model: "claude-sonnet-4.5",
    });
  });

  it("keeps wait and spawn-batch results discriminated by outcome", () => {
    type ErrorResult = Extract<WaitDelegationsResult, { outcome: "error" }>;
    type NonErrorResult = Extract<
      WaitDelegationsResult,
      { outcome: "completed" | "timeout" }
    >;
    type SpawnBatchErrorResult = Extract<
      SpawnReviewerBatchCommandResult,
      { outcome: "error" }
    >;
    type SpawnBatchNonErrorResult = Extract<
      SpawnReviewerBatchCommandResult,
      { outcome: "completed" | "partial" }
    >;

    expectTypeOf<ErrorResult>().toMatchTypeOf<{
      outcome: "error";
      error: WaitDelegationErrorPacket;
    }>();
    expectTypeOf<NonErrorResult>().toMatchTypeOf<{
      outcome: "completed" | "timeout";
      error?: never;
    }>();
    expectTypeOf<SpawnBatchErrorResult>().toMatchTypeOf<{
      outcome: "error";
      error: unknown;
    }>();
    expectTypeOf<SpawnBatchNonErrorResult>().toMatchTypeOf<{
      outcome: "completed" | "partial";
      error?: never;
    }>();
    expectTypeOf<
      SpawnReviewerBatchFailure["kind"]
    >().toEqualTypeOf<"spawn-failed">();
    expectTypeOf<SpawnReviewerBatchFailure>().toEqualTypeOf<
      SpawnDelegationTransportFailurePacket & {
        index: number;
        title: string | null;
      }
    >();
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
    if (result.outcome !== "completed") {
      throw new Error(`unexpected spawn outcome: ${result.outcome}`);
    }

    expect(result).toMatchObject({
      outcome: "completed",
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

  it("returns validation packets for invalid spawn requests before dispatch", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      spawnDelegationCommand("parent/1", { prompt: "Review this change." }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message:
          "parent session id must not contain /, ?, #, or control characters",
      },
    });
    await expect(
      spawnDelegationCommand(42 as never, { prompt: "Review this change." }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "parent session id must be a string",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", { prompt: "   " }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: "prompt must be non-empty",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", { prompt: 42 as never }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "prompt must be a string",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "x".repeat(MAX_DELEGATION_PROMPT_BYTES + 1),
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: `prompt must be no larger than ${MAX_DELEGATION_PROMPT_BYTES} bytes`,
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "界".repeat(Math.floor(MAX_DELEGATION_PROMPT_BYTES / 2)),
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: `prompt must be no larger than ${MAX_DELEGATION_PROMPT_BYTES} bytes`,
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        title: "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1),
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: `title must be no longer than ${MAX_DELEGATION_TITLE_CHARS} characters`,
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        model: "x".repeat(MAX_DELEGATION_MODEL_CHARS + 1),
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: `model must be no longer than ${MAX_DELEGATION_MODEL_CHARS} characters`,
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        title: null as never,
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "title must be omitted instead of null",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        cwd: null as never,
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "cwd must be omitted instead of null",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        agent: null as never,
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "agent must be omitted instead of null",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        model: null as never,
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "model must be omitted instead of null",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        mode: null as never,
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "mode must be omitted instead of null",
      },
    });
    await expect(
      spawnDelegationCommand("parent-1", {
        prompt: "Review this change.",
        writePolicy: null as never,
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "writePolicy must be omitted instead of null",
      },
    });
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
          title: title.trim(),
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
    });
  });

  it("can schedule a backend resume wait after reviewer batch fan-out", async () => {
    const createDelegationWait = vi.fn<
      NonNullable<DelegationCommandTransport["createDelegationWait"]>
    >(async (parentSessionId, request) => ({
      revision: 8,
      serverInstanceId: "server-a",
      resumePromptQueued: true,
      resumeDispatchRequested: true,
      wait: {
        id: "delegation-wait-1",
        parentSessionId,
        delegationIds: request.delegationIds,
        mode: request.mode ?? "all",
        createdAt: "2026-05-09 10:00:00",
        title: request.title ?? null,
      },
    }));
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => {
        const title = request.title ?? "Review";
        const isReact = title === "React review";
        return {
          revision: isReact ? 3 : 4,
          serverInstanceId: "server-a",
          delegation: makeDelegation({
            id: isReact ? "delegation-1" : "delegation-2",
            childSessionId: isReact ? "child-1" : "child-2",
            title,
          }),
          childSession: makeSession({
            id: isReact ? "child-1" : "child-2",
            name: title,
            parentDelegationId: isReact ? "delegation-1" : "delegation-2",
          }),
        };
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
      createDelegationWait,
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch(
        "parent-1",
        [
          { prompt: "Review React.", title: "React review" },
          { prompt: "Review Rust.", title: "Rust review" },
        ],
        { mode: "all", title: "Reviewer fan-in" },
      ),
    ).resolves.toMatchObject({
      outcome: "completed",
      delegationIds: ["delegation-1", "delegation-2"],
      revision: 8,
      serverInstanceId: "server-a",
      resumeWait: {
        outcome: "scheduled",
        resumePromptQueued: true,
        resumeDispatchRequested: true,
        revision: 8,
        serverInstanceId: "server-a",
        wait: {
          id: "delegation-wait-1",
          parentSessionId: "parent-1",
          delegationIds: ["delegation-1", "delegation-2"],
          mode: "all",
          title: "Reviewer fan-in",
        },
      },
    });
    expect(createDelegationWait).toHaveBeenCalledWith("parent-1", {
      delegationIds: ["delegation-1", "delegation-2"],
      mode: "all",
      title: "Reviewer fan-in",
    });
  });

  it("reports resume wait scheduling failures without hiding spawned reviewers", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async () => ({
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation(),
        childSession: makeSession({ parentDelegationId: "delegation-1" }),
      })),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
      createDelegationWait: vi.fn(async () => {
        throw new ApiRequestError(
          "backend-unavailable",
          "The TermAl backend is unavailable.",
          { status: 503, restartRequired: true },
        );
      }),
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch(
        "parent-1",
        [{ prompt: "Review React.", title: "React review" }],
        { mode: "all" },
      ),
    ).resolves.toMatchObject({
      outcome: "completed",
      delegationIds: ["delegation-1"],
      revision: 3,
      serverInstanceId: "server-a",
      resumeWait: {
        outcome: "error",
        error: {
          kind: "resume-wait-failed",
          message: "The TermAl backend is unavailable.",
          status: 503,
          restartRequired: true,
        },
      },
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
          kind: "spawn-failed",
          apiErrorKind: "request-failed",
          status: 409,
          restartRequired: false,
        },
      ],
      revision: 3,
      serverInstanceId: "server-a",
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
          kind: "spawn-failed",
          name: "ApiRequestError",
          message: "Spawn delegation failed.",
          apiErrorKind: "request-failed",
          status: 500,
          restartRequired: false,
        },
      ],
      revision: null,
      serverInstanceId: null,
      error: {
        kind: "all-spawns-failed",
        name: "SpawnReviewerBatchError",
        message: "all reviewer spawns failed",
      },
    });
  });

  it("returns a sanitized error packet for single spawn transport failures", async () => {
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
      createDelegationCommands(transport).spawn_delegation("parent-1", {
        prompt: "Review React.",
        title: "React review",
      }),
    ).resolves.toMatchObject({
      outcome: "error",
      revision: null,
      serverInstanceId: null,
      error: {
        kind: "spawn-failed",
        name: "ApiRequestError",
        message: "Spawn delegation failed.",
        apiErrorKind: "request-failed",
        status: 500,
        restartRequired: false,
      },
    });
  });

  it("redacts non-ApiRequestError spawn failures", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async () => {
        throw new TypeError("network down: file://internal/path");
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
      failed: [
        {
          kind: "spawn-failed",
          name: "TypeError",
          message: "Spawn delegation failed.",
          apiErrorKind: null,
          status: null,
          restartRequired: null,
        },
      ],
      error: { kind: "all-spawns-failed" },
    });
  });

  it("preserves restartRequired on spawn failure packets", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async () => {
        throw new ApiRequestError(
          "backend-unavailable",
          "The TermAl backend is unavailable.",
          { status: 503, restartRequired: true },
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
      failed: [
        {
          kind: "spawn-failed",
          message: "The TermAl backend is unavailable.",
          apiErrorKind: "backend-unavailable",
          status: 503,
          restartRequired: true,
        },
      ],
    });
  });

  it("returns all-spawns-failed when every reviewer spawn fails", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => {
        throw new ApiRequestError(
          "request-failed",
          request.title === "React review"
            ? "session not found"
            : "parent session already has 4 active delegations",
          { status: request.title === "React review" ? 404 : 409 },
        );
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
      spawned: [],
      failed: [
        {
          index: 0,
          title: "React review",
          kind: "spawn-failed",
          message: "session not found",
          status: 404,
        },
        {
          index: 1,
          title: "Rust review",
          kind: "spawn-failed",
          message: "parent session already has 4 active delegations",
          status: 409,
        },
      ],
      error: {
        kind: "all-spawns-failed",
      },
    });
  });

  it.each([
    ["parent session id is required"],
    ["session not found"],
    ["unknown project `project-1`"],
    ["delegation cwd `C:\\repo\\outside` must stay inside project `TermAl`"],
  ])(
    "passes through audited deterministic spawn errors: %s",
    async (message) => {
      const transport: DelegationCommandTransport = {
        createDelegation: vi.fn(async () => {
          throw new ApiRequestError("request-failed", message, { status: 400 });
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
        failed: [
          {
            kind: "spawn-failed",
            name: "ApiRequestError",
            message,
            apiErrorKind: "request-failed",
            status: 400,
          },
        ],
        error: { kind: "all-spawns-failed" },
      });
    },
  );

  it("returns a batch error when reviewer spawns cross backend instances", async () => {
    const responseByTitle = new Map([
      ["Z review", { index: 1, serverInstanceId: "server-z", revision: 3 }],
      ["A review", { index: 2, serverInstanceId: "server-a", revision: 7 }],
      ["M review", { index: 3, serverInstanceId: "server-m", revision: 5 }],
      ["Z follow-up", { index: 4, serverInstanceId: "server-z", revision: 9 }],
    ]);
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => {
        const title = request.title ?? "";
        const response = responseByTitle.get(title);
        if (!response) {
          throw new Error(`unexpected reviewer title: ${title}`);
        }
        return {
          revision: response.revision,
          serverInstanceId: response.serverInstanceId,
          delegation: makeDelegation({
            id: `delegation-${response.index}`,
            childSessionId: `child-${response.index}`,
            title,
          }),
          childSession: makeSession({
            id: `child-${response.index}`,
            name: title,
            parentDelegationId: `delegation-${response.index}`,
          }),
        };
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch("parent-1", [
        { prompt: "Review Z.", title: "Z review" },
        { prompt: "Review A.", title: "A review" },
        { prompt: "Review M.", title: "M review" },
        { prompt: "Review Z again.", title: "Z follow-up" },
      ]),
    ).resolves.toMatchObject({
      outcome: "error",
      delegationIds: [
        "delegation-1",
        "delegation-2",
        "delegation-3",
        "delegation-4",
      ],
      failed: [],
      revision: null,
      serverInstanceId: null,
      error: {
        kind: "mixed-server-instance",
        message:
          "delegation spawn batch contained multiple server instances: server-a, server-m, server-z",
        serverInstanceIds: ["server-a", "server-m", "server-z"],
        recoveryGroups: [
          {
            serverInstanceId: "server-a",
            revision: 7,
            delegationIds: ["delegation-2"],
            childSessionIds: ["child-2"],
          },
          {
            serverInstanceId: "server-m",
            revision: 5,
            delegationIds: ["delegation-3"],
            childSessionIds: ["child-3"],
          },
          {
            serverInstanceId: "server-z",
            revision: 9,
            delegationIds: ["delegation-1", "delegation-4"],
            childSessionIds: ["child-1", "child-4"],
          },
        ],
      },
    });
  });

  it("deduplicates duplicate spawn recovery ids while preserving max revision", async () => {
    const responseByTitle = new Map([
      ["Duplicate low", { serverInstanceId: "server-z", revision: 3 }],
      ["Other instance", { serverInstanceId: "server-a", revision: 5 }],
      ["Duplicate high", { serverInstanceId: "server-z", revision: 9 }],
    ]);
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(async (_parentSessionId, request) => {
        const title = request.title ?? "";
        const response = responseByTitle.get(title);
        if (!response) {
          throw new Error(`unexpected reviewer title: ${title}`);
        }
        const isDuplicate = title.startsWith("Duplicate");
        return {
          revision: response.revision,
          serverInstanceId: response.serverInstanceId,
          delegation: makeDelegation({
            id: isDuplicate ? "delegation-dup" : "delegation-other",
            childSessionId: isDuplicate ? "child-dup" : "child-other",
            title,
          }),
          childSession: makeSession({
            id: isDuplicate ? "child-dup" : "child-other",
            name: title,
            parentDelegationId: isDuplicate
              ? "delegation-dup"
              : "delegation-other",
          }),
        };
      }),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    await expect(
      createDelegationCommands(transport).spawn_reviewer_batch("parent-1", [
        { prompt: "Review duplicate low.", title: "Duplicate low" },
        { prompt: "Review other.", title: "Other instance" },
        { prompt: "Review duplicate high.", title: "Duplicate high" },
      ]),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "mixed-server-instance",
        recoveryGroups: [
          {
            serverInstanceId: "server-a",
            revision: 5,
            delegationIds: ["delegation-other"],
            childSessionIds: ["child-other"],
          },
          {
            serverInstanceId: "server-z",
            revision: 9,
            delegationIds: ["delegation-dup"],
            childSessionIds: ["child-dup"],
          },
        ],
      },
    });
  });

  it("returns validation packets for invalid reviewer batches before dispatch", async () => {
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };
    const commands = createDelegationCommands(transport);

    await expect(
      commands.spawn_reviewer_batch("", [
        { prompt: "Review React.", title: "React review" },
      ]),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: "parent session id must be non-empty",
      },
    });
    await expect(
      commands.spawn_reviewer_batch("parent-1", []),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: "spawn_reviewer_batch requires at least one reviewer",
      },
    });
    await expect(
      commands.spawn_reviewer_batch(
        "parent-1",
        Array.from({ length: MAX_REVIEWER_BATCH_SIZE + 1 }, (_, index) => ({
          prompt: `Review ${index + 1}.`,
          title: `Reviewer ${index + 1}`,
        })),
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: `spawn_reviewer_batch accepts at most ${MAX_REVIEWER_BATCH_SIZE} reviewers`,
      },
    });
    await expect(
      commands.spawn_reviewer_batch("parent-1", [
        { prompt: "   ", title: "Empty prompt" },
      ]),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "RangeError",
        message: "prompt must be non-empty",
      },
    });
    await expect(
      commands.spawn_reviewer_batch("parent-1", null as never),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "spawn_reviewer_batch requests must be an array",
      },
    });
    await expect(
      commands.spawn_reviewer_batch("parent-1", "not-an-array" as never),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "spawn_reviewer_batch requests must be an array",
      },
    });
    await expect(
      commands.spawn_reviewer_batch("parent-1", [null as never]),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "reviewer request 1 must be an object",
      },
    });
    await expect(
      commands.spawn_reviewer_batch("parent-1", [42 as never]),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "validation-failed",
        name: "TypeError",
        message: "reviewer request 1 must be an object",
      },
    });
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
    if (spawnResult.outcome !== "completed") {
      throw new Error(`unexpected spawn outcome: ${spawnResult.outcome}`);
    }
    expect(spawnResult).toMatchObject({
      outcome: "completed",
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

  it("pins wait validation throw messages for wrapper diagnostics", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);
    const excessiveIds = Array.from(
      { length: MAX_DELEGATION_WAIT_IDS + 1 },
      (_, index) => `delegation-${index + 1}`,
    );
    const cases: {
      run: () => Promise<WaitDelegationsResult>;
      message: string;
    }[] = [
      {
        run: () =>
          waitDelegationsCommand(123 as unknown as string, ["delegation-1"]),
        message: "parent session id must be a string",
      },
      {
        run: () => waitDelegationsCommand(" ", ["delegation-1"]),
        message: "parent session id must be non-empty",
      },
      {
        run: () => waitDelegationsCommand("parent/1", ["delegation-1"]),
        message:
          "parent session id must not contain /, ?, #, or control characters",
      },
      {
        run: () =>
          waitDelegationsCommand(
            "parent-1",
            "abc" as unknown as readonly string[],
          ),
        message: "delegation ids must be an array",
      },
      {
        run: () =>
          waitDelegationsCommand("parent-1", [123 as unknown as string]),
        message: "delegation id must be a string",
      },
      {
        run: () => waitDelegationsCommand("parent-1", [" "]),
        message: "delegation id must be non-empty",
      },
      {
        run: () => waitDelegationsCommand("parent-1", ["delegation/1"]),
        message:
          "delegation id must not contain /, ?, #, or control characters",
      },
      {
        run: () => waitDelegationsCommand("parent-1", []),
        message: "wait_delegations requires at least one delegation id",
      },
      {
        run: () => waitDelegationsCommand("parent-1", excessiveIds),
        message: `wait_delegations accepts at most ${MAX_DELEGATION_WAIT_IDS} ids`,
      },
      {
        run: () =>
          waitDelegationsCommand("parent-1", ["delegation-1"], {
            pollIntervalMs: Number.NaN,
          }),
        message: "pollIntervalMs must be a finite positive duration",
      },
      {
        run: () =>
          waitDelegationsCommand("parent-1", ["delegation-1"], {
            timeoutMs: Number.NaN,
          }),
        message: "timeoutMs must be a finite positive duration",
      },
      {
        run: () =>
          waitDelegationsCommand("parent-1", ["delegation-1"], {
            pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS - 1,
          }),
        message: `pollIntervalMs must be at least ${MIN_DELEGATION_WAIT_INTERVAL_MS}ms`,
      },
      {
        run: () =>
          waitDelegationsCommand("parent-1", ["delegation-1"], {
            timeoutMs: MAX_DELEGATION_WAIT_TIMEOUT_MS + 1,
          }),
        message: `timeoutMs must be no greater than ${MAX_DELEGATION_WAIT_TIMEOUT_MS}ms`,
      },
    ];

    for (const { run, message } of cases) {
      await expect(run()).rejects.toThrow(message);
    }
    expect(fetchMock).not.toHaveBeenCalled();
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

  it("rejects non-array delegation id batches before dispatch", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      waitDelegationsCommand(
        "parent-1",
        "abc" as unknown as readonly string[],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: 100,
        },
      ),
    ).rejects.toThrow(/^delegation ids must be an array/);
    await expect(
      waitDelegationsCommand("parent-1", null as unknown as readonly string[], {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: 100,
      }),
    ).rejects.toThrow(/^delegation ids must be an array/);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    ["slash", "parent/1", "delegation-1"],
    ["question mark", "parent-1", "delegation?1"],
    ["fragment", "parent-1", "delegation#1"],
    ["control character", "parent-1", "delegation\u00011"],
    ["DEL character", "parent-1", "delegation\u007f1"],
  ])(
    "rejects unsafe transport ids before dispatch: %s",
    async (_caseName, parentSessionId, delegationId) => {
      const fetchMock = vi.fn<typeof fetch>();
      vi.stubGlobal("fetch", fetchMock);

      await expect(
        getDelegationStatusCommand(parentSessionId, delegationId),
      ).rejects.toThrow(/must not contain \/, \?, #, or control characters/);
      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

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
      {
        revision: 5,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-3",
          childSessionId: "child-3",
          status: "completed",
        }),
      },
    );

    await expect(
      waitDelegationsCommand(
        "parent-1",
        ["delegation-1", "delegation-2", "delegation-3"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: 100,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "mixed-server-instance",
        name: "MixedDelegationServerInstanceError",
        message:
          "delegation status batch contained multiple server instances: server-a, server-b",
        serverInstanceIds: ["server-a", "server-b"],
        recoveryGroups: [
          {
            serverInstanceId: "server-a",
            revision: 5,
            delegationIds: ["delegation-1", "delegation-3"],
            childSessionIds: ["child-1", "child-3"],
          },
          {
            serverInstanceId: "server-b",
            revision: 3,
            delegationIds: ["delegation-2"],
            childSessionIds: ["child-2"],
          },
        ],
      },
    });
  });

  it("sorts three-instance status recovery groups by request order", async () => {
    stubFetchResponses(
      {
        revision: 2,
        serverInstanceId: "server-z",
        delegation: makeDelegation({
          id: "delegation-1",
          status: "completed",
        }),
      },
      {
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "completed",
        }),
      },
      {
        revision: 4,
        serverInstanceId: "server-m",
        delegation: makeDelegation({
          id: "delegation-3",
          childSessionId: "child-3",
          status: "completed",
        }),
      },
    );

    await expect(
      waitDelegationsCommand(
        "parent-1",
        ["delegation-1", "delegation-2", "delegation-3"],
        {
          pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
          timeoutMs: 100,
        },
      ),
    ).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "mixed-server-instance",
        serverInstanceIds: ["server-a", "server-m", "server-z"],
        recoveryGroups: [
          {
            serverInstanceId: "server-z",
            revision: 2,
            delegationIds: ["delegation-1"],
            childSessionIds: ["child-1"],
          },
          {
            serverInstanceId: "server-a",
            revision: 3,
            delegationIds: ["delegation-2"],
            childSessionIds: ["child-2"],
          },
          {
            serverInstanceId: "server-m",
            revision: 4,
            delegationIds: ["delegation-3"],
            childSessionIds: ["child-3"],
          },
        ],
      },
    });
  });

  it("times out at the deadline while a status fetch is still in flight", async () => {
    vi.useFakeTimers();
    const captured = { signal: null as AbortSignal | null };
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(
        (_parentSessionId, _delegationId, options) => {
          captured.signal = options?.signal ?? null;
          return new Promise<never>((_resolve, reject) => {
            captured.signal?.addEventListener("abort", () => {
              reject(new DOMException("Aborted", "AbortError"));
            });
          });
        },
      ),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const commands = createDelegationCommands(transport);
    const wait = commands.wait_delegations("parent-1", ["delegation-1"], {
      pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
      timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 2 - 1,
    });

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
      fetchDelegationStatus: vi.fn(
        (_parentSessionId, _delegationId, options) => {
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
        },
      ),
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
    const runningById = (
      delegationId: string,
    ): DelegationStatusTransportResponse => ({
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
      new ApiRequestError(
        "backend-unavailable",
        "The TermAl backend is unavailable.",
      ),
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
    [
      "JWT token",
      "Authorization: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.signature",
    ],
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
  ])(
    "passes through allowlisted ApiRequestError messages: %s",
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
          message,
          apiErrorKind: "request-failed",
          status: 500,
        },
      });
    },
  );

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
            delegation: makeDelegation({
              id: "delegation-1",
              status: "running",
            }),
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
        recoveryGroups: [
          {
            serverInstanceId: "server-a",
            revision: 2,
            delegationIds: ["delegation-1"],
            childSessionIds: ["child-1"],
          },
          {
            serverInstanceId: "server-b",
            revision: 3,
            delegationIds: ["delegation-1"],
            childSessionIds: ["child-1"],
          },
        ],
      },
    });
  });

  it("scopes previous recovery groups to the delegations fetched in the mixed status poll", async () => {
    vi.useFakeTimers();
    stubFetchResponses(
      {
        revision: 5,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-1",
          status: "running",
        }),
      },
      {
        revision: 3,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-2",
          childSessionId: "child-2",
          status: "completed",
        }),
      },
      {
        revision: 7,
        serverInstanceId: "server-a",
        delegation: makeDelegation({
          id: "delegation-3",
          childSessionId: "child-3",
          status: "running",
        }),
      },
      {
        revision: 8,
        serverInstanceId: "server-b",
        delegation: makeDelegation({
          id: "delegation-1",
          status: "completed",
        }),
      },
      {
        revision: 9,
        serverInstanceId: "server-b",
        delegation: makeDelegation({
          id: "delegation-3",
          childSessionId: "child-3",
          status: "completed",
        }),
      },
    );

    const wait = waitDelegationsCommand(
      "parent-1",
      ["delegation-1", "delegation-2", "delegation-3"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
      },
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      completed: [{ id: "delegation-2", status: "completed" }],
      pending: [
        { id: "delegation-1", status: "running" },
        { id: "delegation-3", status: "running" },
      ],
      error: {
        kind: "mixed-server-instance",
        serverInstanceIds: ["server-a", "server-b"],
        recoveryGroups: [
          {
            serverInstanceId: "server-a",
            revision: 7,
            delegationIds: ["delegation-1", "delegation-3"],
            childSessionIds: ["child-1", "child-3"],
          },
          {
            serverInstanceId: "server-b",
            revision: 9,
            delegationIds: ["delegation-1", "delegation-3"],
            childSessionIds: ["child-1", "child-3"],
          },
        ],
      },
    });
  });

  it("orders mixed-instance recovery group ids by the requested delegation order", async () => {
    type DelegationStatusTransportResponse = Awaited<
      ReturnType<DelegationCommandTransport["fetchDelegationStatus"]>
    >;
    const pendingResponses = new Map<
      string,
      ReturnType<typeof deferred<DelegationStatusTransportResponse>>
    >();
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn(async (_parentSessionId, delegationId) => {
        const pending = deferred<DelegationStatusTransportResponse>();
        pendingResponses.set(delegationId, pending);
        return pending.promise;
      }),
      fetchDelegationResult: vi.fn(),
      cancelDelegation: vi.fn(),
    };

    const wait = createDelegationCommands(transport).wait_delegations(
      "parent-1",
      ["delegation-1", "delegation-2", "delegation-3"],
      {
        pollIntervalMs: MIN_DELEGATION_WAIT_INTERVAL_MS,
        timeoutMs: MIN_DELEGATION_WAIT_INTERVAL_MS * 3,
      },
    );

    expect(transport.fetchDelegationStatus).toHaveBeenCalledTimes(3);
    pendingResponses.get("delegation-3")?.resolve({
      revision: 6,
      serverInstanceId: "server-b",
      delegation: makeDelegation({
        id: "delegation-3",
        childSessionId: "child-3",
        status: "running",
      }),
    });
    pendingResponses.get("delegation-1")?.resolve({
      revision: 5,
      serverInstanceId: "server-b",
      delegation: makeDelegation({
        id: "delegation-1",
        childSessionId: "child-1",
        status: "running",
      }),
    });
    pendingResponses.get("delegation-2")?.resolve({
      revision: 4,
      serverInstanceId: "server-a",
      delegation: makeDelegation({
        id: "delegation-2",
        childSessionId: "child-2",
        status: "running",
      }),
    });

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      error: {
        kind: "mixed-server-instance",
        recoveryGroups: [
          {
            serverInstanceId: "server-b",
            delegationIds: ["delegation-1", "delegation-3"],
            childSessionIds: ["child-1", "child-3"],
          },
          {
            serverInstanceId: "server-a",
            delegationIds: ["delegation-2"],
            childSessionIds: ["child-2"],
          },
        ],
      },
    });
  });

  it("keeps prior-instance recovery groups ordered and aligned after an out-of-order prior poll", async () => {
    vi.useFakeTimers();
    type DelegationStatusTransportResponse = Awaited<
      ReturnType<DelegationCommandTransport["fetchDelegationStatus"]>
    >;
    const statusCalls: {
      delegationId: string;
      pending: ReturnType<typeof deferred<DelegationStatusTransportResponse>>;
    }[] = [];
    const statusResponse = (
      delegationId: string,
      serverInstanceId: string,
      revision: number,
    ): DelegationStatusTransportResponse => ({
      revision,
      serverInstanceId,
      delegation: makeDelegation({
        id: delegationId,
        childSessionId: delegationId.replace("delegation-", "child-"),
        status: "running",
      }),
    });
    const resolveStatus = (
      calls: typeof statusCalls,
      delegationId: string,
      response: DelegationStatusTransportResponse,
    ) => {
      const call = calls.find(
        (candidate) => candidate.delegationId === delegationId,
      );
      expect(call).toBeDefined();
      call?.pending.resolve(response);
    };
    const transport: DelegationCommandTransport = {
      createDelegation: vi.fn(),
      fetchDelegationStatus: vi.fn((_parentSessionId, delegationId) => {
        const pending = deferred<DelegationStatusTransportResponse>();
        statusCalls.push({ delegationId, pending });
        return pending.promise;
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

    expect(statusCalls.map((call) => call.delegationId)).toEqual([
      "delegation-1",
      "delegation-2",
      "delegation-3",
    ]);
    const firstPoll = statusCalls.slice(0, 3);
    resolveStatus(
      firstPoll,
      "delegation-3",
      statusResponse("delegation-3", "server-a", 30),
    );
    resolveStatus(
      firstPoll,
      "delegation-1",
      statusResponse("delegation-1", "server-a", 10),
    );
    resolveStatus(
      firstPoll,
      "delegation-2",
      statusResponse("delegation-2", "server-a", 20),
    );

    await vi.advanceTimersByTimeAsync(MIN_DELEGATION_WAIT_INTERVAL_MS);
    expect(statusCalls.map((call) => call.delegationId)).toEqual([
      "delegation-1",
      "delegation-2",
      "delegation-3",
      "delegation-1",
      "delegation-2",
      "delegation-3",
    ]);

    const secondPoll = statusCalls.slice(3);
    resolveStatus(
      secondPoll,
      "delegation-3",
      statusResponse("delegation-3", "server-b", 60),
    );
    resolveStatus(
      secondPoll,
      "delegation-1",
      statusResponse("delegation-1", "server-b", 40),
    );
    resolveStatus(
      secondPoll,
      "delegation-2",
      statusResponse("delegation-2", "server-b", 50),
    );

    await expect(wait).resolves.toMatchObject({
      outcome: "error",
      pending: [
        { id: "delegation-1", childSessionId: "child-1" },
        { id: "delegation-2", childSessionId: "child-2" },
        { id: "delegation-3", childSessionId: "child-3" },
      ],
      error: {
        kind: "mixed-server-instance",
        recoveryGroups: [
          {
            serverInstanceId: "server-a",
            revision: 30,
            delegationIds: ["delegation-1", "delegation-2", "delegation-3"],
            childSessionIds: ["child-1", "child-2", "child-3"],
          },
          {
            serverInstanceId: "server-b",
            revision: 60,
            delegationIds: ["delegation-1", "delegation-2", "delegation-3"],
            childSessionIds: ["child-1", "child-2", "child-3"],
          },
        ],
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
