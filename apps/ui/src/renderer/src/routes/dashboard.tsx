import { useEffect, useState } from "react";
import { Observer } from "mobx-react-lite";
import { Copy, Mic, StopCircle } from "lucide-react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { StatusPill } from "@/components/status-pill";
import { useStore } from "@/stores/root-store";
import { daemon } from "@/api/client";

export function Dashboard() {
  const store = useStore();
  const [version, setVersion] = useState<string | null>(null);
  const [providerName, setProviderName] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([daemon.GET("/version"), daemon.GET("/provider")])
      .then(([v, p]) => {
        if (cancelled) return;
        if (v.data) setVersion((v.data as { version: string }).version);
        if (p.data) setProviderName(p.data.provider ?? null);
      })
      .catch(() => {
        /* handled by daemon-down banner */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div className="mx-auto max-w-3xl p-8 space-y-6">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-semibold">Dashboard</h1>
          <p className="text-sm text-muted-foreground">
            Press{" "}
            <kbd className="rounded border px-1.5 py-0.5 font-mono text-xs">
              Super+R
            </kbd>{" "}
            to dictate, or use the buttons below.
          </p>
        </div>
        <StatusPill />
      </header>

      <Card>
        <CardHeader>
          <CardTitle>Recording</CardTitle>
          <CardDescription>
            Toggle the dictation pipeline. Status polls {"<"}1s while active.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <Observer>
            {() => {
              const busy = store.status.isBusy;
              return (
                <Button
                  onClick={() => void store.status.toggle()}
                  variant={busy ? "destructive" : "default"}
                  size="lg"
                >
                  {busy ? (
                    <>
                      <StopCircle className="mr-2 h-4 w-4" />
                      Stop recording
                    </>
                  ) : (
                    <>
                      <Mic className="mr-2 h-4 w-4" />
                      Start recording
                    </>
                  )}
                </Button>
              );
            }}
          </Observer>

          <Observer>
            {() => {
              const err = store.status.lastError;
              if (!err) return null;
              return (
                <div className="text-sm text-destructive">Last error: {err}</div>
              );
            }}
          </Observer>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Last transcription</CardTitle>
        </CardHeader>
        <CardContent>
          <Observer>
            {() => {
              const job = store.status.lastCompletedJob;
              if (!job) {
                return (
                  <p className="text-sm text-muted-foreground">
                    No transcriptions yet. Record something to see it here.
                  </p>
                );
              }
              return (
                <div className="space-y-3">
                  <p className="text-sm whitespace-pre-wrap">{job.text}</p>
                  <div className="flex items-center justify-between text-xs text-muted-foreground">
                    <span>{new Date(job.created_at).toLocaleString()}</span>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        void navigator.clipboard.writeText(job.text);
                      }}
                    >
                      <Copy className="mr-1 h-3.5 w-3.5" />
                      Copy
                    </Button>
                  </div>
                </div>
              );
            }}
          </Observer>
        </CardContent>
      </Card>

      <footer className="flex items-center justify-between pt-2 text-xs text-muted-foreground">
        <span>
          audetic
          {version ? ` v${version}` : ""}
          {providerName ? ` · ${providerName}` : ""}
        </span>
        <span>127.0.0.1:3737</span>
      </footer>
    </div>
  );
}
