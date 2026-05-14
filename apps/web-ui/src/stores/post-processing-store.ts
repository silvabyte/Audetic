import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import {
  createJob as apiCreateJob,
  deleteJob as apiDeleteJob,
  listEvents as apiListEvents,
  listJobs as apiListJobs,
  testJob as apiTestJob,
  updateJob as apiUpdateJob,
  type EventDescriptor,
  type Job,
  type NewJob,
  type TestJobResult,
  type UpdateJob,
} from "@/api/post-processing";

type Status = "idle" | "loading" | "loaded" | "error";

/**
 * Backs `/settings/post-processing`. Owns the job list, the event
 * dropdown options, and the most recent test result so the result panel
 * can re-render without re-running the command.
 */
export class PostProcessingStore {
  jobs: Job[] = [];
  jobsState: Status = "idle";

  events: EventDescriptor[] = [];
  eventsState: Status = "idle";

  /** Last test outcome keyed by job id; cleared on new test. */
  lastTest: Record<number, TestJobResult> = {};

  lastError: string | null = null;

  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root">(this, { root: false });
  }

  async loadAll(): Promise<void> {
    await Promise.allSettled([this.loadJobs(), this.loadEvents()]);
  }

  async loadJobs(): Promise<void> {
    runInAction(() => {
      this.jobsState = "loading";
    });
    try {
      const jobs = await apiListJobs();
      runInAction(() => {
        this.jobs = jobs;
        this.jobsState = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.jobsState = "error";
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async loadEvents(): Promise<void> {
    runInAction(() => {
      this.eventsState = "loading";
    });
    try {
      const events = await apiListEvents();
      runInAction(() => {
        this.events = events;
        this.eventsState = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.eventsState = "error";
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async createJob(input: NewJob): Promise<Job | null> {
    try {
      const job = await apiCreateJob(input);
      runInAction(() => {
        this.jobs = [job, ...this.jobs];
      });
      return job;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    }
  }

  async updateJob(id: number, patch: UpdateJob): Promise<Job | null> {
    try {
      const job = await apiUpdateJob(id, patch);
      runInAction(() => {
        this.jobs = this.jobs.map((j) => (j.id === id ? job : j));
      });
      return job;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    }
  }

  async toggleEnabled(id: number, enabled: boolean): Promise<void> {
    await this.updateJob(id, { enabled });
  }

  async deleteJob(id: number): Promise<boolean> {
    try {
      await apiDeleteJob(id);
      runInAction(() => {
        this.jobs = this.jobs.filter((j) => j.id !== id);
        delete this.lastTest[id];
      });
      return true;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return false;
    }
  }

  async testJob(id: number): Promise<TestJobResult | null> {
    try {
      const result = await apiTestJob(id);
      runInAction(() => {
        this.lastTest[id] = result;
      });
      return result;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    }
  }

  clearError(): void {
    this.lastError = null;
  }
}
