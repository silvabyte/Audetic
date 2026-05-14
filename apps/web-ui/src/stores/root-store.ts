import { makeAutoObservable } from "mobx";
import { createContext, useContext } from "react";
import { ConfigStore } from "./config-store";
import { HistoryStore } from "./history-store";
import { MeetingStore } from "./meeting-store";
import { MetaStore } from "./meta-store";
import { OnboardingStore } from "./onboarding-store";
import { PostProcessingStore } from "./post-processing-store";
import { StatusStore } from "./status-store";
import { UiStore } from "./ui-store";

/**
 * RootStore owns every domain / UI store. Stores reach each other via
 * `rootStore.*`. No module-level singletons; construct once at app boot
 * and hand down through React context.
 */
export class RootStore {
  status: StatusStore;
  meta: MetaStore;
  history: HistoryStore;
  meetings: MeetingStore;
  config: ConfigStore;
  postProcessing: PostProcessingStore;
  onboarding: OnboardingStore;
  ui: UiStore;

  constructor() {
    this.status = new StatusStore(this);
    this.meta = new MetaStore(this);
    this.history = new HistoryStore(this);
    this.meetings = new MeetingStore(this);
    this.config = new ConfigStore(this);
    this.postProcessing = new PostProcessingStore(this);
    this.onboarding = new OnboardingStore(this);
    this.ui = new UiStore(this);
    makeAutoObservable(this);
  }

  /** Kick off background polling. Called once at app mount. */
  start(): void {
    this.status.start();
    this.meetings.start();
    // First-run check for ffmpeg. Fire-and-forget — overlay reacts to
    // store state, so a slow daemon doesn't block app render.
    void this.onboarding.check();
    // UiStore.start is async (kept for parity even though localStorage
    // reads are sync). Fire-and-forget — theme flicker is bounded.
    void this.ui.start();
  }

  /** Stop all polling. Called on window close / app quit. */
  stop(): void {
    this.status.stop();
    this.meetings.stop();
  }

  /**
   * True if the daemon is confirmed reachable. Until the first poll
   * completes we return `true` (optimistic) so the daemon-down banner
   * doesn't flash on boot.
   */
  get daemonReachable(): boolean {
    if (!this.status.firstPollDone) return true;
    return this.status.reachable;
  }
}

const RootStoreContext = createContext<RootStore | null>(null);

export const RootStoreProvider = RootStoreContext.Provider;

export function useStore(): RootStore {
  const store = useContext(RootStoreContext);
  if (!store) {
    throw new Error("useStore() must be used inside <RootStoreProvider>");
  }
  return store;
}
