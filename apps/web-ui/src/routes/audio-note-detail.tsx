import { useEffect } from "react";
import { Observer } from "mobx-react-lite";
import { Link, useNavigate, useParams, type RouteObject } from "react-router-dom";
import { ArrowLeft, Copy, RefreshCcw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { NoteTitleHeader } from "@/components/note-title-header";
import { TranscriptPlayer } from "@/components/transcript-player";
import { NoteEnrichment } from "@/components/note-enrichment";
import { NoteArtifactsPanel } from "@/components/note-artifacts-panel";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import { noteNeedsRefresh } from "@/lib/audio-notes";
import { copyText } from "@/lib/clipboard";

export const audioNoteDetailRoute: RouteObject = {
  path: "audio-notes/:id",
  loader: async ({ params }) => {
    const id = Number(params.id);
    if (Number.isSafeInteger(id) && id > 0) await getRootStore().audioNotes.loadDetail(id);
    return null;
  },
  Component: AudioNoteDetailRoute,
};

export function AudioNoteDetailRoute() {
  const id = Number(useParams().id);
  const store = useStore();
  const navigate = useNavigate();
  const validId = Number.isSafeInteger(id) && id > 0;
  useEffect(() => {
    if (!validId) return;
    void store.noteArtifacts.loadPrerequisites();
    void store.noteArtifacts.loadArtifacts(id);
    const timer = window.setInterval(() => {
      const note = store.audioNotes.detailCache[id];
      if (note && (noteNeedsRefresh(note) || store.audioNotes.titleMutationStatus[id] === "generating")) {
        void store.audioNotes.loadDetail(id);
        void store.noteArtifacts.loadArtifacts(id);
      } else if (store.noteArtifacts.byNote[id]?.some((artifact) => artifact.status === "pending" || artifact.status === "running")) {
        void store.noteArtifacts.loadArtifacts(id);
      }
    }, 2000);
    return () => window.clearInterval(timer);
  }, [id, store, validId]);

  return <div className="mx-auto flex max-w-5xl flex-col gap-6 p-4 sm:p-8">
    <Link to="/audio-notes" className="inline-flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground"><ArrowLeft className="size-4" />Audio Notes</Link>
    <Observer>{() => {
      const notes = store.audioNotes;
      const note = notes.detailCache[id];
      if (!validId) return <p role="alert">Invalid audio note id.</p>;
      if (!note) return notes.detailStatus[id] === "error" ? <Card><CardHeader><CardTitle>Note unavailable</CardTitle><CardDescription role="alert">{notes.detailError[id] ?? "This note may have been deleted."}</CardDescription></CardHeader><CardContent><Button onClick={() => void notes.loadDetail(id)} variant="outline">Try again</Button></CardContent></Card> : <><Skeleton className="h-16 w-full" /><Skeleton className="h-64 w-full" /></>;
      return <>
        <NoteTitleHeader note={note} onDelete={async () => { if (!window.confirm("Delete this audio note? It will be hidden from all views; the audio remains on disk.")) return; if (await notes.deleteNote(id)) { toast.success("Audio note deleted"); navigate("/audio-notes"); } else toast.error(notes.lastError ?? "Could not delete note"); }} />
        {notes.detailError[id] && <p role="alert" className="text-sm text-destructive">{notes.detailError[id]} <Button size="sm" variant="ghost" onClick={() => void notes.loadDetail(id)}>Refresh</Button></p>}
        {!["completed", "error", "cancelled"].includes(note.status) && <p className="text-sm text-muted-foreground" role="status">{note.status === "recording" ? "Recording in progress" : note.status === "review" ? "Review this recording in the capture bar above" : "Processing audio… This page updates automatically."}</p>}
        {note.error && <Card><CardHeader><CardTitle>Transcription needs attention</CardTitle><CardDescription>Your saved audio is retained. Check Settings → Setup if a recording or transcription dependency is missing.</CardDescription></CardHeader><CardContent className="flex flex-col gap-3"><pre className="whitespace-pre-wrap text-sm text-destructive">{note.error}</pre><Button className="self-start" disabled={notes.processing[id]} onClick={() => void notes.retryTranscription(id)}><RefreshCcw data-icon="inline-start" />Retry transcription</Button></CardContent></Card>}
        <Card><CardHeader><div className="flex flex-wrap items-center justify-between gap-3"><div><CardTitle>Raw transcript</CardTitle><CardDescription>Your original words, unchanged by AI processing.</CardDescription></div><Button variant="outline" size="sm" disabled={!note.transcript_text} onClick={() => void copyText(note.transcript_text ?? "", "Raw transcript copied")}><Copy data-icon="inline-start" />Copy transcript</Button></div></CardHeader><CardContent><TranscriptPlayer noteId={id} text={note.transcript_text ?? ""} segments={note.transcript_segments} completed={note.status === "completed"} /></CardContent></Card>
        <NoteEnrichment note={note} />
        <NoteArtifactsPanel noteId={id} canGenerate={Boolean(note.transcript_text) && note.status === "completed"} />
        <Card><CardHeader><CardTitle>Source files</CardTitle><CardDescription>Audio is retained independently of the transcript and AI results.</CardDescription></CardHeader><CardContent className="flex flex-col gap-3">{[["Audio", note.audio_path], ["Transcript", note.transcript_path]].map(([label, path]) => path ? <div key={label} className="flex items-center justify-between gap-3"><div className="min-w-0"><p className="text-xs text-muted-foreground">{label}</p><p className="truncate font-mono text-xs">{path}</p></div><Button variant="ghost" size="icon" aria-label={`Copy ${label?.toLowerCase()} path`} onClick={() => void copyText(path, "Path copied")}><Copy /></Button></div> : null)}</CardContent></Card>
      </>;
    }}</Observer>
  </div>;
}
