import { Observer } from "mobx-react-lite";
import { useEffect, useMemo, useState } from "react";
import { type RouteObject } from "react-router-dom";
import { PlayCircle, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import type { EventKind, Job, NewJob } from "@/api/post-processing";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

export const settingsPostProcessingRoute: RouteObject = {
  path: "post-processing",
  loader: async () => {
    // Fire both reads in parallel; the page renders skeletons until each
    // section flips to `loaded`.
    void getRootStore().postProcessing.loadAll();
    return null;
  },
  Component: SettingsPostProcessing,
};

function SettingsPostProcessing(): React.JSX.Element {
  return (
    <div className="space-y-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-xl font-semibold">Post-processing</h2>
          <p className="text-sm text-muted-foreground">
            Run shell commands when Audetic events fire. Each command receives a
            JSON envelope on stdin —{" "}
            <code className="font-mono text-xs">jq</code> it or use the embedded
            identifiers (<code className="font-mono text-xs">dictation_id</code>,{" "}
            <code className="font-mono text-xs">meeting_id</code>) with the
            daemon API.
          </p>
        </div>
        <NewJobButton />
      </header>

      <JobsList />
    </div>
  );
}

function NewJobButton(): React.JSX.Element {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button onClick={() => setOpen(true)} size="sm">
        <Plus className="mr-1 h-4 w-4" />
        New job
      </Button>
      <JobFormDialog open={open} onOpenChange={setOpen} mode="create" />
    </>
  );
}

function JobsList(): React.JSX.Element {
  const store = useStore();

  return (
    <Observer>
      {() => {
        const state = store.postProcessing.jobsState;
        const jobs = store.postProcessing.jobs;

        if (state === "loading" && jobs.length === 0) {
          return (
            <Card>
              <CardContent className="space-y-3 p-6">
                <Skeleton className="h-4 w-2/3" />
                <Skeleton className="h-3 w-1/2" />
              </CardContent>
            </Card>
          );
        }
        if (state === "error" && jobs.length === 0) {
          return (
            <Card>
              <CardContent className="p-6 text-sm text-destructive">
                Couldn't load jobs.
              </CardContent>
            </Card>
          );
        }
        if (jobs.length === 0) {
          return (
            <Card>
              <CardContent className="p-6 text-sm text-muted-foreground">
                No post-processing jobs yet. Click <strong>New job</strong> to
                add one.
              </CardContent>
            </Card>
          );
        }

        return (
          <div className="space-y-3">
            {jobs.map((job) => (
              <JobCard key={job.id} job={job} />
            ))}
          </div>
        );
      }}
    </Observer>
  );
}

function JobCard({ job }: { job: Job }): React.JSX.Element {
  const store = useStore();
  const [editing, setEditing] = useState(false);
  const [testing, setTesting] = useState(false);

  return (
    <Observer>
      {() => {
        const result = store.postProcessing.lastTest[job.id];
        return (
          <Card>
            <CardHeader className="flex-row items-start justify-between gap-4 space-y-0">
              <div className="min-w-0 space-y-1">
                <CardTitle className="text-base">
                  <span className="mr-2 inline-block rounded bg-muted px-1.5 py-0.5 font-mono text-xs">
                    {job.event}
                  </span>
                  {job.name}
                </CardTitle>
                <CardDescription className="break-all font-mono text-xs">
                  {job.action.command}
                </CardDescription>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                <Switch
                  checked={job.enabled}
                  onCheckedChange={async (next) => {
                    await store.postProcessing.toggleEnabled(job.id, next);
                  }}
                  aria-label={job.enabled ? "Disable job" : "Enable job"}
                />
                <Button
                  variant="outline"
                  size="sm"
                  disabled={testing}
                  onClick={async () => {
                    setTesting(true);
                    const r = await store.postProcessing.testJob(job.id);
                    setTesting(false);
                    if (r) {
                      if (r.success) {
                        toast.success(`Job test ok (exit ${r.exit_code ?? "?"})`);
                      } else {
                        toast.error("Job test failed", {
                          description: r.timed_out
                            ? "Timed out"
                            : `exit ${r.exit_code ?? "?"}`,
                        });
                      }
                    }
                  }}
                >
                  <PlayCircle className="mr-1 h-3.5 w-3.5" />
                  {testing ? "Testing…" : "Test"}
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setEditing(true)}
                >
                  Edit
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={async () => {
                    if (
                      window.confirm(
                        `Delete job "${job.name}"? This can't be undone.`,
                      )
                    ) {
                      const ok = await store.postProcessing.deleteJob(job.id);
                      if (ok) toast.success("Job deleted");
                    }
                  }}
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              </div>
            </CardHeader>
            {result && (
              <CardContent className="space-y-2">
                <div className="text-xs text-muted-foreground">
                  Last test:{" "}
                  {result.success
                    ? `success (exit ${result.exit_code ?? "?"})`
                    : result.timed_out
                      ? "timed out"
                      : `failed (exit ${result.exit_code ?? "?"})`}
                </div>
                {result.stdout && (
                  <details>
                    <summary className="cursor-pointer text-xs text-muted-foreground">
                      stdout ({result.stdout.length} chars)
                    </summary>
                    <pre className="mt-1 overflow-x-auto rounded bg-muted p-2 text-xs">
                      {result.stdout}
                    </pre>
                  </details>
                )}
                {result.stderr && (
                  <details>
                    <summary className="cursor-pointer text-xs text-muted-foreground">
                      stderr ({result.stderr.length} chars)
                    </summary>
                    <pre className="mt-1 overflow-x-auto rounded bg-muted p-2 text-xs">
                      {result.stderr}
                    </pre>
                  </details>
                )}
              </CardContent>
            )}
            <JobFormDialog
              open={editing}
              onOpenChange={setEditing}
              mode="edit"
              job={job}
            />
          </Card>
        );
      }}
    </Observer>
  );
}

interface JobFormProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  mode: "create" | "edit";
  job?: Job;
}

function JobFormDialog({
  open,
  onOpenChange,
  mode,
  job,
}: JobFormProps): React.JSX.Element {
  const store = useStore();
  const [name, setName] = useState(job?.name ?? "");
  const [event, setEvent] = useState<EventKind>(
    (job?.event as EventKind) ?? "dictation.completed",
  );
  const [command, setCommand] = useState(job?.action.command ?? "");
  const [timeout, setTimeout] = useState(
    String(job?.action.timeout_seconds ?? 3600),
  );
  const [submitting, setSubmitting] = useState(false);

  // Reset the form when reopening for a different job.
  useEffect(() => {
    if (open) {
      setName(job?.name ?? "");
      setEvent((job?.event as EventKind) ?? "dictation.completed");
      setCommand(job?.action.command ?? "");
      setTimeout(String(job?.action.timeout_seconds ?? 3600));
    }
  }, [open, job]);

  const events = store.postProcessing.events;

  const eventOptions = useMemo(() => {
    if (events.length > 0) return events;
    // Fall back to the two v1 kinds so the form still works if the
    // events fetch failed (e.g. daemon was momentarily unreachable).
    return [
      {
        name: "dictation.completed" as const,
        label: "Dictation completed",
        description: "",
      },
      {
        name: "meeting.completed" as const,
        label: "Meeting completed",
        description: "",
      },
    ];
  }, [events]);

  async function onSubmit(e: React.FormEvent) {
    e.preventDefault();
    const trimmedName = name.trim();
    const trimmedCommand = command.trim();
    if (!trimmedName) {
      toast.error("Name is required");
      return;
    }
    if (!trimmedCommand) {
      toast.error("Command is required");
      return;
    }
    const timeoutNum = Number(timeout) || 3600;

    setSubmitting(true);
    try {
      if (mode === "create") {
        const payload: NewJob = {
          name: trimmedName,
          event,
          action: {
            type: "command",
            command: trimmedCommand,
            timeout_seconds: timeoutNum,
          },
          enabled: true,
        };
        const created = await store.postProcessing.createJob(payload);
        if (created) {
          toast.success("Job created");
          onOpenChange(false);
        } else {
          toast.error("Couldn't create job", {
            description: store.postProcessing.lastError ?? undefined,
          });
        }
      } else if (job) {
        const updated = await store.postProcessing.updateJob(job.id, {
          name: trimmedName,
          event,
          action: {
            type: "command",
            command: trimmedCommand,
            timeout_seconds: timeoutNum,
          },
        });
        if (updated) {
          toast.success("Job updated");
          onOpenChange(false);
        } else {
          toast.error("Couldn't update job", {
            description: store.postProcessing.lastError ?? undefined,
          });
        }
      }
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={onSubmit} className="space-y-4">
          <DialogHeader>
            <DialogTitle>
              {mode === "create" ? "New post-processing job" : "Edit job"}
            </DialogTitle>
            <DialogDescription>
              The command runs via <code className="font-mono">sh -c</code> with
              the event JSON envelope on stdin. Use{" "}
              <code className="font-mono">jq</code> to extract the{" "}
              <code className="font-mono">data</code> object.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-2">
            <Label htmlFor="pp-name">Name</Label>
            <Input
              id="pp-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. Save transcript to notes"
              autoComplete="off"
              required
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="pp-event">Event</Label>
            <select
              id="pp-event"
              className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm focus:outline-none focus:ring-1 focus:ring-ring"
              value={event}
              onChange={(e) => setEvent(e.target.value as EventKind)}
            >
              {eventOptions.map((opt) => (
                <option key={opt.name} value={opt.name}>
                  {opt.label} ({opt.name})
                </option>
              ))}
            </select>
          </div>

          <div className="space-y-2">
            <Label htmlFor="pp-command">Command</Label>
            <textarea
              id="pp-command"
              className="flex min-h-[5rem] w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-xs shadow-sm focus:outline-none focus:ring-1 focus:ring-ring"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder={'jq -r .data.text > /tmp/last-dictation.txt'}
              required
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="pp-timeout">Timeout (seconds)</Label>
            <Input
              id="pp-timeout"
              type="number"
              min={1}
              value={timeout}
              onChange={(e) => setTimeout(e.target.value)}
            />
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={submitting}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={submitting}>
              {submitting
                ? "Saving…"
                : mode === "create"
                  ? "Create"
                  : "Save changes"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
