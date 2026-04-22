import { useEffect } from "react";
import { reaction } from "mobx";
import { useNavigate } from "react-router-dom";
import { useStore } from "@/stores/root-store";

/**
 * Watches `meetings.pendingNavigationId` and jumps the router to the
 * matching detail page exactly once per completion. The store sets
 * `pendingNavigationId` when it sees a phase transition into
 * `completed`; this hook consumes + clears it.
 *
 * Mount once at the App shell level. No UI of its own.
 */
export function MeetingAutoNav(): null {
  const navigate = useNavigate();
  const store = useStore();

  useEffect(() => {
    const dispose = reaction(
      () => store.meetings.pendingNavigationId,
      (id) => {
        if (id === null) return;
        navigate(`/meetings/${id}`);
        store.meetings.clearPendingNavigation();
      },
    );
    return dispose;
  }, [navigate, store]);

  return null;
}
