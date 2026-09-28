import { useState } from "react";
import { Observer } from "mobx-react-lite";
import { Copy, RefreshCcw, Sparkles, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { useStore } from "@/stores/root-store";
import type { AudioNoteArtifact } from "@/stores/note-artifacts-store";
import { copyText } from "@/lib/clipboard";

export function NoteArtifactsPanel({ noteId, canGenerate }: { noteId: number; canGenerate: boolean }) {
  const store = useStore();
  const [templateId, setTemplateId] = useState("");
  const [profileId, setProfileId] = useState("");
  const [context, setContext] = useState("");
  return <Observer>{() => {
    const artifacts = store.noteArtifacts;
    const templates = artifacts.templates;
    const profiles = artifacts.profiles.filter((profile) => profile.enabled);
    const template = templates.find((entry) => entry.id === templateId) ?? templates[0];
    const profile = profiles.find((entry) => String(entry.id) === profileId) ?? profiles.find((entry) => entry.default_profile) ?? profiles[0];
    const saved = artifacts.byNote[noteId] ?? [];
    const generating = artifacts.generatingByNote[noteId];
    return <Card><CardHeader><div className="flex items-start justify-between gap-3"><div><CardTitle>Artifacts</CardTitle><CardDescription>Summaries, actions, and useful outputs from your notes. Generate again with any template.</CardDescription></div><Button size="sm" variant="outline" disabled={artifacts.noteState[noteId] === "loading"} onClick={() => { void artifacts.loadArtifacts(noteId); void artifacts.loadPrerequisites(); }}><RefreshCcw data-icon="inline-start" />Refresh</Button></div></CardHeader>
      <CardContent className="flex flex-col gap-5">
        {artifacts.prerequisitesError && <p role="alert" className="text-sm text-destructive">{artifacts.prerequisitesError}</p>}
        {artifacts.prerequisitesState === "loaded" && !profiles.length && <p className="text-sm text-muted-foreground">No enabled local agent profiles. Configure a local agent to generate artifacts; your transcript stays available.</p>}
        <form className="flex flex-col gap-3" onSubmit={async (event) => { event.preventDefault(); if (!template || !profile) return; const artifact = await artifacts.generateArtifact(noteId, { kind: "summary", template_id: template.id, agent_profile_id: profile.id, custom_context: context.trim() || null }); if (artifact?.status === "completed") { toast.success("Artifact generated"); setContext(""); } else toast.error(artifact?.error ?? artifacts.errors[noteId] ?? "Could not generate artifact"); }}>
          <fieldset disabled={!canGenerate || generating} className="grid gap-3 sm:grid-cols-2"><legend className="sr-only">Artifact generation options</legend><label className="flex flex-col gap-1.5 text-sm">Template<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" value={template?.id ?? ""} onChange={(event) => setTemplateId(event.target.value)}>{!templates.length && <option value="">No templates available</option>}{templates.map((entry) => <option key={entry.id} value={entry.id}>{entry.name}</option>)}</select></label><label className="flex flex-col gap-1.5 text-sm">Local agent<select className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" value={profile?.id ?? ""} onChange={(event) => setProfileId(event.target.value)}>{!profiles.length && <option value="">No agent available</option>}{profiles.map((entry) => <option key={entry.id} value={entry.id}>{entry.name}{!entry.available ? " (unavailable)" : ""}</option>)}</select></label><label className="flex flex-col gap-1.5 text-sm sm:col-span-2">Extra context<textarea className="min-h-20 rounded-md border border-input bg-background px-3 py-2" value={context} onChange={(event) => setContext(event.target.value)} placeholder="Optional audience, emphasis, or formatting preferences" /></label></fieldset>
          {template && <p className="text-xs text-muted-foreground">{template.description}</p>}
          {!canGenerate && <p className="text-xs text-muted-foreground">Artifacts become available after transcription completes.</p>}
          <Button className="self-start" disabled={!canGenerate || generating || !template || !profile?.available}><Sparkles data-icon="inline-start" />{generating ? "Generating…" : "Generate artifact"}</Button>
        </form>
        {artifacts.errors[noteId] && <p role="alert" className="text-sm text-destructive">{artifacts.errors[noteId]}</p>}
        {artifacts.noteState[noteId] === "loading" && !saved.length ? <Skeleton className="h-24 w-full" /> : saved.length ? <div className="flex flex-col gap-3">{saved.map((artifact) => <ArtifactCard key={artifact.id} artifact={artifact} onDelete={async () => { if (!window.confirm(`Delete artifact “${artifact.title}”?`)) return; if (await artifacts.deleteArtifact(noteId, artifact.id)) toast.success("Artifact deleted"); else toast.error(artifacts.errors[noteId] ?? "Could not delete artifact"); }} />)}</div> : <p className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">No artifacts yet. AI processing may create one automatically, or choose a template above.</p>}
      </CardContent>
    </Card>;
  }}</Observer>;
}

export function ArtifactCard({ artifact, onDelete }: { artifact: AudioNoteArtifact; onDelete: () => Promise<void> }) {
  return <Observer>{() => {
    const structured = artifact.content_json == null ? null : JSON.stringify(artifact.content_json, null, 2);
    return <article className="flex flex-col gap-3 rounded-lg border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0"><h3 className="font-medium">{artifact.title}</h3><p className="text-xs text-muted-foreground">{artifact.kind} · {artifact.status} · {new Date(artifact.created_at).toLocaleString()}</p></div>
        <div className="flex gap-1"><Button variant="ghost" size="icon" aria-label="Copy artifact" disabled={!artifact.content_markdown && !structured} onClick={() => void copyText(artifact.content_markdown || structured || "", "Artifact copied")}><Copy /></Button><Button variant="ghost" size="icon" aria-label="Delete artifact" onClick={() => void onDelete()}><Trash2 /></Button></div>
      </div>
      {artifact.error && <pre className="whitespace-pre-wrap text-xs text-destructive">{artifact.error}</pre>}
      {artifact.content_markdown && <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 font-sans text-sm leading-relaxed">{artifact.content_markdown}</pre>}
      {structured && <details><summary className="cursor-pointer text-sm font-medium">Structured data</summary><p className="mt-2 text-xs text-muted-foreground">Extracted information only. No external action has been performed.</p><pre className="my-3 max-h-72 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 text-xs">{structured}</pre><Button variant="outline" size="sm" onClick={() => void copyText(structured, "JSON copied")}><Copy data-icon="inline-start" />Copy JSON</Button></details>}
    </article>;
  }}</Observer>;
}
