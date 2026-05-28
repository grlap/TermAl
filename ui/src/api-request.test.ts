import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ApiRequestError,
  isBackendUnavailableError,
  request,
  requestJsonFirst,
} from "./api-request";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("api-request", () => {
  it("applies JSON headers and parses successful JSON responses", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ ok: true }), {
        headers: { "content-type": "application/json" },
        status: 200,
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      request<{ ok: boolean }>("/api/example", { method: "POST" }),
    ).resolves.toEqual({ ok: true });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/example",
      expect.objectContaining({
        headers: { "Content-Type": "application/json" },
        method: "POST",
      }),
    );
  });

  it("classifies JSON-first HTML fallbacks as backend unavailable", async () => {
    const html = "<!doctype html><title>TermAl</title>";
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(html, {
          headers: {
            "content-length": String(html.length),
            "content-type": "application/json",
          },
          status: 200,
        }),
      ),
    );

    await expect(requestJsonFirst("/api/state")).rejects.toMatchObject({
      kind: "backend-unavailable",
      restartRequired: true,
      status: 200,
    });
  });

  it("wraps oversized JSON-first parse failures as structured request errors", async () => {
    expect.assertions(5);
    const parseError = new SyntaxError("Unexpected end of JSON input");
    const json = vi.fn(async () => {
      throw parseError;
    });
    const clone = vi.fn();
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
        headers: new Headers({
          "content-length": String(64 * 1024 + 1),
          "content-type": "application/json",
        }),
        json,
        clone,
      } as unknown as Response),
    );

    try {
      await requestJsonFirst("/api/large-json");
      throw new Error("Expected requestJsonFirst to reject");
    } catch (error) {
      expect(json).toHaveBeenCalledTimes(1);
      expect(clone).not.toHaveBeenCalled();
      expect(error).toBeInstanceOf(ApiRequestError);
      expect(error).toMatchObject({
        kind: "request-failed",
        status: 200,
        message: "Failed to parse JSON response from /api/large-json.",
      });
      expect((error as ApiRequestError).cause).toBe(parseError);
    }
  });

  it("preserves intentional gateway JSON errors when requested", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ error: "proxy failed" }), {
          headers: { "content-type": "application/json" },
          status: 502,
        }),
      ),
    );

    let caught: unknown = null;
    try {
      await request("/api/proxy", undefined, {
        preserveGatewayErrorBody: true,
      });
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(ApiRequestError);
    expect(caught).toMatchObject({
      kind: "request-failed",
      message: "proxy failed",
      status: 502,
    });
    expect(isBackendUnavailableError(caught)).toBe(false);
  });
});
