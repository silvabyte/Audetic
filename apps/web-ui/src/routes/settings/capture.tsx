import { Observer } from "mobx-react-lite";
import { RefreshCcw } from "lucide-react";
import type { RouteObject } from "react-router-dom";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { getRootStore } from "@/stores/singleton";
import { useStore } from "@/stores/root-store";

const AUTO_PASTE_SOURCES = [
  { source: "microphone", id: "saved-auto-paste-microphone", label: "Microphone notes (dictation)" },
  { source: "microphone_and_system", id: "saved-auto-paste-microphone-and-system", label: "Microphone + system audio notes" },
] as const;

export const settingsCaptureRoute: RouteObject = {
  path: "capture",
  loader: async () => { const store = getRootStore(); await Promise.all([store.noteSettings.load(), store.noteArtifacts.loadPrerequisites()]); return null; },
  Component: SettingsCapture,
};

export function SettingsCapture() {
  const store = useStore();
  return <Observer>{() => {
    const settings = store.noteSettings;
    return <div className="flex flex-col gap-5">
      <header><h2 className="text-xl font-semibold">Capture & delivery</h2><p className="mt-1 text-sm text-muted-foreground">Choose what happens to your original words after transcription.</p></header>
      <Card>
        <CardHeader><CardTitle>Automatic paste</CardTitle><CardDescription>Off by default for every capture source. Each preference is saved by the daemon and persists across app and daemon restarts.</CardDescription></CardHeader>
        <CardContent className="flex flex-col gap-4">
          <p id="saved-auto-paste-help" className="text-sm text-muted-foreground">When enabled for a source, that source's saved raw transcript is pasted into the focused app immediately. AI classification and artifacts arrive later and are never pasted automatically.</p>
          {AUTO_PASTE_SOURCES.map((item) => <div key={item.source} className="flex items-start justify-between gap-4">
            <Label htmlFor={item.id}>{item.label}</Label>
            <Switch id={item.id} checked={settings.savedAutoPaste(item.source)} disabled={settings.state !== "loaded" || settings.saving} aria-describedby="saved-auto-paste-help" onCheckedChange={async (checked) => { if (await settings.saveAutoPaste(item.source, checked)) toast.success("Delivery preference saved"); else toast.error(settings.saveError ?? "Couldn't save delivery preference"); }} />
          </div>)}
          <p className="text-xs text-muted-foreground">Applies to future captures of that source that use the saved preference. An active recording keeps its original delivery choice. Capture options can explicitly turn automatic paste on or off for one recording.</p>
          <p role="status" className="text-sm text-muted-foreground">{settings.saving ? "Saving preference…" : settings.state === "loading" || settings.state === "idle" ? "Loading saved preference… Automatic paste is off until it loads." : settings.state === "error" ? "Saved preference unavailable. Browser captures default to automatic paste off." : `Saved preference: ${AUTO_PASTE_SOURCES.map((item) => `${item.label} ${settings.savedAutoPaste(item.source) ? "on" : "off"}`).join(", ")}.`}</p>
          {settings.loadError && <p role="alert" className="text-sm text-destructive">Couldn't load settings: {settings.loadError}</p>}
          {settings.saveError && <p role="alert" className="text-sm text-destructive">Couldn't confirm the save: {settings.saveError}. Automatic paste stays off in browser captures until the saved preference is reloaded.</p>}
          <Button variant="outline" className="self-start" disabled={settings.saving || settings.state === "loading"} onClick={() => void settings.load()}><RefreshCcw data-icon="inline-start" />{settings.loadError ? "Retry loading" : "Reload saved preference"}</Button>
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>AI processing agent</CardTitle><CardDescription>Classify new notes and generate their useful outputs with a local agent. Sign in using the agent's own CLI first.</CardDescription></CardHeader>
        <CardContent className="flex flex-col gap-3">
          <div className="flex items-start justify-between gap-4">
            <div>
              <Label htmlFor="ai-processing-enabled">Automatic AI processing</Label>
              <p id="ai-processing-help" className="mt-1 text-sm text-muted-foreground">When off, new notes keep their audio and raw transcript. Nothing is classified or generated until you process that note.</p>
            </div>
            <Switch id="ai-processing-enabled" checked={settings.automaticProcessingEnabled()} disabled={settings.state !== "loaded" || settings.saving} aria-describedby="ai-processing-help" onCheckedChange={async (checked) => { if (await settings.saveAiProcessing(checked)) toast.success("AI processing preference saved"); else toast.error(settings.saveError ?? "Couldn't save AI processing preference"); }} />
          </div>
          {settings.automaticProcessingEnabled() ? <>
            <Label htmlFor="default-note-agent">Default agent</Label>
            <select id="default-note-agent" className="rounded-md border bg-background p-2 text-sm" disabled={store.noteArtifacts.selectingDefault || store.noteArtifacts.prerequisitesState !== "loaded"} value={store.noteArtifacts.profiles.find((profile) => profile.default_profile)?.id ?? ""} onChange={async (event) => { if (await store.noteArtifacts.selectDefaultAgent(Number(event.target.value))) toast.success("Default agent saved"); else toast.error("Could not save default agent"); }}>
              <option value="" disabled>Choose an agent</option>
              {store.noteArtifacts.profiles.map((profile) => <option key={profile.id} value={profile.id} disabled={!profile.available}>{profile.name}{profile.available ? "" : " (not installed)"}</option>)}
            </select>
          </> : settings.state === "loaded" ? <p className="text-sm text-muted-foreground">The saved agent is unchanged. Use Process note on a note when you want classification or a generated output.</p> : null}
          {store.noteArtifacts.prerequisitesError && settings.automaticProcessingEnabled() && <p role="alert" className="text-sm text-destructive">{store.noteArtifacts.prerequisitesError}</p>}
          <p className="text-xs text-muted-foreground">Agent failures are shown on the note and can be retried. They never remove its saved audio or transcript. Existing notes keep their outputs.</p>
        </CardContent>
      </Card>
    </div>;
  }}</Observer>;
}
