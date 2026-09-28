import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Check, Pencil, Sparkles, Trash2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
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
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">{editing ? <form className="flex max-w-xl flex-col gap-2" onSubmit={(event) => { event.preventDefault(); void save(draft); }}>
          <label htmlFor="note-title" className="text-xs font-medium">Audio note title</label><div className="flex items-center gap-1"><Input autoFocus id="note-title" value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") setEditing(false); }} aria-invalid={Boolean(error)} disabled={busy} /><Button size="icon" aria-label="Save title" disabled={busy || !draft.trim()}><Check /></Button><Button type="button" variant="ghost" size="icon" aria-label="Cancel title edit" onClick={() => setEditing(false)}><X /></Button></div><RecentTitleSuggestions query={draft} disabled={busy} onSelect={(value) => void save(value)} />
        </form> : <div className="flex items-start gap-2"><h1 className="break-words text-2xl font-semibold tracking-tight">{title}</h1><Button size="icon" variant="ghost" aria-label="Edit audio note title" disabled={busy} onClick={() => { setDraft(note.title ?? ""); setEditing(true); }}><Pencil /></Button></div>}
          <p className="mt-2 text-xs text-muted-foreground">{new Date(note.started_at).toLocaleString()}{note.duration_seconds != null && ` · ${formatDuration(note.duration_seconds)}`} · {note.capture_source.replace(/_/g, " ")} · {note.status}</p>
        </div>
        <div className="flex flex-wrap gap-2"><Button size="sm" variant="outline" disabled={busy || !note.transcript_text || note.status !== "completed"} onClick={() => { if (note.title_source === "manual" && !window.confirm("Replace your manual title with an AI-generated title?")) return; void store.audioNotes.regenerateTitle(note.id); }}><Sparkles data-icon="inline-start" />{status === "generating" ? "Generating…" : "Generate title"}</Button>{["completed", "error", "cancelled"].includes(note.status) && <Button size="sm" variant="ghost" onClick={() => void onDelete()}><Trash2 data-icon="inline-start" />Delete</Button>}</div>
      </div>
      {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
    </header>;
  }}</Observer>;
}
