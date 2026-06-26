import { Observer, observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import {
  Form,
  NavLink,
  useNavigate,
  useParams,
  type ActionFunctionArgs,
  type LoaderFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import {
  AlertTriangle,
  ArrowLeft,
  Bot,
  Copy,
  FileText,
  FolderOpen,
  Loader2,
  RefreshCcw,
  Sparkles,
  Trash2,
  Wrench,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import {
  isDeletableMeetingStatus,
  type MeetingDetail,
} from "@/stores/meeting-store";
import type {
  AgentProfile,
  MeetingArtifact,
  MeetingArtifactsStore,
} from "@/stores/meeting-artifacts-store";

const DETAIL_INTENTS = {
  copyTranscript: "copy-transcript",
  openAudio: "open-audio-folder",
} as const;

export const meetingDetailRoute: RouteObject = {
  path: "meetings/:id",
  loader: async ({ params }: LoaderFunctionArgs) => {
    const id = Number(params.id);
    if (!Number.isFinite(id)) return null;
    await getRootStore().meetings.loadDetail(id);
    return null;
  },
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    switch (intent) {
      case DETAIL_INTENTS.copyTranscript: {
        const text = String(form.get("text") ?? "");
        if (text) {
          await navigator.clipboard.writeText(text);
          toast.success("Transcript copied to clipboard");
        }
        return null;
      }
      case DETAIL_INTENTS.openAudio: {
        // Open the enclosing directory in the user's file manager. In
        // Electron this is available through the preload bridge; for
        // Phase 3 we skip the main-process IPC and just copy the path.
        const path = String(form.get("path") ?? "");
        if (path) {
          await navigator.clipboard.writeText(path);
          toast.success("Path copied to clipboard");
        }
        return null;
      }
      default:
        return null;
    }
  },
  Component: MeetingDetailRoute,
};

function MeetingDetailRoute() {
  const params = useParams();
  const id = Number(params.id);
  const store = useStore();

  // Auto-refresh while transcription is in flight (live recording or post-
  // failure retry). Stops as soon as the row reaches a terminal state. The
  // global `/meetings/status` poll only tracks the live recording machine,
  // not per-meeting retry jobs, so this loop owns refresh for retries.
  useEffect(() => {
    if (!Number.isFinite(id)) return;
    let cancelled = false;
    const tick = (): void => {
      if (cancelled) return;
      const cached = store.meetings.detailCache[id];
      if (!cached) return;
      if (cached.status === "transcribing" || cached.status === "compressing") {
        void store.meetings.loadDetail(id);
      }
    };
    const handle = window.setInterval(tick, 2000);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
    };
  }, [id, store]);

  return (
    <div className="mx-auto max-w-5xl p-8 space-y-6">
      <NavLink
        to="/meetings"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" />
        Meetings
      </NavLink>

      <Observer>
        {() => {
          if (!Number.isFinite(id)) {
            return <p className="text-sm text-destructive">Invalid meeting id.</p>;
          }
          const detail = store.meetings.detailCache[id];
          const status = store.meetings.detailStatus[id];
          if (!detail) {
            if (status === "error") {
              return <p className="text-sm text-destructive">Could not load meeting.</p>;
            }
            return <MeetingDetailSkeleton />;
          }
          return <MeetingDetailBody detail={detail} meetingId={id} />;
        }}
      </Observer>
    </div>
  );
}

const MeetingDetailBody = observer(function MeetingDetailBody({
  detail,
  meetingId,
}: {
  detail: MeetingDetail;
  meetingId: number;
}) {
  const store = useStore();
  const meetingArtifacts = store.meetingArtifacts;
  const navigate = useNavigate();
  const isTranscribing =
    detail.status === "transcribing" || detail.status === "compressing";

  useEffect(() => {
    void meetingArtifacts.loadPrerequisites();
    void meetingArtifacts.loadArtifacts(meetingId);
  }, [meetingArtifacts, meetingId]);

  const handleDelete = async (): Promise<void> => {
    const label = detail.title ?? "this meeting";
    if (!window.confirm(`Delete "${label}"? This hides it from all views.`)) {
      return;
    }
    const ok = await store.meetings.deleteMeeting(meetingId);
    if (ok) {
      toast.success("Meeting deleted");
      navigate("/meetings");
    } else {
      toast.error("Could not delete meeting");
    }
  };

  return (
    <>
      <header className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-semibold">
            {detail.title ?? <span className="text-muted-foreground">Untitled meeting</span>}
          </h1>
          <p className="text-sm text-muted-foreground">
            {new Date(detail.started_at).toLocaleString()}
            {typeof detail.duration_seconds === "number"
              ? ` · ${formatDuration(detail.duration_seconds)}`
              : ""}
            {" · "}
            <span className="font-mono text-xs">{detail.status}</span>
          </p>
        </div>
        {isDeletableMeetingStatus(detail.status) && (
          <Button
            variant="ghost"
            size="sm"
            className="shrink-0 text-muted-foreground hover:text-destructive"
            onClick={() => {
              void handleDelete();
            }}
          >
            <Trash2 className="mr-1 h-3.5 w-3.5" />
            Delete
          </Button>
        )}
      </header>

      {isTranscribing && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base flex items-center gap-2">
              <Loader2 className="h-4 w-4 animate-spin text-primary" />
              Transcribing…
            </CardTitle>
            <CardDescription>
              The audio is being transcribed. This page will update when it
              finishes.
            </CardDescription>
          </CardHeader>
        </Card>
      )}

      {detail.error &&
        !isTranscribing &&
        detail.status !== "completed" &&
        (isFfmpegError(detail.error) ? (
          <Card>
            <CardHeader>
              <CardTitle className="text-base flex items-center gap-2">
                <Wrench className="h-4 w-4 text-primary" />
                FFmpeg required
              </CardTitle>
              <CardDescription>
                This meeting couldn&apos;t be compressed because FFmpeg
                isn&apos;t installed. Use the <strong>Install FFmpeg</strong>
                {" "}card at the top of the page to set it up.
              </CardDescription>
            </CardHeader>
          </Card>
        ) : (
          <Card className="border-destructive/40">
            <CardHeader>
              <CardTitle className="text-destructive">Error</CardTitle>
              <CardDescription>
                Transcription failed. The audio is still on disk — retry to
                run it through the transcription service again.
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3 text-sm">
              <pre className="whitespace-pre-wrap">{detail.error}</pre>
              <Button
                variant="default"
                size="sm"
                onClick={() => {
                  void store.meetings.retryTranscription(meetingId);
                }}
              >
                <RefreshCcw className="mr-1 h-3.5 w-3.5" />
                Retry transcription
              </Button>
            </CardContent>
          </Card>
        ))}

      <Card>
        <CardHeader>
          <CardTitle>Transcript</CardTitle>
          {!detail.transcript_text && (
            <CardDescription>
              Not transcribed yet. If the meeting completed, this should appear shortly.
            </CardDescription>
          )}
        </CardHeader>
        <CardContent className="space-y-3">
          {detail.transcript_text ? (
            <>
              <pre className="whitespace-pre-wrap text-sm font-sans">
                {detail.transcript_text}
              </pre>
              <Form method="post" replace>
                <input
                  type="hidden"
                  name="intent"
                  value={DETAIL_INTENTS.copyTranscript}
                />
                <input
                  type="hidden"
                  name="text"
                  value={detail.transcript_text}
                />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button type="submit" variant="outline" size="sm">
                      <Copy className="mr-1 h-3.5 w-3.5" />
                      Copy all
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>Copy to clipboard</TooltipContent>
                </Tooltip>
              </Form>
            </>
          ) : null}
        </CardContent>
      </Card>

      <MeetingArtifactsPanel
        meetingId={meetingId}
        canGenerate={detail.status === "completed" && Boolean(detail.transcript_text)}
      />

      <Card>
        <CardHeader>
          <CardTitle>Files</CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          <FileRow label="Audio" path={detail.audio_path} />
          {detail.transcript_path && (
            <FileRow label="Transcript" path={detail.transcript_path} />
          )}
        </CardContent>
      </Card>
    </>
  );
});

function FileRow({ label, path }: { label: string; path: string }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <div className="min-w-0 flex-1">
        <div className="text-xs text-muted-foreground">{label}</div>
        <div className="truncate font-mono text-xs">{path}</div>
      </div>
      <Form method="post" replace>
        <input type="hidden" name="intent" value={DETAIL_INTENTS.openAudio} />
        <input type="hidden" name="path" value={path} />
        <Tooltip>
          <TooltipTrigger asChild>
            <Button type="submit" variant="ghost" size="sm">
              <FolderOpen className="h-3.5 w-3.5" />
            </Button>
          </TooltipTrigger>
          <TooltipContent>Copy path to clipboard</TooltipContent>
        </Tooltip>
      </Form>
    </div>
  );
}

const MeetingArtifactsPanel = observer(function MeetingArtifactsPanel({
  meetingId,
  canGenerate,
}: {
  meetingId: number;
  canGenerate: boolean;
}) {
  const store = useStore();
  const artifacts = store.meetingArtifacts;
  const [templateId, setTemplateId] = useState("");
  const [agentProfileId, setAgentProfileId] = useState("");
  const [customContext, setCustomContext] = useState("");

  return (
    <Observer>
      {() => {
        const templates = artifacts.templates;
        const profiles = artifacts.profiles;
        const meetingArtifacts = artifacts.byMeeting[meetingId] ?? [];
        const selectedTemplateId = templateId || templates[0]?.id || "";
        const selectedProfileId = agentProfileId || defaultProfileId(profiles);
        const isGenerating = artifacts.generatingByMeeting[meetingId] === true;
        const isLoadingArtifacts = artifacts.meetingState[meetingId] === "loading";
        const selectedTemplate = templates.find((t) => t.id === selectedTemplateId);

        const handleGenerate = async (): Promise<void> => {
          if (!selectedTemplateId) {
            toast.error("No summary template available");
            return;
          }
          const artifact = await artifacts.generateArtifact(meetingId, {
            kind: "summary",
            template_id: selectedTemplateId,
            agent_profile_id: selectedProfileId ? Number(selectedProfileId) : null,
            custom_context: customContext.trim() || null,
          });
          if (!artifact) {
            toast.error(artifacts.lastError ?? "Could not generate summary");
            return;
          }
          if (artifact.status === "completed") {
            toast.success("Summary generated");
            setCustomContext("");
          } else {
            toast.error(artifact.error ?? "Agent returned an error");
          }
        };

        return (
          <Card>
            <CardHeader>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div>
                  <CardTitle className="flex items-center gap-2">
                    <Sparkles className="h-4 w-4 text-primary" />
                    Meeting intelligence
                  </CardTitle>
                  <CardDescription>
                    Generate Markdown summaries and action reports with a local coding agent.
                  </CardDescription>
                </div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    void artifacts.loadArtifacts(meetingId);
                  }}
                  disabled={isLoadingArtifacts}
                >
                  <RefreshCcw className="mr-1 h-3.5 w-3.5" />
                  Refresh
                </Button>
              </div>
            </CardHeader>
            <CardContent className="space-y-5">
              {!canGenerate && (
                <div className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
                  Summaries are available after the meeting is completed and a transcript exists.
                </div>
              )}

              <div className="grid gap-3 md:grid-cols-[1fr_1fr_auto] md:items-end">
                <label className="space-y-1 text-sm">
                  <span className="font-medium">Template</span>
                  <select
                    name="templateId"
                    className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                    value={selectedTemplateId}
                    onChange={(event) => setTemplateId(event.target.value)}
                    disabled={!canGenerate || templates.length === 0 || isGenerating}
                  >
                    {templates.map((template) => (
                      <option key={template.id} value={template.id}>
                        {template.name}
                      </option>
                    ))}
                  </select>
                </label>

                <label className="space-y-1 text-sm">
                  <span className="font-medium">Agent</span>
                  <select
                    name="agentProfileId"
                    className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
                    value={selectedProfileId}
                    onChange={(event) => setAgentProfileId(event.target.value)}
                    disabled={!canGenerate || profiles.length === 0 || isGenerating}
                  >
                    {profiles.map((profile) => (
                      <option key={profile.id} value={String(profile.id)}>
                        {profileLabel(profile)}
                      </option>
                    ))}
                  </select>
                </label>

                <Button
                  type="button"
                  onClick={() => {
                    void handleGenerate();
                  }}
                  disabled={!canGenerate || !selectedTemplateId || isGenerating}
                >
                  {isGenerating ? (
                    <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Bot className="mr-1 h-3.5 w-3.5" />
                  )}
                  Generate
                </Button>
              </div>

              {selectedTemplate && (
                <p className="text-xs text-muted-foreground">{selectedTemplate.description}</p>
              )}

              <label className="space-y-1 text-sm block">
                <span className="font-medium">Extra context</span>
                <textarea
                  name="customContext"
                  className="min-h-20 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                  placeholder="Optional: audience, decisions to emphasize, formatting preferences…"
                  value={customContext}
                  onChange={(event) => setCustomContext(event.target.value)}
                  disabled={!canGenerate || isGenerating}
                />
              </label>

              {artifacts.lastError && (
                <div className="flex items-start gap-2 rounded-md border border-destructive/40 p-3 text-sm text-destructive">
                  <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
                  <span>{artifacts.lastError}</span>
                </div>
              )}

              <div className="space-y-3">
                <div className="flex items-center justify-between gap-2">
                  <h2 className="text-sm font-semibold">Artifacts</h2>
                  <span className="text-xs text-muted-foreground">
                    {meetingArtifacts.length} saved
                  </span>
                </div>
                {isLoadingArtifacts && meetingArtifacts.length === 0 ? (
                  <div className="space-y-2">
                    <Skeleton className="h-20 w-full" />
                    <Skeleton className="h-20 w-full" />
                  </div>
                ) : meetingArtifacts.length > 0 ? (
                  meetingArtifacts.map((artifact) => (
                    <ArtifactCard
                      key={artifact.id}
                      artifact={artifact}
                      onCopy={() => {
                        void copyArtifact(artifact);
                      }}
                      onDelete={() => {
                        void deleteArtifact(artifacts, meetingId, artifact);
                      }}
                    />
                  ))
                ) : (
                  <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
                    No generated artifacts yet. Choose a template and local agent to create one.
                  </div>
                )}
              </div>
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
});

const ArtifactCard = observer(function ArtifactCard({
  artifact,
  onCopy,
  onDelete,
}: {
  artifact: MeetingArtifact;
  onCopy: () => void;
  onDelete: () => void;
}) {
  const hasContent = Boolean(artifact.content_markdown?.trim());
  return (
    <div className="rounded-lg border bg-card p-4 space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <FileText className="h-4 w-4 text-primary" />
            <h3 className="truncate text-sm font-medium">{artifact.title}</h3>
          </div>
          <p className="text-xs text-muted-foreground">
            {artifact.status} · {new Date(artifact.created_at).toLocaleString()}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <Button type="button" variant="ghost" size="sm" onClick={onCopy} disabled={!hasContent}>
            <Copy className="h-3.5 w-3.5" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="text-muted-foreground hover:text-destructive"
            onClick={onDelete}
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
      {artifact.error && (
        <pre className="whitespace-pre-wrap rounded-md bg-destructive/10 p-3 text-xs text-destructive">
          {artifact.error}
        </pre>
      )}
      {hasContent && (
        <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-muted/40 p-3 text-sm font-sans">
          {artifact.content_markdown}
        </pre>
      )}
    </div>
  );
});

function defaultProfileId(profiles: AgentProfile[]): string {
  const profile = profiles.find((p) => p.default_profile && p.enabled) ?? profiles.find((p) => p.enabled) ?? profiles[0];
  return profile ? String(profile.id) : "";
}

function profileLabel(profile: AgentProfile): string {
  const flags = [profile.available ? "available" : "missing"];
  if (profile.default_profile) flags.push("default");
  return `${profile.name} (${flags.join(", ")})`;
}

async function copyArtifact(artifact: MeetingArtifact): Promise<void> {
  if (!artifact.content_markdown) return;
  await navigator.clipboard.writeText(artifact.content_markdown);
  toast.success("Artifact copied to clipboard");
}

async function deleteArtifact(
  store: MeetingArtifactsStore,
  meetingId: number,
  artifact: MeetingArtifact,
): Promise<void> {
  if (!window.confirm(`Delete "${artifact.title}"?`)) return;
  const ok = await store.deleteArtifact(meetingId, artifact.id);
  if (ok) {
    toast.success("Artifact deleted");
  } else {
    toast.error(store.lastError ?? "Could not delete artifact");
  }
}

function MeetingDetailSkeleton() {
  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Skeleton className="h-7 w-64" />
        <Skeleton className="h-3 w-80" />
      </div>
      <Card>
        <CardHeader>
          <Skeleton className="h-5 w-32" />
        </CardHeader>
        <CardContent className="space-y-2">
          <Skeleton className="h-3 w-full" />
          <Skeleton className="h-3 w-11/12" />
          <Skeleton className="h-3 w-4/5" />
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <Skeleton className="h-5 w-20" />
        </CardHeader>
        <CardContent className="space-y-3">
          <Skeleton className="h-4 w-full" />
          <Skeleton className="h-4 w-2/3" />
        </CardContent>
      </Card>
    </div>
  );
}

function formatDuration(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}m ${secs.toString().padStart(2, "0")}s`;
}

function isFfmpegError(message: string): boolean {
  return /ffmpeg/i.test(message);
}
