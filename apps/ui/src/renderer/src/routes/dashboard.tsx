import { Observer } from "mobx-react-lite";
import {
  Form,
  Link,
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { Copy, Mic, Mic2, StopCircle } from "lucide-react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { StatusPill } from "@/components/status-pill";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

const INTENT = {
  toggleRecording: "toggle-recording",
  copyLastTranscription: "copy-last-transcription",
} as const;

/**
 * Dashboard route — `/`.
 *
 * Loader is a thin bridge: kicks off `meta.prefetch()` (idempotent) so
 * version + provider are available for the footer. Returns null; the
 * component reads from MobX stores via `<Observer>` — no
 * `useLoaderData()`.
 *
 * Action dispatches named intents to store methods. `useFetcher` owns
 * the in-flight UI state; stores own the truth.
 */
export const dashboardRoute: RouteObject = {
  index: true,
  loader: async () => {
    await getRootStore().meta.prefetch();
    return null;
  },
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    switch (intent) {
      case INTENT.toggleRecording:
        await root.status.toggle();
        return null;
      case INTENT.copyLastTranscription: {
        const job = root.status.lastCompletedJob;
        if (job) await navigator.clipboard.writeText(job.text);
        return null;
      }
      default:
        return null;
    }
  },
  Component: Dashboard,
};

function Dashboard() {
  return (
    <div className="mx-auto max-w-3xl p-8 space-y-6">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Dashboard</h1>
          <p className="text-sm text-muted-foreground">
            Press{" "}
            <kbd className="rounded border px-1.5 py-0.5 font-mono text-xs">
              Super+R
            </kbd>{" "}
            to dictate, or use the buttons below.
          </p>
        </div>
        <StatusPill />
      </header>

      <RecordingCard />
      <LastTranscriptionCard />
      <Footer />
    </div>
  );
}

function RecordingCard() {
  const store = useStore();
  const fetcher = useFetcher();
  const submitting = fetcher.state !== "idle";

  return (
    <Card>
      <CardHeader>
        <CardTitle>Recording</CardTitle>
        <CardDescription>
          Toggle the dictation pipeline. Status polls {"<"}1s while active.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <fetcher.Form method="post">
          <input
            type="hidden"
            name="intent"
            value={INTENT.toggleRecording}
          />
          <Observer>
            {() => {
              const busy = store.status.isBusy;
              return (
                <Button
                  type="submit"
                  size="lg"
                  variant={busy ? "destructive" : "default"}
                  disabled={submitting}
                >
                  {busy ? (
                    <>
                      <StopCircle className="mr-2 h-4 w-4" />
                      {submitting ? "Stopping…" : "Stop recording"}
                    </>
                  ) : (
                    <>
                      <Mic className="mr-2 h-4 w-4" />
                      {submitting ? "Starting…" : "Start recording"}
                    </>
                  )}
                </Button>
              );
            }}
          </Observer>
        </fetcher.Form>

        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <span>Need a long-form recording?</span>
          <Link
            to="/meetings"
            className="inline-flex items-center gap-1 font-medium text-foreground hover:underline"
          >
            <Mic2 className="h-3.5 w-3.5" />
            Start a meeting
          </Link>
        </div>

        <Observer>
          {() => {
            const err = store.status.lastError;
            if (!err) return null;
            return (
              <div className="text-sm text-destructive">Last error: {err}</div>
            );
          }}
        </Observer>
      </CardContent>
    </Card>
  );
}

function LastTranscriptionCard() {
  const store = useStore();

  return (
    <Card>
      <CardHeader>
        <CardTitle>Last transcription</CardTitle>
      </CardHeader>
      <CardContent>
        <Observer>
          {() => {
            const job = store.status.lastCompletedJob;
            if (!job) {
              return (
                <p className="text-sm text-muted-foreground">
                  No transcriptions yet. Record something to see it here.
                </p>
              );
            }
            return (
              <div className="space-y-3">
                <p className="text-sm whitespace-pre-wrap">{job.text}</p>
                <div className="flex items-center justify-between text-xs text-muted-foreground">
                  <span>{new Date(job.created_at).toLocaleString()}</span>
                  <Form method="post" replace>
                    <input
                      type="hidden"
                      name="intent"
                      value={INTENT.copyLastTranscription}
                    />
                    <Button variant="ghost" size="sm" type="submit">
                      <Copy className="mr-1 h-3.5 w-3.5" />
                      Copy
                    </Button>
                  </Form>
                </div>
              </div>
            );
          }}
        </Observer>
      </CardContent>
    </Card>
  );
}

function Footer() {
  const store = useStore();
  return (
    <footer className="flex items-center justify-between pt-2 text-xs text-muted-foreground">
      <Observer>
        {() => (
          <span>
            audetic
            {store.meta.version ? ` v${store.meta.version}` : ""}
            {store.meta.providerName ? ` · ${store.meta.providerName}` : ""}
          </span>
        )}
      </Observer>
      <span>127.0.0.1:3737</span>
    </footer>
  );
}
