/** JSON classifications are extensible. Never assume a known kind or well-formed metadata. */
export function classificationFields(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? Object.fromEntries(Object.entries(value)) : null;
}
export function classificationKind(value: unknown): string | null {
  const kind = classificationFields(value)?.kind;
  return typeof kind === "string" && kind.trim() ? kind : null;
}
export function kindLabel(kind: string): string { return kind.replace(/[-_]/g, " "); }
export function noteNeedsRefresh(note: { status: string; enrichment_status: string }): boolean {
  return !["completed", "error", "cancelled"].includes(note.status) || (note.status === "completed" && ["pending", "running"].includes(note.enrichment_status));
}
export function enrichmentLabel(status: string): string {
  switch (status) {
    case "pending": return "Ready for AI processing";
    case "running": return "AI processing";
    case "completed": return "AI processing complete";
    case "error": return "AI processing needs attention";
    default: return "AI status unknown";
  }
}
export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") return error.message;
  return JSON.stringify(error) ?? "Request failed";
}
export function formatDuration(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}
