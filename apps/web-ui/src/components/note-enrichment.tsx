import { Observer } from "mobx-react-lite";
import { Loader2, RefreshCcw, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { useStore } from "@/stores/root-store";
import { classificationFields, classificationKind, enrichmentLabel, kindLabel } from "@/lib/audio-notes";

export function NoteEnrichment({ note }: { note: AudioNoteDetail }) {
  const store = useStore();
  return <Observer>{() => {
    const fields = classificationFields(note.classification);
    const kind = classificationKind(note.classification);
    const busy = ["pending", "running"].includes(note.enrichment_status);
    const confidence = fields?.confidence;
    const topics = Array.isArray(fields?.topics) ? fields.topics.filter((value): value is string => typeof value === "string") : [];
    const participants = Array.isArray(fields?.participants) ? fields.participants.filter((value): value is string => typeof value === "string") : [];
    return <Card><CardHeader><div className="flex flex-wrap items-start justify-between gap-3"><div><CardTitle className="flex items-center gap-2"><Sparkles className="size-4" />Understanding this note</CardTitle><CardDescription>Classification and useful context are added asynchronously. Your raw transcript is already yours.</CardDescription></div>{note.enrichment_status !== "completed" && <Button size="sm" variant="outline" disabled={!note.transcript_text || note.status !== "completed" || store.audioNotes.processing[note.id] || note.enrichment_status === "running"} onClick={() => void store.audioNotes.processNote(note.id)}><RefreshCcw data-icon="inline-start" />{note.enrichment_status === "error" ? "Retry AI processing" : "Process note"}</Button>}</div></CardHeader>
      <CardContent className="flex flex-col gap-4"><p className={note.enrichment_status === "error" ? "flex items-center gap-2 text-sm text-destructive" : "flex items-center gap-2 text-sm text-muted-foreground"} role="status">{busy && <Loader2 className="size-4 animate-spin" />}{enrichmentLabel(note.enrichment_status)}</p>
        {note.enrichment_error && <div role="alert" className="flex flex-col gap-2"><p className="text-sm">AI processing failed, not transcription. You can still copy and use the original transcript.</p><pre className="whitespace-pre-wrap text-xs text-destructive">{note.enrichment_error}</pre></div>}
        <dl className="grid gap-x-5 gap-y-2 text-sm sm:grid-cols-[8rem_1fr]"><dt className="text-muted-foreground">Classification</dt><dd className="capitalize">{kind ? kindLabel(kind) : "Not classified yet"}{typeof confidence === "number" && Number.isFinite(confidence) && confidence >= 0 && confidence <= 1 && <span className="ml-2 text-xs text-muted-foreground">{Math.round(confidence * 100)}% confidence</span>}</dd>{topics.length > 0 && <><dt className="text-muted-foreground">Topics</dt><dd>{topics.join(" · ")}</dd></>}{participants.length > 0 && <><dt className="text-muted-foreground">Participants</dt><dd>{participants.join(", ")}</dd></>}</dl>
        {fields && <details><summary className="cursor-pointer text-xs text-muted-foreground">Classification metadata</summary><pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 text-xs">{JSON.stringify(fields, null, 2)}</pre></details>}
      </CardContent>
    </Card>;
  }}</Observer>;
}
