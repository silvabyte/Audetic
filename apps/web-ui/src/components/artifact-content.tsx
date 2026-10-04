import { isValidElement, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Download, Maximize2, Minus, Plus, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { readNoteDocument, sectionId } from "@/lib/note-document";
import { downloadText } from "@/lib/download";
import type { EffectiveTheme } from "@/stores/ui-store";

export function ArtifactContent({ markdown, theme = "light", documentId, outline = false, onSeek, duration }: { markdown: string; theme?: EffectiveTheme; documentId?: string; outline?: boolean; onSeek?: (seconds: number) => void; duration?: number | null }) {
  const id = useId();
  const prefix = documentId ?? id;
  const document = useMemo(() => readNoteDocument(markdown), [markdown]);
  const hasOutline = outline && document.sections.length > 1;
  const outlineLinks = <ol className="mt-4 space-y-3 border-l pl-4">{document.sections.map((section) => <li key={section.line}><a href={`#${sectionId(prefix, section.line)}`} className="block text-xs leading-relaxed text-muted-foreground transition-colors hover:text-foreground">{section.title}</a></li>)}</ol>;
  return <div className={hasOutline ? "grid min-w-0 gap-6 lg:grid-cols-[minmax(0,1fr)_10rem] lg:gap-14" : "min-w-0"}>
    {hasOutline ? <div className="order-first lg:order-last"><nav aria-label="On this page" className="sticky top-24 hidden lg:block"><p className="text-[0.6875rem] font-medium uppercase tracking-[0.16em] text-muted-foreground">On this page</p>{outlineLinks}<p className="mt-6 text-[0.6875rem] text-muted-foreground">{Math.max(1, Math.ceil(document.words / 220))} min read</p></nav><details className="border-b pb-4 lg:hidden"><summary className="cursor-pointer text-xs text-muted-foreground">On this page<span className="ml-2 opacity-70">· {Math.max(1, Math.ceil(document.words / 220))} min read</span></summary><nav aria-label="Document sections">{outlineLinks}</nav></details></div> : null}
    <div data-document-id={prefix} className="min-w-0 text-[0.9375rem] leading-7 text-foreground">
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      skipHtml
      components={{
        h1: ({ children, node }) => <h2 id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="mb-8 mt-1 scroll-mt-40 text-balance font-serif text-[1.75rem] leading-snug tracking-tight">{children}</h2>,
        h2: ({ children, node }) => <h3 id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="mb-4 mt-10 scroll-mt-40 text-lg font-semibold tracking-tight first:mt-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring sm:scroll-mt-28">{children}</h3>,
        h3: ({ children, node }) => <h3 id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="mb-2 mt-6 scroll-mt-40 font-semibold">{children}</h3>,
        h4: ({ children, node }) => <h4 id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="mb-2 mt-5 scroll-mt-40 font-semibold">{children}</h4>,
        h5: ({ children, node }) => <h5 id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="mb-2 mt-5 scroll-mt-40 font-semibold">{children}</h5>,
        h6: ({ children, node }) => <h6 id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="mb-2 mt-5 scroll-mt-40 font-semibold">{children}</h6>,
        p: ({ children, node }) => <p id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="my-3 scroll-mt-40 leading-7">{children}</p>,
        ul: ({ children }) => <ul className="my-3 list-disc space-y-1 pl-6">{children}</ul>,
        ol: ({ children }) => <ol className="my-3 list-decimal space-y-1 pl-6">{children}</ol>,
        li: ({ children, node }) => <li data-source-line={node?.position?.start.line} tabIndex={-1} className="scroll-mt-40">{children}</li>,
        blockquote: ({ children }) => <blockquote className="my-4 border-l-2 pl-4 text-muted-foreground">{children}</blockquote>,
        a: ({ href, children }) => {
          const match = /^#t=(\d+(?:\.\d+)?)$/.exec(href ?? "");
          if (match) {
            const seconds = Number(match[1]);
            return onSeek && Number.isFinite(seconds) && (duration == null || seconds <= duration)
              ? <button type="button" onClick={() => onSeek(seconds)} className="inline-flex rounded bg-muted/70 px-1.5 font-mono text-xs tabular-nums text-muted-foreground hover:text-foreground" aria-label={`Seek to ${String(children)}`}>{children}</button>
              : <span className="font-mono text-xs text-muted-foreground">{children}</span>;
          }
          const external = Boolean(href && /^(?:https?:)?\/\//i.test(href));
          return <a href={href} target={external ? "_blank" : undefined} rel={external ? "noreferrer noopener" : undefined} className="underline decoration-border underline-offset-4 hover:decoration-foreground">{children}</a>;
        },
        table: ({ children }) => <div className="my-5 overflow-x-auto border-y"><table className="w-full min-w-[32rem] border-collapse text-sm">{children}</table></div>,
        th: ({ children }) => <th className="border-b px-3 py-2 text-left font-medium">{children}</th>,
        tr: ({ children, node }) => <tr id={sectionId(prefix, node?.position?.start.line ?? 0)} tabIndex={-1} className="scroll-mt-40">{children}</tr>,
        td: ({ children }) => <td className="max-w-96 break-words border-b px-3 py-3 align-top leading-relaxed">{children}</td>,
        code: ({ className, children }) => <code className={className ? `${className} font-mono text-xs` : "rounded bg-muted px-1.5 py-0.5 font-mono text-[0.85em]"}>{children}</code>,
        pre: ({ children }) => {
          const mermaid = mermaidSource(children);
          return mermaid === null
            ? <pre className="my-4 overflow-x-auto rounded-md border bg-muted/30 p-4 text-xs leading-6">{children}</pre>
            : <MermaidDiagram source={mermaid} theme={theme} />;
        },
      }}
    >
      {markdown}
    </ReactMarkdown>
    </div>
  </div>;
}

function mermaidSource(children: ReactNode): string | null {
  if (!isValidElement<{ className?: string; children?: ReactNode }>(children)) return null;
  if (!children.props.className?.split(" ").includes("language-mermaid")) return null;
  return String(children.props.children ?? "").replace(/\n$/, "");
}

export function MermaidDiagram({ source, theme }: { source: string; theme: EffectiveTheme }) {
  const renderId = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    let active = true;
    setSvg(null);
    setFailed(false);
    void import("mermaid").then(async ({ default: mermaid }) => {
      mermaid.initialize({ startOnLoad: false, securityLevel: "strict", suppressErrorRendering: true, theme: theme === "dark" ? "dark" : "neutral" });
      const result = await mermaid.render(`artifact-mermaid-${renderId}`, source);
      if (active) setSvg(result.svg);
    }).catch(() => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [renderId, source, theme]);

  if (failed) return <figure className="my-5"><figcaption className="mb-2 text-xs text-muted-foreground">Diagram preview unavailable. Source:</figcaption><pre className="overflow-x-auto rounded-md border bg-muted/30 p-4 text-xs leading-6"><code>{source}</code></pre></figure>;
  if (!svg) return <div className="my-5 min-h-32 animate-pulse rounded-md border bg-muted/30" role="status" aria-label="Rendering diagram" />;
  return <figure className="my-6">
    <DiagramCanvas svg={svg} onExpand={() => setExpanded(true)} />
    <details className="mt-3 text-xs text-muted-foreground"><summary className="cursor-pointer">Diagram source</summary><pre className="mt-3 overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/40 p-4">{source}</pre></details>
    <Dialog open={expanded} onOpenChange={setExpanded}><DialogContent className="max-w-[calc(100vw-2rem)]"><DialogTitle>Mind map</DialogTitle><DialogDescription>Zoom in for detail. Scroll to explore the diagram.</DialogDescription><DiagramCanvas svg={svg} expanded /></DialogContent></Dialog>
  </figure>;
}

function DiagramCanvas({ svg, expanded = false, onExpand }: { svg: string; expanded?: boolean; onExpand?: () => void }) {
  const [zoom, setZoom] = useState(1);
  const [fitWidth, setFitWidth] = useState<number | null>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const instanceId = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const instanceSvg = useMemo(() => {
    // Expanded and inline canvases coexist. Keep SVG definitions and CSS local.
    const ids = [...svg.matchAll(/\bid="([^"]+)"/g)].map((match) => match[1]!).sort((a, b) => b.length - a.length);
    let result = svg;
    for (const id of ids) result = result.replaceAll(`id="${id}"`, `id="${instanceId}-${id}"`).replaceAll(`#${id}`, `#${instanceId}-${id}`);
    return result;
  }, [svg, instanceId]);
  useEffect(() => {
    const element = viewport.current;
    if (!element) return;
    const resize = () => {
      const box = element.querySelector("svg")?.viewBox.baseVal;
      const width = Math.max(1, element.clientWidth - 48);
      const height = Math.max(1, element.clientHeight - 48);
      setFitWidth(box && box.width > 0 && box.height > 0 ? Math.min(width, height * box.width / box.height) : width);
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(element);
    return () => observer.disconnect();
  }, [instanceSvg]);
  return <div className="overflow-hidden rounded-xl border bg-muted/10">
    <div className="flex items-center justify-between gap-2 border-b bg-background px-3 py-2">
      <div className="flex items-center gap-1"><Button variant="ghost" size="icon" aria-label="Zoom out" disabled={zoom <= 0.5} onClick={() => setZoom((value) => Math.max(0.5, value - 0.25))}><Minus /></Button><output aria-live="polite" className="w-12 text-center font-mono text-xs tabular-nums">{Math.round(zoom * 100)}%</output><Button variant="ghost" size="icon" aria-label="Zoom in" disabled={zoom >= 4} onClick={() => setZoom((value) => Math.min(4, value + 0.25))}><Plus /></Button><Button variant="ghost" size="icon" aria-label="Fit diagram" onClick={() => { setZoom(1); viewport.current?.scrollTo(0, 0); }}><RotateCcw /></Button></div>
      <div className="flex gap-1"><Button variant="ghost" size="icon" aria-label="Download diagram as SVG" onClick={() => downloadText(svg, "audetic-mind-map.svg", "image/svg+xml")}><Download /></Button>{onExpand ? <Button variant="ghost" size="icon" aria-label="Expand diagram" onClick={onExpand}><Maximize2 /></Button> : null}</div>
    </div>
    <div ref={viewport} tabIndex={0} role="region" aria-label="Scrollable diagram" className={`overflow-auto p-6 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring ${expanded ? "h-[70dvh]" : "h-[28rem]"}`}>
      <div style={{ width: fitWidth == null ? `${zoom * 100}%` : fitWidth * zoom }} className="mx-auto [&_svg]:h-auto [&_svg]:w-full [&_svg]:!max-w-none" dangerouslySetInnerHTML={{ __html: instanceSvg }} />
    </div>
  </div>;
}
