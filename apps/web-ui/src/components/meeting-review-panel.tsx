import { useEffect, useMemo, useRef, useState } from "react";
import { useFetcher } from "react-router-dom";
import { Scissors, Send, XCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { MEETING_INTENTS } from "@/routes/meetings";

/**
 * Shown when a stopped meeting is parked in the `review` phase. Lets the user
 * play the recording back and trim the start/end before sending it for
 * transcription — the fix for "I forgot to stop and recorded 20 minutes of
 * nothing". Boundaries are sent as float seconds so the daemon trims the
 * lossless WAV sample-accurately.
 *
 * Rendered as a full-width sticky banner section by ActiveMeetingBanner.
 */
export function MeetingReviewPanel({
  meetingId,
  durationSeconds,
  title,
}: {
  meetingId: number;
  durationSeconds: number;
  title: string | null;
}) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const [startSec, setStartSec] = useState(0);
  const [endSec, setEndSec] = useState(durationSeconds);

  const confirmFetcher = useFetcher();
  const cancelFetcher = useFetcher();
  const sending = confirmFetcher.state !== "idle";
  const discarding = cancelFetcher.state !== "idle";

  // The recording's URL is served same-origin by the daemon under /api.
  const audioSrc = `/api/meetings/${meetingId}/audio`;

  const trimmed = startSec > 0.001 || endSec < durationSeconds - 0.001;
  const valid = startSec >= 0 && endSec > startSec && endSec <= durationSeconds + 0.5;
  const resultLen = Math.max(0, endSec - startSec);

  // Only send a bound when it actually moves that edge — otherwise omit it so
  // the daemon keeps the original start/end (and skips a pointless rewrite).
  const startField = startSec > 0.001 ? startSec.toFixed(3) : "";
  const endField =
    endSec < durationSeconds - 0.001 ? endSec.toFixed(3) : "";

  function setStartToPlayhead(): void {
    const t = audioRef.current?.currentTime;
    if (typeof t === "number") setStartSec(clamp(t, 0, endSec));
  }

  function setEndToPlayhead(): void {
    const t = audioRef.current?.currentTime;
    if (typeof t === "number") setEndSec(clamp(t, startSec, durationSeconds));
  }

  return (
    <div className="w-full border-b border-primary/30 bg-primary/5">
      <div className="mx-auto max-w-5xl px-4 py-3 space-y-3">
        <div className="flex items-center gap-2">
          <Scissors className="h-4 w-4 text-primary" />
          <div className="text-sm">
            <span className="font-medium">{title ?? "Untitled meeting"}</span>
            <span className="text-muted-foreground">
              {" "}
              · Review before transcribing
            </span>
          </div>
        </div>

        <audio
          ref={audioRef}
          src={audioSrc}
          controls
          preload="metadata"
          className="w-full"
        />

        <div className="flex flex-wrap items-end gap-4">
          <ClockField
            id="trim-start"
            label="Start"
            seconds={startSec}
            max={durationSeconds}
            onChange={(s) => setStartSec(clamp(s, 0, endSec))}
            onUsePlayhead={setStartToPlayhead}
          />
          <ClockField
            id="trim-end"
            label="End"
            seconds={endSec}
            max={durationSeconds}
            onChange={(s) => setEndSec(clamp(s, startSec, durationSeconds))}
            onUsePlayhead={setEndToPlayhead}
          />

          <div className="text-xs text-muted-foreground pb-2">
            {trimmed ? (
              <>
                Trimmed length:{" "}
                <span className="font-mono">{formatClock(resultLen)}</span>
                {" of "}
                <span className="font-mono">{formatClock(durationSeconds)}</span>
              </>
            ) : (
              <>
                Full recording:{" "}
                <span className="font-mono">{formatClock(durationSeconds)}</span>
              </>
            )}
          </div>

          <div className="ml-auto flex items-center gap-2 pb-1">
            <cancelFetcher.Form method="post" action="/meetings">
              <input type="hidden" name="intent" value={MEETING_INTENTS.cancel} />
              <Button
                type="submit"
                variant="outline"
                size="sm"
                disabled={discarding || sending}
              >
                <XCircle className="mr-1 h-3.5 w-3.5" />
                {discarding ? "Discarding…" : "Discard"}
              </Button>
            </cancelFetcher.Form>

            <confirmFetcher.Form method="post" action="/meetings">
              <input
                type="hidden"
                name="intent"
                value={MEETING_INTENTS.confirm}
              />
              <input type="hidden" name="start_seconds" value={startField} />
              <input type="hidden" name="end_seconds" value={endField} />
              <Button type="submit" size="sm" disabled={!valid || sending || discarding}>
                <Send className="mr-1 h-3.5 w-3.5" />
                {sending
                  ? "Sending…"
                  : trimmed
                    ? "Trim & transcribe"
                    : "Send for transcription"}
              </Button>
            </confirmFetcher.Form>
          </div>
        </div>

        {!valid && (
          <p className="text-xs text-destructive">
            Start must be before end, within the recording.
          </p>
        )}
      </div>
    </div>
  );
}

/**
 * A single mm:ss trim bound: a text field (parsed on blur) plus a "Use
 * playhead" button that snaps the bound to the audio element's current time.
 */
function ClockField({
  id,
  label,
  seconds,
  max,
  onChange,
  onUsePlayhead,
}: {
  id: string;
  label: string;
  seconds: number;
  max: number;
  onChange: (seconds: number) => void;
  onUsePlayhead: () => void;
}) {
  // Local text mirror so the user can type freely; commit on blur/Enter.
  const [text, setText] = useState(() => formatClock(seconds));
  // Re-sync when the value changes from outside (e.g. "Use playhead").
  const formatted = useMemo(() => formatClock(seconds), [seconds]);
  useEffect(() => setText(formatted), [formatted]);

  function commit(): void {
    const parsed = parseClock(text);
    if (parsed != null) onChange(clamp(parsed, 0, max));
    else setText(formatted); // revert unparseable input
  }

  return (
    <div className="space-y-1">
      <Label htmlFor={id} className="text-xs">
        {label}
      </Label>
      <div className="flex items-center gap-1">
        <Input
          id={id}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commit();
            }
          }}
          inputMode="numeric"
          className="h-8 w-24 font-mono"
        />
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-8 px-2 text-xs"
          onClick={onUsePlayhead}
          title="Set to the current playback position"
        >
          Use playhead
        </Button>
      </div>
    </div>
  );
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

/** Format seconds as `M:SS` (or `H:MM:SS` past an hour). */
function formatClock(totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds));
  const hours = Math.floor(s / 3600);
  const mins = Math.floor((s % 3600) / 60);
  const secs = s % 60;
  const pad = (n: number) => n.toString().padStart(2, "0");
  return hours > 0
    ? `${hours}:${pad(mins)}:${pad(secs)}`
    : `${mins}:${pad(secs)}`;
}

/**
 * Parse `SS`, `M:SS`, or `H:MM:SS` (fractional seconds allowed) into a number
 * of seconds. Returns null on unparseable input.
 */
function parseClock(raw: string): number | null {
  const s = raw.trim();
  if (s === "") return null;
  const parts = s.split(":");
  if (parts.length > 3) return null;
  let seconds = 0;
  for (const part of parts) {
    const n = Number(part);
    if (!Number.isFinite(n) || n < 0) return null;
    seconds = seconds * 60 + n;
  }
  return seconds;
}

// MeetingReviewPanel is the default export consumed by the banner.
export default MeetingReviewPanel;
