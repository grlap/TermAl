/// <reference types="vitest" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

function monacoEsmCssStub() {
  return {
    name: "monaco-esm-css-stub",
    enforce: "pre" as const,
    load(id: string) {
      if (id.includes("/monaco-editor/esm/") && id.endsWith(".css")) {
        return "";
      }

      return null;
    },
  };
}

function configureBackendUnavailableProxy(proxy: {
  on(
    event: "error",
    listener: (
      error: Error,
      req: unknown,
      res:
        | {
            headersSent?: boolean;
            writableEnded?: boolean;
            writeHead(
              statusCode: number,
              headers?: Record<string, string>,
            ): void;
            end(body?: string): void;
          }
        | unknown,
    ) => void,
  ): void;
}) {
  proxy.on("error", (_error, _req, res) => {
    if (
      !res ||
      typeof res !== "object" ||
      !("writeHead" in res) ||
      typeof res.writeHead !== "function" ||
      !("end" in res) ||
      typeof res.end !== "function"
    ) {
      return;
    }

    if (res.headersSent || res.writableEnded) {
      return;
    }

    res.writeHead(502, { "Content-Type": "text/plain" });
    res.end(
      "The TermAl backend is unavailable. Start it again and wait for reconnect.",
    );
  });
}

export default defineConfig({
  plugins: [
    monacoEsmCssStub(),
    react({ babel: { compact: false } }),
  ],
  build: {
    // Monaco's language workers are intentionally lazy-loaded but still large enough
    // to overwhelm Vite's default warning threshold under the current toolchain.
    chunkSizeWarningLimit: 7500,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes("node_modules")) {
            return undefined;
          }

          if (id.includes("monaco-editor")) {
            return "monaco";
          }

          if (
            id.includes("react-markdown") ||
            id.includes("remark-gfm") ||
            id.includes("/remark-") ||
            id.includes("/rehype-") ||
            id.includes("/unified/") ||
            id.includes("/micromark") ||
            id.includes("/mdast-") ||
            id.includes("/hast-") ||
            id.includes("/vfile")
          ) {
            return "markdown";
          }

          if (id.includes("highlight.js")) {
            return "highlight";
          }

          return undefined;
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: "./src/test-setup.ts",
  },
  server: {
    host: "127.0.0.1",
    port: 4173,
    proxy: {
      "/api/events": {
        target: "http://127.0.0.1:8787",
        changeOrigin: true,
        // Prevent the proxy from closing the long-lived SSE connection.
        timeout: 0,
        proxyTimeout: 0,
        configure: configureBackendUnavailableProxy,
      },
      "/api": {
        target: "http://127.0.0.1:8787",
        changeOrigin: true,
        // Allow large image attachments (base64-encoded PNGs can exceed 2 MB in the JSON body).
        timeout: 120_000,
        proxyTimeout: 120_000,
        configure: configureBackendUnavailableProxy,
      },
    },
  },
});
