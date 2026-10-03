import { useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import { audioNoteAudioUrl } from "@/api/client";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { formatDuration } from "@/lib/audio-notes";
import { cn } from "@/lib/utils";

export function TranscriptPlayer({ noteId, text, segments, completed = false, currentTime, onSeek, query = "", followPlayback = false }: { noteId: number; text: string; segments: AudioNoteDetail["transcript_segments"]; completed?: boolean; currentTime?: number; onSeek?: (seconds: number) => void; query?: string; followPlayback?: boolean }) {
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
      {segments?.length ? <div className="space-y-2">{segments.map((segment, index) => !query || segment.text.toLocaleLowerCase().includes(query.toLocaleLowerCase()) ? <button key={index} type="button" onClick={() => seek(segment.start)} aria-label={`Seek to ${formatDuration(segment.start)}: ${segment.text}`} aria-current={index === active ? "true" : undefined} ref={index === active && followPlayback && !query ? (element) => { if (element && element.getClientRects().length && previousIndex.current !== active) { previousIndex.current = active; element.scrollIntoView({ block: "nearest" }); } } : undefined} className={cn("flex w-full gap-5 rounded-lg px-3 py-4 text-left text-[0.9375rem] leading-7 transition-colors hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", index === active && "bg-muted/50")}><span className="pt-1 font-mono text-[0.6875rem] tabular-nums text-muted-foreground">{formatDuration(segment.start)}</span><span><HighlightedText text={segment.text} query={query} /></span></button> : null)}</div> : <p className="whitespace-pre-wrap text-[0.9375rem] leading-8"><HighlightedText text={text || (completed ? "No speech was detected. The captured audio is still available." : "No transcript yet. Your words will appear here after transcription.")} query={query} /></p>}
    </div>;
  }}</Observer>;
}

export function HighlightedText({ text, query }: { text: string; query: string }) {
  if (!query) return <>{text}</>;
  const parts = [];
  const source = text.toLocaleLowerCase();
  const needle = query.toLocaleLowerCase();
  let cursor = 0;
  let index = source.indexOf(needle);
  while (index !== -1) {
    parts.push(text.slice(cursor, index), <mark key={index} className="rounded-sm bg-amber-200/60 px-0.5 text-inherit dark:bg-amber-500/25">{text.slice(index, index + query.length)}</mark>);
    cursor = index + query.length;
    index = source.indexOf(needle, cursor);
  }
  parts.push(text.slice(cursor));
  return <>{parts}</>;
}
