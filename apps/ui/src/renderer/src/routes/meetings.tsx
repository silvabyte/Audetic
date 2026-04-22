import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { Mic2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { MeetingRow } from "@/components/meeting-row";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

export const MEETING_INTENTS = {
  start: "start-meeting",
  stop: "stop-meeting",
  cancel: "cancel-meeting",
} as const;

/**
 * /meetings — list + start form.
 *
 * Loader kicks off the list fetch (idempotent). The banner on the
 * AppShell handles in-flight meetings; this page is for "what's in
 * the backlog" and "start a new one".
 */
export const meetingsRoute: RouteObject = {
  path: "meetings",
  loader: async () => {
    await getRootStore().meetings.loadList();
    return null;
  },
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    switch (intent) {
      case MEETING_INTENTS.start: {
        const title = String(form.get("title") ?? "").trim() || undefined;
        await root.meetings.startMeeting(title);
        return null;
      }
      case MEETING_INTENTS.stop:
        await root.meetings.stopMeeting();
        return null;
      case MEETING_INTENTS.cancel:
        await root.meetings.cancelMeeting();
        return null;
      default:
        return null;
    }
  },
  Component: MeetingsRoute,
};

function MeetingsRoute() {
  return (
    <div className="mx-auto max-w-3xl p-8 space-y-6">
      <header>
        <h1 className="text-2xl font-semibold">Meetings</h1>
        <p className="text-sm text-muted-foreground">
          Long-form recordings. Press{" "}
          <kbd className="rounded border px-1.5 py-0.5 font-mono text-xs">
            Super+Shift+R
          </kbd>{" "}
          to toggle via hotkey.
        </p>
      </header>

      <StartMeetingCard />
      <MeetingList />
    </div>
  );
}

function StartMeetingCard() {
  const store = useStore();
  const fetcher = useFetcher();
  const submitting = fetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const { active, phase } = store.meetings;
        const inProgress =
          active ||
          phase === "compressing" ||
          phase === "transcribing" ||
          phase === "running_hook";

        return (
          <Card>
            <CardHeader>
              <CardTitle>Start a meeting</CardTitle>
              <CardDescription>
                {inProgress
                  ? "A meeting is currently in progress. Use the banner above to stop or cancel it."
                  : "Optional title helps find it later. Transcription runs after Stop."}
              </CardDescription>
            </CardHeader>
            <CardContent>
              <fetcher.Form method="post" className="flex gap-2">
                <input type="hidden" name="intent" value={MEETING_INTENTS.start} />
                <Input
                  name="title"
                  type="text"
                  placeholder="Title (optional)"
                  disabled={inProgress || submitting}
                  defaultValue=""
                  autoComplete="off"
                />
                <Button type="submit" disabled={inProgress || submitting}>
                  <Mic2 className="mr-2 h-4 w-4" />
                  {submitting ? "Starting…" : "Start"}
                </Button>
              </fetcher.Form>
            </CardContent>
          </Card>
        );
      }}
    </Observer>
  );
}

function MeetingList() {
  const store = useStore();
  return (
    <section className="space-y-3">
      <Observer>
        {() => {
          if (store.meetings.listStatus === "loading" && store.meetings.list.length === 0) {
            return <p className="text-sm text-muted-foreground">Loading…</p>;
          }
          if (store.meetings.listError) {
            return (
              <p className="text-sm text-destructive">
                {store.meetings.listError}
              </p>
            );
          }
          if (store.meetings.list.length === 0) {
            return (
              <p className="text-sm text-muted-foreground">
                No meetings yet. Start one above or press Super+Shift+R.
              </p>
            );
          }
          return (
            <ul className="space-y-3">
              {store.meetings.list.map((m) => (
                <li key={m.id}>
                  <MeetingRow meeting={m} />
                </li>
              ))}
            </ul>
          );
        }}
      </Observer>
    </section>
  );
}
