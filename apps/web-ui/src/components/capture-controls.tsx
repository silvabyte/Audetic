import { useEffect, useState } from "react";
import { reaction } from "mobx";
import { Observer } from "mobx-react-lite";
import { Link } from "react-router-dom";
import { Check, ChevronDown, Loader2, Mic, Monitor, X } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { RecentTitleSuggestions } from "@/components/note-title-picker";
import { useStore } from "@/stores/root-store";
import { DEFAULT_CAPTURE_OPTIONS } from "@/lib/capture-options";
import { cn } from "@/lib/utils";

/** Options describe acquisition and delivery, never what the recording means. */
export function CaptureControls({ onStarted }: { onStarted?: () => void }) {
  const store = useStore();
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const [systemAudio, setSystemAudio] = useState(false);
  const [review, setReview] = useState(false);
  const [autoPasteOverride, setAutoPasteOverride] = useState<boolean | null>(null);
  const [copy, setCopy] = useState<boolean>(DEFAULT_CAPTURE_OPTIONS.copy_to_clipboard);

  useEffect(() => reaction(
    () => store.audioNotes.active || store.audioNotes.phase === "review",
    (unavailable) => { if (unavailable) setOpen(false); },
  ), [store]);

  async function start(): Promise<void> {
    const ok = await store.audioNotes.startCapture({
      title: title.trim() || null,
      capture_source: systemAudio ? "microphone_and_system" : "microphone",
      review_before_processing: review,
      auto_paste: store.noteSettings.captureAutoPaste(autoPasteOverride),
      copy_to_clipboard: copy,
    });
    if (ok) { setOpen(false); setTitle(""); setAutoPasteOverride(null); onStarted?.(); }
    else toast.error("Couldn't start recording", { description: store.audioNotes.lastError ?? undefined });
  }

  return <Observer>{() => {
    const unavailable = store.audioNotes.active || store.audioNotes.phase === "review";
    const pending = store.audioNotes.commandPending;
    const disabled = !store.daemonReachable || pending || unavailable || store.noteSettings.saving;
    const autoPaste = autoPasteOverride ?? store.noteSettings.effectiveDefault;
    const source = systemAudio ? "Mic + system" : "Microphone";
    const summary = [autoPaste && "Auto-paste on", review && "Review first", copy && "Copy on"].filter(Boolean).join(" · ");
    return <div className="flex min-w-0 items-center gap-2 sm:gap-3" role="group" aria-label="New recording">
      <Popover open={open && !unavailable} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button variant="ghost" className="h-11 min-w-0 gap-2 rounded-lg px-2 text-left sm:px-3" disabled={pending || unavailable} aria-label={`Capture options: ${source}${summary ? `, ${summary}` : ""}`}>
            {systemAudio ? <Monitor className="hidden size-4 text-muted-foreground sm:block" aria-hidden="true" /> : <Mic className="hidden size-4 text-muted-foreground sm:block" aria-hidden="true" />}
            <span className="flex min-w-0 flex-col gap-0.5">
              <span className="truncate text-xs font-medium sm:text-sm">{source}</span>
              <span className="max-w-28 truncate text-[11px] font-normal text-muted-foreground sm:max-w-64 sm:text-xs" title={summary || "Capture options"}>{summary || "Capture options"}</span>
            </span>
            <ChevronDown className={cn("size-3.5 text-muted-foreground motion-safe:transition-transform", open && "rotate-180")} aria-hidden="true" />
          </Button>
        </PopoverTrigger>
        <PopoverContent align="end" sideOffset={12} collisionPadding={12} className="flex max-h-[min(42rem,var(--radix-popover-content-available-height))] w-[min(23rem,calc(100vw-1.5rem))] flex-col overflow-hidden rounded-xl p-0 shadow-lg motion-reduce:animate-none" aria-labelledby="capture-options-title" aria-describedby="capture-options-description">
          <div className="flex items-start justify-between gap-3 px-5 pb-4 pt-5">
            <div><h2 id="capture-options-title" className="text-sm font-semibold">Make it your recording</h2><p id="capture-options-description" className="mt-1 text-xs text-muted-foreground">A quick thought or the whole conversation.</p></div>
            <Button variant="ghost" size="icon" className="-mr-2 -mt-2 size-9 shrink-0 rounded-full text-muted-foreground" onClick={() => setOpen(false)} aria-label="Close capture options"><X className="size-4" /></Button>
          </div>

          <div className="min-h-0 space-y-5 overflow-y-auto overscroll-contain px-5 pb-5">
            <fieldset>
              <legend className="mb-2.5 text-xs font-medium text-muted-foreground">Audio source</legend>
              <div className="grid grid-cols-2 gap-2">
                <SourceOption value="microphone" label="Microphone" description="Just your voice" checked={!systemAudio} onChange={() => setSystemAudio(false)} icon={<Mic className="size-4" />} />
                <SourceOption value="microphone_and_system" label="Mic + system" description="You and your computer" checked={systemAudio} onChange={() => setSystemAudio(true)} icon={<Monitor className="size-4" />} />
              </div>
            </fieldset>
            <CaptureOption id="capture-review" label="Review before sending" description="Listen and trim before transcription." checked={review} onChange={setReview} />

            <div className="divide-y rounded-lg border">
              <details className="group">
                <summary className="flex min-h-12 cursor-pointer list-none items-center justify-between gap-3 rounded-lg px-3 py-3 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
                  <span className="min-w-0 truncate">{title.trim() ? title.trim() : "Add a title"}<span className="ml-2 font-normal text-muted-foreground">{!title.trim() && "Optional"}</span></span>
                  <ChevronDown className="size-3.5 shrink-0 text-muted-foreground group-open:rotate-180" aria-hidden="true" />
                </summary>
                <div className="space-y-2 px-3 pb-3">
                  <Label htmlFor="capture-title" className="sr-only">Title (optional)</Label>
                  <Input id="capture-title" value={title} onChange={(event) => setTitle(event.target.value)} placeholder="Let AI suggest a title" className="h-9 text-sm" />
                  <RecentTitleSuggestions query={title} onSelect={setTitle} />
                </div>
              </details>
              <details className="group" open={store.noteSettings.loadError ? true : undefined}>
                <summary className="flex min-h-12 cursor-pointer list-none items-center justify-between gap-3 rounded-lg px-3 py-3 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
                  <span>Transcript delivery<span className="mt-0.5 block font-normal text-muted-foreground">{autoPaste ? "Auto-paste on" : "Auto-paste off"}{copy ? " · Copy on" : ""}</span></span>
                  <ChevronDown className="size-3.5 shrink-0 text-muted-foreground group-open:rotate-180" aria-hidden="true" />
                </summary>
                <div className="space-y-4 px-3 pb-4">
                  <div className="space-y-2">
                    <Label htmlFor="capture-paste" className="text-xs">Automatic paste</Label>
                    <select id="capture-paste" className="h-10 w-full rounded-md border border-input bg-background px-2 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" value={autoPasteOverride === null ? "default" : autoPasteOverride ? "on" : "off"} onChange={(event) => setAutoPasteOverride(event.target.value === "default" ? null : event.target.value === "on")} aria-describedby="capture-paste-help">
                      <option value="default">Saved preference ({store.noteSettings.effectiveDefault ? "on" : "off"}{store.noteSettings.state !== "loaded" ? " until loaded" : ""})</option>
                      <option value="off">Off for this recording</option>
                      <option value="on">On for this recording</option>
                    </select>
                    <p id="capture-paste-help" className="text-xs leading-relaxed text-muted-foreground">Paste the raw transcript into the focused app. One-time overrides reset when recording starts.</p>
                    {store.noteSettings.loadError && <p role="alert" className="text-xs text-destructive">Couldn't load your preference. Auto-paste stays off unless you enable it here. <Button variant="ghost" size="sm" onClick={() => void store.noteSettings.load()}>Retry</Button></p>}
                  </div>
                  <CaptureOption id="capture-copy" label="Copy to clipboard" description="Copy the raw transcript, not AI output." checked={copy} onChange={setCopy} />
                  <Link to="/settings/capture" className="inline-block rounded-sm text-xs text-muted-foreground underline underline-offset-4 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring" onClick={() => setOpen(false)}>Manage saved delivery preference</Link>
                </div>
              </details>
            </div>
            <p className="text-[11px] leading-relaxed text-muted-foreground">Source, review and clipboard choices stay for this session.</p>
          </div>

          <div className="shrink-0 border-t bg-muted/30 p-3">
            <Button onClick={() => void start()} disabled={disabled} className="h-11 w-full rounded-lg">
              {pending ? <Loader2 className="size-4 motion-safe:animate-spin" aria-hidden="true" /> : <span className="size-2 rounded-full bg-current" aria-hidden="true" />}
              {pending ? "Starting…" : "Start recording"}
            </Button>
            {!store.daemonReachable && <p className="mt-2 text-center text-xs text-muted-foreground">Reconnect to start recording.</p>}
          </div>
        </PopoverContent>
      </Popover>
      <Button onClick={() => void start()} disabled={disabled} className="h-11 shrink-0 gap-2.5 rounded-full px-4 sm:px-5" aria-label={pending ? "Starting recording" : "Record note"}>
        {pending ? <Loader2 className="size-3.5 motion-safe:animate-spin" aria-hidden="true" /> : <span className="size-2 rounded-full bg-current" aria-hidden="true" />}
        <span>{pending ? "Starting…" : <>Record<span className="hidden sm:inline"> note</span></>}</span>
      </Button>
    </div>;
  }}</Observer>;
}

function SourceOption({ value, label, description, checked, onChange, icon }: { value: string; label: string; description: string; checked: boolean; onChange: () => void; icon: React.ReactNode }) {
  return <label className="relative cursor-pointer">
    <input type="radio" name="capture-source" value={value} checked={checked} onChange={onChange} className="peer sr-only" />
    <span className="flex h-full flex-col gap-1 rounded-lg border border-input p-3 transition-colors hover:bg-muted/60 peer-checked:border-foreground/50 peer-checked:bg-muted/60 peer-focus-visible:ring-2 peer-focus-visible:ring-ring peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-background">
      <span className="mb-2 flex items-center justify-between" aria-hidden="true">{icon}{checked && <Check className="size-3.5" />}</span>
      <span className="text-xs font-medium">{label}</span>
      <span className="text-[11px] leading-relaxed text-muted-foreground">{description}</span>
    </span>
  </label>;
}

function CaptureOption({ id, label, description, checked, onChange }: { id: string; label: string; description: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return <div className="flex items-center justify-between gap-3"><div className="flex flex-col gap-1"><Label htmlFor={id} className="text-xs">{label}</Label><p id={`${id}-description`} className="text-xs leading-relaxed text-muted-foreground">{description}</p></div><Switch id={id} checked={checked} onCheckedChange={onChange} aria-describedby={`${id}-description`} className="shrink-0" /></div>;
}
