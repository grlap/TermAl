// Isolated App fixture: no backend proxy, production output only in this worktree's .tmp.
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
export default defineConfig({
  plugins: [react()],
  server: { host: "127.0.0.1", port: 4188, strictPort: true },
  preview: { host: "127.0.0.1", port: 4189, strictPort: true },
  build: { outDir: "../.tmp/app-latency-production",
    rollupOptions: { input: "app-latency.html" }, chunkSizeWarningLimit: 7500 },
});
