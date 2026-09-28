import { daemon } from "./client";
import type { components } from "./schema";
import { errorMessage } from "@/lib/audio-notes";

export type EventKind = components["schemas"]["EventKind"];
export type Job = components["schemas"]["Job"];
export type EventDescriptor = components["schemas"]["EventDescriptor"];
export type NewJob = components["schemas"]["NewJob"];
export type UpdateJob = components["schemas"]["UpdateJob"];
export type TestJobResult = components["schemas"]["TestJobResponse"];

export async function listEvents(): Promise<EventDescriptor[]> {
  const { data, error } = await daemon.GET("/post-processing/events");
  if (error || !data) throw new Error(errorMessage(error ?? "Events unavailable"));
  return data.events;
}
export async function listJobs(event?: EventKind): Promise<Job[]> {
  const { data, error } = await daemon.GET("/post-processing/jobs", { params: { query: { event } } });
  if (error || !data) throw new Error(errorMessage(error ?? "Jobs unavailable"));
  return data.jobs;
}
export async function createJob(body: NewJob): Promise<Job> {
  const { data, error } = await daemon.POST("/post-processing/jobs", { body });
  if (error || !data) throw new Error(errorMessage(error ?? "No job returned"));
  return data;
}
export async function updateJob(id: number, body: UpdateJob): Promise<Job> {
  const { data, error } = await daemon.PATCH("/post-processing/jobs/{id}", { params: { path: { id } }, body });
  if (error || !data) throw new Error(errorMessage(error ?? "No job returned"));
  return data;
}
export async function deleteJob(id: number): Promise<void> {
  const { error } = await daemon.DELETE("/post-processing/jobs/{id}", { params: { path: { id } } });
  if (error) throw new Error(errorMessage(error));
}
export async function testJob(id: number): Promise<TestJobResult> {
  const { data, error } = await daemon.POST("/post-processing/jobs/{id}/test", { params: { path: { id } } });
  if (error || !data) throw new Error(errorMessage(error ?? "No test result returned"));
  return data;
}
