/**
 * Hand-rolled wrapper for the post-processing API.
 *
 * The OpenAPI types in `schema.ts` are regenerated from a running daemon
 * via `bun run codegen`. Until that runs against a daemon with the new
 * `/api/post-processing/*` routes, the typed `openapi-fetch` client
 * doesn't know they exist. This module defines the shapes inline so the
 * settings UI compiles and ships without waiting for codegen.
 *
 * After running `bun run codegen` once, this module can be migrated to
 * `daemon.GET("/post-processing/jobs")` etc., and the local types
 * deleted.
 */

const BASE = "/api/post-processing";

export type EventKind = "dictation.completed" | "meeting.completed";

export interface CommandAction {
  type: "command";
  command: string;
  timeout_seconds: number;
}

export type Action = CommandAction;

export interface Job {
  id: number;
  name: string;
  event: EventKind;
  action: Action;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface EventDescriptor {
  name: EventKind;
  label: string;
  description: string;
}

export interface NewJob {
  name: string;
  event: EventKind;
  action: Action;
  enabled?: boolean;
}

export interface UpdateJob {
  name?: string;
  event?: EventKind;
  action?: Action;
  enabled?: boolean;
}

export interface TestJobResult {
  success: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
  timed_out: boolean;
}

async function unwrap<T>(res: Response, op: string): Promise<T> {
  if (!res.ok) {
    let msg = `${op} failed (HTTP ${res.status})`;
    try {
      const body = (await res.json()) as { message?: string };
      if (body.message) msg = body.message;
    } catch {
      // body wasn't JSON; leave the generic message
    }
    throw new Error(msg);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export async function listEvents(): Promise<EventDescriptor[]> {
  const res = await fetch(`${BASE}/events`);
  const body = await unwrap<{ events: EventDescriptor[] }>(res, "list events");
  return body.events;
}

export async function listJobs(eventFilter?: EventKind): Promise<Job[]> {
  const url = eventFilter
    ? `${BASE}/jobs?event=${encodeURIComponent(eventFilter)}`
    : `${BASE}/jobs`;
  const res = await fetch(url);
  const body = await unwrap<{ jobs: Job[] }>(res, "list jobs");
  return body.jobs;
}

export async function createJob(input: NewJob): Promise<Job> {
  const res = await fetch(`${BASE}/jobs`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  return unwrap<Job>(res, "create job");
}

export async function updateJob(id: number, patch: UpdateJob): Promise<Job> {
  const res = await fetch(`${BASE}/jobs/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(patch),
  });
  return unwrap<Job>(res, "update job");
}

export async function deleteJob(id: number): Promise<void> {
  const res = await fetch(`${BASE}/jobs/${id}`, { method: "DELETE" });
  await unwrap<{ success: boolean; id: number }>(res, "delete job");
}

export async function testJob(id: number): Promise<TestJobResult> {
  const res = await fetch(`${BASE}/jobs/${id}/test`, { method: "POST" });
  return unwrap<TestJobResult>(res, "test job");
}
