import { defineConfig } from "@playwright/test";
import { requireLoopbackOrigin } from "./browser/loopback";

const baseURL = requireLoopbackOrigin(
  process.env.WP004_BASE_URL ?? "http://127.0.0.1:4173",
  "WP004_BASE_URL",
);

export default defineConfig({
  testDir: "./browser",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 30_000,
  expect: { timeout: 8_000 },
  outputDir: "test-results/browser",
  reporter: [["line"]],
  use: {
    baseURL,
    browserName: "chromium",
    // Full Chromium keeps animation frames running on macOS ARM64.
    channel: "chromium",
    headless: true,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
  },
});
