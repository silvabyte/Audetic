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

/** Daemon-persisted defaults, separate from the options fixed for an active capture. */
export class AudioNoteSettingsStore {
  settings: AudioNoteSettings = { auto_paste: { ...AUTO_PASTE_OFF } };
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

  async load(): Promise<void> {
    if (this.state === "loading" || this.saving) return;
    this.state = "loading";
    this.loadError = null;
    try {
      const { data, error } = await this.client.GET("/audio-notes/settings");
      if (error || !data) throw new Error(errorMessage(error ?? "Capture settings unavailable"));
      runInAction(() => { this.settings = data; this.state = "loaded"; this.saveError = null; });
    } catch (error) {
      runInAction(() => { this.state = "error"; this.loadError = errorMessage(error); });
    }
  }

  async saveAutoPaste(source: CaptureSource, enabled: boolean): Promise<boolean> {
    if (this.saving || this.state !== "loaded") return false;
    this.saving = true;
    this.saveError = null;
    const auto_paste = { ...this.settings.auto_paste, [source]: enabled };
    try {
      const { data, error } = await this.client.PUT("/audio-notes/settings", { body: { auto_paste } });
      if (error || !data) throw new Error(errorMessage(error ?? "Capture settings were not saved"));
      // Reflect only the server-confirmed value, never an optimistic preference.
      runInAction(() => { this.settings = data; });
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
