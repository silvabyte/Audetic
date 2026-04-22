import { useEffect, useState } from "react";
import { Button } from "./components/ui/button";
import { daemon } from "./api/client";

type VersionInfo = { name: string; version: string };

export function App() {
  const [version, setVersion] = useState<VersionInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    daemon
      .GET("/version")
      .then(({ data, error }) => {
        if (cancelled) return;
        if (error) throw new Error(String(error));
        // schema.ts types `/version` as generic JSON; narrow here.
        setVersion(data as VersionInfo);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <main className="min-h-screen p-10 flex items-center justify-center">
      <div className="w-full max-w-md rounded-lg border bg-card text-card-foreground shadow-sm p-6 space-y-4">
        <header>
          <h1 className="text-xl font-semibold">Audetic</h1>
          <p className="text-sm text-muted-foreground">
            Desktop UI — phase 0c smoke test
          </p>
        </header>
        <div className="text-sm">
          {error ? (
            <div className="text-destructive">Daemon error: {error}</div>
          ) : version ? (
            <div>
              Connected to <span className="font-mono">{version.name}</span>{" "}
              v<span className="font-mono">{version.version}</span>
            </div>
          ) : (
            <div className="text-muted-foreground">Connecting to daemon…</div>
          )}
        </div>
        <Button onClick={() => window.location.reload()}>Reload</Button>
      </div>
    </main>
  );
}
