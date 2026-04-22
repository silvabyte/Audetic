import { createBrowserRouter } from "react-router-dom";
import { AppShell } from "./App";
import { dashboardRoute } from "./routes/dashboard";
import { historyRoute } from "./routes/history";
import { PlaceholderRoute } from "./routes/placeholder";

/**
 * `createBrowserRouter` eagerly invokes the initial-match loader at
 * construction time (for hydration). We need the RootStore to be
 * registered first, so this is a factory — call it AFTER setRootStore.
 */
export function createRouter(): ReturnType<typeof createBrowserRouter> {
  return createBrowserRouter([
    {
      path: "/",
      element: <AppShell />,
      children: [
        dashboardRoute,
        historyRoute,
        {
          path: "meetings",
          element: <PlaceholderRoute title="Meetings" phase="Phase 3" />,
        },
        {
          path: "settings",
          element: <PlaceholderRoute title="Settings" phase="Phase 4" />,
        },
      ],
    },
  ]);
}
