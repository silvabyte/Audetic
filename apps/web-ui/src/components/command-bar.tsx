import { useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import { Link } from "react-router-dom";
import { ArrowUpRight, Loader2, Scissors, Square, Trash2, WifiOff } from "lucide-react";
import { toast } from "sonner";
import { Button, buttonVariants } from "@/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { CaptureControls } from "@/components/capture-controls";
import { AudioReviewPanel } from "@/components/audio-review-panel";
import { AudeticMark } from "@/components/audetic-mark";
import { useStore } from "@/stores/root-store";
import { formatDuration } from "@/lib/audio-notes";
import { cn } from "@/lib/utils";
import logoLight from "../../../../assets/audetic_logo_light.svg";
import logoDark from "../../../../assets/audetic_logo_dark.svg";

export function CommandBar() {
  const store = useStore();
  const captureActionsRef = useRef<HTMLDivElement>(null);

  // Keep keyboard users at the controls when their action swaps the focused button.
  // Status changes from another client never move focus.
  function focusCaptureActions(): void {
    requestAnimationFrame(() => captureActionsRef.current?.focus());
  }

  return <Observer>{() => {
    const notes = store.audioNotes;
    const recording = notes.phase === "recording";
    const reviewing = notes.phase === "review";
    const processing = notes.phase === "transcribing" || notes.phase === "compressing";
    return <header className="sticky top-0 z-20 w-full border-b bg-background/95 backdrop-blur">
      <div className="flex min-h-[4.5rem] items-center justify-between gap-3 px-3 py-3 sm:gap-6 sm:px-5">
        <Link to="/audio-notes" aria-label="Audetic — audio notes" className="inline-flex h-11 shrink-0 items-center gap-3 rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
          <AudeticMark state={!store.daemonReachable ? "offline" : recording ? "recording" : reviewing ? "review" : processing ? "processing" : notes.lastError ? "error" : "idle"} />
          <img src={logoLight} alt="" className="hidden h-auto w-28 sm:block dark:sm:hidden" /><img src={logoDark} alt="" className="hidden h-auto w-28 dark:sm:block" />
        </Link>

        <div ref={captureActionsRef} tabIndex={-1} role="group" aria-label="Capture actions" className="flex min-w-0 items-center justify-end gap-2 outline-none sm:gap-4">
          {recording && <RecordingActions onComplete={focusCaptureActions} />}
          {reviewing && <div className="flex min-w-0 items-center gap-2.5" role="status">
            <Scissors className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <div className="min-w-0">
              <p className="text-sm font-medium">Ready to review</p>
              <p className="text-xs text-muted-foreground">{formatDuration(notes.durationSeconds)} · Listen, trim, then send</p>
            </div>
          </div>}
          {/* Keep acquisition choices mounted through the whole capture lifecycle. */}
          <div className="min-w-0" hidden={recording || reviewing}><CaptureControls onStarted={focusCaptureActions} /></div>
        </div>
      </div>
      {processing && <div className="flex min-h-11 items-center justify-between gap-3 border-t bg-muted/30 px-3 sm:justify-end sm:px-5">
        <p className="flex items-center gap-2 text-xs text-muted-foreground" role="status">
          <Loader2 className="size-3.5 shrink-0 motion-safe:animate-spin" aria-hidden="true" />
          <span>{notes.phase === "compressing" ? "Preparing audio…" : "Transcribing…"}</span>
          <span className="hidden sm:inline">You can record another note.</span>
        </p>
        {notes.noteId !== null && <Link to={`/audio-notes/${notes.noteId}`} className={cn(buttonVariants({ variant: "ghost", size: "sm" }), "h-11 shrink-0 text-xs")}>
          Open note<ArrowUpRight className="size-3.5" aria-hidden="true" />
        </Link>}
      </div>}
      {!store.daemonReachable && <div role="status" className="flex flex-wrap items-center justify-end gap-x-3 gap-y-1 border-t bg-muted/40 px-4 py-2 text-xs sm:px-5">
        <span className="flex items-center gap-2"><WifiOff className="size-3.5 text-muted-foreground" aria-hidden="true" />Connection lost. Reconnecting…</span>
        <Link to="/settings/setup" className="rounded-sm underline underline-offset-4 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">Check setup</Link>
      </div>}
      {notes.captureDegraded && notes.active && <p role="status" className="border-t px-4 py-2 text-sm text-destructive sm:px-5">Some audio sources are unavailable. Recording continues with the available source.</p>}
      {notes.lastError && <p role="alert" className="border-t px-4 py-2 text-sm text-destructive sm:px-5">{notes.lastError}</p>}
      {reviewing && notes.noteId !== null && <AudioReviewPanel key={notes.noteId} noteId={notes.noteId} durationSeconds={notes.durationSeconds} title={notes.title} startedAt={null} />}
    </header>;
  }}</Observer>;
}

function RecordingActions({ onComplete }: { onComplete: () => void }) {
  const store = useStore();
  const [discardOpen, setDiscardOpen] = useState(false);
  const [discardError, setDiscardError] = useState<string | null>(null);
  const keepRecordingRef = useRef<HTMLButtonElement>(null);

  return <Observer>{() => {
    const notes = store.audioNotes;
    const disabled = notes.commandPending || !store.daemonReachable;
    return <div className="flex items-center gap-2 sm:gap-3" role="group" aria-label="Recording controls">
      <div className="flex items-center gap-2 pr-1 sm:gap-3 sm:pr-2">
        <span className="size-2 rounded-full bg-[#b85346] dark:bg-[#e38a79]" aria-hidden="true" />
        <span className="sr-only sm:not-sr-only sm:text-sm sm:text-muted-foreground" role="status">Recording</span>
        {/* The timer is readable on demand, not announced on every status poll. */}
        <span className="font-mono text-sm tabular-nums" aria-label={`Recording duration ${formatDuration(notes.durationSeconds)}`}>{formatDuration(notes.durationSeconds)}</span>
      </div>
      <Button className="h-11 rounded-full px-4 sm:px-5" disabled={disabled} aria-busy={notes.commandPending} onClick={async () => {
        if (!await notes.stopCapture()) toast.error("Couldn't stop recording", { description: notes.lastError ?? undefined });
        else onComplete();
      }}>
        {notes.commandPending ? <Loader2 className="size-3.5 motion-safe:animate-spin" aria-hidden="true" /> : <Square className="size-3 fill-current" aria-hidden="true" />}
        Stop
      </Button>
      <Dialog open={discardOpen} onOpenChange={(open) => { setDiscardOpen(open); setDiscardError(null); }}>
        <DialogTrigger asChild><Button variant="ghost" size="icon" className="size-11 rounded-full text-muted-foreground hover:text-destructive" disabled={disabled} aria-label="Discard recording" title="Discard recording"><Trash2 className="size-4" /></Button></DialogTrigger>
        <DialogContent className="max-w-sm" onOpenAutoFocus={(event) => { event.preventDefault(); keepRecordingRef.current?.focus(); }}>
          <DialogHeader>
            <DialogTitle>Discard this recording?</DialogTitle>
            <DialogDescription>The audio captured so far will be deleted. Keep recording if you’re not finished yet.</DialogDescription>
          </DialogHeader>
          {discardError && <p role="alert" className="text-sm text-destructive">{discardError}</p>}
          {!store.daemonReachable && <p role="status" className="text-sm text-muted-foreground">Waiting for a connection before you can discard.</p>}
          <DialogFooter className="gap-2 sm:gap-0">
            <DialogClose asChild><Button ref={keepRecordingRef} variant="outline">Keep recording</Button></DialogClose>
            <Button variant="destructive" disabled={disabled} onClick={async () => {
              setDiscardError(null);
              if (!await notes.cancelCapture()) setDiscardError(notes.lastError ?? "Couldn't discard recording. Try again.");
              else onComplete();
            }}>{notes.commandPending ? "Discarding…" : "Discard recording"}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>;
  }}</Observer>;
}
