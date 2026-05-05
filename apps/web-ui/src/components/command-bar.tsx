import { Observer } from "mobx-react-lite";
import { Link, useFetcher } from "react-router-dom";
import { Mic, Mic2, StopCircle, WifiOff } from "lucide-react";
import { Button, buttonVariants } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { StatusPill } from "@/components/status-pill";
import { ActiveMeetingBanner } from "@/components/active-meeting-banner";
import { useStore } from "@/stores/root-store";
import { DICTATIONS_INTENTS } from "@/routes/dictations";

/**
 * Omnipresent command bar — sticky strip at the top of the app shell.
 * Shows current dictation phase + start/stop and a meeting entry point.
 *
 * Subsumes <ActiveMeetingBanner /> by rendering it inline below the
 * strip. The banner self-hides at idle, so the bar collapses to a single
 * row most of the time.
 *
 * Posts the dictation toggle to `/dictations` (action="/dictations") —
 * the dictations route action knows how to toggle the recording
 * pipeline. The post works from any view; react-router accepts absolute
 * paths on fetcher form actions.
 */
export function CommandBar() {
  return (
    <header className="sticky top-0 z-20 w-full border-b bg-background/80 backdrop-blur supports-[backdrop-filter]:bg-background/60">
      <div className="mx-auto flex w-full max-w-5xl items-center gap-3 px-4 py-2">
        <div className="text-sm font-semibold">Audetic</div>
        <StatusPill />
        <DaemonReachabilityChip />
        <div className="flex-1" />
        <DictationToggleButton />
        <MeetingEntryButton />
      </div>
      <ActiveMeetingBanner />
    </header>
  );
}

function DaemonReachabilityChip() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        if (store.daemonReachable) return null;
        return (
          <Tooltip>
            <TooltipTrigger asChild>
              <span className="inline-flex items-center gap-1 rounded-full bg-destructive/10 px-2 py-0.5 text-xs text-destructive">
                <WifiOff className="h-3 w-3" />
                Daemon offline
              </span>
            </TooltipTrigger>
            <TooltipContent>
              No response from 127.0.0.1:3737. Start the daemon to continue.
            </TooltipContent>
          </Tooltip>
        );
      }}
    </Observer>
  );
}

function DictationToggleButton() {
  const store = useStore();
  const fetcher = useFetcher();
  const submitting = fetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const busy = store.status.isBusy;
        const meetingLive = store.meetings.active;
        // Dictation and meeting share the audio pipeline — disable the
        // dictation toggle while a meeting is recording so the user
        // can't accidentally interleave them.
        const disabled = submitting || meetingLive;
        return (
          <fetcher.Form method="post" action="/dictations">
            <input
              type="hidden"
              name="intent"
              value={DICTATIONS_INTENTS.toggle}
            />
            <Button
              type="submit"
              size="sm"
              variant={busy ? "destructive" : "default"}
              disabled={disabled}
            >
              {busy ? (
                <>
                  <StopCircle className="mr-1 h-3.5 w-3.5" />
                  {submitting ? "Stopping…" : "Stop dictation"}
                </>
              ) : (
                <>
                  <Mic className="mr-1 h-3.5 w-3.5" />
                  {submitting ? "Starting…" : "Start dictation"}
                </>
              )}
            </Button>
          </fetcher.Form>
        );
      }}
    </Observer>
  );
}

function MeetingEntryButton() {
  const store = useStore();
  return (
    <Observer>
      {() => {
        // While a meeting is live the ActiveMeetingBanner (rendered
        // directly below) provides Stop/Cancel — no point duplicating
        // here. Hide the entry button so users aren't tempted.
        if (store.meetings.active) return null;
        return (
          <Link
            to="/meetings"
            className={buttonVariants({ variant: "outline", size: "sm" })}
          >
            <Mic2 className="mr-1 h-3.5 w-3.5" />
            New meeting
          </Link>
        );
      }}
    </Observer>
  );
}
