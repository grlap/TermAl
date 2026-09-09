// Owns response-body transport classification regressions for the API helpers.
// New coverage; does not own route payloads or reconnect scheduling.
import { afterEach, expect, it, vi } from "vitest";
import { isBackendUnavailableError, request, requestJsonFirst } from "./api-request";
import { fetchWorkspaceLayout } from "./api";

afterEach(() => vi.unstubAllGlobals());

const workspaceGet = (_endpoint: string, init?: RequestInit) =>
  fetchWorkspaceLayout("workspace-test", { signal: init?.signal ?? undefined });

for (const [name, send] of [["text", request], ["json-first", requestJsonFirst], ["workspace", workspaceGet]] as const) {
  it.each([false, true])(`${name} classifies an AbortError without caller cancellation (signal=%s)`, async (withSignal) => {
    const controller = new AbortController();
    const cause = new DOMException("Body interrupted", "AbortError");
    vi.stubGlobal("fetch", vi.fn(async () => new Response(new ReadableStream({
      start(stream) { stream.error(cause); },
    }), { status: 503, headers: { "content-type": "application/json" } })));
    await expect(send("/api/state", withSignal ? { signal: controller.signal } : undefined))
      .rejects.toMatchObject({ name: "ApiRequestError", kind: "backend-unavailable", status: 503, cause });
  });

  it(`${name} does not infer cancellation from an Error name`, async () => {
    const cause = Object.assign(new Error("Body interrupted"), { name: "AbortError" });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(new ReadableStream({
      start(stream) { stream.error(cause); },
    }), { status: 200, headers: { "content-type": "application/json" } })));
    await expect(send("/api/state")).rejects.toMatchObject({ name: "ApiRequestError", status: 200, cause });
  });

  it(`${name} preserves caller cancellation before headers`, async () => {
    const controller = new AbortController();
    const cause = new DOMException("Cancelled", "AbortError");
    vi.stubGlobal("fetch", vi.fn(async () => { controller.abort(cause); throw cause; }));
    await expect(send("/api/state", { signal: controller.signal })).rejects.toBe(cause);
  });

  it.each([200, 413, 503].flatMap((status) =>
    ["application/json", "text/plain"].flatMap((contentType) =>
      [undefined, "20", "100000"].map((length) => ({ status, contentType, length }))),
  ))(`${name} preserves status and cause after interrupted body: %j`, async ({ status, contentType, length }) => {
    const cause = new TypeError("connection dropped after headers");
    const headers = new Headers({ "content-type": contentType });
    if (length) headers.set("content-length", length);
    vi.stubGlobal("fetch", vi.fn(async () => new Response(new ReadableStream({
      start(controller) { controller.error(cause); },
    }), { status, headers })));
    const error = await send("/api/state").catch((error: unknown) => error);
    expect(error).toMatchObject({ name: "ApiRequestError", status, cause });
    expect(isBackendUnavailableError(error)).toBe(true);
  });

  it(`${name} does not classify invalid JSON as a transport failure`, async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{broken", {
      headers: { "content-type": "application/json" },
    })));
    const error = await send("/api/state").catch((error: unknown) => error);
    expect(error).toBeInstanceOf(SyntaxError);
    expect(isBackendUnavailableError(error)).toBe(false);
  });

  it(`${name} preserves cancellation after headers`, async () => {
    const controller = new AbortController();
    const cause = new DOMException("Cancelled", "AbortError");
    vi.stubGlobal("fetch", vi.fn(async () => {
      controller.abort(cause);
      return new Response(new ReadableStream({ start(stream) { stream.error(cause); } }), {
        headers: { "content-type": "application/json" },
      });
    }));
    await expect(send("/api/state", { signal: controller.signal })).rejects.toBe(cause);
  });
}
