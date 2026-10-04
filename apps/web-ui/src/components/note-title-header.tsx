import { useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import { Link } from "react-router-dom";
import { ArrowLeft, Check, MoreHorizontal, Pencil, Sparkles, Trash2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { RecentTitleSuggestions } from "@/components/note-title-picker";
import { noteDisplayTitle } from "@/lib/note-title";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { useStore } from "@/stores/root-store";

export function NoteTitleHeader({ note, onDelete }: { note: AudioNoteDetail; onDelete: () => Promise<void> }) {
  const store = useStore();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [actionsOpen, setActionsOpen] = useState(false);
  const actionsTrigger = useRef<HTMLButtonElement>(null);
  function finishEditing(): void { setEditing(false); actionsTrigger.current?.focus(); }
  async function save(title: string): Promise<void> { if (await store.audioNotes.updateTitle(note.id, title)) finishEditing(); }
  return <Observer>{() => {
    const status = store.audioNotes.titleMutationStatus[note.id];
    const error = store.audioNotes.titleMutationError[note.id];
    const busy = status === "saving" || status === "generating";
    const title = noteDisplayTitle({ title: note.title, sourceFilename: note.source_filename, startedAt: note.started_at });
    return <header className="flex items-start gap-3">
        <Link to="/audio-notes" aria-label="All audio notes" title="All audio notes" className="-ml-2 inline-flex size-9 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><ArrowLeft className="size-4" /></Link>
        <div className="min-w-0 flex-1 pt-1">{editing ? <form className="flex max-w-xl flex-col gap-2" onSubmit={(event) => { event.preventDefault(); void save(draft); }}>
          <label htmlFor="note-title" className="sr-only">Audio note title</label><div className="flex items-center gap-1"><Input autoFocus id="note-title" value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") finishEditing(); }} aria-invalid={Boolean(error)} disabled={busy} /><Button size="icon" aria-label="Save title" disabled={busy || !draft.trim()}><Check /></Button><Button type="button" variant="ghost" size="icon" aria-label="Cancel title edit" onClick={finishEditing}><X /></Button></div><RecentTitleSuggestions query={draft} disabled={busy} onSelect={(value) => void save(value)} />
        </form> : <h1 title={title} className="line-clamp-2 break-words font-serif text-xl leading-snug tracking-[-0.02em] sm:text-2xl">{title}</h1>}
          <p className="mt-1 text-[0.6875rem] leading-5 text-muted-foreground"><time dateTime={note.started_at}>{new Date(note.started_at).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" })}<span className="mx-1.5" aria-hidden="true">·</span>{new Date(note.started_at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</time><span className="mx-1.5" aria-hidden="true">·</span>{note.capture_source.replace(/_/g, " ")}{note.status !== "completed" && ` · ${note.status}`}</p>
          {status === "generating" && <p className="mt-2 text-xs text-muted-foreground" role="status">Generating title…</p>}
          {error && <p className="mt-2 text-sm text-destructive" role="alert">{error}</p>}
        </div>
        <Popover open={actionsOpen} onOpenChange={setActionsOpen}><PopoverTrigger asChild><Button ref={actionsTrigger} size="icon" variant="ghost" aria-label="Note actions" title="Note actions" className="shrink-0 text-muted-foreground"><MoreHorizontal /></Button></PopoverTrigger><PopoverContent align="end" className="w-52 p-1.5" onCloseAutoFocus={(event) => { if (editing) event.preventDefault(); }}>
          <Button className="w-full justify-start" size="sm" variant="ghost" disabled={busy} onClick={() => { setDraft(title); setEditing(true); setActionsOpen(false); }}><Pencil data-icon="inline-start" />Edit title</Button>
          <Button className="w-full justify-start" size="sm" variant="ghost" disabled={busy || !note.transcript_text || note.status !== "completed"} onClick={() => { if (note.title_source === "manual" && !window.confirm("Replace your manual title with an AI-generated title?")) return; void store.audioNotes.regenerateTitle(note.id); setActionsOpen(false); }}><Sparkles data-icon="inline-start" />{status === "generating" ? "Generating…" : "Generate title"}</Button>
          {["completed", "error", "cancelled"].includes(note.status) && <div className="mt-1 border-t pt-1"><Button className="w-full justify-start text-destructive" size="sm" variant="ghost" onClick={() => { setActionsOpen(false); void onDelete(); }}><Trash2 data-icon="inline-start" />Delete note</Button></div>}
        </PopoverContent></Popover>
    </header>;
  }}</Observer>;
}
