import { useEffect, useState } from "react";
import { Observer } from "mobx-react-lite";
import { Loader2, RefreshCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { useStore } from "@/stores/root-store";
import { BUILT_IN_CLASSIFICATION_KINDS, classificationFields, classificationKind, effectiveClassificationKind, enrichmentLabel, isClassificationSlug, kindLabel } from "@/lib/audio-notes";

export function NoteEnrichment({ note }: { note: AudioNoteDetail }) {
  const store = useStore();
  const persistedKind = effectiveClassificationKind(note) ?? "";
  const [classificationInput, setClassificationInput] = useState(persistedKind);
  useEffect(() => { void store.audioNotes.loadClassificationKinds(); }, [store]);
  useEffect(() => { setClassificationInput(persistedKind); }, [note.id, persistedKind]);

  return <Observer>{() => {
    const fields = classificationFields(note.classification);
    const aiKind = classificationKind(note.classification);
    const effectiveKind = effectiveClassificationKind(note);
    const manualKind = note.classification_kind_override?.trim() || null;
    const busy = ["pending", "running"].includes(note.enrichment_status);
    const saving = store.audioNotes.classificationMutationStatus[note.id] === "saving";
    const confidence = fields?.confidence;
    const topics = Array.isArray(fields?.topics) ? fields.topics.filter((value): value is string => typeof value === "string") : [];
    const participants = Array.isArray(fields?.participants) ? fields.participants.filter((value): value is string => typeof value === "string") : [];
    const kinds = [...new Set([...BUILT_IN_CLASSIFICATION_KINDS, ...store.audioNotes.classificationKinds])].toSorted();
    const normalizedInput = classificationInput.trim();
    return <section aria-labelledby="note-understanding-heading" className="border-y py-6">
      <div className="flex flex-wrap items-start justify-between gap-3"><div><h2 id="note-understanding-heading" className="font-semibold">Note details</h2><p className="mt-1 text-sm text-muted-foreground">AI suggestions remain visible when you organize a note yourself.</p></div>{note.enrichment_status !== "completed" ? <Button size="sm" variant="outline" disabled={!note.transcript_text || note.status !== "completed" || store.audioNotes.processing[note.id] || note.enrichment_status === "running"} onClick={() => void store.audioNotes.processNote(note.id)}><RefreshCcw data-icon="inline-start" />{note.enrichment_status === "error" ? "Retry AI processing" : "Process note"}</Button> : null}</div>
      <p className={note.enrichment_status === "error" ? "mt-5 flex items-center gap-2 text-sm text-destructive" : "mt-5 flex items-center gap-2 text-sm text-muted-foreground"} role="status">{busy ? <Loader2 className="size-4 animate-spin" /> : null}{enrichmentLabel(note.enrichment_status)}</p>
      {note.enrichment_error ? <div role="alert" className="mt-3 flex flex-col gap-2"><p className="text-sm">AI processing failed, not transcription. You can still copy and use the original transcript.</p><pre className="whitespace-pre-wrap text-xs text-destructive">{note.enrichment_error}</pre></div> : null}

      <dl className="mt-5 grid gap-x-5 gap-y-2 text-sm sm:grid-cols-[8rem_1fr]">
        <dt className="text-muted-foreground">Classification</dt><dd className="capitalize">{effectiveKind ? kindLabel(effectiveKind) : "Not classified yet"}{manualKind ? <span className="ml-2 text-xs normal-case text-muted-foreground">set manually</span> : null}</dd>
        <dt className="text-muted-foreground">AI suggestion</dt><dd className="capitalize">{aiKind ? kindLabel(aiKind) : "No suggestion"}{typeof confidence === "number" && Number.isFinite(confidence) && confidence >= 0 && confidence <= 1 ? <span className="ml-2 text-xs normal-case text-muted-foreground">{Math.round(confidence * 100)}% confidence</span> : null}</dd>
        {topics.length > 0 ? <><dt className="text-muted-foreground">Topics</dt><dd>{topics.join(" · ")}</dd></> : null}
        {participants.length > 0 ? <><dt className="text-muted-foreground">Participants</dt><dd>{participants.join(", ")}</dd></> : null}
      </dl>

      <form className="mt-6 flex flex-wrap items-end gap-2 border-t pt-5" onSubmit={async (event) => {
        event.preventDefault();
        if (!isClassificationSlug(normalizedInput)) return;
        if (await store.audioNotes.setClassification(note.id, normalizedInput)) setClassificationInput(normalizedInput);
      }}>
        <label className="flex min-w-52 flex-1 flex-col gap-1.5 text-xs font-medium">Organize as<Input list={`classification-kinds-${note.id}`} value={classificationInput} onChange={(event) => setClassificationInput(event.target.value)} placeholder="e.g. creative-art" aria-invalid={Boolean(normalizedInput) && !isClassificationSlug(normalizedInput)} /></label>
        <datalist id={`classification-kinds-${note.id}`}>{kinds.map((kind) => <option key={kind} value={kind} />)}</datalist>
        <Button type="submit" size="sm" variant="secondary" disabled={saving || !isClassificationSlug(normalizedInput) || normalizedInput === manualKind}>{saving ? "Saving…" : "Set classification"}</Button>
        {manualKind ? <Button type="button" size="sm" variant="ghost" disabled={saving} onClick={async () => { if (await store.audioNotes.clearClassification(note.id)) setClassificationInput(aiKind ?? ""); }}>Revert to AI classification</Button> : null}
      </form>
      {normalizedInput && !isClassificationSlug(normalizedInput) ? <p className="mt-2 text-xs text-destructive">Use lowercase letters, numbers, hyphens, or underscores, beginning with a letter.</p> : null}
      {store.audioNotes.classificationMutationError[note.id] ? <p role="alert" className="mt-2 text-xs text-destructive">{store.audioNotes.classificationMutationError[note.id]}</p> : null}
      {fields ? <details className="mt-5"><summary className="cursor-pointer text-xs text-muted-foreground">Classification metadata</summary><pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 text-xs">{JSON.stringify(fields, null, 2)}</pre></details> : null}
    </section>;
  }}</Observer>;
}
