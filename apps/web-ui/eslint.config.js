import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import mobx from "eslint-plugin-mobx";
import globals from "globals";
import observerBoundary from "./eslint-rules/observer-boundary.js";

export default tseslint.config(
  // Generated / build output — never lint.
  { ignores: ["dist", "node_modules", "src/api/schema.ts"] },

  js.configs.recommended,
  // Non-type-checked baseline (fast, no project service). Upgrade path:
  // tseslint.configs.recommendedTypeChecked once we want type-aware rules.
  ...tseslint.configs.recommended,

  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: { ...globals.browser },
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    plugins: {
      "react-hooks": reactHooks,
      mobx,
      local: { rules: { "observer-boundary": observerBoundary } },
    },
    rules: {
      // Classic, high-signal React hooks rules. (The full v7 React-Compiler
      // rule battery in `recommended-latest` is intentionally NOT enabled — it
      // would flag a lot on code not authored for it; opt in later if wanted.)
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",

      // NOTE: eslint-plugin-react-refresh was evaluated and dropped — this app
      // co-locates route objects + sub-components per file by design, so
      // `only-export-components` produced ~48 unactionable warnings that would
      // bury real errors. Re-add if the file layout ever changes.

      // Safe eslint-plugin-mobx rules (the makeObservable family). NOT
      // `mobx/missing-observer`: it assumes the observer() HOC and would flag
      // every <Observer>-convention component as a false positive.
      "mobx/exhaustive-make-observable": "warn",
      "mobx/unconditional-make-observable": "error",
      "mobx/missing-make-observable": "error",

      // The convention enforcer: store observable reads during render must be
      // inside an <Observer> boundary (see apps/web-ui/feedback_mobx.md).
      "local/observer-boundary": "error",
    },
  },
);
