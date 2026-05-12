import { Observer } from "mobx-react-lite";
import { useFetcher } from "react-router-dom";
import { Mic, Radio, Square, WifiOff } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { StateOrb } from "@/components/state-orb";
import { ActiveMeetingBanner } from "@/components/active-meeting-banner";
import { useStore } from "@/stores/root-store";
import { DICTATIONS_INTENTS } from "@/routes/dictations";
import { MEETING_INTENTS } from "@/routes/meetings";

/**
 * Omnipresent command bar — sticky strip at the top of the app shell.
 *
 * The Audetic icon on the left is the primary state indicator: it
 * pulses, glows, and shifts color based on the live dictation /
 * meeting / pipeline state coming from the daemon. The two icon
 * actions on the right toggle dictation and meeting directly — no
 * navigation required.
 *
 * <ActiveMeetingBanner /> still renders below for meeting-only
 * affordances (capture-state, phase ribbon, Cancel).
 */
export function CommandBar() {
  return (
    <header className="sticky top-0 z-20 w-full border-b bg-background/80 backdrop-blur supports-[backdrop-filter]:bg-background/60">
      <div className="mx-auto flex w-full max-w-5xl items-center gap-3 px-4 py-2">
        <StateOrb />
        <DaemonReachabilityChip />
        <div className="flex-1" />
        <DictationToggleButton />
        <MeetingToggleButton />
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
        const phase = store.status.phase;
        const recording = phase === "recording";
        const processing = phase === "processing";
        const meetingLive = store.meetings.active;
        const meetingPipeline =
          store.meetings.phase === "compressing" ||
          store.meetings.phase === "transcribing" ||
          store.meetings.phase === "running_hook";
        const reachable = store.daemonReachable;

        // Mid-pipeline (processing dictation, or any meeting state) =
        // not actionable. Show a spinner, disable, and explain why.
        // Dictation and meeting share the audio pipeline.
        const inFlight = processing;
        const blockedByMeeting = meetingLive || meetingPipeline;
        const disabled =
          submitting || inFlight || blockedByMeeting || !reachable;

        let Icon = Mic;
        let variant: "ghost" | "destructive" = "ghost";
        let label = submitting
          ? "Starting dictation…"
          : "Start dictation (Super+R)";

        if (recording) {
          Icon = Square;
          variant = "destructive";
          label = submitting ? "Stopping dictation…" : "Stop dictation";
        } else if (processing) {
          // Orb already shows the spinning pipeline state — keep the
          // button inert with the resting icon so we don't double up.
          label = "Transcribing dictation…";
        } else if (meetingLive) {
          label = "Meeting in progress — dictation unavailable";
        } else if (meetingPipeline) {
          label = "Meeting pipeline running — dictation unavailable";
        }

        return (
          <Tooltip>
            <TooltipTrigger asChild>
              <fetcher.Form method="post" action="/dictations">
                <input
                  type="hidden"
                  name="intent"
                  value={DICTATIONS_INTENTS.toggle}
                />
                <Button
                  type="submit"
                  size="icon"
                  variant={variant}
                  disabled={disabled}
                  aria-label={label}
                  className="h-9 w-9 rounded-full"
                >
                  <Icon
                    className="h-4 w-4"
                    {...(recording ? { fill: "currentColor" } : {})}
                  />
                </Button>
              </fetcher.Form>
            </TooltipTrigger>
            <TooltipContent side="bottom">{label}</TooltipContent>
          </Tooltip>
        );
      }}
    </Observer>
  );
}

function MeetingToggleButton() {
  const store = useStore();
  const fetcher = useFetcher();
  const submitting = fetcher.state !== "idle";

  return (
    <Observer>
      {() => {
        const meetingLive = store.meetings.active;
        const meetingPipeline =
          store.meetings.phase === "compressing" ||
          store.meetings.phase === "transcribing" ||
          store.meetings.phase === "running_hook";
        const dictationBusy = store.status.isBusy;
        const reachable = store.daemonReachable;

        // Mid-pipeline = not actionable; show a spinner. Otherwise
        // toggle between start (ghost + Radio) and stop (default +
        // Square). Dictation in any busy phase blocks meeting start.
        const inFlight = meetingPipeline;
        const blockedByDictation = !meetingLive && dictationBusy;
        const disabled =
          submitting || inFlight || blockedByDictation || !reachable;

        let Icon = Radio;
        let variant: "ghost" | "default" = "ghost";
        let intent: string = MEETING_INTENTS.start;
        let label = submitting
          ? "Starting meeting…"
          : "Start meeting (Super+Shift+R)";

        if (meetingLive) {
          Icon = Square;
          variant = "default";
          intent = MEETING_INTENTS.stop;
          label = submitting ? "Stopping meeting…" : "Stop meeting";
        } else if (meetingPipeline) {
          // Orb spins for the pipeline; button stays inert.
          label = `Meeting ${meetingPhaseVerb(store.meetings.phase)}…`;
        } else if (dictationBusy) {
          label = "Dictation in progress — meeting unavailable";
        }

        return (
          <Tooltip>
            <TooltipTrigger asChild>
              <fetcher.Form method="post" action="/meetings">
                <input type="hidden" name="intent" value={intent} />
                <Button
                  type="submit"
                  size="icon"
                  variant={variant}
                  disabled={disabled}
                  aria-label={label}
                  className="h-9 w-9 rounded-full"
                >
                  <Icon
                    className="h-4 w-4"
                    {...(meetingLive ? { fill: "currentColor" } : {})}
                  />
                </Button>
              </fetcher.Form>
            </TooltipTrigger>
            <TooltipContent side="bottom">{label}</TooltipContent>
          </Tooltip>
        );
      }}
    </Observer>
  );
}

function meetingPhaseVerb(phase: string): string {
  switch (phase) {
    case "compressing":
      return "compressing";
    case "transcribing":
      return "transcribing";
    case "running_hook":
      return "running hook";
    default:
      return "working";
  }
}
