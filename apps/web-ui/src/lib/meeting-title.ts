export type MeetingTitleFields = {
  title?: string | null;
  sourceFilename?: string | null;
  startedAt?: string | null;
};

/** Returns the canonical title or the best truthful presentation fallback. */
export function meetingDisplayTitle({
  title,
  sourceFilename,
  startedAt,
}: MeetingTitleFields): string {
  const canonical = title?.trim();
  if (canonical) return canonical;

  const filename = sourceFilename?.trim();
  if (filename) {
    const basename = filename.split(/[/\\]/).pop() ?? filename;
    const withoutExtension = basename.replace(/\.[^./\\]+$/, "").trim();
    if (withoutExtension) return withoutExtension;
  }

  const date = startedAt ? new Date(startedAt) : new Date();
  return Number.isNaN(date.getTime())
    ? new Date().toLocaleString()
    : date.toLocaleString();
}
