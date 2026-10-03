import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Check, MoreHorizontal, Pencil, Sparkles, Trash2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { RecentTitleSuggestions } from "@/components/note-title-picker";
import { noteDisplayTitle } from "@/lib/note-title";
import { formatDuration } from "@/lib/audio-notes";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { useStore } from "@/stores/root-store";

export function NoteTitleHeader({ note, onDelete }: { note: AudioNoteDetail; onDelete: () => Promise<void> }) {
  const store = useStore();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  async function save(title: string): Promise<void> { if (await store.audioNotes.updateTitle(note.id, title)) setEditing(false); }
  return <Observer>{() => {
    const status = store.audioNotes.titleMutationStatus[note.id];
    const error = store.audioNotes.titleMutationError[note.id];
    const busy = status === "saving" || status === "generating";
    const title = noteDisplayTitle({ title: note.title, sourceFilename: note.source_filename, startedAt: note.started_at });
    return <header className="flex flex-col gap-4">
      <p className="text-[0.625rem] font-medium uppercase tracking-[0.2em] text-muted-foreground">Audio note<span className="mx-2" aria-hidden="true">/</span>{new Date(note.started_at).toLocaleDateString(undefined, { month: "long", day: "numeric", year: "numeric" })}</p>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">{editing ? <form className="flex max-w-xl flex-col gap-2" onSubmit={(event) => { event.preventDefault(); void save(draft); }}>
          <label htmlFor="note-title" className="text-xs font-medium">Audio note title</label><div className="flex items-center gap-1"><Input autoFocus id="note-title" value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") setEditing(false); }} aria-invalid={Boolean(error)} disabled={busy} /><Button size="icon" aria-label="Save title" disabled={busy || !draft.trim()}><Check /></Button><Button type="button" variant="ghost" size="icon" aria-label="Cancel title edit" onClick={() => setEditing(false)}><X /></Button></div><RecentTitleSuggestions query={draft} disabled={busy} onSelect={(value) => void save(value)} />
        </form> : <div className="flex items-start gap-2"><h1 className="max-w-3xl break-words text-[1.75rem] font-medium leading-[1.2] tracking-[-0.035em] sm:text-4xl">{title}</h1><Button size="icon" variant="ghost" className="shrink-0 text-muted-foreground" aria-label="Edit audio note title" disabled={busy} onClick={() => { setDraft(note.title ?? ""); setEditing(true); }}><Pencil className="size-3.5" /></Button></div>}
          <p className="mt-4 text-xs text-muted-foreground">{new Date(note.started_at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}{note.duration_seconds != null && ` · ${formatDuration(note.duration_seconds)}`} · {note.capture_source.replace(/_/g, " ")}{note.status !== "completed" && ` · ${note.status}`}</p>
        </div>
        <Popover><PopoverTrigger asChild><Button size="icon" variant="ghost" aria-label="Note actions" className="text-muted-foreground"><MoreHorizontal /></Button></PopoverTrigger><PopoverContent align="end" className="w-52 p-2"><Button className="w-full justify-start" size="sm" variant="ghost" disabled={busy || !note.transcript_text || note.status !== "completed"} onClick={() => { if (note.title_source === "manual" && !window.confirm("Replace your manual title with an AI-generated title?")) return; void store.audioNotes.regenerateTitle(note.id); }}><Sparkles data-icon="inline-start" />{status === "generating" ? "Generating…" : "Generate title"}</Button>{["completed", "error", "cancelled"].includes(note.status) && <Button className="w-full justify-start text-destructive" size="sm" variant="ghost" onClick={() => void onDelete()}><Trash2 data-icon="inline-start" />Delete note</Button>}</PopoverContent></Popover>
      </div>
      {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
    </header>;
  }}</Observer>;
}
