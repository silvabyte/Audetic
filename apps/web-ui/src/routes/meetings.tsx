import { useRef, useState } from "react";
import { Observer } from "mobx-react-lite";
import {
  useFetcher,
  type ActionFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { ChevronDown, Radio, Upload } from "lucide-react";
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
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import { MeetingTitlePickerContent } from "@/components/meeting-title-picker";
import { MeetingRow } from "@/components/meeting-row";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";

export const MEETING_INTENTS = {
  start: "start-meeting",
  stop: "stop-meeting",
  confirm: "confirm-meeting",
  cancel: "cancel-meeting",
} as const;

/// Accepted media extensions, mirrored from the daemon's
/// `mime_type_for_extension` in `crates/audetic/src/transcription/jobs_client.rs`.
/// The daemon is the source of truth — this is a UX nicety so the file picker
/// doesn't even surface unsupported formats. The daemon will still reject
/// anything outside this list with a 400.
const ACCEPTED_EXTENSIONS = [
  ".wav",
  ".mp3",
  ".m4a",
  ".flac",
  ".ogg",
  ".opus",
  ".mp4",
  ".mkv",
  ".webm",
  ".avi",
  ".mov",
] as const;
const ACCEPTED_ATTR = ACCEPTED_EXTENSIONS.join(",");

/**
 * /meetings — list + immediate start control.
 *
 * Loader kicks off the list fetch (idempotent). The banner on the
 * AppShell handles in-flight meetings; this page is for "what's in
 * the backlog" and "start a new one".
 *
 * Action owns toast side-effects for the three intents. Store methods
 * set `lastError` on failure; we diff it pre/post to decide whether
 * to fire a toast, without putting toast calls inside the store
 * itself (keeps stores framework-agnostic per feedback_mobx.md).
 */
export const meetingsRoute: RouteObject = {
  path: "meetings",
  loader: async () => {
    await getRootStore().meetings.loadList();
    return null;
  },
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    switch (intent) {
      case MEETING_INTENTS.start: {
        const title = String(form.get("title") ?? "").trim() || undefined;
        const errBefore = root.meetings.lastError;
        await root.meetings.startMeeting(title);
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't start meeting", { description: errAfter });
        }
        return null;
      }
      case MEETING_INTENTS.stop: {
        const errBefore = root.meetings.lastError;
        await root.meetings.stopMeeting();
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't stop meeting", { description: errAfter });
        }
        return null;
      }
      case MEETING_INTENTS.confirm: {
        const start = parseOptionalSeconds(form.get("start_seconds"));
        const end = parseOptionalSeconds(form.get("end_seconds"));
        const errBefore = root.meetings.lastError;
        await root.meetings.confirmMeeting(start, end);
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't send for transcription", {
            description: errAfter,
          });
        } else {
          toast.success("Sent for transcription");
        }
        return null;
      }
      case MEETING_INTENTS.cancel: {
        const errBefore = root.meetings.lastError;
        await root.meetings.cancelMeeting();
        const errAfter = root.meetings.lastError;
        if (errAfter && errAfter !== errBefore) {
          toast.error("Couldn't cancel meeting", { description: errAfter });
        } else {
          toast.success("Meeting cancelled");
        }
        return null;
      }
      default:
        return null;
    }
  },
  Component: MeetingsRoute,
};

/**
 * Parse a form field into seconds, or `undefined` when blank/absent. Used by
 * the confirm intent — an absent bound means "keep that edge of the
 * recording" rather than trimming to zero.
 */
function parseOptionalSeconds(raw: FormDataEntryValue | null): number | undefined {
  if (raw === null) return undefined;
  const s = String(raw).trim();
  if (s === "") return undefined;
  const n = Number(s);
  return Number.isFinite(n) ? n : undefined;
}

function MeetingsRoute() {
  return (
    <ImportDropZone>
      <div className="mx-auto max-w-3xl space-y-6 p-4 sm:p-8">
        <header className="flex flex-col items-start justify-between gap-4 sm:flex-row">
          <div>
            <h1 className="text-2xl font-semibold">Meetings</h1>
            <p className="text-sm text-muted-foreground">
              Long-form recordings. Press{" "}
              <kbd className="rounded border px-1.5 py-0.5 font-mono text-xs">
                Super+Shift+R
              </kbd>{" "}
              to toggle via hotkey, or drop an audio/video file to import.
            </p>
          </div>
          <div className="flex w-full flex-wrap items-center justify-end gap-2 sm:w-auto">
            <ImportFileButton />
            <StartMeetingButton />
          </div>
        </header>

        <MeetingList />
      </div>
    </ImportDropZone>
  );
}

/**
 * Wraps the route in a page-level drop target. Hovers light up an
 * overlay; releasing a file kicks off the import. Multi-file drops
 * import each in sequence (one POST each — the daemon happily
 * processes them concurrently).
 *
 * Drop events use a ref-based counter to handle nested dragenter/leave
 * cleanly. Without it, dragging over a child element fires `dragleave`
 * on the parent and the overlay flickers.
 */
function ImportDropZone({ children }: { children: React.ReactNode }) {
  const store = useStore();
  const [dragging, setDragging] = useState(false);
  const counter = useRef(0);

  function isFileDrag(e: React.DragEvent<HTMLDivElement>): boolean {
    return Array.from(e.dataTransfer.types).includes("Files");
  }

  function handleDragEnter(e: React.DragEvent<HTMLDivElement>): void {
    if (!isFileDrag(e)) return;
    counter.current += 1;
    setDragging(true);
  }

  function handleDragLeave(e: React.DragEvent<HTMLDivElement>): void {
    if (!isFileDrag(e)) return;
    counter.current = Math.max(0, counter.current - 1);
    if (counter.current === 0) setDragging(false);
  }

  function handleDragOver(e: React.DragEvent<HTMLDivElement>): void {
    if (!isFileDrag(e)) return;
    // Must call preventDefault on dragover, otherwise drop never fires.
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  }

  async function handleDrop(e: React.DragEvent<HTMLDivElement>): Promise<void> {
    if (!isFileDrag(e)) return;
    e.preventDefault();
    counter.current = 0;
    setDragging(false);
    const files = Array.from(e.dataTransfer.files);
    if (files.length === 0) return;
    await importFiles(store, files);
  }

  return (
    <div
      className="relative"
      onDragEnter={handleDragEnter}
      onDragLeave={handleDragLeave}
      onDragOver={handleDragOver}
      onDrop={handleDrop}
    >
      {children}
      {dragging && (
        <div className="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-background/80 backdrop-blur-sm">
          <div className="rounded-lg border-2 border-dashed border-primary px-12 py-10 text-center">
            <Upload className="mx-auto h-10 w-10 text-primary" />
            <p className="mt-3 text-base font-medium">
              Drop to import as a meeting
            </p>
            <p className="text-xs text-muted-foreground">
              Audio (wav/mp3/m4a/flac/ogg/opus) or video (mp4/mkv/webm/avi/mov)
            </p>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * Button-driven import — opens the native file picker. Same code path as
 * drag-and-drop, just sourced from `<input type="file">`. Multi-select
 * imports each selected file.
 */
function ImportFileButton() {
  const store = useStore();
  const inputRef = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);

  async function handleChange(
    e: React.ChangeEvent<HTMLInputElement>,
  ): Promise<void> {
    const files = e.target.files ? Array.from(e.target.files) : [];
    // Reset value so re-selecting the same file fires `onChange` again.
    e.target.value = "";
    if (files.length === 0) return;
    setBusy(true);
    try {
      await importFiles(store, files);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <input
        ref={inputRef}
        type="file"
        accept={ACCEPTED_ATTR}
        multiple
        hidden
        onChange={handleChange}
      />
      <Button
        type="button"
        variant="outline"
        disabled={busy}
        onClick={() => inputRef.current?.click()}
      >
        <Upload className="mr-2 h-4 w-4" />
        {busy ? "Importing…" : "Import file"}
      </Button>
    </>
  );
}

/**
 * Shared import flow used by both the drop zone and the file-picker
 * button. Uploads each file in sequence and toasts per file. Sequential
 * (not parallel) so the user gets coherent progress feedback in the
 * toast stream — the daemon can run them concurrently if it wants.
 */
async function importFiles(
  store: ReturnType<typeof useStore>,
  files: File[],
): Promise<void> {
  for (const file of files) {
    const errBefore = store.meetings.lastError;
    const result = await store.meetings.importFile(file);
    const errAfter = store.meetings.lastError;
    if (result) {
      toast.success(`Imported ${file.name}`, {
        description: `Meeting #${result.meetingId} — transcription running.`,
      });
    } else if (errAfter && errAfter !== errBefore) {
      toast.error(`Couldn't import ${file.name}`, { description: errAfter });
    }
  }
}

function StartMeetingButton() {
  const store = useStore();
  const fetcher = useFetcher();
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState("");
  const submitting = fetcher.state !== "idle";

  function startWithTitle(selectedTitle: string): void {
    fetcher.submit(
      { intent: MEETING_INTENTS.start, title: selectedTitle },
      { method: "post", action: "/meetings" },
    );
    setOpen(false);
    setTitle("");
  }

  return (
    <Observer>
      {() => {
        const { active, phase } = store.meetings;
        const inProgress =
          active ||
          phase === "review" ||
          phase === "compressing" ||
          phase === "transcribing" ||
          phase === "running_hook";

        return (
          <div className="inline-flex rounded-md shadow-xs">
            <fetcher.Form method="post" action="/meetings">
              <input type="hidden" name="intent" value={MEETING_INTENTS.start} />
              <Button
                type="submit"
                disabled={inProgress || submitting}
                className="rounded-r-none"
              >
                <Radio className="mr-2 h-4 w-4" />
                {submitting ? "Starting…" : "New meeting"}
              </Button>
            </fetcher.Form>
            <Popover
              open={open}
              onOpenChange={(nextOpen) => {
                setOpen(nextOpen);
                if (!nextOpen) setTitle("");
              }}
            >
              <PopoverTrigger asChild>
                <Button
                  type="button"
                  disabled={inProgress || submitting}
                  className="rounded-l-none border-l border-primary-foreground/20 px-2"
                  aria-label="Start meeting with a title"
                >
                  <ChevronDown className="h-4 w-4" />
                </Button>
              </PopoverTrigger>
              <PopoverContent
                align="end"
                className="w-80 p-2"
                aria-label="Choose a meeting title"
              >
                <div className="px-2 pb-2 pt-1">
                  <p className="text-xs font-medium">Start with a title</p>
                  <p className="text-[11px] text-muted-foreground">
                    Search recent titles or enter a new one.
                  </p>
                </div>
                <MeetingTitlePickerContent
                  value={title}
                  onValueChange={setTitle}
                  onSubmit={startWithTitle}
                  submitLabel="Start"
                  disabled={submitting}
                />
              </PopoverContent>
            </Popover>
          </div>
        );
      }}
    </Observer>
  );
}

function MeetingList() {
  const store = useStore();
  return (
    <section className="space-y-3">
      <Observer>
        {() => {
          if (
            store.meetings.listStatus === "loading" &&
            store.meetings.list.length === 0
          ) {
            return <MeetingListSkeleton />;
          }
          if (store.meetings.listError) {
            return (
              <Card>
                <CardHeader>
                  <CardTitle className="text-base text-destructive">
                    Couldn't load meetings
                  </CardTitle>
                  <CardDescription>{store.meetings.listError}</CardDescription>
                </CardHeader>
              </Card>
            );
          }
          if (store.meetings.list.length === 0) {
            return (
              <Card>
                <CardContent className="p-6 text-sm text-muted-foreground">
                  No meetings yet. Start one above or press Super+Shift+R.
                </CardContent>
              </Card>
            );
          }
          return (
            <ul className="space-y-3">
              {store.meetings.list.map((m) => (
                <li key={m.id}>
                  <MeetingRow meeting={m} />
                </li>
              ))}
            </ul>
          );
        }}
      </Observer>
    </section>
  );
}

function MeetingListSkeleton() {
  return (
    <ul className="space-y-3">
      {[0, 1, 2].map((i) => (
        <li key={i}>
          <Card>
            <CardContent className="p-4 flex items-center gap-4">
              <Skeleton className="h-5 w-5 rounded-full" />
              <div className="min-w-0 flex-1 space-y-2">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-3 w-60" />
              </div>
              <Skeleton className="h-5 w-20 rounded-full" />
            </CardContent>
          </Card>
        </li>
      ))}
    </ul>
  );
}
