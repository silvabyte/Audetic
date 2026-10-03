import { memo, useState } from "react";
import { Observer } from "mobx-react-lite";
import { Copy, Download, FileText, Loader2, Network, Plus, RefreshCcw, Sparkles, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Link } from "react-router-dom";
import { ArtifactContent } from "@/components/artifact-content";
import { NoteMindMap } from "@/components/note-mind-map";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useStore } from "@/stores/root-store";
import type { AudioNoteArtifact } from "@/stores/note-artifacts-store";
import type { EffectiveTheme } from "@/stores/ui-store";
import { copyText } from "@/lib/clipboard";
import { downloadText } from "@/lib/download";
import { readNoteDocument, sectionId } from "@/lib/note-document";

export const NoteArtifactsPanel = memo(function NoteArtifactsPanel({ noteId, canGenerate, hasSegments = false, view = "document", onChangeView, onSeek, duration }: {
  noteId: number;
  canGenerate: boolean;
  hasSegments?: boolean;
  view?: "document" | "map";
  onChangeView?: (view: "document" | "map") => void;
  onSeek?: (seconds: number) => void;
  duration?: number | null;
}) {
  const store = useStore();
  const [templateId, setTemplateId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [context, setContext] = useState("");
  const [selection, setSelection] = useState<{ document: number | null; map: number | null }>({ document: null, map: null });
  const [showGenerator, setShowGenerator] = useState(false);

  return <Observer>{() => {
    const artifacts = store.noteArtifacts;
    const templates = artifacts.templates;
    const profiles = artifacts.profiles.filter((profile) => profile.enabled);
    const eligibleTemplates = templates.filter((entry) => !entry.requires_timestamps || hasSegments);
    const defaultTemplate = view === "map" ? "mind_map" : "general_note";
    const template = eligibleTemplates.find((entry) => entry.id === templateId) ?? eligibleTemplates.find((entry) => entry.id === defaultTemplate) ?? eligibleTemplates[0];
    const profile = profiles.find((entry) => String(entry.id) === profileId) ?? profiles.find((entry) => entry.default_profile && entry.available) ?? profiles.find((entry) => entry.available) ?? profiles[0];
    const saved = artifacts.byNote[noteId] ?? [];
    const candidates = saved.filter((artifact) => view === "map" ? ["mind_map", "summary", "meeting_minutes"].includes(artifact.kind) : artifact.kind !== "mind_map");
    const selected = candidates.find((artifact) => artifact.id === selection[view])
      ?? candidates.find((artifact) => artifact.status === "completed" && (view === "map" ? artifact.kind === "mind_map" : ["summary", "meeting_minutes"].includes(artifact.kind)))
      ?? candidates.find((artifact) => artifact.status === "completed") ?? candidates[0];
    const generating = artifacts.generatingByNote[noteId] || saved.some((artifact) => artifact.status === "pending" || artifact.status === "running");
    const theme = store.ui.effectiveTheme;
    const document = selected?.content_markdown ? readNoteDocument(selected.content_markdown) : null;
    const generatorVisible = showGenerator || !selected;

    async function deleteSelected(): Promise<void> {
      if (!selected || !window.confirm(`Delete artifact “${selected.title}”?`)) return;
      if (await artifacts.deleteArtifact(noteId, selected.id)) {
        setSelection((previous) => ({ ...previous, [view]: null }));
        toast.success("Artifact deleted");
      } else toast.error(artifacts.errors[noteId] ?? "Could not delete artifact");
    }

    function readSection(line: number): void {
      if (!selected) return;
      setSelection((previous) => ({ ...previous, document: selected.id }));
      onChangeView?.("document");
      // The document panel is mounted on the next React commit.
      window.requestAnimationFrame(() => window.requestAnimationFrame(() => {
        const heading = window.document.getElementById(sectionId(`artifact-${selected.id}`, line));
        heading?.focus({ preventScroll: true });
        heading?.scrollIntoView({ block: "start" });
      }));
    }

    return <section aria-label={view === "map" ? "Mind map" : "Summary and saved views"} className="min-w-0">
      <div className="mb-9 flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          {view === "map" ? <Network className="size-4 text-muted-foreground" /> : <FileText className="size-4 text-muted-foreground" />}
          {selected ? <label className="min-w-0"><span className="sr-only">Saved view</span><select value={selected.id} onChange={(event) => setSelection((previous) => ({ ...previous, [view]: Number(event.target.value) }))} className="max-w-full truncate rounded-md bg-transparent py-2 pr-6 text-xs text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">{candidates.map((artifact) => <option key={artifact.id} value={artifact.id}>{artifactKindLabel(artifact.kind)} · {new Date(artifact.created_at).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })} · #{artifact.id}{artifact.status !== "completed" ? ` · ${artifact.status}` : ""}</option>)}</select></label> : <span className="text-xs text-muted-foreground">{view === "map" ? "See the connections" : "The important parts, together"}</span>}
        </div>
        <div className="flex flex-wrap gap-1">{selected ? <ArtifactActions artifact={selected} onDelete={deleteSelected} /> : null}<Button size="sm" variant="ghost" onClick={() => { setTemplateId(defaultTemplate); setShowGenerator(!generatorVisible); }} aria-expanded={generatorVisible} aria-controls="note-generator"><Plus data-icon="inline-start" />New view</Button><Button size="icon" variant="ghost" aria-label="Refresh generated views" disabled={artifacts.noteState[noteId] === "loading"} onClick={() => { void artifacts.loadArtifacts(noteId); void artifacts.loadPrerequisites(); }}><RefreshCcw className="size-3.5" /></Button></div>
      </div>

      {artifacts.prerequisitesError ? <p role="alert" className="mb-4 text-sm text-destructive">{artifacts.prerequisitesError}</p> : null}
      {artifacts.errors[noteId] ? <p role="alert" className="mb-4 text-sm text-destructive">{artifacts.errors[noteId]}</p> : null}
      {generating ? <p role="status" className="mb-6 flex items-center gap-2 text-sm text-muted-foreground"><Loader2 className="size-4 animate-spin" />Creating a new view. You can keep reading.</p> : null}

      {generatorVisible ? <form id="note-generator" className="mb-10 rounded-xl border bg-muted/15 p-5 sm:p-6" onSubmit={async (event) => {
        event.preventDefault();
        if (!template || !profile) return;
        const artifact = await artifacts.generateArtifact(noteId, { template_id: template.id, agent_profile_id: profile.id, custom_context: context.trim() || null });
        if (artifact) {
          if (artifact.status === "completed") {
            const mapEligible = ["mind_map", "summary", "meeting_minutes"].includes(artifact.kind);
            setSelection((previous) => ({ ...previous, document: artifact.id, ...(mapEligible ? { map: artifact.id } : {}) }));
            onChangeView?.(artifact.kind === "mind_map" || (view === "map" && mapEligible) ? "map" : "document");
          }
          setContext("");
          setShowGenerator(false);
          toast.success(artifact.status === "completed" ? "View generated" : "View generation started");
        } else toast.error(artifacts.errors[noteId] ?? "Could not generate view");
      }}>
        <h3 className="font-medium">A fresh perspective</h3><p className="mb-5 mt-1 text-sm text-muted-foreground">Turn the transcript into a useful document. Your saved views stay here.</p>
        {artifacts.prerequisitesState === "loaded" && !profiles.length ? <p className="mb-4 text-sm text-muted-foreground">Set up a local agent in <Link className="underline underline-offset-4" to="/settings/post-processing">Settings → Post-processing</Link> to generate a view.</p> : null}
        <fieldset disabled={!canGenerate || generating} className="grid gap-4 sm:grid-cols-2"><legend className="sr-only">Generated view options</legend>
          <label className="flex flex-col gap-1.5 text-xs font-medium">View type<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm font-normal" value={template?.id ?? ""} onChange={(event) => setTemplateId(event.target.value)}>{!templates.length ? <option value="">No templates available</option> : null}{templates.map((entry) => <option key={entry.id} value={entry.id} disabled={entry.requires_timestamps && !hasSegments}>{entry.name}{entry.requires_timestamps && !hasSegments ? " (needs timestamps)" : ""}</option>)}</select></label>
          <label className="flex flex-col gap-1.5 text-xs font-medium">Local agent<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm font-normal" value={profile?.id ?? ""} onChange={(event) => setProfileId(event.target.value)}>{!profiles.length ? <option value="">No agent available</option> : null}{profiles.map((entry) => <option key={entry.id} value={entry.id}>{entry.name}{!entry.available ? " (unavailable)" : ""}</option>)}</select></label>
          <label className="flex flex-col gap-1.5 text-xs font-medium sm:col-span-2">Extra context <span className="font-normal text-muted-foreground">Optional</span><textarea className="min-h-20 rounded-md border border-input bg-background px-3 py-2 text-sm font-normal" value={context} onChange={(event) => setContext(event.target.value)} placeholder="What would you like to focus on?" /></label>
        </fieldset>
        {template ? <p className="my-4 text-xs leading-relaxed text-muted-foreground">{template.description}</p> : null}
        {!canGenerate ? <p className="my-4 text-xs text-muted-foreground">Generated views become available after transcription completes.</p> : null}
        <Button size="sm" disabled={!canGenerate || generating || !template || !profile?.available}><Sparkles data-icon="inline-start" />{generating ? "Generating…" : "Generate view"}</Button>
      </form> : null}

      {artifacts.noteState[noteId] === "loading" && !saved.length ? <Skeleton className="h-72 w-full" /> : selected ? <>
        {view === "map" && selected.kind !== "mind_map" && document?.sections.length ? <NoteMindMap key={selected.id} title={document.title ?? "Audio note"} sections={document.sections} onReadSection={readSection} />
          : view === "map" && selected.kind !== "mind_map" && selected.status === "completed" ? <div className="py-16 text-center"><Network className="mx-auto mb-4 size-6 text-muted-foreground" /><h3 className="font-serif text-2xl">Give this note a little perspective.</h3><p className="mx-auto mt-3 max-w-sm text-sm leading-relaxed text-muted-foreground">This view has no topic headings to map. Generate a Mind Map from the transcript to explore its connections.</p><Button className="mt-5" variant="outline" onClick={() => { setTemplateId("mind_map"); setShowGenerator(true); }}>Create mind map</Button></div>
          : <ArtifactCard artifact={selected} theme={theme} onSeek={onSeek} duration={duration} showToolbar={false} onDelete={deleteSelected} />}
      </> : <div className="py-12 text-center"><FileText className="mx-auto mb-4 size-6 text-muted-foreground" /><h3 className="font-serif text-2xl">Make room for the important things.</h3><p className="mx-auto mt-3 max-w-sm text-sm leading-relaxed text-muted-foreground">{view === "map" ? "Generate a summary with topic headings or a dedicated mind map to see how the ideas connect." : "A summary brings the context, key ideas, and next steps into focus. Your original transcript is always one tab away."}</p></div>}
    </section>;
  }}</Observer>;
});

export function ArtifactCard({ artifact, onDelete, theme = "light", onSeek, duration, showToolbar = true }: { artifact: AudioNoteArtifact; onDelete: () => Promise<void>; theme?: EffectiveTheme; onSeek?: (seconds: number) => void; duration?: number | null; showToolbar?: boolean }) {
  return <Observer>{() => {
    const structured = artifact.content_json == null ? null : JSON.stringify(artifact.content_json, null, 2);
    return <article className="min-w-0">
      {showToolbar ? <div className="mb-6 flex flex-wrap items-center justify-between gap-3 border-b pb-4">
        <p className="text-xs text-muted-foreground">{artifactKindLabel(artifact.kind)}<span className="mx-2" aria-hidden="true">·</span>{artifact.status === "completed" ? "Generated from your transcript" : artifact.status}</p>
        <ArtifactActions artifact={artifact} onDelete={onDelete} />
      </div> : null}
      {artifact.error ? <div role="alert" className="mb-6 rounded-lg border border-destructive/20 p-4"><p className="mb-2 text-sm font-medium">This view could not be generated</p><pre className="whitespace-pre-wrap text-xs text-destructive">{artifact.error}</pre><p className="mt-3 text-xs text-muted-foreground">Use New view to try again. Your transcript and other saved views are available.</p></div> : null}
      {artifact.content_markdown ? <ArtifactContent markdown={artifact.content_markdown} theme={theme} documentId={`artifact-${artifact.id}`} outline={artifact.kind !== "mind_map"} onSeek={onSeek} duration={duration} /> : null}
      {structured ? <details className="mt-6 border-t pt-4"><summary className="cursor-pointer text-sm font-medium">Structured data</summary><p className="mt-2 text-xs text-muted-foreground">Extracted information only. No external action has been performed.</p><pre className="my-3 max-h-72 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 text-xs">{structured}</pre><Button variant="outline" size="sm" onClick={() => void copyText(structured, "JSON copied")}><Copy data-icon="inline-start" />Copy JSON</Button></details> : null}
    </article>;
  }}</Observer>;
}

function ArtifactActions({ artifact, onDelete }: { artifact: AudioNoteArtifact; onDelete: () => Promise<void> }) {
  return <Observer>{() => {
    const content = artifact.content_markdown || (artifact.content_json == null ? "" : JSON.stringify(artifact.content_json, null, 2));
    return <div className="flex gap-1 text-muted-foreground"><Button variant="ghost" size="icon" aria-label="Copy artifact" disabled={!content} onClick={() => void copyText(content, "Artifact copied")}><Copy className="size-3.5" /></Button><Button variant="ghost" size="icon" aria-label="Download artifact" disabled={!content} onClick={() => downloadText(content, `audetic-note-${artifact.note_id}-${artifact.id}.${artifact.content_markdown ? "md" : "json"}`, artifact.content_markdown ? undefined : "application/json")}><Download className="size-3.5" /></Button><Button variant="ghost" size="icon" aria-label="Delete artifact" onClick={() => void onDelete()}><Trash2 className="size-3.5" /></Button></div>;
  }}</Observer>;
}

export function artifactKindLabel(kind: string): string {
  switch (kind) {
    case "meeting_minutes": return "Minutes";
    case "summary": return "Summary";
    case "action_items": return "Actions";
    case "talking_points": return "Talking points";
    case "mind_map": return "Mind map";
    case "cleaned_text": return "Cleaned text";
    case "intent": return "Intent";
    case "shopping_items": return "Shopping items";
    default: return kind ? kind.replace(/[-_]/g, " ") : "Generated view";
  }
}
