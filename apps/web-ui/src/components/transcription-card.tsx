import { Form } from "react-router-dom";
import type { ReactNode } from "react";
import { Observer } from "mobx-react-lite";
import { Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { HistoryEntry } from "@/stores/history-store";
import { DICTATIONS_INTENTS } from "@/routes/dictations";

export function TranscriptionCard({ entry }: { entry: HistoryEntry }) {
  return (
    <Observer>
      {() => (
        <Card>
          <CardContent className="p-4 space-y-3">
            <p className="text-sm whitespace-pre-wrap">{entry.text}</p>
            <div className="flex flex-wrap gap-1.5 text-[11px] text-muted-foreground">
              {entry.upload_state === "pending" || entry.upload_state === "uploading" ? (
                <StateChip>Pending upload</StateChip>
              ) : null}
              {entry.upload_state === "needs_attention" ? (
                <StateChip>Upload needs attention</StateChip>
              ) : null}
              {entry.offline ? <StateChip>Hub offline</StateChip> : null}
              {entry.read_only ? <StateChip>Read only</StateChip> : null}
              <StateChip>
                {entry.source === "shared" ? "Shared" : "Local"} · {entry.origin_device_id.slice(0, 8)}
              </StateChip>
            </div>
            <div className="flex items-center justify-between text-xs text-muted-foreground">
              <span>{new Date(entry.created_at).toLocaleString()}</span>
              <Form method="post" replace>
                <input type="hidden" name="intent" value={DICTATIONS_INTENTS.copy} />
                <input type="hidden" name="text" value={entry.text} />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button variant="ghost" size="sm" type="submit">
                      <Copy className="mr-1 h-3.5 w-3.5" />
                      Copy
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>Copy to clipboard</TooltipContent>
                </Tooltip>
              </Form>
            </div>
          </CardContent>
        </Card>
      )}
    </Observer>
  );
}

function StateChip({ children }: { children: ReactNode }) {
  return (
    <span className="rounded-full border border-border/70 bg-muted/45 px-2 py-0.5">
      {children}
    </span>
  );
}
