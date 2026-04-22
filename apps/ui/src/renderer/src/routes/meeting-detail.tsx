import { Observer } from "mobx-react-lite";
import {
  Form,
  NavLink,
  useParams,
  type ActionFunctionArgs,
  type LoaderFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { ArrowLeft, Copy, FolderOpen } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import type { MeetingDetail } from "@/stores/meeting-store";

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
        if (text) await navigator.clipboard.writeText(text);
        return null;
      }
      case DETAIL_INTENTS.openAudio: {
        // Open the enclosing directory in the user's file manager. In
        // Electron this is available through the preload bridge; for
        // Phase 3 we skip the main-process IPC and just copy the path.
        const path = String(form.get("path") ?? "");
        if (path) await navigator.clipboard.writeText(path);
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

  return (
    <div className="mx-auto max-w-3xl p-8 space-y-6">
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
            return <p className="text-sm text-muted-foreground">Loading…</p>;
          }
          return <MeetingDetailBody detail={detail} />;
        }}
      </Observer>
    </div>
  );
}

function MeetingDetailBody({ detail }: { detail: MeetingDetail }) {
  return (
    <>
      <header>
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
      </header>

      {detail.error && (
        <Card className="border-destructive/40">
          <CardHeader>
            <CardTitle className="text-destructive">Error</CardTitle>
          </CardHeader>
          <CardContent className="text-sm whitespace-pre-wrap">
            {detail.error}
          </CardContent>
        </Card>
      )}

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
                <Button type="submit" variant="outline" size="sm">
                  <Copy className="mr-1 h-3.5 w-3.5" />
                  Copy all
                </Button>
              </Form>
            </>
          ) : null}
        </CardContent>
      </Card>

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
}

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
        <Button type="submit" variant="ghost" size="sm" title="Copy path">
          <FolderOpen className="h-3.5 w-3.5" />
        </Button>
      </Form>
    </div>
  );
}

function formatDuration(seconds: number): string {
  const mins = Math.floor(seconds / 60);
  const secs = seconds % 60;
  return `${mins}m ${secs.toString().padStart(2, "0")}s`;
}

