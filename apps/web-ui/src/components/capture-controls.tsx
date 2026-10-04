import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Link } from "react-router-dom";
import { Mic, SlidersHorizontal } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { RecentTitleSuggestions } from "@/components/note-title-picker";
import { useStore } from "@/stores/root-store";
import { DEFAULT_CAPTURE_OPTIONS } from "@/lib/capture-options";

/** Options describe acquisition and delivery, never what the recording means. */
export function CaptureControls() {
  const store = useStore();
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [systemAudio, setSystemAudio] = useState(false);
  const [review, setReview] = useState(false);
  const [autoPasteOverride, setAutoPasteOverride] = useState<boolean | null>(null);
  const [copy, setCopy] = useState<boolean>(DEFAULT_CAPTURE_OPTIONS.copy_to_clipboard);

  async function start(): Promise<void> {
    const ok = await store.audioNotes.startCapture({
      title: title.trim() || null,
      capture_source: systemAudio ? "microphone_and_system" : "microphone",
      review_before_processing: review,
      auto_paste: store.noteSettings.captureAutoPaste(autoPasteOverride),
      copy_to_clipboard: copy,
    });
    if (ok) { setOpen(false); setTitle(""); setAutoPasteOverride(null); }
    else toast.error("Couldn't start recording", { description: store.audioNotes.lastError ?? undefined });
  }

  return <Observer>{() => {
    const disabled = !store.daemonReachable || store.audioNotes.commandPending || store.audioNotes.active || store.audioNotes.phase === "review" || store.noteSettings.saving;
    const autoPaste = autoPasteOverride ?? store.noteSettings.effectiveDefault;
    return <div className="flex items-center gap-1">
      <Button onClick={() => void start()} disabled={disabled} size="sm" className="gap-1.5" aria-label="Record note" title="Record note"><Mic className="size-4" /><span>Record<span className="hidden sm:inline"> note</span></span></Button>
      {autoPaste && <span className="text-xs text-muted-foreground">Auto-paste on</span>}
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild><Button variant="outline" size="icon" disabled={disabled} aria-label="Capture options"><SlidersHorizontal /></Button></PopoverTrigger>
        <PopoverContent align="end" className="w-[min(24rem,calc(100vw-2rem))] max-h-[75vh] overflow-auto">
          <div className="flex flex-col gap-4">
            <div><h2 className="font-semibold">Capture options</h2><p className="text-xs text-muted-foreground">Just record. AI figures out the kind of note afterward.</p></div>
            <div className="flex flex-col gap-1.5"><Label htmlFor="capture-title">Title (optional)</Label><Input id="capture-title" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Let AI suggest a title" /><RecentTitleSuggestions query={title} onSelect={setTitle} /></div>
            <CaptureOption id="capture-system" label="Include system audio" description="Microphone is always included." checked={systemAudio} onChange={setSystemAudio} />
            <CaptureOption id="capture-review" label="Review before processing" description="Listen and trim the recording before transcribing." checked={review} onChange={setReview} />
            <div className="flex flex-col gap-2">
              <Label htmlFor="capture-paste">Automatic paste for this recording</Label>
              <select id="capture-paste" className="h-10 rounded-md border border-input bg-background px-3 text-sm" value={autoPasteOverride === null ? "default" : autoPasteOverride ? "on" : "off"} onChange={(event) => setAutoPasteOverride(event.target.value === "default" ? null : event.target.value === "on")} aria-describedby="capture-paste-help">
                <option value="default">Use saved preference ({store.noteSettings.effectiveDefault ? "on" : "off"}{store.noteSettings.state !== "loaded" ? " until loaded" : ""})</option>
                <option value="off">Off for this recording</option>
                <option value="on">On for this recording</option>
              </select>
              <p id="capture-paste-help" className="text-xs text-muted-foreground">Pastes the raw transcript into the focused app as soon as it is saved, before AI processing. The override resets after recording starts.</p>
              <Link to="/settings/capture" className="text-xs underline underline-offset-4" onClick={() => setOpen(false)}>Change saved delivery preference</Link>
              {store.noteSettings.loadError && <p role="alert" className="text-xs text-destructive">Couldn't load the saved preference. Automatic paste stays off unless explicitly enabled here. {store.noteSettings.loadError} <Button variant="ghost" size="sm" onClick={() => void store.noteSettings.load()}>Retry</Button></p>}
            </div>
            <CaptureOption id="capture-copy" label="Copy transcript to clipboard" description="Off by default. Copies raw text, not AI output." checked={copy} onChange={setCopy} />
            <p className="text-xs text-muted-foreground">Source, review, and clipboard options apply to recordings started here during this session. Saved delivery changes never affect an active capture.</p>
            <Button onClick={() => void start()} disabled={disabled}><Mic data-icon="inline-start" />Start recording</Button>
          </div>
        </PopoverContent>
      </Popover>
    </div>;
  }}</Observer>;
}

function CaptureOption({ id, label, description, checked, onChange }: { id: string; label: string; description: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return <div className="flex items-start justify-between gap-3"><div className="flex flex-col gap-1"><Label htmlFor={id}>{label}</Label><p id={`${id}-description`} className="text-xs text-muted-foreground">{description}</p></div><Switch id={id} checked={checked} onCheckedChange={onChange} aria-describedby={`${id}-description`} /></div>;
}
