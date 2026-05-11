import createClient from "openapi-fetch";
import type { paths } from "./schema";

/**
 * Typed client for the Audetic daemon.
 *
 * Production: same-origin — the daemon serves the SPA at `/` and the API
 * under `/api`. Dev: vite proxies `/api` → `http://127.0.0.1:3737/api`
 * (see vite.config.ts), so `baseUrl: "/api"` works in both modes.
 *
 * All paths and bodies are driven by the generated OpenAPI schema in
 * `./schema.ts`. Regenerate with `bun run codegen` (or `make codegen`)
 * whenever the daemon's API changes.
 */
export const daemon = createClient<paths>({
  baseUrl: "/api",
});
