import { useEffect, useRef, useState } from "react";
import { untracked } from "mobx";
import { Observer } from "mobx-react-lite";
import { Link, type ActionFunctionArgs, type RouteObject } from "react-router-dom";
import { AlertCircle, ArrowRight, AudioLines, ChevronLeft, ChevronRight, Loader2, RefreshCcw, Search, Upload, X } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { getRootStore } from "@/stores/singleton";
import { useStore } from "@/stores/root-store";
import type { AudioNoteSummary } from "@/stores/audio-notes-store";
import { BUILT_IN_CLASSIFICATION_KINDS, effectiveClassificationKind, enrichmentLabel, formatDuration, kindLabel } from "@/lib/audio-notes";
import { noteDisplayTitle } from "@/lib/note-title";
import { groupNotesByDate, transcriptExcerpt } from "@/lib/audio-note-list";
import { cn } from "@/lib/utils";

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
  const search = useRef<HTMLInputElement>(null);
  const searchTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const dragDepth = useRef(0);
  const importPending = useRef(false);
  const [dragging, setDragging] = useState(false);
  const [importing, setImporting] = useState(false);
  const [query, setQuery] = useState(() => untracked(() => store.audioNotes.query));

  useEffect(() => {
    if (query.trim() === store.audioNotes.query) return;
    searchTimer.current = setTimeout(() => {
      void store.audioNotes.setFilters(query, store.audioNotes.kind);
      search.current?.closest("main")?.scrollTo({ top: 0 });
    }, 250);
    return () => clearTimeout(searchTimer.current);
  }, [query, store]);

  useEffect(() => {
    function focusSearch(event: KeyboardEvent) {
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey || event.defaultPrevented) return;
      const target = event.target;
      if (target instanceof HTMLElement && (target.isContentEditable || target.closest("input, textarea, select, [role='dialog']"))) return;
      event.preventDefault();
      search.current?.focus();
    }
    window.addEventListener("keydown", focusSearch);
    return () => window.removeEventListener("keydown", focusSearch);
  }, []);

  function applyFilters(nextQuery: string, nextKind: string) {
    clearTimeout(searchTimer.current);
    setQuery(nextQuery);
    void store.audioNotes.setFilters(nextQuery, nextKind);
    search.current?.closest("main")?.scrollTo({ top: 0 });
  }

  async function importFiles(files: File[]): Promise<void> {
    if (importPending.current || !files.length) return;
    importPending.current = true;
    setImporting(true);
    try {
      for (const file of files) {
        const id = await store.audioNotes.importFile(file);
        if (id !== null) toast.success(`Imported ${file.name}`, { description: "Transcription will appear in your stream.", action: { label: "Open note", onClick: () => { window.location.assign(`/audio-notes/${id}`); } } });
        else toast.error(`Couldn't import ${file.name}`, { description: store.audioNotes.lastError ?? undefined });
      }
    } finally { importPending.current = false; setImporting(false); }
  }
  return <div className="relative min-h-full" onDragEnter={(event) => { if (event.dataTransfer.types.includes("Files")) { dragDepth.current++; setDragging(true); } }} onDragLeave={() => { dragDepth.current = Math.max(0, dragDepth.current - 1); if (!dragDepth.current) setDragging(false); }} onDragOver={(event) => { if (event.dataTransfer.types.includes("Files")) event.preventDefault(); }} onDrop={(event) => { event.preventDefault(); dragDepth.current = 0; setDragging(false); void importFiles(Array.from(event.dataTransfer.files)); }}>
    <div className="flex flex-col px-4 pb-8 sm:px-6 lg:px-8">
      <div className="sticky top-0 z-10 -mx-4 border-b bg-background/95 px-4 pt-5 backdrop-blur sm:-mx-6 sm:px-6 lg:-mx-8 lg:px-8">
        <header className="flex flex-wrap items-center gap-x-5 gap-y-3">
          <h1 className="mr-auto text-xl font-semibold tracking-tight">Audio Notes</h1>
          <form role="search" className="order-last w-full md:order-none md:w-auto md:min-w-64 md:max-w-md md:flex-1" onSubmit={(event) => { event.preventDefault(); applyFilters(query, store.audioNotes.kind); }}>
            <div className="relative">
              <Search aria-hidden="true" className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input ref={search} aria-label="Search notes" aria-keyshortcuts="/" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") { applyFilters("", store.audioNotes.kind); } }} placeholder="Search titles and transcripts…" type="search" className="h-9 bg-muted/35 pl-9 pr-10 shadow-none [&::-webkit-search-cancel-button]:appearance-none" />
              {query ? <Button type="button" variant="ghost" size="icon" className="absolute right-0.5 top-0.5 size-8" aria-label="Clear search" onClick={() => { applyFilters("", store.audioNotes.kind); search.current?.focus(); }}><X className="size-3.5" /></Button> : <kbd aria-hidden="true" className="pointer-events-none absolute right-3 top-1/2 hidden -translate-y-1/2 rounded border px-1.5 font-mono text-[11px] text-muted-foreground md:block">/</kbd>}
            </div>
          </form>
          <input ref={input} type="file" multiple hidden accept=".wav,.mp3,.m4a,.flac,.ogg,.opus,.mp4,.mkv,.webm,.avi,.mov" onChange={(event) => { const files = Array.from(event.target.files ?? []); event.target.value = ""; void importFiles(files); }} />
          <Button variant="outline" size="sm" disabled={importing} onClick={() => input.current?.click()} title="Import audio or video files"><Upload data-icon="inline-start" />{importing ? "Importing…" : "Import"}</Button>
        </header>
        <Observer>{() => {
          const notes = store.audioNotes;
          const quickKinds = [{ value: "", label: "All notes" }, { value: "meeting", label: "Meetings" }, { value: "dictation", label: "Dictation" }];
          const moreKinds = [...new Set([...BUILT_IN_CLASSIFICATION_KINDS, ...notes.classificationKinds, ...(notes.kind ? [notes.kind] : [])])].filter((value) => !quickKinds.some((quick) => quick.value === value)).toSorted();
          const moreSelected = moreKinds.includes(notes.kind);
          return <div className="mt-3 flex min-w-0 items-center justify-between gap-2">
            <div role="group" aria-label="Filter by classification" className="flex min-w-0 items-center gap-1 overflow-x-auto pb-3">
              {quickKinds.map(({ value, label }) => <Button key={value} variant={notes.kind === value ? "secondary" : "ghost"} size="sm" aria-pressed={notes.kind === value} className="h-8 shrink-0 rounded-full px-2.5 text-xs sm:px-3" onClick={() => applyFilters(query, value)}>{label}</Button>)}
              <select aria-label="More classifications" value={moreSelected ? notes.kind : ""} onChange={(event) => applyFilters(query, event.target.value)} className={cn("h-8 w-24 shrink-0 rounded-full border border-transparent bg-transparent px-2 text-xs capitalize text-muted-foreground outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring sm:w-32", moreSelected && "bg-secondary font-medium text-secondary-foreground")}>
                <option value="">More</option>
                {moreKinds.map((value) => <option key={value} value={value}>{kindLabel(value)}</option>)}
              </select>
            </div>
            <Button variant="ghost" size="icon" className="mb-3 size-8 shrink-0 text-muted-foreground" disabled={notes.listStatus === "loading"} aria-label="Refresh notes" title="Refresh notes" onClick={() => void notes.loadList()}><RefreshCcw className={cn("size-3.5", notes.listStatus === "loading" && "motion-safe:animate-spin")} /></Button>
          </div>;
        }}</Observer>
      </div>
      <Observer>{() => {
        const notes = store.audioNotes;
        const loading = notes.listStatus === "loading" || query.trim() !== notes.query;
        const filtered = Boolean(notes.query || notes.kind);
        const groups = groupNotesByDate(notes.list);
        return <section aria-label="Audio note stream" aria-busy={loading}>
          <div className="grid min-h-11 items-center gap-x-5 text-xs text-muted-foreground lg:grid-cols-[minmax(0,1fr)_9rem_4rem_5.5rem_1rem]">
            <div className="flex items-center justify-between gap-2">
              <p role="status">{loading ? "Updating notes…" : notes.listError ? "Notes unavailable" : `${notes.offset && notes.list.length ? `${notes.offset + 1}–${notes.offset + notes.list.length}` : notes.list.length}${notes.hasMore ? "+" : ""} ${filtered ? "matching " : ""}note${notes.list.length === 1 ? "" : "s"}`}<span aria-hidden="true" className="mx-2 text-border">/</span>Newest first</p>
              {filtered && <Button variant="ghost" size="sm" className="h-7 px-2 text-xs" onClick={() => applyFilters("", "")}>Reset filters<X data-icon="inline-end" /></Button>}
            </div>
            {notes.list.length > 0 && !notes.listError && <><span aria-hidden="true" className="hidden text-right text-[11px] lg:block">Type</span><span aria-hidden="true" className="hidden text-right text-[11px] lg:block">Length</span><span aria-hidden="true" className="hidden text-right text-[11px] lg:block">Recorded</span></>}
          </div>
          {notes.listError ? <div role="alert" className="flex flex-wrap items-center gap-3 rounded-lg border border-destructive/25 p-5"><AlertCircle className="size-5 text-destructive" /><div className="min-w-0 flex-1"><h2 className="text-sm font-medium">Couldn't load notes</h2><p className="mt-1 break-words text-sm text-muted-foreground">{notes.listError}</p></div><Button variant="outline" size="sm" onClick={() => void notes.loadList()}>Try again</Button></div>
            : notes.listStatus === "loading" && !notes.list.length ? <div className="space-y-2" aria-hidden="true">{Array.from({ length: 6 }, (_, index) => <Skeleton key={index} className="h-20 w-full" />)}</div>
            : notes.list.length ? <div className={cn("transition-opacity", loading && "opacity-50")}>
              {groups.map((group) => <section key={group.key} aria-label={group.label} className="mb-4">
                <div className="flex items-center gap-3 py-2"><h2 className="text-xs font-medium text-muted-foreground">{group.label}</h2><span className="h-px flex-1 bg-border/60" /></div>
                <ol className="divide-y divide-border/50">{group.notes.map((note) => <li key={note.id} className="[content-visibility:auto] [contain-intrinsic-size:auto_88px]"><AudioNoteRow note={note} query={notes.query} /></li>)}</ol>
              </section>)}
            </div>
            : <div className="flex flex-col items-center py-20 text-center">
              {filtered ? <Search className="mb-4 size-7 text-muted-foreground" /> : <AudioLines className="mb-4 size-7 text-muted-foreground" />}
              <h2 className="text-base font-medium">{filtered ? "No matching notes" : "Your next thought starts here"}</h2>
              <p className="mt-2 max-w-sm text-sm leading-6 text-muted-foreground">{filtered ? "Try another phrase or clear your filters to see all notes." : "Record a note above, or drop an audio or video file anywhere in this view."}</p>
              <Button variant="outline" size="sm" className="mt-5" disabled={!filtered && importing} onClick={() => filtered ? applyFilters("", "") : input.current?.click()}>{filtered ? "Clear filters" : "Import your first recording"}</Button>
            </div>}
          {!notes.listError && (notes.offset > 0 || notes.hasMore) && <nav aria-label="Audio notes pagination" className="mt-4 flex items-center justify-between gap-3 border-t pt-4"><Button variant="outline" size="sm" disabled={notes.offset === 0 || loading} onClick={() => { void notes.setPage(notes.offset - notes.pageSize); search.current?.closest("main")?.scrollTo({ top: 0 }); }}><ChevronLeft data-icon="inline-start" />Newer</Button><span className="text-xs tabular-nums text-muted-foreground">Page {Math.floor(notes.offset / notes.pageSize) + 1}</span><Button variant="outline" size="sm" disabled={!notes.hasMore || loading} onClick={() => { void notes.setPage(notes.offset + notes.pageSize); search.current?.closest("main")?.scrollTo({ top: 0 }); }}>Older<ChevronRight data-icon="inline-end" /></Button></nav>}
        </section>;
      }}</Observer>
    </div>
    {dragging && <div className="pointer-events-none absolute inset-0 z-20 border-2 border-dashed border-primary bg-background/95"><p className="sticky top-1/3 flex items-center justify-center gap-3 p-6 text-lg font-medium"><Upload />Drop to import audio or video</p></div>}
  </div>;
}

export function AudioNoteRow({ note, query = "" }: { note: AudioNoteSummary; query?: string }) {
  return <Observer>{() => {
    const kind = effectiveClassificationKind(note);
    const title = noteDisplayTitle({ title: note.title, sourceFilename: note.source_filename, startedAt: note.started_at });
    const needsAttention = note.status === "error" || (note.status === "completed" && note.enrichment_status === "error");
    const status = note.status === "completed" ? note.enrichment_status === "completed" ? null : enrichmentLabel(note.enrichment_status) : note.status === "error" ? "Transcription needs attention" : kindLabel(note.status);
    const processing = !["completed", "error", "cancelled"].includes(note.status) || (note.status === "completed" && note.enrichment_status === "running");
    const source = note.capture_source === "microphone_and_system" ? "Mic + system" : note.capture_source === "import" ? "Imported" : "Microphone";
    return <Link to={`/audio-notes/${note.id}`} className="group -mx-2 grid items-center gap-x-5 gap-y-2 rounded-md px-2 py-3.5 transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring lg:grid-cols-[minmax(0,1fr)_9rem_4rem_5.5rem_1rem]" title={title}>
      <div className="min-w-0">
        <h3 className="truncate text-sm font-semibold tracking-tight"><HighlightedText text={title} query={query} /></h3>
        <p className="mt-1 truncate text-sm leading-5 text-muted-foreground"><HighlightedText text={note.transcript_text ? transcriptExcerpt(note.transcript_text, query) : note.status === "error" ? "Your audio is retained. Open this note to retry." : note.status === "completed" ? "No speech detected. Your audio is retained." : note.status === "cancelled" ? "Recording discarded." : "Waiting for the transcript…"} query={query} /></p>
        {status && <p className={cn("mt-1.5 flex items-center gap-1.5 text-xs", needsAttention ? "text-destructive" : "text-muted-foreground")}>
          {needsAttention ? <AlertCircle aria-hidden="true" className="size-3 shrink-0" /> : processing ? <Loader2 aria-hidden="true" className="size-3 shrink-0 motion-safe:animate-spin" /> : null}{status}
        </p>}
      </div>
      <div className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground lg:contents">
        <span className="truncate capitalize lg:text-right" title={kind ? kindLabel(kind) : "Not classified yet"}>{kind ? kindLabel(kind) : "Unclassified"}</span>
        <span aria-hidden="true" className="lg:hidden">·</span>
        <span className="shrink-0 tabular-nums lg:text-right" aria-label={note.duration_seconds != null ? `Duration ${formatDuration(note.duration_seconds)}` : "Duration unavailable"}>{note.duration_seconds != null ? formatDuration(note.duration_seconds) : "—"}</span>
        <span aria-hidden="true" className="lg:hidden">·</span>
        <time className="shrink-0 tabular-nums lg:text-right" dateTime={note.started_at} title={`${new Date(note.started_at).toLocaleString()} · ${source}`}>{new Date(note.started_at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</time>
        <span className="sr-only">{source}</span>
      </div>
      <ArrowRight aria-hidden="true" className="hidden size-3.5 text-muted-foreground/50 transition-transform group-hover:translate-x-0.5 group-hover:text-foreground lg:block" />
    </Link>;
  }}</Observer>;
}

function HighlightedText({ text, query }: { text: string; query: string }) {
  const term = query.trim();
  const index = term ? text.toLocaleLowerCase().indexOf(term.toLocaleLowerCase()) : -1;
  if (index < 0) return text;
  return <>{text.slice(0, index)}<mark className="rounded-sm bg-amber-200/60 text-inherit dark:bg-amber-400/25">{text.slice(index, index + term.length)}</mark>{text.slice(index + term.length)}</>;
}
