import { useState, type CSSProperties } from "react";
import { ArrowUpRight, ChevronDown, Network } from "lucide-react";
import { Button } from "@/components/ui/button";
import { shortMapLabel, type NoteSection } from "@/lib/note-document";

/** A navigable map of saved summary text; never invents another interpretation. */
export function NoteMindMap({ title, sections, onReadSection }: { title: string; sections: NoteSection[]; onReadSection: (line: number) => void }) {
  const [collapsed, setCollapsed] = useState<Set<number>>(() => new Set());
  const allCollapsed = sections.length > 0 && sections.every((section) => collapsed.has(section.line));
  return <section aria-label="Summary mind map" className="py-2">
    <div className="mb-10 flex flex-wrap items-center justify-between gap-3">
      <p className="text-xs text-muted-foreground">Mapped from your saved summary. Select a branch to explore.</p>
      <Button size="sm" variant="ghost" onClick={() => setCollapsed(allCollapsed ? new Set() : new Set(sections.map((section) => section.line)))}>{allCollapsed ? "Expand branches" : "Collapse branches"}</Button>
    </div>
    <div className="note-map-root mx-auto mb-12 flex max-w-md flex-col items-center gap-3 text-center">
      <Network className="size-5 text-muted-foreground" />
      <h3 className="text-balance font-serif text-2xl leading-snug tracking-tight">{title}</h3>
      <span className="text-xs text-muted-foreground">{sections.length} connected topics</span>
    </div>
    <div className="note-map-branches grid gap-x-8 gap-y-10 md:grid-cols-2 xl:grid-cols-3">
      {sections.map((section, index) => {
        const open = !collapsed.has(section.line);
        return <article key={section.line} className="note-map-branch min-w-0" style={{ "--branch-color": ["var(--map-sage)", "var(--map-blue)", "var(--map-ochre)"][index % 3] } as CSSProperties}>
          <button type="button" aria-expanded={open} aria-controls={`map-branch-${section.line}`} onClick={() => setCollapsed((previous) => { const next = new Set(previous); if (open) next.add(section.line); else next.delete(section.line); return next; })} className="flex w-full items-center gap-3 rounded-lg border border-[var(--branch-color)]/25 bg-[var(--branch-color)]/8 px-4 py-3 text-left text-sm font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
            <span className="size-1.5 shrink-0 rounded-full bg-[var(--branch-color)]" /><span className="flex-1">{section.title}</span><ChevronDown className={`size-4 shrink-0 text-muted-foreground transition-transform ${open ? "" : "-rotate-90"}`} />
          </button>
          <div id={`map-branch-${section.line}`} hidden={!open} className="ml-5 border-l border-[var(--branch-color)]/30 pl-5">
            <ul className="space-y-4 pt-5">{section.points.slice(0, 5).map((point, pointIndex) => <li key={pointIndex} className="note-map-leaf relative text-[0.8125rem] leading-relaxed text-muted-foreground">{shortMapLabel(point)}</li>)}</ul>
            <button type="button" onClick={() => onReadSection(section.line)} className="mt-4 inline-flex items-center gap-1 py-2 text-xs font-medium text-foreground underline-offset-4 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">{section.points.length > 5 ? `Read all ${section.points.length} points` : "Read section"}<ArrowUpRight className="size-3" /></button>
          </div>
        </article>;
      })}
    </div>
  </section>;
}
