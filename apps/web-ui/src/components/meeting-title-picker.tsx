import { useEffect, useRef } from "react";
import { Observer } from "mobx-react-lite";
import { CornerDownLeft, Loader2, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useStore } from "@/stores/root-store";

type RecentTitleSuggestionsProps = {
  query: string;
  onSelect: (title: string) => void;
  disabled?: boolean;
};

export function RecentTitleSuggestions({
  query,
  onSelect,
  disabled = false,
}: RecentTitleSuggestionsProps) {
  const store = useStore();

  useEffect(() => {
    void store.meetings.loadRecentTitles();
  }, [store]);

  return (
    <Observer>
      {() => {
        const normalized = query.trim().toLocaleLowerCase();
        const titles = store.meetings.recentTitles.filter((title) =>
          title.toLocaleLowerCase().includes(normalized),
        );

        if (store.meetings.recentTitlesStatus === "loading") {
          return (
            <div className="flex items-center gap-2 px-2 py-3 text-xs text-muted-foreground">
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
              Loading recent titles…
            </div>
          );
        }

        if (store.meetings.recentTitlesError) {
          return (
            <p className="px-2 py-3 text-xs text-muted-foreground" role="status">
              Recent titles are unavailable. You can still enter a title.
            </p>
          );
        }

        if (titles.length === 0) {
          return (
            <p className="px-2 py-3 text-xs text-muted-foreground">
              {normalized ? "No matching recent titles." : "No recent titles yet."}
            </p>
          );
        }

        return (
          <div className="max-h-48 overflow-y-auto py-1" role="listbox" aria-label="Recent meeting titles">
            {titles.map((title) => (
              <button
                key={title}
                type="button"
                role="option"
                aria-selected={title === query.trim()}
                disabled={disabled}
                className="flex w-full items-center rounded-sm px-2 py-1.5 text-left text-sm outline-none transition-colors hover:bg-muted focus-visible:bg-muted disabled:pointer-events-none disabled:opacity-50"
                onClick={() => onSelect(title)}
                onKeyDown={(event) => {
                  if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
                    return;
                  }
                  event.preventDefault();
                  const options = Array.from(
                    event.currentTarget
                      .closest('[role="listbox"]')
                      ?.querySelectorAll<HTMLButtonElement>('[role="option"]') ?? [],
                  );
                  const current = options.indexOf(event.currentTarget);
                  const next =
                    event.key === "Home"
                      ? 0
                      : event.key === "End"
                        ? options.length - 1
                        : event.key === "ArrowDown"
                          ? Math.min(current + 1, options.length - 1)
                          : Math.max(current - 1, 0);
                  options[next]?.focus();
                }}
              >
                <span className="truncate">{title}</span>
              </button>
            ))}
          </div>
        );
      }}
    </Observer>
  );
}

type MeetingTitlePickerContentProps = {
  value: string;
  onValueChange: (value: string) => void;
  onSubmit: (title: string) => void;
  submitLabel: string;
  disabled?: boolean;
};

export function MeetingTitlePickerContent({
  value,
  onValueChange,
  onSubmit,
  submitLabel,
  disabled = false,
}: MeetingTitlePickerContentProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const trimmed = value.trim();

  return (
    <form
      className="space-y-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (trimmed) onSubmit(trimmed);
      }}
    >
      <div className="relative">
        <Search className="pointer-events-none absolute left-2.5 top-2.5 h-3.5 w-3.5 text-muted-foreground" />
        <Input
          ref={inputRef}
          value={value}
          onChange={(event) => onValueChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              inputRef.current
                ?.closest("form")
                ?.querySelector<HTMLButtonElement>('[role="option"]')
                ?.focus();
            }
          }}
          placeholder="Search or enter a title"
          autoComplete="off"
          autoFocus
          className="h-9 pl-8"
          aria-label="Meeting title"
          disabled={disabled}
        />
      </div>

      <div className="border-y">
        <RecentTitleSuggestions
          query={value}
          onSelect={onSubmit}
          disabled={disabled}
        />
      </div>

      <Button
        type="submit"
        variant="ghost"
        size="sm"
        className="h-8 w-full justify-between px-2 text-xs"
        disabled={!trimmed || disabled}
      >
        <span className="truncate">{trimmed ? `${submitLabel} “${trimmed}”` : "Enter a title"}</span>
        <CornerDownLeft className="ml-2 h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      </Button>
    </form>
  );
}
