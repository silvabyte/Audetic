import { memo, useRef, useState, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { Observer } from "mobx-react-lite";
import { Copy, Download, Loader2, MoreHorizontal, Network, Plus, RefreshCcw, Sparkles, Trash2, X } from "lucide-react";
import { toast } from "sonner";
import { Link } from "react-router-dom";
import { ArtifactContent } from "@/components/artifact-content";
import { NoteMindMap } from "@/components/note-mind-map";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { useStore } from "@/stores/root-store";
import type { AudioNoteArtifact } from "@/stores/note-artifacts-store";
import type { EffectiveTheme } from "@/stores/ui-store";
import { copyText } from "@/lib/clipboard";
import { downloadText } from "@/lib/download";
import { sectionId } from "@/lib/note-document";

export const NoteArtifactsPanel = memo(function NoteArtifactsPanel({ noteId, canGenerate, hasSegments = false, view = "document", toolbarContainer, onChangeView, onReadTranscript, onSeek, duration }: {
  noteId: number;
  canGenerate: boolean;
  hasSegments?: boolean;
  view?: "document" | "map";
  toolbarContainer?: HTMLElement | null;
  onChangeView?: (view: "document" | "map") => void;
  onReadTranscript?: () => void;
  onSeek?: (seconds: number) => void;
  duration?: number | null;
}) {
  const store = useStore();
  const [templateId, setTemplateId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [context, setContext] = useState("");
  const [selection, setSelection] = useState<{ document: number | null; map: number | null }>({ document: null, map: null });
  const [showGenerator, setShowGenerator] = useState(false);
  const generatorTrigger = useRef<HTMLButtonElement>(null);

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
      ?? (view === "map" ? candidates.find((artifact) => artifact.id === selection.document && artifact.status === "completed") : undefined)
      ?? candidates.find((artifact) => artifact.status === "completed" && ["summary", "meeting_minutes"].includes(artifact.kind))
      ?? candidates.find((artifact) => artifact.status === "completed") ?? candidates[0];
    const generating = artifacts.generatingByNote[noteId] || saved.some((artifact) => artifact.status === "pending" || artifact.status === "running");
    const theme = store.ui.effectiveTheme;
    const generatorVisible = showGenerator;

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
        const heading = window.document.getElementById(sectionId(`artifact-${selected.id}`, line))
          ?? window.document.querySelector<HTMLElement>(`[data-document-id="artifact-${selected.id}"] [data-source-line="${line}"]`);
        heading?.focus({ preventScroll: true });
        heading?.scrollIntoView({ block: "start" });
      }));
    }

    const viewPicker = selected ? <label className="min-w-0"><span className="sr-only">Saved view</span><select value={selected.id} onChange={(event) => {
            const id = Number(event.target.value);
            const artifact = candidates.find((candidate) => candidate.id === id);
            setSelection((previous) => artifact && ["summary", "meeting_minutes"].includes(artifact.kind) ? { document: id, map: id } : { ...previous, [view]: id });
          }} className="max-w-44 truncate rounded-md bg-transparent py-2 pr-2 text-xs text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">{candidates.map((artifact) => <option key={artifact.id} value={artifact.id}>{artifactKindLabel(artifact.kind)}{candidates.filter((candidate) => candidate.kind === artifact.kind).length > 1 ? ` · ${new Date(artifact.created_at).toLocaleDateString(undefined, { month: "short", day: "numeric" })} · #${artifact.id}` : ""}{artifact.status !== "completed" ? ` · ${artifact.status}` : ""}</option>)}</select></label> : null;
    const toolbar = selected ? <div className="flex min-w-0 items-center gap-1">
        <div className="hidden min-w-0 @2xl:block">{viewPicker}</div>
        <ArtifactActions artifact={selected} viewPicker={viewPicker} triggerRef={generatorTrigger} onDelete={deleteSelected} onNewView={() => { setTemplateId(defaultTemplate); setShowGenerator(true); }} onRefresh={() => { void artifacts.loadArtifacts(noteId); void artifacts.loadPrerequisites(); }} refreshing={artifacts.noteState[noteId] === "loading"} />
      </div> : null;

    return <section aria-label={view === "map" ? "Mind map" : "Summary and saved views"} className="min-w-0">
      {toolbarContainer ? createPortal(toolbar, toolbarContainer) : toolbar ? <div className="mb-6 flex justify-end">{toolbar}</div> : null}

      {artifacts.prerequisitesError || artifacts.errors[noteId] ? <div role="alert" className="mb-4 flex flex-wrap items-center gap-2 text-sm text-destructive"><div>{artifacts.errors[noteId] ? <p>{artifacts.errors[noteId]}</p> : null}{artifacts.prerequisitesError ? <p>{artifacts.prerequisitesError}</p> : null}</div><Button size="sm" variant="ghost" disabled={artifacts.noteState[noteId] === "loading"} onClick={() => { void artifacts.loadArtifacts(noteId); void artifacts.loadPrerequisites(); }}><RefreshCcw data-icon="inline-start" />Try again</Button></div> : null}
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
        <div className="flex items-start justify-between gap-3"><div><h3 className="font-medium">Create a new view</h3><p className="mb-5 mt-1 text-sm text-muted-foreground">Choose how to shape your transcript.</p></div><Button type="button" variant="ghost" size="icon" aria-label="Close view options" onClick={() => { setShowGenerator(false); window.requestAnimationFrame(() => generatorTrigger.current?.focus()); }}><X /></Button></div>
        {artifacts.prerequisitesState === "loaded" && !profiles.length ? <p className="mb-4 text-sm text-muted-foreground">Set up a local agent in <Link className="underline underline-offset-4" to="/settings/post-processing">Settings → Post-processing</Link> to generate a view.</p> : null}
        <fieldset disabled={!canGenerate || generating} className="grid gap-4 sm:grid-cols-2"><legend className="sr-only">Generated view options</legend>
          <label className="flex flex-col gap-1.5 text-xs font-medium">View type<select autoFocus className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm font-normal" value={template?.id ?? ""} onChange={(event) => setTemplateId(event.target.value)}>{!templates.length ? <option value="">No templates available</option> : null}{templates.map((entry) => <option key={entry.id} value={entry.id} disabled={entry.requires_timestamps && !hasSegments}>{entry.name}{entry.requires_timestamps && !hasSegments ? " (needs timestamps)" : ""}</option>)}</select></label>
          <label className="flex flex-col gap-1.5 text-xs font-medium">Local agent<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm font-normal" value={profile?.id ?? ""} onChange={(event) => setProfileId(event.target.value)}>{!profiles.length ? <option value="">No agent available</option> : null}{profiles.map((entry) => <option key={entry.id} value={entry.id}>{entry.name}{!entry.available ? " (unavailable)" : ""}</option>)}</select></label>
          <label className="flex flex-col gap-1.5 text-xs font-medium sm:col-span-2">Extra context <span className="font-normal text-muted-foreground">Optional</span><textarea className="min-h-20 rounded-md border border-input bg-background px-3 py-2 text-sm font-normal" value={context} onChange={(event) => setContext(event.target.value)} placeholder="What would you like to focus on?" /></label>
        </fieldset>
        {template ? <p className="my-4 text-xs leading-relaxed text-muted-foreground">{template.description}</p> : null}
        {!canGenerate ? <p className="my-4 text-xs text-muted-foreground">Generated views become available after transcription completes.</p> : null}
        <Button size="sm" disabled={!canGenerate || generating || !template || !profile?.available}><Sparkles data-icon="inline-start" />{generating ? "Generating…" : "Generate view"}</Button>
      </form> : null}

      {artifacts.noteState[noteId] === "loading" && !saved.length ? <Skeleton className="h-72 w-full" /> : selected ? <>
        {view === "map" && selected.kind !== "mind_map" && selected.content_markdown ? <NoteMindMap key={selected.id} markdown={selected.content_markdown} theme={theme} onReadSection={readSection} />
          : view === "map" && selected.kind !== "mind_map" && selected.status === "completed" ? <div className="py-16 text-center"><Network className="mx-auto mb-4 size-6 text-muted-foreground" /><h3 className="font-serif text-2xl">Give this note a little perspective.</h3><p className="mx-auto mt-3 max-w-sm text-sm leading-relaxed text-muted-foreground">This view has no summary text to map. Generate a Mind Map from the transcript to explore its connections.</p><Button className="mt-5" variant="outline" onClick={() => { setTemplateId("mind_map"); setShowGenerator(true); }}>Create mind map</Button></div>
          : <ArtifactCard artifact={selected} theme={theme} onSeek={onSeek} duration={duration} showToolbar={false} onDelete={deleteSelected} />}
      </> : !generatorVisible && artifacts.noteState[noteId] !== "error" ? <div className="mx-auto max-w-sm py-8 text-center sm:py-12"><h3 className="font-serif text-2xl">{view === "map" ? "See how your ideas connect." : "The important parts, together."}</h3><p className="mt-3 text-sm leading-relaxed text-muted-foreground">{view === "map" ? "Create a mind map to explore this recording, one idea at a time." : "Create a summary of the key ideas and next steps, or start with the original transcript."}</p><div className="mt-6 flex flex-wrap items-center justify-center gap-2"><Button ref={generatorTrigger} variant="outline" size="sm" onClick={() => { setTemplateId(defaultTemplate); setShowGenerator(true); }} aria-expanded={false} aria-controls="note-generator"><Plus data-icon="inline-start" />{view === "map" ? "Create mind map" : "Create summary"}</Button>{onReadTranscript ? <Button variant="ghost" size="sm" onClick={onReadTranscript}>Read transcript</Button> : null}</div></div> : null}
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

function ArtifactActions({ artifact, onDelete, onRefresh, onNewView, viewPicker, triggerRef, refreshing = false }: { artifact: AudioNoteArtifact; onDelete: () => Promise<void>; onRefresh?: () => void; onNewView?: () => void; viewPicker?: ReactNode; triggerRef?: RefObject<HTMLButtonElement | null>; refreshing?: boolean }) {
  const [open, setOpen] = useState(false);
  const openingGenerator = useRef(false);
  return <Observer>{() => {
    const content = artifact.content_markdown || (artifact.content_json == null ? "" : JSON.stringify(artifact.content_json, null, 2));
    return <div className="flex text-muted-foreground"><Button variant="ghost" size="icon" className="hidden size-9 @2xl:inline-flex" aria-label="Copy artifact" title="Copy view" disabled={!content} onClick={() => void copyText(content, "Artifact copied")}><Copy className="size-3.5" /></Button><Popover open={open} onOpenChange={setOpen}><PopoverTrigger asChild><Button ref={triggerRef} variant="ghost" size="icon" className="size-9 shrink-0" aria-label="Saved view actions" title="Saved view actions"><MoreHorizontal className="size-4" /></Button></PopoverTrigger><PopoverContent align="end" className="w-52 p-1.5" onCloseAutoFocus={(event) => { if (openingGenerator.current) { event.preventDefault(); openingGenerator.current = false; } }}>
      {viewPicker ? <div className="mb-1 border-b px-2 pb-2"><p className="pt-2 text-[0.6875rem] text-muted-foreground">Saved view</p>{viewPicker}</div> : null}
      {onNewView ? <Button variant="ghost" size="sm" className="w-full justify-start" onClick={() => { openingGenerator.current = true; onNewView(); setOpen(false); }}><Plus data-icon="inline-start" />New view</Button> : null}
      <Button variant="ghost" size="sm" className="w-full justify-start" disabled={!content} onClick={() => { void copyText(content, "Artifact copied"); setOpen(false); }}><Copy data-icon="inline-start" />Copy view</Button>
      <Button variant="ghost" size="sm" className="w-full justify-start" disabled={!content} onClick={() => { downloadText(content, `audetic-note-${artifact.note_id}-${artifact.id}.${artifact.content_markdown ? "md" : "json"}`, artifact.content_markdown ? undefined : "application/json"); setOpen(false); }}><Download data-icon="inline-start" />Download view</Button>{onRefresh ? <Button variant="ghost" size="sm" className="w-full justify-start" disabled={refreshing} onClick={() => { onRefresh(); setOpen(false); }}><RefreshCcw data-icon="inline-start" />Refresh views</Button> : null}<div className="mt-1 border-t pt-1"><Button variant="ghost" size="sm" className="w-full justify-start text-destructive" onClick={() => { setOpen(false); void onDelete(); }}><Trash2 data-icon="inline-start" />Delete view</Button></div></PopoverContent></Popover></div>;
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
