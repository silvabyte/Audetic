import { useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import { audioNoteAudioUrl } from "@/api/client";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { formatDuration } from "@/lib/audio-notes";
import { cn } from "@/lib/utils";

export function TranscriptPlayer({ noteId, text, segments, completed = false, currentTime, onSeek }: { noteId: number; text: string; segments: AudioNoteDetail["transcript_segments"]; completed?: boolean; currentTime?: number; onSeek?: (seconds: number) => void }) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const previousIndex = useRef(-1);
  const [time, setTime] = useState(0);
  const [audioError, setAudioError] = useState(false);
  function seek(seconds: number): void {
    if (onSeek) {
      onSeek(seconds);
      return;
    }
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    audio.play().catch(() => setAudioError(true));
  }
  return <Observer>{() => {
    const playbackTime = currentTime ?? time;
    let active = -1;
    for (let index = 0; index < (segments?.length ?? 0); index++) { if (playbackTime >= segments![index]!.start) active = index; else break; }
    return <div className="flex flex-col gap-3">
      {!onSeek ? <audio key={noteId} ref={audioRef} controls preload="metadata" className="w-full" src={audioNoteAudioUrl(noteId)} aria-label="Audio note playback" onTimeUpdate={(event) => setTime(event.currentTarget.currentTime)} onError={() => setAudioError(true)} onCanPlay={() => setAudioError(false)} /> : null}
      {!onSeek && audioError ? <p role="status" className="text-xs text-muted-foreground">Audio is unavailable or could not play. Older imported notes may no longer have their source file; the saved transcript remains available.</p> : null}
      {segments?.length ? <div className="divide-y border-y">{segments.map((segment, index) => <button key={index} type="button" onClick={() => seek(segment.start)} aria-label={`Seek to ${formatDuration(segment.start)}: ${segment.text}`} aria-current={index === active ? "true" : undefined} ref={index === active ? (element) => { const playing = onSeek || (audioRef.current && !audioRef.current.paused); if (element && previousIndex.current !== active && playing) { previousIndex.current = active; element.scrollIntoView({ block: "nearest" }); } } : undefined} className={cn("flex w-full gap-4 px-1 py-3 text-left text-sm leading-relaxed hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", index === active && "bg-muted/60")}><span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">{formatDuration(segment.start)}</span><span>{segment.text}</span></button>)}</div> : <p className="whitespace-pre-wrap text-sm leading-7">{text || (completed ? "No speech was detected. The captured audio is still available." : "No transcript yet. Your words will appear here after transcription.")}</p>}
    </div>;
  }}</Observer>;
}
