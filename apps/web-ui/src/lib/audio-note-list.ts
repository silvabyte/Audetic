/** Group the daemon's chronological results by local calendar day without reordering them. */
export function groupNotesByDate<T extends { started_at: string }>(notes: T[], now = new Date()): { key: string; label: string; notes: T[] }[] {
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  const groups = new Map<string, { key: string; label: string; notes: T[] }>();
  for (const note of notes) {
    const date = new Date(note.started_at);
    const key = date.toDateString();
    let group = groups.get(key);
    if (!group) {
      const label = key === now.toDateString() ? "Today" : key === yesterday.toDateString() ? "Yesterday" : date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric", ...(date.getFullYear() !== now.getFullYear() ? { year: "numeric" } as const : {}) });
      group = { key, label, notes: [] };
      groups.set(key, group);
    }
    group.notes.push(note);
  }
  return [...groups.values()];
}

/** Bring a transcript match into the preview, even when it occurs deep in a recording. */
export function transcriptExcerpt(text: string, query: string): string {
  const normalized = text.replace(/\s+/g, " ").trim();
  const term = query.trim();
  const match = term ? normalized.toLocaleLowerCase().indexOf(term.toLocaleLowerCase()) : -1;
  if (match <= 60) return normalized;
  const start = normalized.indexOf(" ", match - 60);
  return `…${normalized.slice(start < 0 || start >= match ? match : start + 1)}`;
}
