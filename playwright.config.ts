import { defineConfig } from "@playwright/test";
import { resolve } from "node:path";

export default defineConfig({
  testDir: "./tests/e2e",
  timeout: 30_000,
  retries: 0,
  workers: 1,
  reporter: "line",
  webServer: {
    command: "cargo run --manifest-path src-tauri/Cargo.toml -- --server",
    url: "http://127.0.0.1:48731/api/health",
    reuseExistingServer: false,
    timeout: 120_000,
    env: {
      ...process.env,
      PROMLEX_DATA_DIR: resolve("test-results/e2e-data"),
      TRANOVA_DATA_DIR: resolve("test-results/e2e-data"),
    },
  },
  use: {
    baseURL: process.env.PROMLEX_WEB_URL || process.env.TRANOVA_WEB_URL || "http://127.0.0.1:48731",
    browserName: "chromium",
    headless: true,
    launchOptions: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH }
      : undefined,
  },
});
