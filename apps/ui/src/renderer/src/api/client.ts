import createClient from "openapi-fetch";
import type { paths } from "./schema";

/**
 * Typed client for the Audetic daemon at 127.0.0.1:3737.
 *
 * All paths and bodies are driven by the generated OpenAPI schema in
 * `./schema.ts`. Regenerate with `bun run codegen` (or
 * `make electron-codegen`) whenever the daemon's API changes.
 */
export const daemon = createClient<paths>({
  baseUrl: "http://127.0.0.1:3737",
});
