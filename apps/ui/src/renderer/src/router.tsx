import { createBrowserRouter } from "react-router-dom";
import { AppShell } from "./App";
import { Dashboard } from "./routes/dashboard";
import { PlaceholderRoute } from "./routes/placeholder";

export const router = createBrowserRouter([
  {
    path: "/",
    element: <AppShell />,
    children: [
      { index: true, element: <Dashboard /> },
      {
        path: "history",
        element: <PlaceholderRoute title="History" phase="Phase 2" />,
      },
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
