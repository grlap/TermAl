// Owns browser worker-base transport, summary and safe-error regressions.
// Does not own backend worktree creation or live-provider approval behavior.
// Split from the delegation command/error test surfaces for the worker contract.
import { afterEach, describe, expect, it, vi } from "vitest";

import { createDelegation, type CreateDelegationRequest } from "./api";
import { ApiRequestError } from "./api-request";
import {
  getDelegationStatusCommand,
  spawnDelegationCommand,
} from "./delegation-commands";
import { spawnDelegationFailurePacket } from "./delegation-error-packets";
import type { DelegationRecord, Session } from "./types";

const BASE_REVISION = "1234567890abcdef1234567890abcdef12345678";
const WORKER_REQUEST: CreateDelegationRequest = {
  prompt: "Implement the requested change.",
  mode: "worker",
  writePolicy: {
    kind: "isolatedWorktree",
    ownedPaths: [],
    worktreePath: "C:/workspace/worker",
    base: "worker-base",
  },
};

function stubWorkerResponse(baseRevision?: string) {
  const delegation: DelegationRecord = {
    id: "delegation-worker",
    parentSessionId: "parent-1",
    childSessionId: "worker-1",
    mode: "worker",
    status: "running",
    title: "Worker",
    prompt: WORKER_REQUEST.prompt,
    cwd: "C:/workspace/worker",
    agent: "Codex",
    writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
    baseRevision,
    createdAt: "2026-10-17 04:00:00",
    reviewResultRequired: false,
  };
  const childSession: Session = {
    id: "worker-1",
    name: "Worker",
    emoji: "AI",
    agent: "Codex",
    workdir: "C:/workspace/worker",
    model: "codex",
    status: "active",
    preview: "",
    messages: [],
    messagesLoaded: false,
  };
  const fetchMock = vi.fn<typeof fetch>(async () =>
    new Response(
      JSON.stringify({
        revision: 2,
        serverInstanceId: "worker-server",
        delegation,
        childSession,
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    ),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("worker delegation browser contract", () => {
  it("serializes an explicit base through the typed create API", async () => {
    const fetchMock = stubWorkerResponse(BASE_REVISION);
    await createDelegation("parent-1", WORKER_REQUEST);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("/api/sessions/parent-1/delegations");
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual(WORKER_REQUEST);
  });

  it.each([BASE_REVISION, undefined])(
    "preserves optional baseRevision in spawn and status summaries: %s",
    async (baseRevision) => {
      const fetchMock = stubWorkerResponse(baseRevision);
      const spawned = await spawnDelegationCommand("parent-1", WORKER_REQUEST);
      if (spawned.outcome !== "completed") {
        throw new Error(`unexpected spawn outcome: ${spawned.outcome}`);
      }
      const status = await getDelegationStatusCommand(
        "parent-1",
        "delegation-worker",
      );
      for (const summary of [spawned.delegation, status.delegation]) {
        if (baseRevision === undefined) {
          expect(summary).not.toHaveProperty("baseRevision");
        } else {
          expect(summary.baseRevision).toBe(baseRevision);
        }
        expect(summary).not.toHaveProperty("prompt");
        expect(summary).not.toHaveProperty("cwd");
      }
      const [, init] = fetchMock.mock.calls[0];
      expect(JSON.parse(String(init?.body)).writePolicy.base).toBe("worker-base");
      expect(fetchMock).toHaveBeenCalledTimes(2);
    },
  );

  it.each([
    "worker mode requires writePolicy `isolatedWorktree`; other writable modes are not supported",
    "sharedWorktree delegation write policy is not implemented yet",
    "writePolicy base is supported only for worker mode with isolatedWorktree",
    "worker base must name one commit or ref, not an option or empty value",
    "worker base must resolve to exactly one unambiguous commit",
    "delegation cwd is not a directory in the resolved worker base",
  ])("preserves exact safe refusal and redacts extra detail: %s", (message) => {
    const packet = (text: string) =>
      spawnDelegationFailurePacket(
        new ApiRequestError("request-failed", text, { status: 400 }),
        { parentSessionId: "parent-1" },
      );
    expect(packet(message).message).toBe(message);
    expect(packet(`${message}: token=private`).message).toBe(
      "Spawn delegation failed.",
    );
  });
});
