import { useState, type RefObject } from "react";
import { FastForward, Pause, Play, Rewind } from "lucide-react";
import { audioNoteAudioUrl } from "@/api/client";
import { Button } from "@/components/ui/button";
import { formatDuration } from "@/lib/audio-notes";
import type { TalkingPoint } from "@/lib/talking-points";

export function AudioTransport({
  noteId,
  audioRef,
  currentTime,
  durationHint,
  markers = [],
  onTimeChange,
}: {
  noteId: number;
  audioRef: RefObject<HTMLAudioElement | null>;
  currentTime: number;
  durationHint?: number | null;
  markers?: readonly TalkingPoint[];
  onTimeChange: (seconds: number) => void;
}) {
  const [playing, setPlaying] = useState(false);
  const [duration, setDuration] = useState(durationHint ?? 0);
  const [speed, setSpeed] = useState(1);
  const [audioError, setAudioError] = useState(false);
  const boundedDuration = Number.isFinite(duration) && duration > 0 ? duration : durationHint ?? 0;

  function seek(seconds: number): void {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = Math.max(0, Math.min(seconds, boundedDuration || seconds));
    onTimeChange(audio.currentTime);
  }

  async function togglePlayback(): Promise<void> {
    const audio = audioRef.current;
    if (!audio) return;
    try {
      if (audio.paused) await audio.play();
      else audio.pause();
    } catch {
      setAudioError(true);
    }
  }

  return <section aria-label="Audio transport" className="border-y bg-background/95 py-4 backdrop-blur supports-[backdrop-filter]:bg-background/85">
    <audio
      key={noteId}
      ref={audioRef}
      preload="metadata"
      src={audioNoteAudioUrl(noteId)}
      onLoadedMetadata={(event) => setDuration(Number.isFinite(event.currentTarget.duration) ? event.currentTarget.duration : durationHint ?? 0)}
      onTimeUpdate={(event) => onTimeChange(event.currentTarget.currentTime)}
      onPlay={() => setPlaying(true)}
      onPause={() => setPlaying(false)}
      onEnded={() => setPlaying(false)}
      onError={() => setAudioError(true)}
      onCanPlay={() => setAudioError(false)}
    />
    <div className="flex flex-wrap items-center gap-2 sm:flex-nowrap sm:gap-3">
      <Button type="button" variant="ghost" size="icon" aria-label="Rewind 10 seconds" onClick={() => seek(currentTime - 10)}><Rewind /></Button>
      <Button type="button" variant="outline" size="icon" aria-label={playing ? "Pause" : "Play"} onClick={() => void togglePlayback()}>{playing ? <Pause /> : <Play />}</Button>
      <Button type="button" variant="ghost" size="icon" aria-label="Forward 10 seconds" onClick={() => seek(currentTime + 10)}><FastForward /></Button>
      <span className="w-24 shrink-0 text-center font-mono text-xs tabular-nums text-muted-foreground">{formatDuration(currentTime)} / {formatDuration(boundedDuration)}</span>
      <div className="relative order-last basis-full py-2 sm:order-none sm:min-w-20 sm:flex-1 sm:basis-auto">
        <input
          type="range"
          min={0}
          max={Math.max(boundedDuration, 0)}
          step={0.1}
          value={Math.min(currentTime, boundedDuration || 0)}
          onChange={(event) => seek(Number(event.target.value))}
          aria-label="Playback position"
          className="block w-full accent-foreground"
        />
        {boundedDuration > 0 ? markers.map((marker, index) => {
          const next = markers[index + 1];
          const active = currentTime >= marker.seconds && (!next || currentTime < next.seconds);
          return <button
            key={marker.seconds}
            type="button"
            aria-label={`Jump to ${marker.label} at ${marker.timestamp}`}
            aria-current={active ? "true" : undefined}
            title={`${marker.timestamp} ${marker.label}`}
            onClick={() => seek(marker.seconds)}
            className="absolute top-1/2 z-10 flex h-5 w-3 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            style={{ left: `${Math.min((marker.seconds / boundedDuration) * 100, 100)}%` }}
          ><span className={active ? "h-3.5 w-1 rounded-full bg-foreground" : "h-2.5 w-0.5 bg-foreground"} /></button>;
        }) : null}
      </div>
      <label className="flex shrink-0 items-center gap-2 text-xs text-muted-foreground">
        <span className="sr-only sm:not-sr-only">Speed</span>
        <select
          aria-label="Playback speed"
          value={speed}
          onChange={(event) => {
            const next = Number(event.target.value);
            setSpeed(next);
            if (audioRef.current) audioRef.current.playbackRate = next;
          }}
          className="h-8 rounded-md border bg-background px-2 text-xs text-foreground"
        >
          {[0.75, 1, 1.25, 1.5, 2].map((value) => <option key={value} value={value}>{value}×</option>)}
        </select>
      </label>
    </div>
    {audioError ? <p role="status" className="mt-2 text-xs text-muted-foreground">Audio is unavailable or could not play. The saved transcript remains available.</p> : null}
  </section>;
}
