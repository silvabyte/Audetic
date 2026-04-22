import React from "react";
import ReactDOM from "react-dom/client";
import { RouterProvider } from "react-router-dom";
import { configure } from "mobx";
import { RootStore, RootStoreProvider } from "./stores/root-store";
import { setRootStore } from "./stores/singleton";
import { createRouter } from "./router";
import "./index.css";

// MobX strict mode — fail loud in dev if any of our rules break.
configure({
  enforceActions: "always",
  computedRequiresReaction: true,
  reactionRequiresObservable: true,
  observableRequiresReaction: true,
  disableErrorBoundaries: false,
});

const rootStore = new RootStore();
// Register the singleton BEFORE `createRouter()` — that call eagerly
// runs the initial-match loader, which imports `getRootStore()`.
setRootStore(rootStore);
rootStore.start();

const router = createRouter();

if (import.meta.env.DEV) {
  // Expose for chrome-devtools-mcp evaluate_script assertions during smoke tests.
  // Never used at runtime.
  (window as unknown as {
    __rootStore: RootStore;
    __router: typeof router;
  }).__rootStore = rootStore;
  (window as unknown as {
    __rootStore: RootStore;
    __router: typeof router;
  }).__router = router;
}

window.addEventListener("beforeunload", () => {
  rootStore.stop();
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <RootStoreProvider value={rootStore}>
      <RouterProvider router={router} />
    </RootStoreProvider>
  </React.StrictMode>,
);
