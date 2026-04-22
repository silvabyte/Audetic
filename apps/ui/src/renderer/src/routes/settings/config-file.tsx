import { useState } from "react";
import { FileText, Check, TriangleAlert } from "lucide-react";
import type { RouteObject } from "react-router-dom";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

export const settingsConfigFileRoute: RouteObject = {
  path: "config-file",
  Component: SettingsConfigFile,
};

/**
 * The daemon has no PUT /config, so `[whisper]`, `[behavior]`, and
 * `[meeting]` tuning happens by editing ~/.config/audetic/config.toml
 * directly. This page is the escape hatch — opens the file via the
 * preload bridge → main → shell.openPath.
 *
 * When the API grows write endpoints these sections get dedicated
 * pages and this one narrows down to "advanced / raw toml".
 */
function SettingsConfigFile() {
  const [state, setState] = useState<"idle" | "opening" | "ok" | "error">("idle");
  const [message, setMessage] = useState<string | null>(null);

  async function handleOpen(): Promise<void> {
    setState("opening");
    setMessage(null);
    try {
      const err = await window.audetic.openConfigFile();
      if (err) {
        setState("error");
        setMessage(err);
      } else {
        setState("ok");
      }
    } catch (e) {
      setState("error");
      setMessage(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-xl font-semibold">Config file</h2>
        <p className="text-sm text-muted-foreground">
          The daemon has no write endpoints yet, so these sections are
          tuned by editing{" "}
          <code className="font-mono text-xs">
            ~/.config/audetic/config.toml
          </code>{" "}
          directly.
        </p>
      </header>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Open in your editor</CardTitle>
          <CardDescription>
            Opens the file through{" "}
            <code className="font-mono text-xs">shell.openPath</code> —
            system will hand it to whatever handler owns{" "}
            <code className="font-mono text-xs">.toml</code> (typically
            your default editor).
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <Button onClick={handleOpen} disabled={state === "opening"}>
            <FileText className="mr-2 h-4 w-4" />
            {state === "opening" ? "Opening…" : "Open config.toml"}
          </Button>
          {state === "ok" && (
            <div className="flex items-center gap-2 text-sm text-primary">
              <Check className="h-4 w-4" />
              Opened.
            </div>
          )}
          {state === "error" && message && (
            <div className="flex items-start gap-2 text-sm text-destructive">
              <TriangleAlert className="mt-0.5 h-4 w-4 shrink-0" />
              <span>{message}</span>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">What's in there</CardTitle>
        </CardHeader>
        <CardContent>
          <dl className="grid grid-cols-[8rem_1fr] gap-y-2 text-sm">
            <dt className="font-mono text-xs text-muted-foreground">[whisper]</dt>
            <dd>Provider, model, language, API key, endpoint.</dd>

            <dt className="font-mono text-xs text-muted-foreground">[behavior]</dt>
            <dd>auto_paste, preserve_clipboard, delete_audio_files, audio_feedback.</dd>

            <dt className="font-mono text-xs text-muted-foreground">[meeting]</dt>
            <dd>post_command, post_command_timeout_seconds.</dd>

            <dt className="font-mono text-xs text-muted-foreground">[wayland]</dt>
            <dd>input_method (e.g. wtype, ydotool).</dd>

            <dt className="font-mono text-xs text-muted-foreground">[ui]</dt>
            <dd>notification_color, waybar text / tooltips.</dd>
          </dl>
        </CardContent>
      </Card>
    </div>
  );
}
