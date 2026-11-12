import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";
import { errorMessage } from "@/lib/audio-notes";

type AudioNoteSettings = components["schemas"]["AudioNoteSettings"];
type CaptureSource = components["schemas"]["AudioNoteCaptureSource"];

const AUTO_PASTE_OFF: AudioNoteSettings["auto_paste"] = {
  microphone: false,
  microphone_and_system: false,
};

function normalizeSettings(data: AudioNoteSettings): AudioNoteSettings {
  return {
    auto_paste: data.auto_paste,
    // A response that predates the key keeps today's behavior: processing on.
    ai_processing: data.ai_processing ?? { enabled: true },
  };
}

/** Daemon-persisted defaults, separate from the options fixed for an active capture. */
export class AudioNoteSettingsStore {
  settings: AudioNoteSettings = { auto_paste: { ...AUTO_PASTE_OFF }, ai_processing: { enabled: true } };
  state: "idle" | "loading" | "loaded" | "error" = "idle";
  saving = false;
  loadError: string | null = null;
  saveError: string | null = null;

  constructor(private client = daemon) {
    makeAutoObservable<this, "client">(this, { client: false });
  }

  /** Never inherit an unknown preference. Explicit per-capture choices always win. */
  captureAutoPaste(override: boolean | null): boolean | null {
    if (override !== null) return override;
    return this.state === "loaded" ? null : false;
  }

  /** Saved preference for one source. Off until settings have loaded, and off for an unknown source. */
  savedAutoPaste(source: CaptureSource): boolean {
    if (this.state !== "loaded") return false;
    return this.settings.auto_paste[source] ?? false;
  }

  /** On only after a confirmed load. Unloaded and failed loads do not claim the switch is on. */
  automaticProcessingEnabled(): boolean {
    return this.state === "loaded" && this.settings.ai_processing.enabled;
  }

  async load(): Promise<void> {
    if (this.state === "loading" || this.saving) return;
    this.state = "loading";
    this.loadError = null;
    try {
      const { data, error } = await this.client.GET("/audio-notes/settings");
      if (error || !data) throw new Error(errorMessage(error ?? "Capture settings unavailable"));
      runInAction(() => { this.settings = normalizeSettings(data); this.state = "loaded"; this.saveError = null; });
    } catch (error) {
      runInAction(() => { this.state = "error"; this.loadError = errorMessage(error); });
    }
  }

  async saveAutoPaste(source: CaptureSource, enabled: boolean): Promise<boolean> {
    if (this.saving || this.state !== "loaded") return false;
    const auto_paste = { ...this.settings.auto_paste, [source]: enabled };
    return this.persist({ auto_paste, ai_processing: this.settings.ai_processing });
  }

  async saveAiProcessing(enabled: boolean): Promise<boolean> {
    if (this.saving || this.state !== "loaded") return false;
    return this.persist({ auto_paste: this.settings.auto_paste, ai_processing: { enabled } });
  }

  private async persist(body: AudioNoteSettings): Promise<boolean> {
    this.saving = true;
    this.saveError = null;
    try {
      const { data, error } = await this.client.PUT("/audio-notes/settings", { body });
      if (error || !data) throw new Error(errorMessage(error ?? "Capture settings were not saved"));
      // Reflect only the server-confirmed value, never an optimistic preference.
      runInAction(() => { this.settings = normalizeSettings(data); });
      return true;
    } catch (error) {
      runInAction(() => {
        this.saveError = errorMessage(error);
        // A lost PUT response may have committed. Do not silently inherit an unverified value.
        this.state = "error";
        this.loadError = "Reload the saved preference to verify its current value.";
      });
      return false;
    } finally {
      runInAction(() => { this.saving = false; });
    }
  }
}
