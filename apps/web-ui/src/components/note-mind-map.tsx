import { useEffect, useMemo, useRef, useState } from "react";
import type { Markmap } from "markmap-view";
import { ArrowUpRight, Download, Maximize2, Minus, Plus, Scan } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { readNoteMap, toMarkmapData, type NoteMapNode } from "@/lib/note-map";
import { downloadText } from "@/lib/download";
import type { EffectiveTheme } from "@/stores/ui-store";

export function NoteMindMap({ markdown, theme, onReadSection }: { markdown: string; theme: EffectiveTheme; onReadSection: (line: number) => void }) {
  const tree = useMemo(() => readNoteMap(markdown), [markdown]);
  const [expanded, setExpanded] = useState(false);
  return <section aria-label="Summary mind map">
    <div className="mb-5 flex flex-wrap items-end justify-between gap-3">
      <div><p className="text-[0.6875rem] font-medium uppercase tracking-[0.16em] text-muted-foreground">One note. Every connection.</p><h3 className="mt-2 font-serif text-2xl tracking-tight">Explore the conversation</h3></div>
      <p className="text-xs text-muted-foreground">{tree.children.length} branches · From your saved summary</p>
    </div>
    <MindMapCanvas tree={tree} theme={theme} onReadSection={onReadSection} onExpand={() => setExpanded(true)} />
    <details className="mt-5 border-b pb-4 text-sm"><summary className="w-fit cursor-pointer text-muted-foreground">Browse as an outline</summary><MapOutline nodes={tree.children} onReadSection={onReadSection} /></details>
    <Dialog open={expanded} onOpenChange={setExpanded}><DialogContent className="max-w-[calc(100vw-2rem)] gap-2 p-4 sm:p-5"><DialogTitle className="pr-8 font-serif leading-snug">{tree.label}</DialogTitle><DialogDescription>Expand a branch to explore. Select its text to read the source.</DialogDescription><MindMapCanvas tree={tree} theme={theme} expanded onReadSection={(line) => { setExpanded(false); onReadSection(line); }} /></DialogContent></Dialog>
  </section>;
}

function MapOutline({ nodes, onReadSection }: { nodes: NoteMapNode[]; onReadSection: (line: number) => void }) {
  return <ul className="mt-3 space-y-3 border-l pl-4">{nodes.map((node, index) => <li key={`${node.line}-${index}`}>
    {node.children.length ? <details><summary className="cursor-pointer leading-relaxed">{node.label}</summary><button type="button" className="my-2 inline-flex items-center gap-1 text-xs underline underline-offset-4" onClick={() => onReadSection(node.line)}>Read in summary<ArrowUpRight className="size-3" /></button><MapOutline nodes={node.children} onReadSection={onReadSection} /></details>
      : <button type="button" className="text-left leading-relaxed text-muted-foreground hover:text-foreground" onClick={() => onReadSection(node.line)}>{node.label}</button>}
  </li>)}</ul>;
}

function MindMapCanvas({ tree, theme, onReadSection, expanded = false, onExpand }: { tree: NoteMapNode; theme: EffectiveTheme; onReadSection: (line: number) => void; expanded?: boolean; onExpand?: () => void }) {
  const svgRef = useRef<SVGSVGElement>(null);
  const mapRef = useRef<Markmap | null>(null);
  const scaleRef = useRef(1);
  const readRef = useRef(onReadSection);
  const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");
  const [zoom, setZoom] = useState(100);
  useEffect(() => { readRef.current = onReadSection; }, [onReadSection]);

  useEffect(() => {
    const svg = svgRef.current;
    if (!svg) return;
    let disposed = false;
    let map: Markmap | undefined;
    let resize: ResizeObserver | undefined;
    let mutations: MutationObserver | undefined;
    setStatus("loading");
    const readSource = async (event: MouseEvent) => {
      if (event.target instanceof SVGElement && event.target.tagName === "circle" && map) {
        // Keep newly revealed branches in view, rather than growing off the right edge.
        const target = event.target;
        const node = map.g.selectAll<SVGGElement, NonNullable<Markmap["state"]["data"]>>("g.markmap-node").filter(function() { return this.contains(target); }).datum();
        if (node) {
          event.stopPropagation();
          await map.toggleNode(node);
          if (!disposed) await map.centerNode(node);
        }
        return;
      }
      const button = event.target instanceof Element ? event.target.closest<HTMLElement>("[data-source-line]") : null;
      if (button) readRef.current(Number(button.dataset.sourceLine));
    };
    const keyboardToggle = (event: KeyboardEvent) => {
      if (event.target instanceof SVGElement && event.target.tagName === "circle" && ["Enter", " "].includes(event.key)) {
        event.preventDefault();
        event.target.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      }
    };
    svg.addEventListener("click", readSource, true);
    svg.addEventListener("keydown", keyboardToggle);
    void import("markmap-view").then(async ({ Markmap }) => {
      if (disposed) return;
      const colors = theme === "dark" ? ["#92bca3", "#94b4cf", "#d1b17a", "#ce9c96", "#b2a5d2", "#83bbb7"] : ["#47745e", "#52758f", "#997539", "#a46059", "#80709e", "#468680"];
      map = Markmap.create(svg, {
        duration: 0, initialExpandLevel: 2, maxWidth: svg.clientWidth < 640 ? 240 : 330, spacingHorizontal: 75, spacingVertical: 16,
        paddingX: 14, autoFit: false, scrollForPan: true, zoom: true, pan: true,
        color: (node) => colors[Math.max(0, Number(node.payload?.branch ?? 0)) % colors.length]!,
        lineWidth: (node) => node.state.depth <= 2 ? 2 : 1.2,
        style: (id) => `.${id}{--markmap-text-color:${theme === "dark" ? "#e7e5e0" : "#292b27"};--markmap-circle-open-bg:${theme === "dark" ? "#1c1d1a" : "#fff"};font:14px/1.65 ui-sans-serif,system-ui,sans-serif}.${id} foreignObject button{all:unset;cursor:pointer;text-align:left;color:inherit;overflow-wrap:anywhere}.${id} foreignObject button:hover{text-decoration:underline;text-underline-offset:4px}.${id} foreignObject button:focus-visible,.${id} circle:focus-visible{outline:2px solid currentColor;outline-offset:3px}.${id} [data-depth="1"] foreignObject{font:22px/1.4 Georgia,serif}.${id} [data-depth="2"] foreignObject{font-weight:600}`,
      });
      mapRef.current = map;
      const decorate = () => {
        map?.g.selectAll<SVGGElement, NonNullable<Markmap["state"]["data"]>>("g.markmap-node").each(function(node) {
          const circle = this.querySelector("circle");
          if (!circle) return;
          circle.setAttribute("tabindex", "0");
          circle.setAttribute("role", "button");
          circle.setAttribute("aria-expanded", String(!node.payload?.fold));
          circle.setAttribute("aria-label", `${node.payload?.fold ? "Expand" : "Collapse"} ${node.payload?.label ?? "branch"}`);
        });
      };
      mutations = new MutationObserver(decorate);
      mutations.observe(svg, { childList: true, subtree: true, attributes: true, attributeFilter: ["fill"] });
      map.zoom.on("zoom.audetic", (event: { transform: { k: number } }) => {
        scaleRef.current = event.transform.k;
        if (!disposed) setZoom(Math.round(event.transform.k * 100));
      });
      await map.setData(toMarkmapData(tree));
      if (disposed) return;
      // A phone-sized fit would shrink all the text to a miniature. Start on readable topics.
      const fitReadable = async () => {
        if (!map || disposed) return;
        await map.fit();
        if (!disposed && svg.clientWidth < 640 && scaleRef.current < 1) {
          await map.rescale(1 / scaleRef.current);
          const topics = map.state.data?.children;
          const middle = topics?.[Math.floor(topics.length / 2)];
          if (!disposed && middle) await map.centerNode(middle);
        }
      };
      await fitReadable();
      if (disposed) return;
      decorate();
      setStatus("ready");
      resize = new ResizeObserver(() => { if (svg.clientWidth && svg.clientHeight) void fitReadable(); });
      resize.observe(svg);
    }).catch(() => { if (!disposed) setStatus("error"); });
    return () => {
      disposed = true;
      resize?.disconnect();
      mutations?.disconnect();
      map?.destroy();
      mapRef.current = null;
      svg.removeEventListener("click", readSource, true);
      svg.removeEventListener("keydown", keyboardToggle);
    };
  }, [tree, theme]);

  async function setDepth(all: boolean): Promise<void> {
    const map = mapRef.current;
    if (!map) return;
    await map.setData(toMarkmapData(tree), { initialExpandLevel: all ? -1 : 2 });
    await map.fit();
  }

  function exportMap(): void {
    const map = mapRef.current;
    const svg = svgRef.current?.cloneNode(true) as SVGSVGElement | undefined;
    if (!map || !svg) return;
    const { x1, x2, y1, y2 } = map.state.rect;
    svg.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    svg.setAttribute("viewBox", `${x1 - 24} ${y1 - 24} ${x2 - x1 + 48} ${y2 - y1 + 48}`);
    svg.setAttribute("width", String(x2 - x1 + 48));
    svg.setAttribute("height", String(y2 - y1 + 48));
    svg.style.background = theme === "dark" ? "#1c1d1a" : "#fff";
    svg.querySelector("g")?.removeAttribute("transform");
    downloadText(new XMLSerializer().serializeToString(svg), "audetic-summary-map.svg", "image/svg+xml");
  }

  return <div className="note-map-canvas overflow-hidden rounded-xl border bg-background">
    <div className="flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
      <div className="flex gap-1"><Button variant="ghost" size="sm" disabled={status !== "ready"} onClick={() => void setDepth(false)}>Topics only</Button><Button variant="ghost" size="sm" disabled={status !== "ready"} onClick={() => void setDepth(true)}>Expand all</Button></div>
      <div className="flex items-center gap-1"><Button variant="ghost" size="icon" aria-label="Zoom out" disabled={status !== "ready"} onClick={() => void mapRef.current?.rescale(0.8)}><Minus /></Button><output className="w-12 text-center font-mono text-xs tabular-nums" aria-label="Map zoom">{zoom}%</output><Button variant="ghost" size="icon" aria-label="Zoom in" disabled={status !== "ready"} onClick={() => void mapRef.current?.rescale(1.25)}><Plus /></Button><Button variant="ghost" size="icon" aria-label="Fit map" disabled={status !== "ready"} onClick={() => void mapRef.current?.fit()}><Scan /></Button><Button variant="ghost" size="icon" aria-label="Download map as SVG" disabled={status !== "ready"} onClick={exportMap}><Download /></Button>{onExpand ? <Button variant="ghost" size="icon" aria-label="Expand map" onClick={onExpand}><Maximize2 /></Button> : null}</div>
    </div>
    <div className="relative">
      <svg ref={svgRef} aria-label="Interactive summary mind map" className={`note-map-viewport block w-full touch-none ${expanded ? "h-[calc(100dvh-15rem)] min-h-64" : "h-[28rem] max-h-[70dvh] min-h-80 sm:h-[36rem]"}`} />
      {status !== "ready" ? <p role="status" className="absolute inset-0 grid place-items-center p-6 text-center text-sm text-muted-foreground">{status === "error" ? "The canvas could not load. You can browse every branch in the outline below." : "Drawing connections…"}</p> : null}
    </div>
    <p className="border-t px-4 py-3 text-[0.6875rem] text-muted-foreground">Drag to pan · Pinch to zoom · Circles expand branches · Text opens the summary</p>
  </div>;
}
