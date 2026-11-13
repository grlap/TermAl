// dev-proxy.ts
//
// Owns: how the Vite dev-server proxy handles a backend connection that
// fails or closes. Before the response has started it answers 502 with a
// readable message. After the response has started (a streaming response
// such as the /api/events event stream) it closes the browser-side response
// when the backend side dies, so the browser's EventSource sees the stream
// end and reconnects instead of staying open with no data.
//
// Does not own: the proxy routes and timeouts (vite.config.ts), or the
// client's reconnect and fallback logic (src/app-live-state-transport.ts and
// the SharedWorker hub in src/shared-live-events.ts).
//
// Split out of: ui/vite.config.ts (configureBackendUnavailableProxy), so it can
// be unit tested.

type ProxiedResponse = {
  headersSent?: boolean;
  writableEnded?: boolean;
  writeHead(statusCode: number, headers?: Record<string, string>): void;
  end(body?: string): void;
  destroy?(): void;
};

type ClosableUpstream = {
  on(event: "close", listener: () => void): unknown;
};

/** The part of the http-proxy server this module listens on. */
export type DevProxyEvents = {
  on(event: "error", listener: (error: Error, req: unknown, res: unknown) => void): unknown;
  on(
    event: "proxyRes",
    listener: (proxyRes: ClosableUpstream, req: unknown, res: unknown) => void,
  ): unknown;
};

function isProxiedResponse(res: unknown): res is ProxiedResponse {
  return (
    !!res &&
    typeof res === "object" &&
    "writeHead" in res &&
    typeof res.writeHead === "function" &&
    "end" in res &&
    typeof res.end === "function"
  );
}

/** Ends a started response that the backend can no longer complete. */
function closeStartedResponse(res: ProxiedResponse) {
  if (!res.writableEnded) res.destroy?.();
}

export function configureBackendUnavailableProxy(proxy: DevProxyEvents) {
  proxy.on("error", (_error: unknown, _req: unknown, res: unknown) => {
    if (!isProxiedResponse(res)) {
      return;
    }

    if (res.headersSent || res.writableEnded) {
      // The backend died after the response started, for example a host
      // restart under a proxied event stream. Leaving the browser side open
      // would keep its EventSource OPEN with no data and no reconnect.
      closeStartedResponse(res);
      return;
    }

    res.writeHead(502, { "Content-Type": "text/plain" });
    res.end(
      "The TermAl backend is unavailable. Start it again and wait for reconnect.",
    );
  });

  proxy.on("proxyRes", (proxyRes: ClosableUpstream, _req: unknown, res: unknown) => {
    // A piped upstream that closes without ending (a reset socket) does not
    // end the browser-side response by itself. A normal end has already
    // ended it by the time "close" fires.
    proxyRes.on("close", () => {
      if (isProxiedResponse(res)) closeStartedResponse(res);
    });
  });
}
