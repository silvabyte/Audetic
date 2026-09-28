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
        <CardHeader><CardTitle>Automatic paste</CardTitle><CardDescription>Off by default. This preference is saved by the daemon and persists across app and daemon restarts.</CardDescription></CardHeader>
        <CardContent className="flex flex-col gap-4">
          <div className="flex items-start justify-between gap-4">
            <div className="flex flex-col gap-1.5"><Label htmlFor="saved-auto-paste">Automatically paste raw transcripts</Label><p id="saved-auto-paste-help" className="text-sm text-muted-foreground">When enabled, the saved raw transcript is pasted into the focused app immediately. AI classification and artifacts arrive later and are never pasted automatically.</p></div>
            <Switch id="saved-auto-paste" checked={settings.effectiveDefault} disabled={settings.state !== "loaded" || settings.saving} aria-describedby="saved-auto-paste-help" onCheckedChange={async (checked) => { if (await settings.saveAutoPaste(checked)) toast.success("Delivery preference saved"); else toast.error(settings.saveError ?? "Couldn't save delivery preference"); }} />
          </div>
          <p className="text-xs text-muted-foreground">Applies to future captures that use the saved preference. An active recording keeps its original delivery choice. Capture options can explicitly turn automatic paste on or off for one recording.</p>
          <p role="status" className="text-sm text-muted-foreground">{settings.saving ? "Saving preference…" : settings.state === "loading" || settings.state === "idle" ? "Loading saved preference… Automatic paste is off until it loads." : settings.state === "error" ? "Saved preference unavailable. Browser captures default to automatic paste off." : `Saved preference: ${settings.settings.auto_paste ? "on" : "off"}`}</p>
          {settings.loadError && <p role="alert" className="text-sm text-destructive">Couldn't load settings: {settings.loadError}</p>}
          {settings.saveError && <p role="alert" className="text-sm text-destructive">Couldn't confirm the save: {settings.saveError}. Automatic paste stays off in browser captures until the saved preference is reloaded.</p>}
          <Button variant="outline" className="self-start" disabled={settings.saving || settings.state === "loading"} onClick={() => void settings.load()}><RefreshCcw data-icon="inline-start" />{settings.loadError ? "Retry loading" : "Reload saved preference"}</Button>
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>AI processing agent</CardTitle><CardDescription>Automatically classify new notes and generate their useful outputs with this local agent. Sign in using the agent's own CLI first.</CardDescription></CardHeader>
        <CardContent className="flex flex-col gap-3">
          <Label htmlFor="default-note-agent">Default agent</Label>
          <select id="default-note-agent" className="rounded-md border bg-background p-2 text-sm" disabled={store.noteArtifacts.selectingDefault || store.noteArtifacts.prerequisitesState !== "loaded"} value={store.noteArtifacts.profiles.find((profile) => profile.default_profile)?.id ?? ""} onChange={async (event) => { if (await store.noteArtifacts.selectDefaultAgent(Number(event.target.value))) toast.success("Default agent saved"); else toast.error("Could not save default agent"); }}>
            <option value="" disabled>Choose an agent</option>
            {store.noteArtifacts.profiles.map((profile) => <option key={profile.id} value={profile.id} disabled={!profile.available}>{profile.name}{profile.available ? "" : " (not installed)"}</option>)}
          </select>
          {store.noteArtifacts.prerequisitesError && <p role="alert" className="text-sm text-destructive">{store.noteArtifacts.prerequisitesError}</p>}
          <p className="text-xs text-muted-foreground">Agent failures are shown on the note and can be retried. They never remove its saved audio or transcript.</p>
        </CardContent>
      </Card>
    </div>;
  }}</Observer>;
}
