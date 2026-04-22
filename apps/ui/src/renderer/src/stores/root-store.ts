import { makeAutoObservable } from "mobx";
import { createContext, useContext } from "react";
import { StatusStore } from "./status-store";

/**
 * RootStore owns every domain / UI store. Stores reach each other via
 * `rootStore.*`. No module-level singletons; construct once at app boot
 * and hand down through React context.
 */
export class RootStore {
  status: StatusStore;

  constructor() {
    this.status = new StatusStore(this);
    makeAutoObservable(this);
  }

  /** Kick off background polling. Called once at app mount. */
  start(): void {
    this.status.start();
  }

  /** Stop all polling. Called on window close / app quit. */
  stop(): void {
    this.status.stop();
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
