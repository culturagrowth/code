import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "node",
    include: ["test/**/*.test.ts"],
    testTimeout: 10_000,
    // The tests run the real SQL on Node's built-in SQLite; hide its "experimental" banner.
    execArgv: ["--disable-warning=ExperimentalWarning"],
  },
});
