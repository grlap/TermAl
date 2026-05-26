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
