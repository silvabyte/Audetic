import { useState, type RefObject } from "react";
import { List, Pause, Play, RotateCcw, RotateCw } from "lucide-react";
import { audioNoteAudioUrl } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
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
  const [chaptersOpen, setChaptersOpen] = useState(false);
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

  return <section aria-label="Audio transport" className="bg-background/95 py-2 backdrop-blur supports-[backdrop-filter]:bg-background/85">
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
    <div className="flex flex-wrap items-center gap-1 @xl:flex-nowrap @xl:gap-2">
      <Button type="button" size="icon" className="shrink-0 rounded-full" aria-label={playing ? "Pause" : "Play"} title={playing ? "Pause" : "Play"} onClick={() => void togglePlayback()}>{playing ? <Pause className="size-4" /> : <Play className="size-4" />}</Button>
      <Button type="button" variant="ghost" size="icon" className="shrink-0 text-muted-foreground" aria-label="Rewind 10 seconds" title="Rewind 10 seconds" disabled={audioError} onClick={() => seek(currentTime - 10)}><RotateCcw className="size-3.5" /></Button>
      <Button type="button" variant="ghost" size="icon" className="shrink-0 text-muted-foreground" aria-label="Forward 10 seconds" title="Forward 10 seconds" disabled={audioError} onClick={() => seek(currentTime + 10)}><RotateCw className="size-3.5" /></Button>
      <span className="hidden shrink-0 px-1 font-mono text-[0.6875rem] tabular-nums text-muted-foreground @xl:block">{formatDuration(currentTime)} <span className="opacity-50">/</span> {formatDuration(boundedDuration)}</span>
      <div className="order-last flex basis-full items-center gap-3 py-2 @xl:order-none @xl:mx-2 @xl:min-w-12 @xl:flex-1 @xl:basis-auto">
        <div className="relative min-w-0 flex-1">
        <input
          type="range"
          min={0}
          max={Math.max(boundedDuration, 0)}
          step={0.1}
          value={Math.min(currentTime, boundedDuration || 0)}
          onChange={(event) => seek(Number(event.target.value))}
          aria-label="Playback position"
          aria-valuetext={`${formatDuration(currentTime)} of ${formatDuration(boundedDuration)}`}
          disabled={audioError || boundedDuration <= 0}
          className="note-playback-range block h-5 w-full cursor-pointer accent-foreground disabled:cursor-default disabled:opacity-40"
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
        <span className="shrink-0 font-mono text-[0.6875rem] tabular-nums text-muted-foreground @xl:hidden">{formatDuration(currentTime)} / {formatDuration(boundedDuration)}</span>
      </div>
      <label className="ml-auto flex shrink-0 items-center text-xs text-muted-foreground @xl:ml-0">
        <span className="sr-only">Speed</span>
        <select
          aria-label="Playback speed"
          value={speed}
          onChange={(event) => {
            const next = Number(event.target.value);
            setSpeed(next);
            if (audioRef.current) audioRef.current.playbackRate = next;
          }}
          className="h-9 rounded-md bg-transparent px-1 text-xs text-muted-foreground hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {[0.75, 1, 1.25, 1.5, 2].map((value) => <option key={value} value={value}>{value}×</option>)}
        </select>
      </label>
      {markers.length ? <Popover open={chaptersOpen} onOpenChange={setChaptersOpen}><PopoverTrigger asChild><Button variant="ghost" size="icon" aria-label={`${markers.length} recording chapters`} title="Recording chapters" className="shrink-0 text-muted-foreground"><List className="size-4" /></Button></PopoverTrigger><PopoverContent align="end" className="w-80 max-w-[calc(100vw-2rem)] p-2"><h2 className="px-2 py-2 text-xs font-medium">Recording chapters</h2><ol className="max-h-72 overflow-auto">{markers.map((point) => <li key={point.seconds}><button type="button" onClick={() => { seek(point.seconds); setChaptersOpen(false); }} className="flex min-h-10 w-full gap-3 rounded px-2 py-2.5 text-left text-xs leading-relaxed text-muted-foreground hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><span className="font-mono tabular-nums">{point.timestamp}</span>{point.label}</button></li>)}</ol></PopoverContent></Popover> : null}
    </div>
    {audioError ? <p role="status" className="mt-2 text-xs text-muted-foreground">Audio is unavailable or could not play. The saved transcript remains available.</p> : null}
  </section>;
}
