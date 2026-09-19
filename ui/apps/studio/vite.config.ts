import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

/**
 * The engine serves the built Studio in production (09-ui §9), so every engine path the app talks to
 * is relative: `/vwp/v1` (the VWP WebSocket, §1.1), `/rpc` (JSON-RPC over HTTP, §6.2) and
 * `/world/{hash}.vwb` (the world payload, §3.1.6). In development Vite proxies those three onto the
 * mock engine, which keeps the app same-origin — the mock sets
 * `cross-origin-resource-policy: same-origin` on every response (§1.1), so a cross-origin fetch of
 * the world would be blocked.
 */
const engine = process.env.VWP_ENGINE ?? "http://127.0.0.1:8787";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "127.0.0.1",
    port: Number(process.env.VWP_STUDIO_PORT ?? 5173),
    strictPort: true,
    proxy: {
      "/vwp/v1": { target: engine, ws: true, changeOrigin: false },
      "/rpc": { target: engine, changeOrigin: false },
      "/world": { target: engine, changeOrigin: false },
      "/healthz": { target: engine, changeOrigin: false },
    },
  },
  preview: {
    host: "127.0.0.1",
    proxy: {
      "/vwp/v1": { target: engine, ws: true, changeOrigin: false },
      "/rpc": { target: engine, changeOrigin: false },
      "/world": { target: engine, changeOrigin: false },
      "/healthz": { target: engine, changeOrigin: false },
    },
  },
  build: {
    target: "es2022",
    sourcemap: true,
    chunkSizeWarningLimit: 1200,
  },
});
