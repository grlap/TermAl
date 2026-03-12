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

export default defineConfig({
  plugins: [monacoEsmCssStub(), react()],
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
      },
      "/api": {
        target: "http://127.0.0.1:8787",
        changeOrigin: true,
      },
    },
  },
});
