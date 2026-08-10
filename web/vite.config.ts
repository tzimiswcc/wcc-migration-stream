import react from "@vitejs/plugin-react";
import { defaultExclude, defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  publicDir: false,
  build: {
    outDir: "dist",
    assetsDir: "assets",
    sourcemap: false,
    emptyOutDir: true,
  },
  server: {
    host: "127.0.0.1",
    port: 4173,
    strictPort: true,
    proxy: {
      "/api": "http://127.0.0.1:3210",
    },
  },
  preview: {
    host: "127.0.0.1",
    port: 4174,
    strictPort: true,
  },
  test: {
    environment: "jsdom",
    exclude: [...defaultExclude, "browser/**"],
    setupFiles: ["./src/test/setup.ts"],
    restoreMocks: true,
    clearMocks: true,
    css: true,
  },
});
