import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Copy, Search, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { TranscriptPlayer } from "@/components/transcript-player";
import { copyText } from "@/lib/clipboard";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";

export function NoteTranscript({ note, currentTime, onSeek }: { note: AudioNoteDetail; currentTime: number; onSeek: (seconds: number) => void }) {
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(false);
  return <Observer>{() => {
    const search = query.trim();
    const text = note.transcript_segments?.length ? note.transcript_segments.map((segment) => segment.text).join("\n") : note.transcript_text ?? "";
    const matches = search ? text.toLocaleLowerCase().split(search.toLocaleLowerCase()).length - 1 : 0;
    return <section className="mx-auto max-w-3xl" aria-label="Original transcript">
      <div className="mb-8 flex flex-wrap items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground">Original words{note.transcript_segments?.length ? " · Select a passage to seek" : " · No timestamps available"}</p>
        <Button variant="ghost" size="sm" disabled={!text} onClick={() => void copyText(text, "Raw transcript copied")}><Copy data-icon="inline-start" />Copy transcript</Button>
      </div>
      <div className="mb-8 flex flex-wrap items-center gap-4">
        <div className="flex min-w-48 flex-1 items-center gap-2 rounded-lg border px-3"><Search className="size-4 text-muted-foreground" /><input aria-label="Search transcript" type="search" value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") setQuery(""); }} placeholder="Find in transcript…" className="h-11 min-w-0 flex-1 bg-transparent text-sm outline-none" />{query ? <button type="button" aria-label="Clear transcript search" onClick={() => setQuery("")} className="rounded p-1 text-muted-foreground hover:text-foreground"><X className="size-4" /></button> : null}</div>
        {note.transcript_segments?.length ? <label className="flex items-center gap-2 text-xs text-muted-foreground"><input type="checkbox" checked={follow} onChange={(event) => setFollow(event.target.checked)} className="accent-foreground" />Follow playback</label> : null}
      </div>
      {search ? <p role="status" className="mb-5 text-xs text-muted-foreground">{matches ? `${matches} ${matches === 1 ? "match" : "matches"}` : `No matches for “${search}”`}</p> : null}
      <TranscriptPlayer noteId={note.id} text={note.transcript_text ?? ""} segments={note.transcript_segments} completed={note.status === "completed"} currentTime={currentTime} onSeek={onSeek} query={search} followPlayback={follow} />
    </section>;
  }}</Observer>;
}
