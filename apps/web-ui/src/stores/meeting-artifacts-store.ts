import { makeAutoObservable, runInAction } from "mobx";
import type { RootStore } from "./root-store";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";

export type AgentProfile = components["schemas"]["AgentProfile"];
export type SummaryTemplate = components["schemas"]["SummaryTemplate"];
export type MeetingArtifact = components["schemas"]["MeetingArtifact"];
export type GenerateArtifactRequest = components["schemas"]["GenerateArtifactRequest"];

type Status = "idle" | "loading" | "loaded" | "error";

/**
 * Owns local-agent summary generation data for the meeting detail workspace:
 * available templates, available local coding-agent profiles, and generated
 * artifacts keyed by meeting id.
 */
export class MeetingArtifactsStore {
  templates: SummaryTemplate[] = [];
  templatesState: Status = "idle";

  profiles: AgentProfile[] = [];
  profilesState: Status = "idle";

  byMeeting: Record<number, MeetingArtifact[]> = {};
  meetingState: Record<number, Status> = {};
  generatingByMeeting: Record<number, boolean> = {};

  lastError: string | null = null;

  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root">(this, { root: false });
  }

  async loadPrerequisites(): Promise<void> {
    await Promise.allSettled([this.loadTemplates(), this.loadProfiles()]);
  }

  async loadTemplates(): Promise<void> {
    if (this.templatesState === "loading" || this.templatesState === "loaded") return;
    runInAction(() => {
      this.templatesState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/summary/templates");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.templates = data.templates;
        this.templatesState = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.templatesState = "error";
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async loadProfiles(): Promise<void> {
    if (this.profilesState === "loading" || this.profilesState === "loaded") return;
    runInAction(() => {
      this.profilesState = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/agent-profiles");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.profiles = data.profiles;
        this.profilesState = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.profilesState = "error";
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async loadArtifacts(meetingId: number): Promise<void> {
    if (this.meetingState[meetingId] === "loading") return;
    runInAction(() => {
      this.meetingState[meetingId] = "loading";
    });
    try {
      const { data, error } = await daemon.GET("/meetings/{id}/artifacts", {
        params: { path: { id: meetingId } },
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.byMeeting[meetingId] = data.artifacts;
        this.meetingState[meetingId] = "loaded";
      });
    } catch (e) {
      runInAction(() => {
        this.meetingState[meetingId] = "error";
        this.lastError = e instanceof Error ? e.message : String(e);
      });
    }
  }

  async generateArtifact(
    meetingId: number,
    request: GenerateArtifactRequest,
  ): Promise<MeetingArtifact | null> {
    runInAction(() => {
      this.generatingByMeeting[meetingId] = true;
      this.lastError = null;
    });
    try {
      const { data, error } = await daemon.POST("/meetings/{id}/artifacts", {
        params: { path: { id: meetingId } },
        body: request,
      });
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        const existing = this.byMeeting[meetingId] ?? [];
        this.byMeeting[meetingId] = [data.artifact, ...existing.filter((a) => a.id !== data.artifact.id)];
        this.meetingState[meetingId] = "loaded";
      });
      return data.artifact;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return null;
    } finally {
      runInAction(() => {
        this.generatingByMeeting[meetingId] = false;
      });
    }
  }

  async deleteArtifact(meetingId: number, artifactId: number): Promise<boolean> {
    try {
      const { error } = await daemon.DELETE("/meetings/{id}/artifacts/{artifact_id}", {
        params: { path: { id: meetingId, artifact_id: artifactId } },
      });
      if (error) throw new Error(formatError(error));
      runInAction(() => {
        this.byMeeting[meetingId] = (this.byMeeting[meetingId] ?? []).filter(
          (artifact) => artifact.id !== artifactId,
        );
      });
      return true;
    } catch (e) {
      runInAction(() => {
        this.lastError = e instanceof Error ? e.message : String(e);
      });
      return false;
    }
  }

  clearError(): void {
    this.lastError = null;
  }
}

function formatError(err: unknown): string {
  if (typeof err === "string") return err;
  if (
    err &&
    typeof err === "object" &&
    "message" in err &&
    typeof (err as { message: unknown }).message === "string"
  ) {
    return (err as { message: string }).message;
  }
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
