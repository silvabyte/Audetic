import { useEffect, useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import { Link, type ActionFunctionArgs, type RouteObject } from "react-router-dom";
import { ArrowRight, AudioLines, ChevronLeft, ChevronRight, RefreshCcw, Search, Upload, Users } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { getRootStore } from "@/stores/singleton";
import { useStore } from "@/stores/root-store";
import type { AudioNoteSummary } from "@/stores/audio-notes-store";
import { BUILT_IN_CLASSIFICATION_KINDS, effectiveClassificationKind, enrichmentLabel, formatDuration, kindLabel } from "@/lib/audio-notes";
import { noteDisplayTitle } from "@/lib/note-title";

export const NOTE_INTENTS = { confirm: "confirm-note", cancel: "cancel-note" } as const;
export const audioNotesRoute: RouteObject = {
  path: "audio-notes",
  loader: async () => { const notes = getRootStore().audioNotes; await Promise.all([notes.loadList(), notes.loadClassificationKinds()]); return null; },
  action: async ({ request }: ActionFunctionArgs) => {
    const data = await request.formData();
    const notes = getRootStore().audioNotes;
    const seconds = (key: string): number | undefined => {
      const value = String(data.get(key) ?? "").trim();
      if (!value) return undefined;
      const number = Number(value);
      if (!Number.isFinite(number) || number < 0) throw new Error("Invalid trim boundary");
      return number;
    };
    try {
      const ok = data.get("intent") === NOTE_INTENTS.confirm
        ? await notes.confirmCapture(seconds("start_seconds"), seconds("end_seconds"))
        : data.get("intent") === NOTE_INTENTS.cancel ? await notes.cancelCapture() : false;
      if (!ok) toast.error(notes.lastError ?? "Could not update recording");
    } catch (error) { toast.error(error instanceof Error ? error.message : String(error)); }
    return null;
  },
  Component: AudioNotesRoute,
};

export function AudioNotesRoute() {
  const store = useStore();
  const input = useRef<HTMLInputElement>(null);
  const dragDepth = useRef(0);
  const [dragging, setDragging] = useState(false);
  const [importing, setImporting] = useState(false);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState("");
  useEffect(() => { setQuery(store.audioNotes.query); setKind(store.audioNotes.kind); }, [store]);
  async function importFiles(files: File[]): Promise<void> {
    if (importing) return;
    setImporting(true);
    try {
      for (const file of files) {
        const id = await store.audioNotes.importFile(file);
        if (id !== null) toast.success(`Imported ${file.name}`, { description: "Transcription will appear in your stream.", action: { label: "Open note", onClick: () => { window.location.assign(`/audio-notes/${id}`); } } });
        else toast.error(`Couldn't import ${file.name}`, { description: store.audioNotes.lastError ?? undefined });
      }
    } finally { setImporting(false); }
  }
  return <div className="relative min-h-full" onDragEnter={(event) => { if (event.dataTransfer.types.includes("Files")) { dragDepth.current++; setDragging(true); } }} onDragLeave={() => { dragDepth.current = Math.max(0, dragDepth.current - 1); if (!dragDepth.current) setDragging(false); }} onDragOver={(event) => { if (event.dataTransfer.types.includes("Files")) event.preventDefault(); }} onDrop={(event) => { event.preventDefault(); dragDepth.current = 0; setDragging(false); void importFiles(Array.from(event.dataTransfer.files)); }}>
    <div className="mx-auto flex max-w-4xl flex-col gap-7 p-4 sm:p-8">
      <header className="flex flex-wrap items-start justify-between gap-4"><div><p className="mb-2 text-xs font-medium uppercase tracking-[0.18em] text-muted-foreground">Capture first. Make sense later.</p><h1 className="text-3xl font-semibold tracking-tight">Audio Notes</h1><p className="mt-2 max-w-xl text-sm text-muted-foreground">Every thought, conversation, and recording. Raw words saved first, useful context added afterward.</p></div>
        <input ref={input} type="file" multiple hidden accept=".wav,.mp3,.m4a,.flac,.ogg,.opus,.mp4,.mkv,.webm,.avi,.mov" onChange={(event) => { const files = Array.from(event.target.files ?? []); event.target.value = ""; void importFiles(files); }} />
        <Button variant="outline" disabled={importing} onClick={() => input.current?.click()}><Upload data-icon="inline-start" />{importing ? "Importing…" : "Import audio / video"}</Button>
      </header>
      <form className="flex flex-wrap items-end gap-3 border-y py-5" onSubmit={(event) => { event.preventDefault(); void store.audioNotes.setFilters(query, kind); }}>
        <label className="flex min-w-48 flex-1 flex-col gap-1.5 text-xs font-medium">Search notes<Input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Titles and transcripts" type="search" /></label>
        <label className="flex w-44 flex-col gap-1.5 text-xs font-medium">Classification<Input list="note-kinds" value={kind} onChange={(event) => setKind(event.target.value)} placeholder="All kinds" /><Observer>{() => <datalist id="note-kinds">{[...new Set([...BUILT_IN_CLASSIFICATION_KINDS, ...store.audioNotes.classificationKinds])].toSorted().map((value) => <option key={value} value={value} />)}</datalist>}</Observer></label>
        <Button type="button" variant={kind === "meeting" ? "secondary" : "ghost"} onClick={() => { const next = kind === "meeting" ? "" : "meeting"; setKind(next); void store.audioNotes.setFilters(query, next); }}><Users data-icon="inline-start" />Meetings</Button>
        <Button type="submit" variant="secondary"><Search data-icon="inline-start" />Search</Button>
      </form>
      <Observer>{() => {
        const notes = store.audioNotes;
        return <section className="flex flex-col gap-4" aria-label="Audio note stream" aria-busy={notes.listStatus === "loading"}>
          <div className="flex items-center justify-between text-xs text-muted-foreground"><span>Newest first{notes.kind ? ` · ${kindLabel(notes.kind)}` : ""}</span><Button variant="ghost" size="sm" disabled={notes.listStatus === "loading"} onClick={() => void notes.loadList()}><RefreshCcw data-icon="inline-start" />Refresh</Button></div>
          {notes.listError && <Card><CardHeader><CardTitle>Couldn't load notes</CardTitle><CardDescription role="alert">{notes.listError}</CardDescription></CardHeader><CardContent><Button variant="outline" onClick={() => void notes.loadList()}>Try again</Button></CardContent></Card>}
          {notes.listStatus === "loading" && !notes.list.length ? <div className="divide-y border-y"><Skeleton className="my-6 h-24 w-full" /><Skeleton className="my-6 h-24 w-full" /><Skeleton className="my-6 h-24 w-full" /></div> : notes.list.length ? <ol className="divide-y border-y">{notes.list.map((note) => <li key={note.id} className="[content-visibility:auto]"><AudioNoteRow note={note} /></li>)}</ol> : !notes.listError && <Card><CardHeader><AudioLines className="mb-3 size-8 text-muted-foreground" /><CardTitle>{notes.query || notes.kind ? "No matching notes" : "A place for everything you say"}</CardTitle><CardDescription>{notes.query || notes.kind ? "Try a different search or classification. Unclassified notes still appear in All kinds." : "Record your first note above, or drop an audio or video file here. You don't need to decide what kind of note it is."}</CardDescription></CardHeader></Card>}
          <nav aria-label="Audio notes pagination" className="flex items-center justify-between gap-3"><Button variant="outline" size="sm" disabled={notes.offset === 0 || notes.listStatus === "loading"} onClick={() => void notes.setPage(notes.offset - notes.pageSize)}><ChevronLeft data-icon="inline-start" />Newer</Button><span className="text-xs text-muted-foreground">Page {Math.floor(notes.offset / notes.pageSize) + 1}</span><Button variant="outline" size="sm" disabled={!notes.hasMore || notes.listStatus === "loading"} onClick={() => void notes.setPage(notes.offset + notes.pageSize)}>Older<ChevronRight data-icon="inline-end" /></Button></nav>
        </section>;
      }}</Observer>
    </div>
    {dragging && <div className="pointer-events-none absolute inset-0 flex items-center justify-center border-2 border-dashed border-primary bg-background/95"><p className="flex items-center gap-3 text-lg font-medium"><Upload />Drop to create Audio Notes</p></div>}
  </div>;
}

export function AudioNoteRow({ note }: { note: AudioNoteSummary }) {
  return <Observer>{() => {
    const kind = effectiveClassificationKind(note);
    return <Link to={`/audio-notes/${note.id}`} className="group grid gap-4 px-1 py-7 transition-colors hover:bg-muted/25 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring sm:grid-cols-[9.5rem_minmax(0,1fr)_auto] sm:px-4">
      <div className="text-xs text-muted-foreground"><time dateTime={note.started_at}>{new Date(note.started_at).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" })}</time><p className="mt-1">{new Date(note.started_at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}{note.duration_seconds != null ? ` · ${formatDuration(note.duration_seconds)}` : ""}</p></div>
      <div className="min-w-0"><h2 className="truncate font-semibold tracking-tight">{noteDisplayTitle({ title: note.title, sourceFilename: note.source_filename, startedAt: note.started_at })}</h2><p className="mt-2 line-clamp-2 whitespace-pre-wrap text-sm leading-6 text-muted-foreground">{note.transcript_text || (note.status === "error" ? "Transcription needs attention. Your audio is retained." : note.status === "completed" ? "No speech was detected. Your audio is retained." : note.status === "cancelled" ? "Recording discarded." : "Waiting for the transcript…")}</p><div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs"><span className="capitalize text-foreground/80">{kind ? kindLabel(kind) : "Not classified yet"}</span><span className={note.enrichment_status === "error" ? "text-destructive" : "text-muted-foreground"}>{note.status === "completed" ? enrichmentLabel(note.enrichment_status) : note.status}</span><span className="text-muted-foreground">{note.capture_source === "microphone_and_system" ? "Mic + system" : note.capture_source === "import" ? "Imported" : "Microphone"}</span></div></div>
      <ArrowRight className="hidden size-4 shrink-0 self-center text-muted-foreground transition-transform group-hover:translate-x-1 sm:block" />
    </Link>;
  }}</Observer>;
}
