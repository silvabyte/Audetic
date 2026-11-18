import { NON_OPERATION_ROUTES, UNCOVERED_OPERATIONS } from "./contract.ts";

const seen = new Set<string>();

const HTTP_METHODS = new Set([
  "get",
  "put",
  "post",
  "delete",
  "patch",
  "options",
  "head",
  "trace",
]);

export interface HttpResult {
  readonly status: number;
  readonly body: unknown;
  readonly headers: Headers;
}

export function resetSeenOperations(): void {
  seen.clear();
}

export function seenOperations(): ReadonlySet<string> {
  return seen;
}

/** Turn a concrete request into the OpenAPI operation key. */
export function operationKey(method: string, apiPath: string): string {
  const stripped = apiPath.replace(/^\/api/, "").split("?")[0] ?? apiPath;
  // `GET /api` is the service operation. Stripping the prefix leaves an empty path.
  const path = stripped === "" ? "/" : stripped;
  const templated = path
    .replace(
      /^\/audio-notes\/\d+\/artifacts\/\d+$/,
      "/audio-notes/{id}/artifacts/{artifact_id}",
    )
    .replace(/^\/audio-notes\/\d+(\/.*)?$/, (_match, rest: string | undefined) => {
      return `/audio-notes/{id}${rest ?? ""}`;
    })
    .replace(/^\/models\/[^/]+(\/.*)?$/, (_match, rest: string | undefined) => {
      return `/models/{id}${rest ?? ""}`;
    })
    .replace(/^\/integrations\/keys\/[^/]+$/, "/integrations/keys/{id}")
    .replace(
      /^\/post-processing\/jobs\/\d+(\/.*)?$/,
      (_match, rest: string | undefined) => `/post-processing/jobs/{id}${rest ?? ""}`,
    )
    .replace(/^\/agent-profiles\/\d+(\/.*)?$/, (_match, rest: string | undefined) => {
      return `/agent-profiles/{id}${rest ?? ""}`;
    });
  return `${method.toUpperCase()} ${templated}`;
}

export async function request(
  baseUrl: string,
  method: string,
  apiPath: string,
  init: RequestInit = {},
): Promise<HttpResult> {
  seen.add(operationKey(method, apiPath));
  const response = await fetch(new URL(apiPath, `${baseUrl}/`), {
    ...init,
    method,
    signal: AbortSignal.timeout(20_000),
  });
  const buffer = Buffer.from(await response.arrayBuffer());
  const type = response.headers.get("content-type") ?? "";
  let body: unknown = buffer;
  if (type.includes("json") || looksLikeJson(buffer)) {
    body = buffer.length === 0 ? null : JSON.parse(buffer.toString("utf8"));
  }
  return { status: response.status, body, headers: response.headers };
}

export async function getJson(
  baseUrl: string,
  apiPath: string,
  init: RequestInit = {},
): Promise<HttpResult> {
  return request(baseUrl, "GET", apiPath, init);
}

function looksLikeJson(buffer: Buffer): boolean {
  const text = buffer.toString("utf8").trimStart();
  return text.startsWith("{") || text.startsWith("[");
}

export interface OpenApiOperation {
  readonly method: string;
  readonly path: string;
  readonly key: string;
}

export function openApiOperations(
  paths: Record<string, Record<string, unknown>>,
): OpenApiOperation[] {
  const operations: OpenApiOperation[] = [];
  for (const [path, item] of Object.entries(paths)) {
    for (const method of Object.keys(item)) {
      if (!HTTP_METHODS.has(method)) continue;
      operations.push({
        method: method.toUpperCase(),
        path,
        key: `${method.toUpperCase()} ${path}`,
      });
    }
  }
  operations.sort((left, right) => left.key.localeCompare(right.key));
  return operations;
}

/**
 * Every served operation is either requested by this process or listed with
 * a reason. Non-operation probes (the document itself, a missing route) are
 * allowed in the seen set.
 */
export function unclassifiedOperations(
  paths: Record<string, Record<string, unknown>>,
): string[] {
  const operations = openApiOperations(paths);
  const allowed = new Set<string>([
    ...Object.keys(UNCOVERED_OPERATIONS),
    ...NON_OPERATION_ROUTES,
  ]);
  const missing: string[] = [];
  for (const operation of operations) {
    if (!seen.has(operation.key) && !allowed.has(operation.key)) {
      missing.push(operation.key);
    }
  }
  for (const key of seen) {
    if (allowed.has(key)) continue;
    if (!operations.some((operation) => operation.key === key)) {
      missing.push(`seen but not in the OpenAPI document: ${key}`);
    }
  }
  for (const key of Object.keys(UNCOVERED_OPERATIONS)) {
    if (!operations.some((operation) => operation.key === key)) {
      missing.push(`uncovered list names an operation the document does not have: ${key}`);
    }
    if (seen.has(key)) {
      missing.push(`uncovered operation was actually called: ${key}`);
    }
  }
  return missing;
}
