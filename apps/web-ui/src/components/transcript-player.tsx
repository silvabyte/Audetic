import { useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import type { AudioNoteDetail } from "@/stores/audio-notes-store";
import { formatDuration } from "@/lib/audio-notes";
import { cn } from "@/lib/utils";

export function TranscriptPlayer({ noteId, text, segments, completed = false }: { noteId: number; text: string; segments: AudioNoteDetail["transcript_segments"]; completed?: boolean }) {
  const audioRef = useRef<HTMLAudioElement>(null);
  const previousIndex = useRef(-1);
  const [time, setTime] = useState(0);
  const [audioError, setAudioError] = useState(false);
  function seek(seconds: number): void {
    const audio = audioRef.current;
    if (!audio) return;
    audio.currentTime = seconds;
    audio.play().catch(() => setAudioError(true));
  }
  return <Observer>{() => {
    let active = -1;
    for (let index = 0; index < (segments?.length ?? 0); index++) { if (time >= segments![index]!.start) active = index; else break; }
    return <div className="flex flex-col gap-3">
      <audio key={noteId} ref={audioRef} controls preload="metadata" className="w-full" src={`/api/audio-notes/${noteId}/audio`} aria-label="Audio note playback" onTimeUpdate={(event) => setTime(event.currentTarget.currentTime)} onError={() => setAudioError(true)} onCanPlay={() => setAudioError(false)} />
      {audioError && <p role="status" className="text-xs text-muted-foreground">Audio is unavailable or could not play. Older imported notes may no longer have their source file; the saved transcript remains available.</p>}
      {segments?.length ? <div className="max-h-[28rem] divide-y overflow-auto rounded-md border">{segments.map((segment, index) => <button key={index} type="button" onClick={() => seek(segment.start)} aria-label={`Seek to ${formatDuration(segment.start)}: ${segment.text}`} aria-current={index === active ? "true" : undefined} ref={index === active ? (element) => { if (element && previousIndex.current !== active && audioRef.current && !audioRef.current.paused) { previousIndex.current = active; element.scrollIntoView({ block: "nearest" }); } } : undefined} className={cn("flex w-full gap-3 px-3 py-2 text-left text-sm hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", index === active && "bg-primary/10")}><span className="shrink-0 font-mono text-xs tabular-nums text-muted-foreground">{formatDuration(segment.start)}</span><span>{segment.text}</span></button>)}</div> : <p className="whitespace-pre-wrap text-sm leading-relaxed">{text || (completed ? "No speech was detected. The captured audio is still available." : "No transcript yet. Your words will appear here after transcription.")}</p>}
    </div>;
  }}</Observer>;
}
