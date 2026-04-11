import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ApiRequestError,
  createOrchestratorInstance,
  deleteWorkspaceLayout,
  fetchWorkspaceLayout,
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

    await saveFile("/repo/src/app.ts", "updated", {
      sessionId: "",
      baseHash: "sha256:base",
    });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      path: "/repo/src/app.ts",
      content: "updated",
      baseHash: "sha256:base",
    });
    expect(Object.prototype.hasOwnProperty.call(parsedBody, "sessionId")).toBe(false);
  });

  it("omits empty base hashes from the request body", async () => {
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

    await saveFile("/repo/src/app.ts", "updated", {
      baseHash: " ",
    });

    const [, init] = fetchMock.mock.calls[0] ?? [];
    const parsedBody = JSON.parse(String(init?.body)) as Record<string, unknown>;
    expect(parsedBody).toEqual({
      path: "/repo/src/app.ts",
      content: "updated",
    });
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

  it("classifies proxy 503 and 504 failures as structured backend-unavailable errors", async () => {
    expect.assertions(6);

    for (const status of [503, 504]) {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(
          new Response(JSON.stringify({ error: `proxy failed (${status})` }), {
            status,
            headers: {
              "Content-Type": "application/json",
            },
          }),
        ),
      );

      try {
        await fetchState();
        throw new Error(`Expected fetchState to reject for HTTP ${status}`);
      } catch (error) {
        expect(isBackendUnavailableError(error)).toBe(true);
        expect((error as ApiRequestError).status).toBe(status);
        expect((error as Error).message).toBe(
          "The TermAl backend is unavailable.",
        );
      }
    }
  });

  it("preserves the original fetch rejection on ApiRequestError.cause", async () => {
    expect.assertions(4);
    const rootCause = new TypeError("network unreachable");
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(rootCause));

    try {
      await fetchState();
      throw new Error("Expected fetchState to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).cause).toBe(rootCause);
      expect((error as ApiRequestError).status).toBeNull();
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

describe("fetchWorkspaceLayout", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    vi.restoreAllMocks();
    if (originalFetch === undefined) {
      delete (globalThis as Partial<typeof globalThis>).fetch;
      return;
    }
    globalThis.fetch = originalFetch;
  });

  it("returns null for JSON 404 responses", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ error: "layout missing" }), {
        status: 404,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchWorkspaceLayout("workspace/one two")).resolves.toBeNull();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/workspaces/workspace%2Fone%20two",
      expect.objectContaining({
        headers: expect.objectContaining({
          "Content-Type": "application/json",
        }),
      }),
    );
  });

  it("treats HTML 404 fallbacks as restart-required backend errors", async () => {
    expect.assertions(5);
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response("<!DOCTYPE html><html><body>Not found</body></html>", {
          status: 404,
          headers: {
            "Content-Type": "text/html",
          },
        }),
      ),
    );

    try {
      await fetchWorkspaceLayout("workspace/one two");
      throw new Error("Expected fetchWorkspaceLayout to reject");
    } catch (error) {
      expect(isBackendUnavailableError(error)).toBe(true);
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).restartRequired).toBe(true);
      expect((error as Error).message).toContain(
        "/api/workspaces/workspace%2Fone%20two (HTTP 404)",
      );
      expect((error as Error).message).not.toContain("/api/workspaces/workspace/one two");
    }
  });
});
