import { fileURLToPath } from "node:url";

import { defineConfig } from "vitest/config";

const repositoryRoot = fileURLToPath(new URL(".", import.meta.url));
const uiSource = fileURLToPath(new URL("./crates/alms-gateway/static/ui/", import.meta.url));
const uiDist = fileURLToPath(new URL("./crates/alms-gateway/static/ui-dist/", import.meta.url));

export default defineConfig({
  root: uiSource,
  base: "/ui/",
  server: {
    fs: {
      allow: [repositoryRoot],
    },
  },
  build: {
    outDir: uiDist,
    emptyOutDir: true,
    sourcemap: false,
    assetsDir: "assets",
  },
  test: {
    root: repositoryRoot,
    environment: "jsdom",
    setupFiles: ["./frontend/test/setup.ts"],
    include: ["frontend/**/*.test.ts", "frontend/**/*.test.tsx"],
    restoreMocks: true,
  },
});
