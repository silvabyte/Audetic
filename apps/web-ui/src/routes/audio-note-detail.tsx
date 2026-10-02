import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { Observer } from "mobx-react-lite";
import { Link, useNavigate, useParams, type RouteObject } from "react-router-dom";
import { ArrowLeft, Copy, RefreshCcw } from "lucide-react";
import { toast } from "sonner";
import { AudioTransport } from "@/components/audio-transport";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { NoteTitleHeader } from "@/components/note-title-header";
import { TranscriptPlayer } from "@/components/transcript-player";
import { NoteEnrichment } from "@/components/note-enrichment";
import { NoteArtifactsPanel } from "@/components/note-artifacts-panel";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import { noteNeedsRefresh } from "@/lib/audio-notes";
import { copyText } from "@/lib/clipboard";
import { parseTalkingPoints } from "@/lib/talking-points";
import { cn } from "@/lib/utils";

const TABS = ["transcript", "generated", "details"] as const;
type NoteTab = typeof TABS[number];

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
  const audioRef = useRef<HTMLAudioElement>(null);
  const [currentTime, setCurrentTime] = useState(0);
  const [activeTab, setActiveTab] = useState<NoteTab>("transcript");
  const validId = Number.isSafeInteger(id) && id > 0;

  useEffect(() => {
    if (!validId) return;
    void store.audioNotes.loadClassificationKinds();
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

  useEffect(() => {
    setCurrentTime(0);
    setActiveTab("transcript");
  }, [id]);

  function seek(seconds: number): void {
    if (!audioRef.current) return;
    audioRef.current.currentTime = seconds;
    setCurrentTime(seconds);
  }

  function handleTabKeyDown(event: KeyboardEvent<HTMLButtonElement>, tab: NoteTab): void {
    const index = TABS.indexOf(tab);
    let nextIndex: number | null = null;
    if (event.key === "ArrowRight") nextIndex = (index + 1) % TABS.length;
    if (event.key === "ArrowLeft") nextIndex = (index - 1 + TABS.length) % TABS.length;
    if (event.key === "Home") nextIndex = 0;
    if (event.key === "End") nextIndex = TABS.length - 1;
    if (nextIndex === null) return;
    event.preventDefault();
    const next = TABS[nextIndex];
    if (!next) return;
    setActiveTab(next);
    event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]")[nextIndex]?.focus();
  }

  return <div className="mx-auto flex max-w-5xl flex-col px-4 py-6 sm:px-8 sm:py-10">
    <Link to="/audio-notes" className="mb-8 inline-flex w-fit items-center gap-2 text-sm text-muted-foreground hover:text-foreground"><ArrowLeft className="size-4" />Audio Notes</Link>
    <Observer>{() => {
      const notes = store.audioNotes;
      const note = notes.detailCache[id];
      if (!validId) return <p role="alert">Invalid audio note id.</p>;
      if (!note) return notes.detailStatus[id] === "error" ? <Card><CardHeader><CardTitle>Note unavailable</CardTitle><CardDescription role="alert">{notes.detailError[id] ?? "This note may have been deleted."}</CardDescription></CardHeader><CardContent><Button onClick={() => void notes.loadDetail(id)} variant="outline">Try again</Button></CardContent></Card> : <><Skeleton className="h-16 w-full" /><Skeleton className="mt-8 h-64 w-full" /></>;
      const talkingPoints = parseTalkingPoints(store.noteArtifacts.byNote[id] ?? [], note.duration_seconds);
      return <>
        <NoteTitleHeader note={note} onDelete={async () => { if (!window.confirm("Delete this audio note? It will be hidden from all views; the audio remains on disk.")) return; if (await notes.deleteNote(id)) { toast.success("Audio note deleted"); navigate("/audio-notes"); } else toast.error(notes.lastError ?? "Could not delete note"); }} />
        {notes.detailError[id] ? <p role="alert" className="mt-5 text-sm text-destructive">{notes.detailError[id]} <Button size="sm" variant="ghost" onClick={() => void notes.loadDetail(id)}>Refresh</Button></p> : null}
        {!["completed", "error", "cancelled"].includes(note.status) ? <p className="mt-5 text-sm text-muted-foreground" role="status">{note.status === "recording" ? "Recording in progress" : note.status === "review" ? "Review this recording in the capture bar above" : "Processing audio… This page updates automatically."}</p> : null}
        {note.error ? <Card className="mt-6"><CardHeader><CardTitle>Transcription needs attention</CardTitle><CardDescription>Your saved audio is retained. Check Settings → Setup if a recording or transcription dependency is missing.</CardDescription></CardHeader><CardContent className="flex flex-col gap-3"><pre className="whitespace-pre-wrap text-sm text-destructive">{note.error}</pre><Button className="self-start" disabled={notes.processing[id]} onClick={() => void notes.retryTranscription(id)}><RefreshCcw data-icon="inline-start" />Retry transcription</Button></CardContent></Card> : null}

        <div className="sticky top-0 z-10 mt-8">
          <AudioTransport key={id} noteId={id} audioRef={audioRef} currentTime={currentTime} durationHint={note.duration_seconds} markers={talkingPoints} onTimeChange={setCurrentTime} />
          {talkingPoints.length ? <div className="flex gap-2 overflow-x-auto border-b bg-background/95 px-1 py-3 backdrop-blur" aria-label="Recording chapters">{talkingPoints.map((point) => <button key={point.seconds} type="button" onClick={() => seek(point.seconds)} className="shrink-0 rounded-full border px-3 py-1 text-xs text-muted-foreground hover:border-foreground/30 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><span className="mr-1.5 font-mono tabular-nums">{point.timestamp}</span>{point.label}</button>)}</div> : null}
        </div>

        <div role="tablist" aria-label="Audio note sections" className="mt-8 flex border-b">
          {TABS.map((tab) => <button key={tab} id={`note-tab-${tab}`} type="button" role="tab" aria-selected={activeTab === tab} aria-controls={`note-panel-${tab}`} tabIndex={activeTab === tab ? 0 : -1} onClick={() => setActiveTab(tab)} onKeyDown={(event) => handleTabKeyDown(event, tab)} className={cn("border-b-2 border-transparent px-4 py-3 text-sm capitalize text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring", activeTab === tab && "border-foreground text-foreground")}>{tab}</button>)}
        </div>

        <main className="mx-auto min-h-96 w-full max-w-3xl py-8">
          <div id="note-panel-transcript" role="tabpanel" aria-labelledby="note-tab-transcript" tabIndex={0} hidden={activeTab !== "transcript"} className="focus-visible:outline-none"><section aria-labelledby="transcript-heading"><div className="mb-6 flex flex-wrap items-end justify-between gap-3 border-b pb-4"><div><p className="mb-1 text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">Source text</p><h2 id="transcript-heading" className="text-xl font-semibold tracking-tight">Raw transcript</h2></div><Button variant="ghost" size="sm" disabled={!note.transcript_text} onClick={() => void copyText(note.transcript_text ?? "", "Raw transcript copied")}><Copy data-icon="inline-start" />Copy transcript</Button></div><TranscriptPlayer noteId={id} text={note.transcript_text ?? ""} segments={note.transcript_segments} completed={note.status === "completed"} currentTime={currentTime} onSeek={seek} /></section></div>
          <div id="note-panel-generated" role="tabpanel" aria-labelledby="note-tab-generated" tabIndex={0} hidden={activeTab !== "generated"} className="focus-visible:outline-none"><NoteArtifactsPanel noteId={id} canGenerate={Boolean(note.transcript_text) && note.status === "completed"} hasSegments={Boolean(note.transcript_segments?.length)} /></div>
          <div id="note-panel-details" role="tabpanel" aria-labelledby="note-tab-details" tabIndex={0} hidden={activeTab !== "details"} className="focus-visible:outline-none"><div className="flex flex-col gap-10"><NoteEnrichment key={note.id} note={note} /><SourceFiles note={note} /></div></div>
        </main>
      </>;
    }}</Observer>
  </div>;
}

function SourceFiles({ note }: { note: AudioNoteDetail }) {
  return <section aria-labelledby="source-files-heading"><h2 id="source-files-heading" className="mb-1 font-semibold">Source files</h2><p className="mb-5 text-sm text-muted-foreground">Audio is retained independently of the transcript and generated views.</p><div className="divide-y border-y">{[["Audio", note.audio_path], ["Transcript", note.transcript_path]].map(([label, path]) => path ? <div key={label} className="flex items-center justify-between gap-3 py-4"><div className="min-w-0"><p className="text-xs text-muted-foreground">{label}</p><p className="truncate font-mono text-xs">{path}</p></div><Button variant="ghost" size="icon" aria-label={`Copy ${label?.toLowerCase()} path`} onClick={() => void copyText(path, "Path copied")}><Copy /></Button></div> : null)}</div></section>;
}
