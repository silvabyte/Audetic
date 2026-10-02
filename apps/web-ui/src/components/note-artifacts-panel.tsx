import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Braces, Clock3, Copy, FileText, ListChecks, Network, RefreshCcw, ShoppingBasket, Sparkles, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { ArtifactContent } from "@/components/artifact-content";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { useStore } from "@/stores/root-store";
import type { AudioNoteArtifact } from "@/stores/note-artifacts-store";
import type { EffectiveTheme } from "@/stores/ui-store";
import { copyText } from "@/lib/clipboard";
import { cn } from "@/lib/utils";

export function NoteArtifactsPanel({ noteId, canGenerate, hasSegments = false }: { noteId: number; canGenerate: boolean; hasSegments?: boolean }) {
  const store = useStore();
  const [templateId, setTemplateId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [context, setContext] = useState("");
  const [selectedArtifactId, setSelectedArtifactId] = useState<number | null>(null);
  return <Observer>{() => {
    const artifacts = store.noteArtifacts;
    const templates = artifacts.templates;
    const profiles = artifacts.profiles.filter((profile) => profile.enabled);
    const eligibleTemplates = templates.filter((entry) => !entry.requires_timestamps || hasSegments);
    const template = eligibleTemplates.find((entry) => entry.id === templateId) ?? eligibleTemplates[0];
    const profile = profiles.find((entry) => String(entry.id) === profileId) ?? profiles.find((entry) => entry.default_profile) ?? profiles[0];
    const saved = artifacts.byNote[noteId] ?? [];
    const selected = saved.find((artifact) => artifact.id === selectedArtifactId) ?? saved[0];
    const generating = artifacts.generatingByNote[noteId];
    const theme = store.ui.effectiveTheme;
    return <section aria-labelledby="generated-heading" className="min-w-0">
      <div className="mb-6 flex items-end justify-between gap-4 border-b pb-4">
        <div><p className="mb-1 text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">Saved documents</p><h2 id="generated-heading" className="text-xl font-semibold tracking-tight">Generated views</h2></div>
        <Button size="sm" variant="ghost" disabled={artifacts.noteState[noteId] === "loading"} onClick={() => { void artifacts.loadArtifacts(noteId); void artifacts.loadPrerequisites(); }}><RefreshCcw data-icon="inline-start" />Refresh</Button>
      </div>
      {artifacts.prerequisitesError ? <p role="alert" className="mb-4 text-sm text-destructive">{artifacts.prerequisitesError}</p> : null}
      {artifacts.errors[noteId] ? <p role="alert" className="mb-4 text-sm text-destructive">{artifacts.errors[noteId]}</p> : null}
      {artifacts.noteState[noteId] === "loading" && !saved.length ? <Skeleton className="h-72 w-full" /> : saved.length && selected ? <div className="grid min-w-0 gap-6 md:grid-cols-[11rem_minmax(0,1fr)]">
        <nav role="tablist" aria-label="Generated views" className="flex gap-1 overflow-x-auto border-b pb-3 md:flex-col md:overflow-visible md:border-b-0 md:border-r md:pb-0 md:pr-4">
          {saved.map((artifact, index) => {
            const Icon = artifactKindIcon(artifact.kind);
            const selectedArtifact = artifact.id === selected.id;
            const generatedAt = new Date(artifact.created_at).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" });
            return <button key={artifact.id} id={`artifact-tab-${artifact.id}`} type="button" role="tab" aria-selected={selectedArtifact} aria-controls="artifact-panel" aria-label={`${artifact.title}, ${artifactKindLabel(artifact.kind)}, generated ${generatedAt}, view ${artifact.id}`} tabIndex={selectedArtifact ? 0 : -1} title={artifact.title} onClick={() => setSelectedArtifactId(artifact.id)} onKeyDown={(event) => {
              let nextIndex: number | null = null;
              if (["ArrowRight", "ArrowDown"].includes(event.key)) nextIndex = (index + 1) % saved.length;
              if (["ArrowLeft", "ArrowUp"].includes(event.key)) nextIndex = (index - 1 + saved.length) % saved.length;
              if (event.key === "Home") nextIndex = 0;
              if (event.key === "End") nextIndex = saved.length - 1;
              if (nextIndex === null) return;
              event.preventDefault();
              const next = saved[nextIndex];
              if (!next) return;
              setSelectedArtifactId(next.id);
              event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>("[role=tab]")[nextIndex]?.focus();
            }} className={cn("flex shrink-0 items-start gap-2 rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted/50 hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring", selectedArtifact && "bg-muted text-foreground")}><Icon className="mt-0.5 size-4 shrink-0" /><span className="min-w-0"><span className="block max-w-32 truncate">{artifact.title}</span><span className="block text-[0.6875rem] text-muted-foreground">{artifactKindLabel(artifact.kind)} · {generatedAt}</span></span></button>;
          })}
        </nav>
        <div id="artifact-panel" role="tabpanel" aria-labelledby={`artifact-tab-${selected.id}`} tabIndex={0} className="min-w-0 focus-visible:outline-none"><ArtifactCard artifact={selected} theme={theme} onDelete={async () => { if (!window.confirm(`Delete artifact “${selected.title}”?`)) return; if (await artifacts.deleteArtifact(noteId, selected.id)) { setSelectedArtifactId(null); toast.success("Artifact deleted"); } else toast.error(artifacts.errors[noteId] ?? "Could not delete artifact"); }} /></div>
      </div> : <p className="border-y py-10 text-sm text-muted-foreground">No generated views yet. Your transcript remains the source of truth.</p>}

      <details className="mt-8 border-t pt-5">
        <summary className="cursor-pointer text-sm font-medium text-muted-foreground hover:text-foreground">Generate another view</summary>
        <div className="mt-5">
          {artifacts.prerequisitesState === "loaded" && !profiles.length ? <p className="mb-4 text-sm text-muted-foreground">No enabled local agent profiles. Configure a local agent to generate views; your transcript stays available.</p> : null}
          <form className="flex flex-col gap-4" onSubmit={async (event) => {
            event.preventDefault();
            if (!template || !profile) return;
            const artifact = await artifacts.generateArtifact(noteId, { template_id: template.id, agent_profile_id: profile.id, custom_context: context.trim() || null });
            if (artifact) {
              setSelectedArtifactId(artifact.id);
              setContext("");
              toast.success(artifact.status === "completed" ? "View generated" : "View generation started");
            } else toast.error(artifacts.errors[noteId] ?? "Could not generate view");
          }}>
            <fieldset disabled={!canGenerate || generating} className="grid gap-4 sm:grid-cols-2"><legend className="sr-only">Generated view options</legend>
              <label className="flex flex-col gap-1.5 text-sm">View type<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" value={template?.id ?? ""} onChange={(event) => setTemplateId(event.target.value)}>{!templates.length ? <option value="">No templates available</option> : null}{templates.map((entry) => <option key={entry.id} value={entry.id} disabled={entry.requires_timestamps && !hasSegments}>{artifactKindLabel(entry.kind)} · {entry.name}{entry.requires_timestamps && !hasSegments ? " (needs timestamps)" : ""}</option>)}</select></label>
              <label className="flex flex-col gap-1.5 text-sm">Local agent<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" value={profile?.id ?? ""} onChange={(event) => setProfileId(event.target.value)}>{!profiles.length ? <option value="">No agent available</option> : null}{profiles.map((entry) => <option key={entry.id} value={entry.id}>{entry.name}{!entry.available ? " (unavailable)" : ""}</option>)}</select></label>
              <label className="flex flex-col gap-1.5 text-sm sm:col-span-2">Extra context<textarea className="min-h-20 rounded-md border border-input bg-background px-3 py-2" value={context} onChange={(event) => setContext(event.target.value)} placeholder="Optional audience, emphasis, or formatting preferences" /></label>
            </fieldset>
            {template ? <p className="text-xs text-muted-foreground">{template.description}</p> : null}
            {!canGenerate ? <p className="text-xs text-muted-foreground">Generated views become available after transcription completes.</p> : null}
            <Button className="self-start" disabled={!canGenerate || generating || !template || !profile?.available}><Sparkles data-icon="inline-start" />{generating ? "Generating…" : "Generate view"}</Button>
          </form>
        </div>
      </details>
    </section>;
  }}</Observer>;
}

export function ArtifactCard({ artifact, onDelete, theme = "light" }: { artifact: AudioNoteArtifact; onDelete: () => Promise<void>; theme?: EffectiveTheme }) {
  return <Observer>{() => {
    const structured = artifact.content_json == null ? null : JSON.stringify(artifact.content_json, null, 2);
    const Icon = artifactKindIcon(artifact.kind);
    return <article className="min-w-0">
      <header className="mb-6 flex items-start justify-between gap-4">
        <div className="min-w-0"><div className="mb-2 flex items-center gap-2 text-xs text-muted-foreground"><Icon className="size-4" /><span>{artifactKindLabel(artifact.kind)}</span><span aria-hidden="true">·</span><span>{artifact.status}</span></div><h3 className="text-2xl font-semibold tracking-tight">{artifact.title}</h3><p className="mt-1 text-xs text-muted-foreground">{new Date(artifact.created_at).toLocaleString()}</p></div>
        <div className="flex gap-1"><Button variant="ghost" size="icon" aria-label="Copy artifact" disabled={!artifact.content_markdown && !structured} onClick={() => void copyText(artifact.content_markdown || structured || "", "Artifact copied")}><Copy /></Button><Button variant="ghost" size="icon" aria-label="Delete artifact" onClick={() => void onDelete()}><Trash2 /></Button></div>
      </header>
      {artifact.error ? <pre className="whitespace-pre-wrap text-xs text-destructive">{artifact.error}</pre> : null}
      {artifact.content_markdown ? <ArtifactContent markdown={artifact.content_markdown} theme={theme} /> : null}
      {structured ? <details className="mt-6 border-t pt-4"><summary className="cursor-pointer text-sm font-medium">Structured data</summary><p className="mt-2 text-xs text-muted-foreground">Extracted information only. No external action has been performed.</p><pre className="my-3 max-h-72 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 text-xs">{structured}</pre><Button variant="outline" size="sm" onClick={() => void copyText(structured, "JSON copied")}><Copy data-icon="inline-start" />Copy JSON</Button></details> : null}
    </article>;
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

function artifactKindIcon(kind: string) {
  switch (kind) {
    case "meeting_minutes": return FileText;
    case "action_items": return ListChecks;
    case "talking_points": return Clock3;
    case "mind_map": return Network;
    case "intent": return Braces;
    case "shopping_items": return ShoppingBasket;
    default: return FileText;
  }
}
