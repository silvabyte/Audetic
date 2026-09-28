import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";
import { errorMessage } from "@/lib/audio-notes";

export type AudioNoteArtifact = components["schemas"]["AudioNoteArtifact"];
export type GenerateArtifactRequest = components["schemas"]["GenerateArtifactRequest"];
type LoadState = "idle" | "loading" | "loaded" | "error";

export class NoteArtifactsStore {
  templates: components["schemas"]["SummaryTemplate"][] = [];
  profiles: components["schemas"]["AgentProfile"][] = [];
  prerequisitesState: LoadState = "idle";
  byNote: Record<number, AudioNoteArtifact[]> = {};
  noteState: Record<number, LoadState> = {};
  generatingByNote: Record<number, boolean> = {};
  errors: Record<number, string | null> = {};
  prerequisitesError: string | null = null;
  selectingDefault = false;

  constructor() { makeAutoObservable(this); }

  async selectDefaultAgent(id: number): Promise<boolean> {
    if (this.selectingDefault) return false;
    this.selectingDefault = true;
    this.prerequisitesError = null;
    try {
      const { error } = await daemon.POST("/agent-profiles/{id}/default", { params: { path: { id } } });
      if (error) throw new Error(errorMessage(error));
      await this.loadPrerequisites();
      return true;
    } catch (error) {
      runInAction(() => { this.prerequisitesError = errorMessage(error); });
      return false;
    } finally { runInAction(() => { this.selectingDefault = false; }); }
  }

  async loadPrerequisites(): Promise<void> {
    if (this.prerequisitesState === "loading") return;
    this.prerequisitesState = "loading"; this.prerequisitesError = null;
    try {
      const [{ data: templateData, error: templateError }, { data: profileData, error: profileError }] = await Promise.all([daemon.GET("/summary/templates"), daemon.GET("/agent-profiles")]);
      if (templateError || !templateData) throw new Error(errorMessage(templateError ?? "Templates unavailable"));
      if (profileError || !profileData) throw new Error(errorMessage(profileError ?? "Agent profiles unavailable"));
      runInAction(() => { this.templates = templateData.templates; this.profiles = profileData.profiles; this.prerequisitesState = "loaded"; });
    } catch (error) { runInAction(() => { this.prerequisitesState = "error"; this.prerequisitesError = errorMessage(error); }); }
  }
  async loadArtifacts(id: number): Promise<void> {
    if (this.noteState[id] === "loading") return;
    this.noteState[id] = "loading";
    try {
      const { data, error } = await daemon.GET("/audio-notes/{id}/artifacts", { params: { path: { id } } });
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => { this.byNote[id] = data.artifacts; this.noteState[id] = "loaded"; this.errors[id] = null; });
    } catch (error) { runInAction(() => { this.noteState[id] = "error"; this.errors[id] = errorMessage(error); }); }
  }
  async generateArtifact(id: number, request: GenerateArtifactRequest): Promise<AudioNoteArtifact | null> {
    if (this.generatingByNote[id]) return null;
    this.generatingByNote[id] = true; this.errors[id] = null;
    try {
      const { data, error } = await daemon.POST("/audio-notes/{id}/artifacts", { params: { path: { id } }, body: request });
      if (error || !data) throw new Error(errorMessage(error ?? "Empty response"));
      runInAction(() => { this.byNote[id] = [data.artifact, ...(this.byNote[id] ?? []).filter((artifact) => artifact.id !== data.artifact.id)]; });
      return data.artifact;
    } catch (error) { runInAction(() => { this.errors[id] = errorMessage(error); }); return null; }
    finally { runInAction(() => { this.generatingByNote[id] = false; }); void this.loadArtifacts(id); }
  }
  async deleteArtifact(id: number, artifact_id: number): Promise<boolean> {
    try {
      const { error } = await daemon.DELETE("/audio-notes/{id}/artifacts/{artifact_id}", { params: { path: { id, artifact_id } } });
      if (error) throw new Error(errorMessage(error));
      runInAction(() => { this.byNote[id] = (this.byNote[id] ?? []).filter((artifact) => artifact.id !== artifact_id); });
      return true;
    } catch (error) { runInAction(() => { this.errors[id] = errorMessage(error); }); return false; }
  }
}
