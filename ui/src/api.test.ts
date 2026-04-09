import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ApiRequestError,
  createOrchestratorInstance,
  deleteWorkspaceLayout,
  fetchState,
  isBackendUnavailableError,
  saveFile,
} from "./api";

describe("createOrchestratorInstance", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("omits null project ids so the backend can fall back to the template project", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ orchestrator: {}, state: {} }), {
        status: 201,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await createOrchestratorInstance("template-run", null);

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/orchestrators",
      expect.objectContaining({
        method: "POST",
      }),
    );
    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({ templateId: "template-run" });
    expect(Object.prototype.hasOwnProperty.call(parsedBody, "projectId")).toBe(false);
  });

  it("includes inline template drafts when provided", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ orchestrator: {}, state: {} }), {
        status: 201,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await createOrchestratorInstance("template-run", "project-a", {
      name: "Run Flow",
      description: "Remote launch draft.",
      projectId: "project-a",
      sessions: [],
      transitions: [],
    });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      templateId: "template-run",
      projectId: "project-a",
      template: {
        name: "Run Flow",
        description: "Remote launch draft.",
        projectId: "project-a",
        sessions: [],
        transitions: [],
      },
    });
  });
});

describe("saveFile", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("omits empty session ids from the request body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          path: "/repo/src/app.ts",
          content: "updated",
        }),
        {
          status: 200,
          headers: {
            "Content-Type": "application/json",
          },
        },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    await saveFile("/repo/src/app.ts", "updated", { sessionId: "" });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      path: "/repo/src/app.ts",
      content: "updated",
    });
    expect(Object.prototype.hasOwnProperty.call(parsedBody, "sessionId")).toBe(false);
  });
});

describe("deleteWorkspaceLayout", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("uses DELETE with an encoded workspace id and parses the remaining summaries", async () => {
    const responseBody = {
      workspaces: [
        {
          id: "workspace-two",
          revision: 9,
          updatedAt: "2026-04-06 10:15:00",
          controlPanelSide: "right",
        },
      ],
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(responseBody), {
        status: 200,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const result = await deleteWorkspaceLayout("workspace/one two");

    expect(fetchMock).toHaveBeenCalledWith(
      "/api/workspaces/workspace%2Fone%20two",
      expect.objectContaining({
        method: "DELETE",
      }),
    );
    expect(result).toEqual(responseBody);
  });
});

describe("fetchState", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("classifies proxy 502 failures as structured backend-unavailable errors", async () => {
    expect.assertions(3);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: "proxy failed while reading C:\\internal\\server.ts",
          }),
          {
            status: 502,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      ),
    );

    try {
      await fetchState();
      throw new Error("Expected fetchState to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(Error);
      expect((error as Error).message).toBe(
        "The TermAl backend is unavailable.",
      );
    }
  });

  it("sets restartRequired on HTML fallback errors from an incompatible backend", async () => {
    expect.assertions(4);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(
          "<!DOCTYPE html><html><body>Old backend</body></html>",
          {
            status: 200,
            headers: {
              "Content-Type": "text/html",
            },
          },
        ),
      ),
    );

    try {
      await fetchState();
      throw new Error("Expected fetchState to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).restartRequired).toBe(true);
      expect((error as Error).message).toContain("Restart TermAl");
    }
  });
});
