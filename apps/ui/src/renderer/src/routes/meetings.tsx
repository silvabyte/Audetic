import { useState } from "react";
import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { Mic2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { MeetingRow } from "@/components/meeting-row";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

export const MEETING_INTENTS = {
  start: "start-meeting",
  stop: "stop-meeting",
  cancel: "cancel-meeting",
} as const;

/**
 * /meetings — list + start dialog.
 *
 * Loader kicks off the list fetch (idempotent). The banner on the
 * AppShell handles in-flight meetings; this page is for "what's in
 * the backlog" and "start a new one".
 *
 * Action owns toast side-effects for the three intents. Store methods
 * set `lastError` on failure; we diff it pre/post to decide whether
 * to fire a toast, without putting toast calls inside the store
 * itself (keeps stores framework-agnostic per feedback_mobx.md).
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
        const errBefore = root.meetings.lastError;
        await root.meetings.startMeeting(title);
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't start meeting", { description: errAfter });
        }
        return null;
      }
      case MEETING_INTENTS.stop: {
        const errBefore = root.meetings.lastError;
        await root.meetings.stopMeeting();
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't stop meeting", { description: errAfter });
        }
        return null;
      }
      case MEETING_INTENTS.cancel: {
        const errBefore = root.meetings.lastError;
        await root.meetings.cancelMeeting();
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't cancel meeting", { description: errAfter });
        } else {
          toast.success("Meeting cancelled");
        }
        return null;
      }
      default:
        return null;
    }
  },
  Component: MeetingsRoute,
};

function MeetingsRoute() {
  return (
    <div className="mx-auto max-w-3xl p-8 space-y-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-semibold">Meetings</h1>
          <p className="text-sm text-muted-foreground">
            Long-form recordings. Press{" "}
            <kbd className="rounded border px-1.5 py-0.5 font-mono text-xs">
              Super+Shift+R
            </kbd>{" "}
            to toggle via hotkey.
          </p>
        </div>
        <StartMeetingButton />
      </header>

      <MeetingList />
    </div>
  );
}

function StartMeetingButton() {
  const store = useStore();
  const fetcher = useFetcher();
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const submitting = fetcher.state !== "idle";

  // Close dialog once a successful submit resolves. fetcher.data stays
  // null (our actions return null) so we key off idle + previously submit.
  // Simple approach: reset + close on submit happening.
  function handleSubmit(e: React.FormEvent<HTMLFormElement>): void {
    e.preventDefault();
    const formData = new FormData(e.currentTarget);
    fetcher.submit(formData, { method: "post", action: "/meetings" });
    setOpen(false);
    setTitle("");
  }

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
          <Dialog open={open} onOpenChange={setOpen}>
            <DialogTrigger asChild>
              <Button disabled={inProgress || submitting}>
                <Mic2 className="mr-2 h-4 w-4" />
                New meeting
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Start a meeting</DialogTitle>
                <DialogDescription>
                  Optional title helps find it later. Transcription runs
                  after you press Stop.
                </DialogDescription>
              </DialogHeader>
              <form onSubmit={handleSubmit} className="space-y-4">
                <input
                  type="hidden"
                  name="intent"
                  value={MEETING_INTENTS.start}
                />
                <div className="space-y-2">
                  <Label htmlFor="meeting-title">Title</Label>
                  <Input
                    id="meeting-title"
                    name="title"
                    type="text"
                    placeholder="e.g. Design sync with Alex"
                    value={title}
                    onChange={(e) => setTitle(e.target.value)}
                    autoFocus
                    autoComplete="off"
                  />
                </div>
                <DialogFooter>
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => setOpen(false)}
                  >
                    Cancel
                  </Button>
                  <Button type="submit" disabled={submitting}>
                    <Mic2 className="mr-2 h-4 w-4" />
                    Start
                  </Button>
                </DialogFooter>
              </form>
            </DialogContent>
          </Dialog>
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
          if (
            store.meetings.listStatus === "loading" &&
            store.meetings.list.length === 0
          ) {
            return <MeetingListSkeleton />;
          }
          if (store.meetings.listError) {
            return (
              <Card>
                <CardHeader>
                  <CardTitle className="text-base text-destructive">
                    Couldn't load meetings
                  </CardTitle>
                  <CardDescription>{store.meetings.listError}</CardDescription>
                </CardHeader>
              </Card>
            );
          }
          if (store.meetings.list.length === 0) {
            return (
              <Card>
                <CardContent className="p-6 text-sm text-muted-foreground">
                  No meetings yet. Start one above or press Super+Shift+R.
                </CardContent>
              </Card>
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

function MeetingListSkeleton() {
  return (
    <ul className="space-y-3">
      {[0, 1, 2].map((i) => (
        <li key={i}>
          <Card>
            <CardContent className="p-4 flex items-center gap-4">
              <Skeleton className="h-5 w-5 rounded-full" />
              <div className="min-w-0 flex-1 space-y-2">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-3 w-60" />
              </div>
              <Skeleton className="h-5 w-20 rounded-full" />
            </CardContent>
          </Card>
        </li>
      ))}
    </ul>
  );
}
