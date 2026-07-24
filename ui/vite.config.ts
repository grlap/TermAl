/// <reference types="vitest" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";

const SERIALIZED_REACT_TESTS = [
  "src/App.control-panel.test.tsx",
  "src/App.diff-preview.test.tsx",
  "src/App.live-state.watchdog.test.tsx",
  "src/SessionPaneView.retry-display.test.tsx",
];

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
    // Keep React/jsdom suites below machine-wide CPU saturation.
    maxWorkers: 4,
    setupFiles: "./src/test-setup.ts",
    testTimeout: 10_000,
    projects: [
      {
        extends: true,
        test: {
          name: "default",
          exclude: [
            ...configDefaults.exclude,
            ...SERIALIZED_REACT_TESTS,
          ],
          sequence: {
            groupOrder: 0,
          },
        },
      },
      {
        extends: true,
        test: {
          name: "serialized-react",
          include: SERIALIZED_REACT_TESTS,
          // These integration suites are fast in isolation but can exceed
          // their timeout when several heavyweight App/jsdom files compile
          // and run beside them. Run them one at a time after the parallel
          // default project so contention cannot cause false timeouts or
          // poison later React `act()` state.
          maxWorkers: 1,
          sequence: {
            groupOrder: 1,
          },
        },
      },
    ],
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
