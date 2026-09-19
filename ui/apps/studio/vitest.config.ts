import { defineConfig } from "vitest/config";

/**
 * Unit tests for the Studio's projection layer.
 *
 * These run in plain Node: everything under test (`state/store.ts`, `state/engine.ts`'s stream
 * handlers, `lib/history.ts`) is framework- and DOM-free by design — the React components are
 * covered by the Playwright suite in `e2e/`, which drives a real browser.
 */
export default defineConfig({
  test: {
    environment: "node",
    include: ["test/**/*.test.ts"],
    globals: false,
  },
});
