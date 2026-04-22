import type { RootStore } from "./root-store";

/**
 * Module-level RootStore reference, set once by main.tsx after
 * construction. Loaders and actions run outside React's render cycle
 * (before context is available), so they can't use `useStore()`. This
 * is the accepted workaround — route-level code imports
 * `getRootStore()`; component code still uses `useStore()`.
 *
 * Rule (see feedback_mobx.md): module-level *reference* to a store is
 * OK, module-level *instance creation* of a store is not.
 */
let rootStore: RootStore | null = null;

export function setRootStore(store: RootStore): void {
  rootStore = store;
}

export function getRootStore(): RootStore {
  if (!rootStore) {
    throw new Error(
      "RootStore not initialized — main.tsx must call setRootStore() before any loader runs",
    );
  }
  return rootStore;
}
