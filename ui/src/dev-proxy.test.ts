// Pins how the Vite dev-server proxy (../dev-proxy.ts) handles a backend
// connection that fails or closes: a 502 before the response starts, and a
// closed browser-side response once it has started, so a proxied event stream
// ends and the browser's EventSource reconnects.
import { describe, expect, it, vi } from "vitest";
import { configureBackendUnavailableProxy, type DevProxyEvents } from "../dev-proxy";

type Listener = (...args: unknown[]) => void;

function fakeProxy() {
  const listeners = new Map<string, Listener>();
  const proxy = {
    on(event: string, listener: Listener) {
      listeners.set(event, listener);
      return proxy;
    },
  };
  // The fake records listeners untyped; the module only registers them.
  configureBackendUnavailableProxy(proxy as unknown as DevProxyEvents);
  return {
    emitError: (res: unknown) => listeners.get("error")?.(new Error("ECONNRESET"), {}, res),
    emitProxyRes: (upstream: unknown, res: unknown) => listeners.get("proxyRes")?.(upstream, {}, res),
  };
}

function fakeResponse(state: { headersSent?: boolean; writableEnded?: boolean } = {}) {
  return {
    headersSent: state.headersSent ?? false,
    writableEnded: state.writableEnded ?? false,
    writeHead: vi.fn(),
    end: vi.fn(),
    destroy: vi.fn(),
  };
}

function fakeUpstream() {
  const handlers: Array<() => void> = [];
  return {
    on: vi.fn((event: string, listener: () => void) => {
      if (event === "close") handlers.push(listener);
    }),
    close: () => handlers.forEach((handler) => handler()),
  };
}

describe("dev proxy backend failures", () => {
  it("answers 502 when the backend fails before the response starts", () => {
    const proxy = fakeProxy();
    const res = fakeResponse();
    proxy.emitError(res);

    expect(res.writeHead).toHaveBeenCalledWith(502, { "Content-Type": "text/plain" });
    expect(res.end).toHaveBeenCalledOnce();
    expect(res.destroy).not.toHaveBeenCalled();
  });

  it("closes a started response when the backend fails mid-stream", () => {
    const proxy = fakeProxy();
    const res = fakeResponse({ headersSent: true });
    proxy.emitError(res);

    expect(res.destroy).toHaveBeenCalledOnce();
    expect(res.writeHead).not.toHaveBeenCalled();
  });

  it("leaves an already ended response alone", () => {
    const proxy = fakeProxy();
    const res = fakeResponse({ headersSent: true, writableEnded: true });
    proxy.emitError(res);

    expect(res.destroy).not.toHaveBeenCalled();
    expect(res.writeHead).not.toHaveBeenCalled();
  });

  it("ignores a non-HTTP target such as a websocket", () => {
    const proxy = fakeProxy();
    const socket = { end: vi.fn() };
    proxy.emitError(socket);

    expect(socket.end).not.toHaveBeenCalled();
  });

  it("closes the browser side when the upstream closes without ending", () => {
    const proxy = fakeProxy();
    const res = fakeResponse({ headersSent: true });
    const upstream = fakeUpstream();
    proxy.emitProxyRes(upstream, res);
    upstream.close();

    expect(res.destroy).toHaveBeenCalledOnce();
  });

  it("does nothing when the upstream closes after a normal end", () => {
    const proxy = fakeProxy();
    const res = fakeResponse({ headersSent: true });
    const upstream = fakeUpstream();
    proxy.emitProxyRes(upstream, res);
    res.writableEnded = true;
    upstream.close();

    expect(res.destroy).not.toHaveBeenCalled();
  });
});
