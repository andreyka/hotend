// The window is a host with Hotend as its one module (DESIGN_SPEC 3.1). Routes use the
// address's hash, so they work the same from Tauri's asset protocol and from a server.
import { CSPProvider } from "@base-ui/react/csp-provider";
import { MotionConfig } from "motion/react";
import { useEffect } from "react";
import { createHashRouter, Navigate, RouterProvider } from "react-router";

import { TooltipProvider } from "@/components/ui/tooltip";
import type { CoreClient } from "@/lib/core";
import { HotendProvider, useHotend } from "@/hotend/context";
import { Home } from "@/hotend/Home";
import { Studio } from "@/hotend/Studio";

import { CoreContext, HostProvider, useHost } from "./core-context";
import { ErrorBoundary } from "./ErrorBoundary";
import { Frame } from "./Frame";
import { SettingsScreen } from "./Settings";
import { Setup } from "./Setup";

/**
 * Which screen shows at start is derived from the settings and the project list, never
 * stored: Setup while no provider is chosen and no project exists, Home otherwise.
 */
function Start() {
  const settings = useHost((state) => state.settings);
  const settingsFailed = useHost((state) => state.error !== null);
  const projects = useHotend((state) => state.projects);
  const projectsFailed = useHotend((state) => state.projectsError !== null);
  const loadProjects = useHotend((state) => state.loadProjects);
  useEffect(() => {
    void loadProjects();
  }, [loadProjects]);
  if (settingsFailed || projectsFailed) return <Navigate to="/hotend" replace />;
  if (settings === null || projects === null) return null;
  return <Navigate to={settings.provider === null && projects.length === 0 ? "/setup" : "/hotend"} replace />;
}

const router = createHashRouter([
  {
    element: <Frame />,
    children: [
      { index: true, element: <Start /> },
      { path: "setup", element: <Setup /> },
      { path: "hotend", element: <Home /> },
      { path: "hotend/p/:project/:stage", element: <Studio /> },
      { path: "hotend/p/:project", element: <Studio /> },
      { path: "settings/:section", element: <SettingsScreen /> },
      { path: "settings", element: <Navigate to="/settings/ai" replace /> },
      { path: "*", element: <Navigate to="/hotend" replace /> },
    ],
  },
]);

export function App({ client }: { client: CoreClient }) {
  return (
    <ErrorBoundary>
      <CoreContext value={client}>
        {/* Motion follows the system's reduced-motion setting (DESIGN_SPEC 6.5). */}
        <MotionConfig reducedMotion="user">
          <HostProvider>
            <HotendProvider>
              {/* The window's policy allows no inline <style>: the one rule Base UI would
                  inject (a list without a scrollbar) is in app.css instead. */}
              <CSPProvider disableStyleElements>
                <TooltipProvider>
                  <RouterProvider router={router} />
                </TooltipProvider>
              </CSPProvider>
            </HotendProvider>
          </HostProvider>
        </MotionConfig>
      </CoreContext>
    </ErrorBoundary>
  );
}
