export interface TalkingPoint {
  label: string;
  seconds: number;
  timestamp: string;
}

interface TalkingPointArtifact {
  id: number;
  kind: string;
  status: string;
  content_markdown?: string | null;
  completed_at?: string | null;
  updated_at: string;
  created_at: string;
}

const TALKING_POINT_LINE = /^\s*[-–—*+]\s+\[?((?:\d+):(?:\d{2})(?::\d{2})?)\]?\s*(?:[-–—]\s*)?(.+?)\s*$/;

export function parseTalkingPoints(
  artifacts: readonly TalkingPointArtifact[],
  durationSeconds?: number | null,
): TalkingPoint[] {
  const artifact = artifacts
    .filter((entry) => entry.kind === "talking_points" && entry.status === "completed" && entry.content_markdown)
    .toSorted((left, right) => artifactTime(right) - artifactTime(left) || right.id - left.id)[0];
  if (!artifact?.content_markdown) return [];

  const upperBound = typeof durationSeconds === "number" && Number.isFinite(durationSeconds) && durationSeconds >= 0
    ? durationSeconds
    : null;
  const bySecond = new Map<number, TalkingPoint>();
  for (const line of artifact.content_markdown.split(/\r?\n/)) {
    const match = TALKING_POINT_LINE.exec(line);
    if (!match) continue;
    const timestamp = match[1];
    const label = match[2]?.trim();
    if (!timestamp || !label) continue;
    const seconds = timestampSeconds(timestamp);
    if (seconds === null || (upperBound !== null && seconds > upperBound) || bySecond.has(seconds)) continue;
    bySecond.set(seconds, { label, seconds, timestamp });
  }
  return [...bySecond.values()].toSorted((left, right) => left.seconds - right.seconds);
}

function artifactTime(artifact: TalkingPointArtifact): number {
  const value = Date.parse(artifact.completed_at ?? artifact.updated_at ?? artifact.created_at);
  return Number.isFinite(value) ? value : 0;
}

function timestampSeconds(timestamp: string): number | null {
  const parts = timestamp.split(":").map(Number);
  if (parts.length !== 2 && parts.length !== 3) return null;
  if (parts.some((part) => !Number.isSafeInteger(part) || part < 0)) return null;
  if (parts.length === 2) {
    const [minutes, seconds] = parts;
    if (minutes === undefined || seconds === undefined || seconds >= 60) return null;
    return minutes * 60 + seconds;
  }
  const [hours, minutes, seconds] = parts;
  if (hours === undefined || minutes === undefined || seconds === undefined || minutes >= 60 || seconds >= 60) return null;
  return hours * 3600 + minutes * 60 + seconds;
}
