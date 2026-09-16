import { Observer, observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
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
  AlignLeft,
  ArrowLeft,
  Bot,
  BrainCircuit,
  Check,
  ChevronDown,
  ClipboardList,
  Copy,
  FastForward,
  FileText,
  FolderOpen,
  Loader2,
  ListChecks,
  ListTree,
  Pause,
  Pencil,
  Play,
  RefreshCcw,
  Rewind,
  Sparkles,
  Trash2,
  Wrench,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { ArtifactContent } from "@/components/artifact-content";
import { Input } from "@/components/ui/input";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { meetingDisplayTitle } from "@/lib/meeting-title";
import { RecentTitleSuggestions } from "@/components/meeting-title-picker";
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
  SummaryTemplate,
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
    <div className="mx-auto max-w-7xl space-y-6 p-4 sm:p-8">
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
  const [activeView, setActiveView] = useState<MeetingView>("transcript");
  const isTranscribing =
    detail.status === "transcribing" || detail.status === "compressing";
  const talkingPoints = extractTalkingPoints(
    meetingArtifacts.byMeeting[meetingId] ?? [],
  );

  useEffect(() => {
    void meetingArtifacts.loadPrerequisites();
    void meetingArtifacts.loadArtifacts(meetingId);
  }, [meetingArtifacts, meetingId]);

  const handleDelete = async (): Promise<void> => {
    const label = meetingDisplayTitle({
      title: detail.title,
      sourceFilename: detail.source_filename,
      startedAt: detail.started_at,
    });
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
      <MeetingTitleHeader
        meetingId={meetingId}
        title={detail.title ?? null}
        sourceFilename={detail.source_filename ?? null}
        startedAt={detail.started_at}
        durationSeconds={detail.duration_seconds ?? null}
        status={detail.status}
        showGenerate={detail.status === "completed"}
        canGenerate={
          detail.status === "completed" && Boolean(detail.transcript_text)
        }
        canDelete={isDeletableMeetingStatus(detail.status)}
        onDelete={handleDelete}
      />

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

      <section className="overflow-hidden rounded-xl border bg-card shadow-sm">
        <MeetingViewTabs
          activeView={activeView}
          artifactCount={meetingArtifacts.byMeeting[meetingId]?.length ?? 0}
          onChange={setActiveView}
        />

        <div className="p-4 sm:p-6 lg:p-8">
          {activeView === "transcript" && (
            <div className="mb-4 flex items-start justify-between gap-4">
              <div>
                <h2 className="text-lg font-semibold tracking-tight">Transcript</h2>
                <p className="text-sm text-muted-foreground">
                  Play the recording and select any passage to jump to it.
                </p>
              </div>
              {detail.transcript_text && (
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
                      <Button type="submit" variant="ghost" size="sm">
                        <Copy className="mr-1 h-3.5 w-3.5" />
                        Copy all
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent>Copy transcript to clipboard</TooltipContent>
                  </Tooltip>
                </Form>
              )}
            </div>
          )}

          {detail.transcript_text && (
            <TranscriptPlayer
              meetingId={meetingId}
              text={detail.transcript_text}
              segments={detail.transcript_segments}
              showTranscript={activeView === "transcript"}
              talkingPoints={talkingPoints}
              durationHint={detail.duration_seconds ?? 0}
            />
          )}

          {activeView === "transcript" && !detail.transcript_text && (
            <div className="rounded-lg border border-dashed px-5 py-12 text-center text-sm text-muted-foreground">
              Not transcribed yet. The transcript will appear here when processing finishes.
            </div>
          )}

          {activeView === "notes" && (
            <div className={detail.transcript_text ? "mt-8" : undefined}>
              <MeetingArtifactsPanel
                meetingId={meetingId}
                canGenerate={
                  detail.status === "completed" && Boolean(detail.transcript_text)
                }
                hasTimestamps={Boolean(detail.transcript_segments?.length)}
              />
            </div>
          )}

          {activeView === "details" && (
            <div
              className={cn(
                "mx-auto max-w-3xl space-y-6",
                detail.transcript_text && "mt-8",
              )}
            >
              <div>
                <h2 className="text-lg font-semibold tracking-tight">Recording details</h2>
                <p className="text-sm text-muted-foreground">
                  Source files remain local to this machine.
                </p>
              </div>
              <div className="divide-y rounded-lg border">
                <FileRow label="Audio" path={detail.audio_path} />
                {detail.transcript_path && (
                  <FileRow label="Transcript" path={detail.transcript_path} />
                )}
              </div>
            </div>
          )}
        </div>
      </section>
    </>
  );
});

type MeetingView = "transcript" | "notes" | "details";

function MeetingViewTabs({
  activeView,
  artifactCount,
  onChange,
}: {
  activeView: MeetingView;
  artifactCount: number;
  onChange: (view: MeetingView) => void;
}) {
  const tabs: Array<{
    id: MeetingView;
    label: string;
    icon: typeof FileText;
    count?: number;
  }> = [
    { id: "transcript", label: "Transcript", icon: FileText },
    { id: "notes", label: "Notes", icon: Sparkles, count: artifactCount },
    { id: "details", label: "Details", icon: FolderOpen },
  ];

  return (
    <div
      className="flex gap-6 overflow-x-auto border-b px-4 sm:px-6 lg:px-8"
      role="tablist"
      aria-label="Meeting content"
    >
      {tabs.map((tab) => {
        const Icon = tab.icon;
        const selected = activeView === tab.id;
        return (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={selected}
            onClick={() => onChange(tab.id)}
            className={cn(
              "relative flex h-14 shrink-0 items-center gap-2 text-sm font-medium transition-colors",
              selected
                ? "text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            <Icon className="h-4 w-4" />
            {tab.label}
            {typeof tab.count === "number" && tab.count > 0 && (
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] tabular-nums text-muted-foreground">
                {tab.count}
              </span>
            )}
            {selected && (
              <span className="absolute inset-x-0 bottom-0 h-0.5 rounded-full bg-primary" />
            )}
          </button>
        );
      })}
    </div>
  );
}

function MeetingTitleHeader({
  meetingId,
  title,
  sourceFilename,
  startedAt,
  durationSeconds,
  status,
  showGenerate,
  canGenerate,
  canDelete,
  onDelete,
}: {
  meetingId: number;
  title: string | null;
  sourceFilename: string | null;
  startedAt: string;
  durationSeconds: number | null;
  status: string;
  showGenerate: boolean;
  canGenerate: boolean;
  canDelete: boolean;
  onDelete: () => Promise<void>;
}) {
  const store = useStore();
  const inputRef = useRef<HTMLInputElement>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(title ?? "");
  const [validationError, setValidationError] = useState<string | null>(null);

  useEffect(() => {
    if (!editing) setDraft(title ?? "");
  }, [editing, title]);

  useEffect(() => {
    if (status === "completed" && !title?.trim()) {
      store.meetings.watchForGeneratedTitle(meetingId);
    }
  }, [meetingId, status, store, title]);

  useEffect(() => {
    if (!editing) return;
    inputRef.current?.focus();
    inputRef.current?.select();
  }, [editing]);

  function cancelEditing(): void {
    setDraft(title ?? "");
    setValidationError(null);
    store.meetings.clearTitleMutationFeedback(meetingId);
    setEditing(false);
  }

  async function saveTitle(nextTitle = draft): Promise<void> {
    const trimmed = nextTitle.trim();
    if (!trimmed) {
      setValidationError("Title cannot be blank.");
      inputRef.current?.focus();
      return;
    }
    setValidationError(null);
    const saved = await store.meetings.updateTitle(meetingId, trimmed);
    if (saved) {
      setDraft(trimmed);
      setEditing(false);
    }
  }

  async function generateTitle(): Promise<void> {
    const accepted = await store.meetings.regenerateTitle(meetingId);
    if (accepted) {
      setEditing(false);
      toast.message("Generating title from transcript", {
        description: "The title will update here when generation finishes.",
      });
    }
  }

  return (
    <Observer>
      {() => {
        const mutationStatus = store.meetings.titleMutationStatus[meetingId] ?? "idle";
        const mutationError = store.meetings.titleMutationError[meetingId];
        const saving = mutationStatus === "saving";
        const generating = mutationStatus === "generating";
        const displayTitle = meetingDisplayTitle({
          title,
          sourceFilename,
          startedAt,
        });
        const usingDateFallback = !title?.trim() && !sourceFilename?.trim();

        return (
          <header className="flex flex-col items-start justify-between gap-4 sm:flex-row">
            <div className="min-w-0 flex-1">
              {editing ? (
                <Popover open onOpenChange={(open) => !open && cancelEditing()}>
                  <PopoverAnchor asChild>
                    <div className="flex max-w-2xl items-center gap-1 rounded-md border border-primary/50 bg-background p-1 shadow-sm ring-2 ring-primary/10">
                      <Input
                        ref={inputRef}
                        value={draft}
                        onChange={(event) => {
                          setDraft(event.target.value);
                          setValidationError(null);
                        }}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") {
                            event.preventDefault();
                            void saveTitle();
                          } else if (event.key === "Escape") {
                            event.preventDefault();
                            cancelEditing();
                          }
                        }}
                        aria-label="Meeting title"
                        aria-invalid={Boolean(validationError || mutationError)}
                        autoComplete="off"
                        disabled={saving}
                        className="h-12 min-w-0 border-0 bg-transparent px-2 text-2xl font-semibold shadow-none focus-visible:ring-0 sm:text-3xl"
                      />
                      <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground" />
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8 shrink-0"
                        aria-label="Save title"
                        disabled={saving}
                        onClick={() => void saveTitle()}
                      >
                        {saving ? (
                          <Loader2 className="h-4 w-4 animate-spin" />
                        ) : (
                          <Check className="h-4 w-4" />
                        )}
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="h-8 w-8 shrink-0"
                        aria-label="Cancel title edit"
                        disabled={saving}
                        onClick={cancelEditing}
                      >
                        <X className="h-4 w-4" />
                      </Button>
                    </div>
                  </PopoverAnchor>
                  <PopoverContent
                    align="start"
                    className="w-[min(24rem,calc(100vw-2rem))] p-2"
                    onOpenAutoFocus={(event) => event.preventDefault()}
                    aria-label="Recent meeting titles"
                  >
                    <div className="px-2 pb-1 pt-0.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
                      Recent titles
                    </div>
                    <RecentTitleSuggestions
                      query={draft === (title ?? "") ? "" : draft}
                      disabled={saving}
                      onSelect={(selectedTitle) => {
                        setDraft(selectedTitle);
                        void saveTitle(selectedTitle);
                      }}
                    />
                  </PopoverContent>
                </Popover>
              ) : (
                <div className="group flex max-w-full items-center gap-2">
                  <h1
                    className={cn(
                      "min-w-0 text-3xl font-semibold leading-tight tracking-tight sm:text-4xl",
                      !title?.trim() && "text-muted-foreground",
                      !generating && "cursor-text",
                    )}
                    onClick={() => {
                      if (generating) return;
                      store.meetings.clearTitleMutationFeedback(meetingId);
                      setDraft(title ?? "");
                      setValidationError(null);
                      setEditing(true);
                    }}
                  >
                    {displayTitle}
                  </h1>
                  <button
                    type="button"
                    className="shrink-0 rounded-sm p-1 text-muted-foreground opacity-0 outline-none transition-opacity hover:text-foreground group-hover:opacity-100 focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
                    onClick={() => {
                      store.meetings.clearTitleMutationFeedback(meetingId);
                      setDraft(title ?? "");
                      setValidationError(null);
                      setEditing(true);
                    }}
                    aria-label={`Edit meeting title: ${displayTitle}`}
                    disabled={generating}
                  >
                    <Pencil className="h-3.5 w-3.5" />
                  </button>
                </div>
              )}

              <p className="mt-1 text-sm text-muted-foreground">
                {!usingDateFallback && new Date(startedAt).toLocaleString()}
                {typeof durationSeconds === "number"
                  ? `${usingDateFallback ? "" : " · "}${formatDuration(durationSeconds)}`
                  : ""}
                {(durationSeconds !== null || !usingDateFallback) && " · "}
                <span className="rounded-full bg-muted px-2 py-0.5 text-[11px] font-medium capitalize">
                  {status.replace(/_/g, " ")}
                </span>
              </p>
              {(validationError || mutationError || generating) && (
                <p
                  className={cn(
                    "mt-1 flex items-center gap-1.5 text-xs",
                    generating ? "text-muted-foreground" : "text-destructive",
                  )}
                  role="status"
                >
                  {generating && <Loader2 className="h-3 w-3 animate-spin" />}
                  {generating
                    ? "Generating a title from the transcript…"
                    : validationError ?? mutationError}
                </p>
              )}
            </div>

            <div className="flex shrink-0 flex-wrap items-center gap-1">
              {showGenerate && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!canGenerate || saving || generating}
                  onClick={() => void generateTitle()}
                  title={canGenerate ? undefined : "A transcript is required"}
                >
                  {generating ? (
                    <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Sparkles className="mr-1 h-3.5 w-3.5" />
                  )}
                  {generating ? "Generating…" : "Auto-title"}
                </Button>
              )}
              {canDelete && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="text-muted-foreground hover:text-destructive"
                  onClick={() => void onDelete()}
                >
                  <Trash2 className="mr-1 h-3.5 w-3.5" />
                  Delete
                </Button>
              )}
            </div>
          </header>
        );
      }}
    </Observer>
  );
}

type TranscriptSegment = NonNullable<MeetingDetail["transcript_segments"]>[number];

/// Renders the transcript. With segment timestamps it shows an audio player and
/// clickable lines that seek + highlight as the audio plays; without them it
/// falls back to a plain text block.
function TranscriptPlayer({
  meetingId,
  text,
  segments,
  showTranscript,
  talkingPoints,
  durationHint,
}: {
  meetingId: number;
  text: string;
  segments: TranscriptSegment[] | null | undefined;
  showTranscript: boolean;
  talkingPoints: TalkingPoint[];
  durationHint: number;
}) {
  const audioRef = useRef<HTMLAudioElement>(null);
  // Last index we scrolled to, so the follow-the-playhead scroll fires only when
  // the active line actually changes — not on every timeupdate re-render.
  const lastScrolledIndex = useRef(-1);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(durationHint);
  const [playing, setPlaying] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1);

  const seekTo = (seconds: number): void => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    // play() rejects when the media can't start (audio 404, autoplay policy).
    // Swallow it: the seek already happened and there's nothing to recover.
    audio.play().catch(() => {});
  };

  const movePlayhead = (deltaSeconds: number): void => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = Math.min(
      Math.max(audio.currentTime + deltaSeconds, 0),
      duration || Number.POSITIVE_INFINITY,
    );
    setCurrentTime(audio.currentTime);
  };

  const togglePlayback = (): void => {
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) {
      audio.play().catch(() => {});
    } else {
      audio.pause();
    }
  };

  const cyclePlaybackRate = (): void => {
    const rates = [0.75, 1, 1.25, 1.5, 2];
    const currentIndex = rates.indexOf(playbackRate);
    const nextRate = rates[(currentIndex + 1) % rates.length] ?? 1;
    setPlaybackRate(nextRate);
    if (audioRef.current) audioRef.current.playbackRate = nextRate;
  };

  // The segment fields (`segments[..].start/.text`) are MobX observables, so the
  // render that reads them must run inside a reactive context — otherwise it
  // won't re-render when the meeting detail loads/changes. Reads stay inside
  // <Observer>; the hooks above are plain React state and stay outside it.
  return (
    <div className="overflow-hidden rounded-xl border bg-background">
      <audio
        ref={audioRef}
        preload="metadata"
        src={`/api/meetings/${meetingId}/audio`}
        onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)}
        onTimeUpdate={(event) => setCurrentTime(event.currentTarget.currentTime)}
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => setPlaying(false)}
      />

      <div className="flex flex-wrap items-center gap-3 border-b bg-muted/25 px-3 py-3 sm:px-5">
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-8 w-8 rounded-full"
          onClick={() => movePlayhead(-10)}
          aria-label="Go back 10 seconds"
        >
          <Rewind className="h-3.5 w-3.5" />
        </Button>
        <Button
          type="button"
          size="icon"
          className="h-10 w-10 rounded-full"
          onClick={togglePlayback}
          aria-label={playing ? "Pause recording" : "Play recording"}
        >
          {playing ? (
            <Pause className="h-4 w-4" fill="currentColor" />
          ) : (
            <Play className="ml-0.5 h-4 w-4" fill="currentColor" />
          )}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-8 w-8 rounded-full"
          onClick={() => movePlayhead(10)}
          aria-label="Go forward 10 seconds"
        >
          <FastForward className="h-3.5 w-3.5" />
        </Button>
        <span className="w-10 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
          {formatTimestamp(currentTime)}
        </span>
        <div className="relative min-w-32 flex-1 py-2">
          <input
            type="range"
            min={0}
            max={duration || 0}
            step={0.1}
            value={Math.min(currentTime, duration || 0)}
            onChange={(event) => {
              const nextTime = Number(event.target.value);
              if (audioRef.current) audioRef.current.currentTime = nextTime;
              setCurrentTime(nextTime);
            }}
            className="block h-1 w-full cursor-pointer accent-primary"
            aria-label="Recording position"
          />
          {duration > 0 &&
            talkingPoints.map((point) => (
              <button
                key={`${point.seconds}-${point.label}`}
                type="button"
                className="absolute top-1/2 h-3 w-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full border border-background bg-primary shadow-sm transition-transform hover:scale-150 focus-visible:scale-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                style={{ left: `${Math.min((point.seconds / duration) * 100, 100)}%` }}
                onClick={() => seekTo(point.seconds)}
                aria-label={`Jump to ${point.label} at ${formatTimestamp(point.seconds)}`}
                title={`${formatTimestamp(point.seconds)} · ${point.label}`}
              />
            ))}
        </div>
        <span className="w-10 font-mono text-[11px] tabular-nums text-muted-foreground">
          {duration ? formatTimestamp(duration) : "--:--"}
        </span>
        <button
          type="button"
          onClick={cyclePlaybackRate}
          className="rounded-md border bg-background px-2 py-1 font-mono text-[10px] font-semibold tabular-nums text-muted-foreground transition-colors hover:text-foreground"
          aria-label={`Playback speed ${playbackRate} times`}
          title="Change playback speed"
        >
          {playbackRate}x
        </button>
      </div>

      {talkingPoints.length > 0 && (
        <div className="flex gap-2 overflow-x-auto border-b px-4 py-2.5 sm:px-5">
          {talkingPoints.map((point) => {
            const active = isTalkingPointActive(point, talkingPoints, currentTime);
            return (
              <button
                key={`${point.seconds}-${point.label}-chip`}
                type="button"
                onClick={() => seekTo(point.seconds)}
                className={cn(
                  "flex shrink-0 items-center gap-2 rounded-full border px-3 py-1.5 text-xs transition-colors",
                  active
                    ? "border-primary/30 bg-primary/10 text-primary"
                    : "bg-background text-muted-foreground hover:text-foreground",
                )}
                title={point.description || point.label}
              >
                <span className="font-mono text-[10px] tabular-nums">
                  {formatTimestamp(point.seconds)}
                </span>
                <span className="max-w-52 truncate font-medium">{point.label}</span>
              </button>
            );
          })}
        </div>
      )}

      {showTranscript && (
        <Observer>
          {() => {
          if (!segments || segments.length === 0) {
            return (
              <div className="mx-auto max-w-3xl px-5 py-8 sm:px-8 sm:py-10">
                <p className="whitespace-pre-wrap text-[15px] leading-7">{text}</p>
              </div>
            );
          }

        // The active line is the last segment whose start is at/under the playhead.
        let activeIndex = -1;
        for (let i = 0; i < segments.length; i += 1) {
          if (currentTime + 0.15 >= segments[i]!.start) activeIndex = i;
          else break;
        }

        // Keep the active line in view as playback advances. Gate on actual
        // playback so loading the page (currentTime 0 → first line active)
        // doesn't scroll-jack the transcript into view before the user presses
        // play, and only scroll when the active line changes.
        const followPlayhead = (el: HTMLButtonElement | null): void => {
          if (!el || lastScrolledIndex.current === activeIndex) return;
          lastScrolledIndex.current = activeIndex;
          const audio = audioRef.current;
          if (audio && !audio.paused) el.scrollIntoView({ block: "nearest" });
        };

          return (
            <div className="max-h-[42rem] overflow-y-auto px-3 py-5 sm:px-6 sm:py-8">
              <div className="mx-auto max-w-3xl space-y-1">
              {segments.map((segment, i) => (
                <button
                  key={i}
                  ref={i === activeIndex ? followPlayhead : undefined}
                  type="button"
                  onClick={() => seekTo(segment.start)}
                  className={cn(
                    "grid w-full grid-cols-[3.5rem_1fr] gap-3 rounded-lg px-3 py-2.5 text-left transition-colors hover:bg-muted/60 sm:grid-cols-[4.5rem_1fr]",
                    i === activeIndex && "bg-primary/10 text-foreground",
                  )}
                >
                  <span className="pt-1 font-mono text-[11px] tabular-nums text-muted-foreground">
                    {formatTimestamp(segment.start)}
                  </span>
                  <span className="min-w-0 text-[15px] leading-7">{segment.text}</span>
                </button>
              ))}
              </div>
            </div>
          );
          }}
        </Observer>
      )}
    </div>
  );
}

function formatTimestamp(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

interface TalkingPoint {
  seconds: number;
  label: string;
  description: string;
}

function extractTalkingPoints(artifacts: MeetingArtifact[]): TalkingPoint[] {
  const artifact = artifacts.find(
    (candidate) =>
      candidate.kind === "talking_points" &&
      candidate.status === "completed" &&
      candidate.content_markdown,
  );
  if (!artifact?.content_markdown) return [];

  const points: TalkingPoint[] = [];
  const linePattern =
    /^\s*[-*]\s+\[(?:(\d+):)?(\d{1,2}):(\d{2})\]\s+(.+?)(?:\s+[—–-]\s+(.+))?\s*$/gm;
  for (const match of artifact.content_markdown.matchAll(linePattern)) {
    const hours = Number(match[1] ?? 0);
    const minutes = Number(match[2]);
    const seconds = Number(match[3]);
    const label = match[4]?.trim();
    if (!label || minutes > 59 || seconds > 59) continue;
    points.push({
      seconds: hours * 3600 + minutes * 60 + seconds,
      label,
      description: match[5]?.trim() ?? "",
    });
  }

  return points.sort((a, b) => a.seconds - b.seconds);
}

function isTalkingPointActive(
  point: TalkingPoint,
  points: TalkingPoint[],
  currentTime: number,
): boolean {
  const index = points.indexOf(point);
  const next = points[index + 1];
  return currentTime >= point.seconds && (!next || currentTime < next.seconds);
}

function FileRow({ label, path }: { label: string; path: string }) {
  return (
    <div className="flex items-center justify-between gap-3 px-4 py-3">
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
  hasTimestamps,
}: {
  meetingId: number;
  canGenerate: boolean;
  hasTimestamps: boolean;
}) {
  const store = useStore();
  const artifacts = store.meetingArtifacts;
  const [templateId, setTemplateId] = useState("");
  const [agentProfileId, setAgentProfileId] = useState("");
  const [customContext, setCustomContext] = useState("");
  const [selectedArtifactId, setSelectedArtifactId] = useState<number | null>(null);

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
        const featuredTemplates = [
          "standard_meeting",
          "concise_summary",
          "action_items",
          "talking_points",
          "mind_map",
        ]
          .map((id) => templates.find((template) => template.id === id))
          .filter((template): template is SummaryTemplate => Boolean(template));
        const selectedArtifact =
          meetingArtifacts.find((artifact) => artifact.id === selectedArtifactId) ??
          meetingArtifacts[0];
        const selectedArtifactSnapshot = selectedArtifact
          ? { ...selectedArtifact }
          : undefined;
        const selectedTemplateNeedsTimestamps =
          selectedTemplate?.requires_timestamps === true;
        const generationDisabled =
          !canGenerate ||
          !selectedTemplateId ||
          isGenerating ||
          (selectedTemplateNeedsTimestamps && !hasTimestamps);

        const handleGenerate = async (): Promise<void> => {
          if (!selectedTemplateId) {
            toast.error("No summary template available");
            return;
          }
          const artifact = await artifacts.generateArtifact(meetingId, {
            template_id: selectedTemplateId,
            agent_profile_id: selectedProfileId ? Number(selectedProfileId) : null,
            custom_context: customContext.trim() || null,
          });
          if (!artifact) {
            toast.error(artifacts.lastError ?? "Could not generate summary");
            return;
          }
          if (artifact.status === "completed") {
            toast.success(`${selectedTemplate?.name ?? "Output"} generated`);
            setCustomContext("");
            setSelectedArtifactId(artifact.id);
          } else {
            toast.error(artifact.error ?? "Agent returned an error");
          }
        };

        return (
          <Card className="border-0 shadow-none">
            <CardHeader className="px-0 pt-0">
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div>
                  <CardTitle className="flex items-center gap-2">
                    <Sparkles className="h-4 w-4 text-primary" />
                    Meeting notes
                  </CardTitle>
                  <CardDescription>
                    Minutes, summaries, actions, chapters, and visual maps generated locally.
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
            <CardContent className="flex flex-col gap-5 px-0 pb-0">
              {!canGenerate && (
                <div className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
                  Generated views are available after the meeting is completed and a transcript exists.
                </div>
              )}

              <div className="border-t pt-6">
                <h3 className="text-sm font-semibold">Generate another view</h3>
                <p className="text-xs text-muted-foreground">
                  Choose the shape that best fits what you need next.
                </p>
              </div>

              <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
                {featuredTemplates.map((template) => {
                  const selected = template.id === selectedTemplateId;
                  const unavailable = template.requires_timestamps && !hasTimestamps;
                  return (
                    <button
                      key={template.id}
                      type="button"
                      className={cn(
                        "group min-h-32 rounded-xl border p-3 text-left transition-all",
                        selected
                          ? "border-primary/40 bg-primary/5 shadow-sm ring-1 ring-primary/15"
                          : "bg-background hover:-translate-y-0.5 hover:border-primary/25 hover:shadow-sm",
                        unavailable && "cursor-not-allowed opacity-50 hover:translate-y-0",
                      )}
                      onClick={() => !unavailable && setTemplateId(template.id)}
                      disabled={!canGenerate || isGenerating || unavailable}
                      title={
                        unavailable
                          ? "Timestamped transcript segments are required"
                          : template.description
                      }
                    >
                      <OutputKindIcon
                        kind={template.kind}
                        className={cn(
                          "mb-4 h-5 w-5",
                          selected ? "text-primary" : "text-muted-foreground",
                        )}
                      />
                      <span className="block text-sm font-semibold leading-tight">
                        {template.name}
                      </span>
                      <span className="mt-1.5 line-clamp-2 block text-[11px] leading-4 text-muted-foreground">
                        {unavailable ? "Needs timestamps" : template.description}
                      </span>
                    </button>
                  );
                })}
              </div>

              <div className="grid gap-3 md:grid-cols-[1fr_1fr_auto] md:items-end">
                <label className="space-y-1 text-sm">
                  <span className="font-medium">Format</span>
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
                  disabled={generationDisabled}
                >
                  {isGenerating ? (
                    <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <Bot className="mr-1 h-3.5 w-3.5" />
                  )}
                  Generate {selectedTemplate?.name ?? "view"}
                </Button>
              </div>

              {selectedTemplateNeedsTimestamps && !hasTimestamps && (
                <p className="text-xs text-muted-foreground">
                  Talking points need timestamped transcript segments. This recording only has plain text.
                </p>
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

              <div className="-order-1 space-y-3">
                <div className="flex items-center justify-between gap-2">
                  <h2 className="text-sm font-semibold">Saved views</h2>
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
                  <div className="overflow-hidden rounded-xl border bg-background">
                    <div
                      className="flex gap-1 overflow-x-auto border-b p-1.5"
                      role="tablist"
                      aria-label="Generated meeting views"
                    >
                      {meetingArtifacts.map((artifact) => {
                        const selected = artifact.id === selectedArtifact?.id;
                        return (
                          <button
                            key={artifact.id}
                            type="button"
                            role="tab"
                            aria-selected={selected}
                            onClick={() => setSelectedArtifactId(artifact.id)}
                            className={cn(
                              "flex shrink-0 items-center gap-2 rounded-lg px-3 py-2 text-xs font-medium transition-colors",
                              selected
                                ? "bg-primary/10 text-primary"
                                : "text-muted-foreground hover:bg-muted hover:text-foreground",
                            )}
                          >
                            <OutputKindIcon kind={artifact.kind} className="h-3.5 w-3.5" />
                            {artifactKindLabel(artifact.kind)}
                          </button>
                        );
                      })}
                    </div>
                    {selectedArtifactSnapshot && (
                      <ArtifactCard
                        artifact={selectedArtifactSnapshot}
                        onCopy={() => {
                          void copyArtifact(selectedArtifactSnapshot);
                        }}
                        onDelete={() => {
                          void deleteArtifact(
                            artifacts,
                            meetingId,
                            selectedArtifactSnapshot,
                          );
                          setSelectedArtifactId(null);
                        }}
                      />
                    )}
                  </div>
                ) : (
                  <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
                    No saved views yet. Choose what would make this recording useful next.
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

function ArtifactCard({
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
    <div className="space-y-5 p-4 sm:p-6">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <OutputKindIcon kind={artifact.kind} className="h-4 w-4 text-primary" />
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
        <ArtifactContent markdown={artifact.content_markdown ?? ""} />
      )}
    </div>
  );
}

function defaultProfileId(profiles: AgentProfile[]): string {
  const profile =
    profiles.find((p) => p.default_profile && p.enabled && p.available) ??
    profiles.find((p) => p.enabled && p.available) ??
    profiles.find((p) => p.enabled) ??
    profiles[0];
  return profile ? String(profile.id) : "";
}

function OutputKindIcon({
  kind,
  className,
}: {
  kind: string;
  className?: string;
}) {
  const Icon = (() => {
    switch (kind) {
      case "meeting_minutes":
        return ClipboardList;
      case "summary":
        return AlignLeft;
      case "action_items":
        return ListChecks;
      case "talking_points":
        return ListTree;
      case "mind_map":
        return BrainCircuit;
      default:
        return FileText;
    }
  })();
  return <Icon className={className} />;
}

function artifactKindLabel(kind: string): string {
  switch (kind) {
    case "meeting_minutes":
      return "Minutes";
    case "summary":
      return "Summary";
    case "action_items":
      return "Actions";
    case "talking_points":
      return "Talking points";
    case "mind_map":
      return "Mind map";
    default:
      return kind.replace(/_/g, " ");
  }
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
