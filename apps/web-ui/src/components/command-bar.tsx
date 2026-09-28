import { Observer } from "mobx-react-lite";
import { Link } from "react-router-dom";
import { AudioLines, Square, WifiOff, X } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { CaptureControls } from "@/components/capture-controls";
import { AudioReviewPanel } from "@/components/audio-review-panel";
import { useStore } from "@/stores/root-store";
import { formatDuration } from "@/lib/audio-notes";

export function CommandBar() {
  const store = useStore();
  return <Observer>{() => {
    const notes = store.audioNotes;
    const recording = notes.phase === "recording";
    return <header className="sticky top-0 z-20 w-full border-b bg-background/95 backdrop-blur">
      <div className="mx-auto flex w-full max-w-5xl flex-wrap items-center gap-3 px-4 py-3">
        <AudioLines className={recording ? "size-6 animate-pulse text-destructive" : "size-6 text-primary"} aria-hidden="true" />
        <div className="min-w-0 flex-1"><Link to="/audio-notes" className="font-semibold tracking-tight">Audetic</Link><p className="text-xs text-muted-foreground" role="status">{recording ? `Recording · ${formatDuration(notes.durationSeconds)}` : notes.phase === "review" ? "Ready for review" : notes.phase === "transcribing" || notes.phase === "compressing" ? "Processing audio in the background" : "Your voice, one stream"}</p></div>
        {!store.daemonReachable && <span className="flex items-center gap-1 text-xs text-destructive" role="status"><WifiOff className="size-3" />Daemon offline</span>}
        {recording ? <>
          <Button variant="outline" size="sm" disabled={notes.commandPending} onClick={() => { if (window.confirm("Discard this recording?")) void notes.cancelCapture(); }}><X data-icon="inline-start" />Cancel</Button>
          <Button variant="destructive" size="sm" disabled={notes.commandPending || !store.daemonReachable} onClick={async () => { if (!await notes.stopCapture()) toast.error("Couldn't stop recording", { description: notes.lastError ?? undefined }); }}><Square data-icon="inline-start" />Stop</Button>
        </> : null}
        <div hidden={recording}><CaptureControls /></div>
      </div>
      {notes.captureDegraded && notes.active && <p role="status" className="mx-auto max-w-5xl px-4 pb-2 text-sm text-destructive">Some audio sources are unavailable. Recording continues with the available source.</p>}
      {notes.lastError && <p role="alert" className="mx-auto max-w-5xl px-4 pb-2 text-sm text-destructive">{notes.lastError}</p>}
      {notes.phase === "review" && notes.noteId !== null && <AudioReviewPanel key={notes.noteId} noteId={notes.noteId} durationSeconds={notes.durationSeconds} title={notes.title} startedAt={null} />}
    </header>;
  }}</Observer>;
}
