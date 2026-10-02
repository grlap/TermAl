/// <reference types="vitest" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";
import { assertVitestResourceBudget } from "../scripts/vitest-resource-preflight.mjs";
import { CATEGORY_PROJECTS } from "./test-categories";

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

if (process.env.VITEST === "true") {
  assertVitestResourceBudget();
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
    // tm-g4z: Vitest 4 defaults to forked Node processes. Under sustained
    // multi-agent load those workers can time out before startup and starve a
    // 10-second test timer for minutes. This pure JS/jsdom suite uses worker
    // threads instead, avoiding repeated process boot while retaining isolated
    // worker contexts. Forks remain the fallback if a future native dependency
    // needs process-level crash or process.exit isolation.
    pool: "threads",
    // Keep React/jsdom suites below machine-wide CPU saturation. A single
    // worker prevents heavyweight files from stealing each other's unchanged
    // 10-second diagnostic budget on developer machines that are also running
    // long-lived agent sessions. Parallel test processes are deliberately not
    // traded for retries or longer per-test timeouts.
    maxWorkers: 1,
    setupFiles: "./src/test-setup.ts",
    testTimeout: 10_000,
    // Four projects from ui/test-categories.ts: unit, component, heavy and
    // app, every one on jsdom. One worker keeps the stage to one file at a
    // time; each project's own groupOrder sets the order between them. The
    // heavy and app files either render the full App or exercise the
    // scheduler-sensitive SessionPaneView and virtualizer lifecycle, so they
    // must not share the runner with other files: oversubscribing the
    // lifecycle under test, or abandoning an open `act()` scope, would make
    // them fail for reasons that are not theirs.
    projects: CATEGORY_PROJECTS.map(({ name, include, exclude, groupOrder }) => ({
      extends: true,
      test: {
        name,
        include: [...include],
        exclude: [...configDefaults.exclude, ...exclude],
        maxWorkers: 1,
        sequence: {
          groupOrder,
        },
      },
    })),
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
        // Allow large image attachments (base64-encoded PNGs can exceed 2 MB in the JSON body)
        // and the backend's longest bounded request: Engram enablement runs the store doctor
        // under a 300 s deadline, and a multi-GB store takes about two minutes. Cutting the
        // proxy earlier turns a successful Verify into a spurious "backend is unavailable".
        timeout: 360_000,
        proxyTimeout: 360_000,
        configure: configureBackendUnavailableProxy,
      },
    },
  },
});
