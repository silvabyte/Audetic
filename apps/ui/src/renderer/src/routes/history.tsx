import { useEffect, useRef } from "react";
import { Observer } from "mobx-react-lite";
import {
  Form,
  useNavigation,
  useSearchParams,
  useSubmit,
  type ActionFunctionArgs,
  type LoaderFunctionArgs,
  type RouteObject,
} from "react-router-dom";
import { format, parseISO } from "date-fns";
import { CalendarIcon, Search, X } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import { TranscriptionCard } from "@/components/transcription-card";
import { useStore } from "@/stores/root-store";
import { getRootStore } from "@/stores/singleton";
import type { HistoryQuery } from "@/stores/history-store";
import { cn } from "@/lib/utils";

export const HISTORY_INTENTS = {
  copy: "copy-transcription",
  refresh: "refresh",
} as const;

const SEARCH_DEBOUNCE_MS = 250;
const ISO_DATE = "yyyy-MM-dd";

/**
 * History route — `/history`.
 *
 * The URL is the source of truth for search + date range
 * (`?q=&from=&to=`). The loader reads them, calls `history.load(...)`,
 * and returns null. The component reads entries from the MobX store
 * via `<Observer>` — no `useLoaderData()`.
 *
 * Search-as-you-type is implemented as a debounced form submit
 * (`useSubmit`) that updates the URL in-place (`replace: true`),
 * re-invoking the loader. Debounce lives in the component because it's
 * a UI concern, not a store concern.
 *
 * Date filters use a shadcn Calendar inside a Popover. The Calendar's
 * onSelect patches the hidden input's value and submits the form —
 * keeping the URL as the single source of truth.
 */
export const historyRoute: RouteObject = {
  path: "history",
  loader: async ({ request }: LoaderFunctionArgs) => {
    const url = new URL(request.url);
    const params: HistoryQuery = {
      q: url.searchParams.get("q") ?? undefined,
      from: url.searchParams.get("from") ?? undefined,
      to: url.searchParams.get("to") ?? undefined,
    };
    await getRootStore().history.load(params);
    return null;
  },
  action: async ({ request }: ActionFunctionArgs) => {
    const form = await request.formData();
    const intent = form.get("intent");
    const root = getRootStore();
    switch (intent) {
      case HISTORY_INTENTS.copy: {
        const text = String(form.get("text") ?? "");
        if (text) {
          await navigator.clipboard.writeText(text);
          toast.success("Copied to clipboard");
        }
        return null;
      }
      case HISTORY_INTENTS.refresh:
        await root.history.invalidate();
        return null;
      default:
        return null;
    }
  },
  Component: HistoryRoute,
};

function HistoryRoute() {
  const [searchParams] = useSearchParams();
  const submit = useSubmit();
  const navigation = useNavigation();
  const store = useStore();

  const formRef = useRef<HTMLFormElement>(null);
  const debounceRef = useRef<number | null>(null);

  const routeLoading =
    navigation.state === "loading" &&
    navigation.location?.pathname === "/history";

  // Initial values come from the URL; defaultValue means we don't have
  // to mirror URL into local state.
  const currentQ = searchParams.get("q") ?? "";
  const currentFrom = searchParams.get("from") ?? "";
  const currentTo = searchParams.get("to") ?? "";

  useEffect(() => {
    return () => {
      if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    };
  }, []);

  function scheduleSubmit(replace: boolean): void {
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => {
      if (formRef.current) submit(formRef.current, { replace });
    }, SEARCH_DEBOUNCE_MS);
  }

  function handleDatePick(field: "from" | "to", date: Date | undefined): void {
    if (!formRef.current) return;
    const input = formRef.current.elements.namedItem(field) as
      | HTMLInputElement
      | null;
    if (!input) return;
    input.value = date ? format(date, ISO_DATE) : "";
    // Flush debounce and submit immediately — date clicks are deliberate.
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    submit(formRef.current, { replace: true });
  }

  return (
    <div className="mx-auto max-w-3xl p-8 space-y-6">
      <header>
        <h1 className="text-2xl font-semibold">History</h1>
        <p className="text-sm text-muted-foreground">
          Past dictations. Copy, filter by date, or search across transcripts.
        </p>
      </header>

      <Form
        ref={formRef}
        role="search"
        onChange={() => scheduleSubmit(true)}
        onSubmit={(e) => {
          e.preventDefault();
          if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
          if (formRef.current) submit(formRef.current, { replace: true });
        }}
        className="flex flex-wrap items-end gap-3"
      >
        <div className="flex-1 min-w-[200px] space-y-1">
          <label className="text-xs text-muted-foreground" htmlFor="q">
            Search
          </label>
          <div className="relative">
            <Search className="absolute left-2.5 top-2.5 h-4 w-4 text-muted-foreground pointer-events-none" />
            <Input
              id="q"
              name="q"
              type="search"
              placeholder="Find a transcription…"
              defaultValue={currentQ}
              className="pl-8"
            />
          </div>
        </div>

        <DatePickerField
          label="From"
          name="from"
          value={currentFrom}
          onPick={(d) => handleDatePick("from", d)}
        />
        <DatePickerField
          label="To"
          name="to"
          value={currentTo}
          onPick={(d) => handleDatePick("to", d)}
        />

        {(currentQ || currentFrom || currentTo) && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => {
              if (!formRef.current) return;
              formRef.current.reset();
              submit(new FormData(), { replace: true });
            }}
          >
            <X className="mr-1 h-3.5 w-3.5" />
            Clear
          </Button>
        )}
      </Form>

      <section>
        <Observer>
          {() => {
            if (
              (routeLoading || store.history.isLoading) &&
              store.history.entries.length === 0
            ) {
              return <HistoryListSkeleton />;
            }
            if (store.history.error) {
              return (
                <p className="text-sm text-destructive">
                  {store.history.error}
                </p>
              );
            }
            if (store.history.entries.length === 0) {
              return (
                <p className="text-sm text-muted-foreground">
                  No transcriptions
                  {currentQ || currentFrom || currentTo
                    ? " match the current filters."
                    : " yet. Press Super+R to record one."}
                </p>
              );
            }
            return (
              <ul className="space-y-3">
                {store.history.entries.map((entry) => (
                  <li key={entry.id}>
                    <TranscriptionCard entry={entry} />
                  </li>
                ))}
              </ul>
            );
          }}
        </Observer>
      </section>
    </div>
  );
}

function DatePickerField({
  label,
  name,
  value,
  onPick,
}: {
  label: string;
  name: "from" | "to";
  value: string;
  onPick: (d: Date | undefined) => void;
}) {
  // Hidden input is the authoritative form value. The button is the
  // visible UI that opens the popover.
  const parsed = value ? tryParse(value) : undefined;
  return (
    <div className="space-y-1">
      <label className="text-xs text-muted-foreground" htmlFor={`${name}-btn`}>
        {label}
      </label>
      <input type="hidden" name={name} defaultValue={value} />
      <Popover>
        <PopoverTrigger asChild>
          <Button
            id={`${name}-btn`}
            type="button"
            variant="outline"
            className={cn(
              "w-[9.5rem] justify-start text-left font-normal",
              !parsed && "text-muted-foreground",
            )}
          >
            <CalendarIcon className="mr-2 h-4 w-4" />
            {parsed ? format(parsed, "MMM d, yyyy") : "Any"}
          </Button>
        </PopoverTrigger>
        <PopoverContent align="start" className="w-auto p-0">
          <Calendar
            mode="single"
            selected={parsed}
            onSelect={onPick}
            autoFocus
          />
          {parsed && (
            <div className="border-t p-2">
              <Button
                type="button"
                variant="ghost"
                size="sm"
                className="w-full"
                onClick={() => onPick(undefined)}
              >
                <X className="mr-1 h-3.5 w-3.5" />
                Clear
              </Button>
            </div>
          )}
        </PopoverContent>
      </Popover>
    </div>
  );
}

function tryParse(iso: string): Date | undefined {
  try {
    const d = parseISO(iso);
    return Number.isNaN(d.getTime()) ? undefined : d;
  } catch {
    return undefined;
  }
}

function HistoryListSkeleton() {
  return (
    <ul className="space-y-3">
      {[0, 1, 2].map((i) => (
        <li key={i}>
          <Card>
            <CardContent className="p-4 space-y-3">
              <Skeleton className="h-4 w-full" />
              <Skeleton className="h-4 w-5/6" />
              <div className="flex items-center justify-between">
                <Skeleton className="h-3 w-36" />
                <Skeleton className="h-7 w-16" />
              </div>
            </CardContent>
          </Card>
        </li>
      ))}
    </ul>
  );
}
