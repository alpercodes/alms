import eslint from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "node_modules/**",
      "crates/alms-gateway/static/ui-dist/**",
      "crates/alms-gateway/static/ui/**",
    ],
  },
  eslint.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    ...tseslint.configs.disableTypeChecked,
    files: [
      "crates/alms-gateway/static/ui/api/client.js",
      "crates/alms-gateway/static/ui/hooks/use-agent-events.js",
      "crates/alms-gateway/static/ui/hooks/use-session-stream.js",
    ],
    languageOptions: {
      globals: {
        URLSearchParams: "readonly",
        console: "readonly",
        document: "readonly",
        EventSource: "readonly",
        fetch: "readonly",
        localStorage: "readonly",
        requestAnimationFrame: "readonly",
        cancelAnimationFrame: "readonly",
        setTimeout: "readonly",
        clearTimeout: "readonly",
      },
    },
  },
  {
    files: ["frontend/**/*.{ts,tsx}", "vite.config.ts", "playwright.config.ts"],
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      "@typescript-eslint/consistent-type-imports": "error",
      "@typescript-eslint/no-confusing-void-expression": "error",
    },
  },
);
