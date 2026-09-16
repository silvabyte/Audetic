import { useEffect, useId, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { AlertTriangle } from "lucide-react";

export function ArtifactContent({ markdown }: { markdown: string }) {
  return (
    <div className="artifact-content text-[15px] leading-7">
      <Markdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => (
            <h1 className="mb-5 text-2xl font-semibold tracking-tight">{children}</h1>
          ),
          h2: ({ children }) => (
            <h2 className="mb-2 mt-7 text-base font-semibold tracking-tight first:mt-0">
              {children}
            </h2>
          ),
          h3: ({ children }) => (
            <h3 className="mb-1.5 mt-5 text-sm font-semibold">{children}</h3>
          ),
          p: ({ children }) => <p className="my-3 text-foreground/85">{children}</p>,
          ul: ({ children }) => (
            <ul className="my-3 list-disc space-y-1.5 pl-5 text-foreground/85">
              {children}
            </ul>
          ),
          ol: ({ children }) => (
            <ol className="my-3 list-decimal space-y-1.5 pl-5 text-foreground/85">
              {children}
            </ol>
          ),
          li: ({ children }) => <li className="pl-1">{children}</li>,
          strong: ({ children }) => (
            <strong className="font-semibold text-foreground">{children}</strong>
          ),
          blockquote: ({ children }) => (
            <blockquote className="my-4 border-l-2 border-primary/40 pl-4 text-muted-foreground">
              {children}
            </blockquote>
          ),
          table: ({ children }) => (
            <div className="my-5 overflow-x-auto rounded-lg border">
              <table className="w-full border-collapse text-left text-sm">{children}</table>
            </div>
          ),
          th: ({ children }) => (
            <th className="border-b bg-muted/50 px-3 py-2 font-semibold">{children}</th>
          ),
          td: ({ children }) => (
            <td className="border-b px-3 py-2 align-top last:border-b-0">{children}</td>
          ),
          code: ({ className, children }) => {
            const code = String(children).replace(/\n$/, "");
            if (className === "language-mermaid") {
              return <MermaidDiagram source={code} />;
            }
            return className ? (
              <code className="my-4 block overflow-x-auto rounded-lg bg-muted p-4 font-mono text-xs">
                {code}
              </code>
            ) : (
              <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.85em]">
                {children}
              </code>
            );
          },
        }}
      >
        {markdown}
      </Markdown>
    </div>
  );
}

function MermaidDiagram({ source }: { source: string }) {
  const reactId = useId();
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function renderDiagram(): Promise<void> {
      try {
        const { default: mermaid } = await import("mermaid");
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          theme: document.documentElement.classList.contains("dark") ? "dark" : "base",
          themeVariables: document.documentElement.classList.contains("dark")
            ? undefined
            : {
                primaryColor: "#dbeafe",
                primaryTextColor: "#172033",
                primaryBorderColor: "#60a5fa",
                secondaryColor: "#ccfbf1",
                tertiaryColor: "#ffedd5",
                lineColor: "#94a3b8",
              },
          mindmap: { padding: 20 },
        });
        const id = `audetic-mind-map-${reactId.replace(/:/g, "")}`;
        const rendered = await mermaid.render(id, source);
        if (!cancelled) {
          setSvg(rendered.svg);
          setError(null);
        }
      } catch (cause) {
        if (!cancelled) {
          setSvg(null);
          setError(cause instanceof Error ? cause.message : "Could not render mind map");
        }
      }
    }

    void renderDiagram();
    return () => {
      cancelled = true;
    };
  }, [reactId, source]);

  if (error) {
    return (
      <div className="my-5 rounded-xl border border-destructive/30 bg-destructive/5 p-4">
        <div className="flex items-center gap-2 text-sm font-medium text-destructive">
          <AlertTriangle className="h-4 w-4" />
          Mind map could not be rendered
        </div>
        <p className="mt-1 text-xs text-muted-foreground">{error}</p>
        <pre className="mt-3 overflow-x-auto whitespace-pre-wrap font-mono text-xs">
          {source}
        </pre>
      </div>
    );
  }

  if (!svg) {
    return (
      <div className="my-5 flex min-h-64 items-center justify-center rounded-xl border bg-muted/20 text-sm text-muted-foreground">
        Drawing mind map…
      </div>
    );
  }

  return (
    <div
      className="my-5 min-h-72 overflow-auto rounded-xl border bg-white p-4 text-slate-900 [&_svg]:mx-auto [&_svg]:h-auto [&_svg]:min-w-[42rem] [&_svg]:max-w-none"
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}
