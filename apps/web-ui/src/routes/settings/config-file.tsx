import { useState } from "react";
import { Copy, Check } from "lucide-react";
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

const CONFIG_PATH = "~/.config/audetic/config.toml";

/**
 * The daemon has no PUT /config, so `[whisper]`, `[behavior]`, and
 * `[meeting]` tuning happens by editing the config file directly.
 * Browsers can't shell-open a path, so we just display the path with a
 * Copy button.
 *
 * When the API grows write endpoints these sections get dedicated
 * pages and this one narrows down to "advanced / raw toml".
 */
function SettingsConfigFile() {
  const [copied, setCopied] = useState(false);

  async function handleCopy(): Promise<void> {
    try {
      await navigator.clipboard.writeText(CONFIG_PATH);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard can fail (insecure context, permission). Silently ignore;
      // the path is on screen and selectable.
    }
  }

  return (
    <div className="space-y-6">
      <header>
        <h2 className="text-xl font-semibold">Config file</h2>
        <p className="text-sm text-muted-foreground">
          The daemon has no write endpoints yet, so these sections are
          tuned by editing the config file directly.
        </p>
      </header>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Path</CardTitle>
          <CardDescription>
            Open this file in your editor of choice. The daemon picks up
            changes on next start.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex items-center gap-2">
          <code className="flex-1 rounded bg-muted px-3 py-2 font-mono text-sm">
            {CONFIG_PATH}
          </code>
          <Button variant="outline" size="sm" onClick={handleCopy}>
            {copied ? (
              <>
                <Check className="mr-1 h-3.5 w-3.5" />
                Copied
              </>
            ) : (
              <>
                <Copy className="mr-1 h-3.5 w-3.5" />
                Copy
              </>
            )}
          </Button>
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
