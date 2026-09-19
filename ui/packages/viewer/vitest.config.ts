import { defineConfig } from "vitest/config";

export default defineConfig({
  test: { environment: "node", include: ["test/**/*.test.ts"], testTimeout: 120_000 },
  resolve: {
    alias: { "@vwp/protocol": new URL("../protocol/src/index.ts", import.meta.url).pathname },
  },
});
