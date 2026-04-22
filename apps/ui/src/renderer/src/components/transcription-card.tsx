import { Form } from "react-router-dom";
import { Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { HistoryEntry } from "@/stores/history-store";
import { HISTORY_INTENTS } from "@/routes/history";

export function TranscriptionCard({ entry }: { entry: HistoryEntry }) {
  return (
    <Card>
      <CardContent className="p-4 space-y-3">
        <p className="text-sm whitespace-pre-wrap">{entry.text}</p>
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span>{new Date(entry.created_at).toLocaleString()}</span>
          <Form method="post" replace>
            <input type="hidden" name="intent" value={HISTORY_INTENTS.copy} />
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
  );
}
