// The host's frame: the 44 px title bar and the screen under it (DESIGN_SPEC 3.2). The
// left end, where the Home button is, is where a module switch would go. Hotend fills
// the middle of the bar while one of its projects is open.
import { House, PanelLeft, PanelRight, Settings } from "lucide-react";
import { useCallback, useEffect } from "react";
import { Outlet, useLocation, useMatch, useNavigate } from "react-router";

import { Kbd } from "@/components/ui/kbd";
import { IconButton } from "@/hotend/bits";
import { S } from "@/hotend/strings";
import { PrinterChip, ProjectMenu, StageSwitch } from "@/hotend/TitleBar";
import { useShownLayout, useStudioUi } from "@/hotend/ui";
import type { Stage } from "@/hotend/verdict";

import { BrandMark, Wordmark } from "./BrandMark";
import { useCore } from "./core-context";
import { ErrorBoundary } from "./ErrorBoundary";

const MOD = typeof navigator !== "undefined" && /Mac/.test(navigator.platform) ? "⌘" : "Ctrl";

export function Frame() {
  const client = useCore();
  const navigate = useNavigate();
  const location = useLocation();
  const studio = useMatch("/hotend/p/:project/:stage");
  const layout = useShownLayout();
  const toggleColumn = useStudioUi((state) => state.toggleColumn);
  const unseen = useStudioUi((state) => state.unseen);
  const stage = studio?.params.stage;
  const inStudio = studio !== null && (stage === "design" || stage === "prepare" || stage === "print");
  const inSetup = location.pathname.startsWith("/setup");

  // Settings remembers where the maker was, for its Back button.
  const here = location.pathname + location.search;
  const inSettings = location.pathname.startsWith("/settings");
  const toSettings = useCallback(
    (section: string) => {
      const from = inSettings ? (location.state as { from?: unknown } | null)?.from : here;
      void navigate(`/settings/${section}`, { state: { from }, replace: inSettings });
    },
    [navigate, here, inSettings, location.state],
  );

  // Keys of the host (6.3): Home with the prompt focused, Settings, the shortcuts.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      if (event.key === "n" || event.key === "N") void navigate("/hotend", { state: { focus: Date.now() } });
      else if (event.key === ",") toSettings("ai");
      else if (event.key === "/") toSettings("shortcuts");
      else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [navigate, toSettings]);

  return (
    <div className="flex h-full flex-col bg-app-bg text-app-text" data-transport={client.transport}>
      <header className="relative flex min-h-11 shrink-0 items-center gap-3 border-b border-app-line bg-app-bg px-2" data-testid="titlebar">
        <div className="flex min-w-0 flex-1 items-center gap-1">
          <IconButton
            label={S.titlebar.home}
            onClick={() => {
              void navigate("/hotend");
            }}
            data-testid="home-button"
          >
            <House strokeWidth={1.5} />
          </IconButton>
          {inStudio ? (
            <>
              <span className="relative">
                <IconButton
                  label={S.titlebar.conversation}
                  hint={<Kbd>{MOD} \</Kbd>}
                  pressed={!layout.hideConversation}
                  onClick={() => {
                    toggleColumn("conversation");
                  }}
                  data-testid="toggle-conversation"
                >
                  <PanelLeft strokeWidth={1.5} />
                </IconButton>
                {unseen && layout.hideConversation ? (
                  <span className="pointer-events-none absolute top-1 right-1 size-1.5 rounded-pill bg-app-accent" role="status" aria-label={S.titlebar.newText} />
                ) : null}
              </span>
              <ProjectMenu />
            </>
          ) : (
            // Outside a project the bar says whose window this is: the mark and the name.
            <span className="flex items-center gap-2 px-1.5 text-app-text" data-testid="brand">
              <BrandMark />
              <Wordmark />
            </span>
          )}
        </div>
        {inStudio && studio.params.project ? (
          <div className="shrink-0">
            <StageSwitch projectId={studio.params.project} stage={stage as Stage} />
          </div>
        ) : null}
        {/* This side keeps the room its controls need; where the bar is crowded the name at the left gives way. */}
        <div className="flex flex-1 items-center justify-end gap-1">
          <PrinterChip />
          {inStudio ? (
            <IconButton
              label={S.titlebar.inspector}
              hint={<Kbd>{MOD} ⇧ \</Kbd>}
              pressed={!layout.hideInspector}
              onClick={() => {
                toggleColumn("inspector");
              }}
              data-testid="toggle-inspector"
            >
              <PanelRight strokeWidth={1.5} />
            </IconButton>
          ) : null}
          {inSetup ? null : (
            <IconButton
              label={S.titlebar.settings}
              hint={<Kbd>{MOD} ,</Kbd>}
              pressed={inSettings}
              onClick={() => {
                toSettings("ai");
              }}
              data-testid="settings-button"
            >
              <Settings strokeWidth={1.5} />
            </IconButton>
          )}
        </div>
      </header>
      <main className="min-h-0 flex-1">
        <ErrorBoundary>
          <Outlet />
        </ErrorBoundary>
      </main>
    </div>
  );
}
