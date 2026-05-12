import { createBrowserRouter, redirect } from "react-router-dom";
import { AppShell } from "./App";
import { dictationsRoute } from "./routes/dictations";
import { meetingsRoute } from "./routes/meetings";
import { meetingDetailRoute } from "./routes/meeting-detail";
import { settingsAppearanceRoute } from "./routes/settings/appearance";
import { settingsConfigFileRoute } from "./routes/settings/config-file";
import { settingsKeybindRoute } from "./routes/settings/keybind";
import { settingsLayoutRoute } from "./routes/settings/layout";
import { settingsProviderRoute } from "./routes/settings/provider";
import { settingsUpdatesRoute } from "./routes/settings/updates";

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
        { index: true, loader: () => redirect("/dictations") },
        dictationsRoute,
        meetingsRoute,
        meetingDetailRoute,
        settingsLayoutRoute([
          settingsProviderRoute,
          settingsKeybindRoute,
          settingsUpdatesRoute,
          settingsAppearanceRoute,
          settingsConfigFileRoute,
        ]),
      ],
    },
  ]);
}
