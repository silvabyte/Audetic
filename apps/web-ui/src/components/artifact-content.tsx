import { isValidElement, useEffect, useId, useState, type ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { EffectiveTheme } from "@/stores/ui-store";

export function ArtifactContent({ markdown, theme = "light" }: { markdown: string; theme?: EffectiveTheme }) {
  return <div className="min-w-0 text-[0.9375rem] leading-7 text-foreground">
    <ReactMarkdown
      remarkPlugins={[remarkGfm]}
      skipHtml
      components={{
        h1: ({ children }) => <h1 className="mb-5 mt-1 text-2xl font-semibold tracking-tight">{children}</h1>,
        h2: ({ children }) => <h2 className="mb-3 mt-8 border-b pb-2 text-lg font-semibold tracking-tight first:mt-0">{children}</h2>,
        h3: ({ children }) => <h3 className="mb-2 mt-6 font-semibold">{children}</h3>,
        p: ({ children }) => <p className="my-3 leading-7">{children}</p>,
        ul: ({ children }) => <ul className="my-3 list-disc space-y-1 pl-6">{children}</ul>,
        ol: ({ children }) => <ol className="my-3 list-decimal space-y-1 pl-6">{children}</ol>,
        blockquote: ({ children }) => <blockquote className="my-4 border-l-2 pl-4 text-muted-foreground">{children}</blockquote>,
        a: ({ href, children }) => {
          const external = Boolean(href && /^(?:https?:)?\/\//i.test(href));
          return <a href={href} target={external ? "_blank" : undefined} rel={external ? "noreferrer noopener" : undefined} className="underline decoration-border underline-offset-4 hover:decoration-foreground">{children}</a>;
        },
        table: ({ children }) => <div className="my-5 overflow-x-auto border-y"><table className="w-full min-w-max border-collapse text-sm">{children}</table></div>,
        th: ({ children }) => <th className="border-b px-3 py-2 text-left font-medium">{children}</th>,
        td: ({ children }) => <td className="border-b px-3 py-2 align-top">{children}</td>,
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
  </div>;
}

function mermaidSource(children: ReactNode): string | null {
  if (!isValidElement<{ className?: string; children?: ReactNode }>(children)) return null;
  if (!children.props.className?.split(" ").includes("language-mermaid")) return null;
  return String(children.props.children ?? "").replace(/\n$/, "");
}

function MermaidDiagram({ source, theme }: { source: string; theme: EffectiveTheme }) {
  const renderId = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let active = true;
    setSvg(null);
    setFailed(false);
    void import("mermaid").then(async ({ default: mermaid }) => {
      mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: theme === "dark" ? "dark" : "neutral" });
      const result = await mermaid.render(`artifact-mermaid-${renderId}`, source);
      if (active) setSvg(result.svg);
    }).catch(() => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [renderId, source, theme]);

  if (failed) return <figure className="my-5"><figcaption className="mb-2 text-xs text-muted-foreground">Diagram preview unavailable. Source:</figcaption><pre className="overflow-x-auto rounded-md border bg-muted/30 p-4 text-xs leading-6"><code>{source}</code></pre></figure>;
  if (!svg) return <div className="my-5 min-h-32 animate-pulse rounded-md border bg-muted/30" role="status" aria-label="Rendering diagram" />;
  return <div className="my-5 overflow-x-auto rounded-md border bg-background p-4 [&_svg]:mx-auto [&_svg]:h-auto [&_svg]:max-w-full" dangerouslySetInnerHTML={{ __html: svg }} />;
}
