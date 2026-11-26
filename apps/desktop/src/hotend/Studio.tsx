// The studio (DESIGN_SPEC 3.2): talk on the left, the part in the centre, the numbers of
// the stage in view on the right. Left and centre never change their job; only the
// inspector changes with the stage.
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import { useCallback, useEffect, useRef } from "react";
import { Link, Navigate, useLocation, useNavigate, useParams, useSearchParams } from "react-router";

import { buttonVariants } from "@/components/ui/button";
import { useHost } from "@/host/core-context";
import { cn } from "@/lib/utils";

import { ApiErrorCard } from "./bits";
import { useHotend, useHotendStore, useVerdictInput } from "./context";
import { Conversation } from "./Conversation";
import { EXAMPLE_CODE } from "./example";
import { PreparePanel } from "./inspector/PreparePanel";
import { PrintPanel } from "./inspector/PrintPanel";
import { Proof } from "./inspector/Proof";
import { Sizes } from "./inspector/Sizes";
import { proofOf } from "./proof";
import { sizesOf } from "./sizes";
import { Stage } from "./Stage";
import { S } from "./strings";
import { LAYOUT_DEFAULT, LAYOUT_LIMITS, shownLayout, STAGE_MIN, useShownLayout, useStudioUi } from "./ui";
import { verdictFor, type Stage as StageName, type VerdictAction } from "./verdict";
import { VersionRail } from "./VersionRail";
import type { ViewportHandle } from "./viewport/Viewport";

// The window's two easings (tokens.css: --app-ease, --app-ease-out).
const EASE = [0.2, 0, 0, 1] as const;
const EASE_OUT = [0.4, 0, 1, 1] as const;

/** A drag handle between two columns: 8 px to hit, 1 px to see. */
function Handle({ column }: { column: "conversation" | "inspector" }) {
  const shown = useShownLayout();
  const width = shown[column];
  const setLayout = useStudioUi((state) => state.setLayout);
  const [min, max] = LAYOUT_LIMITS[column];
  const sign = column === "conversation" ? 1 : -1;
  const set = (next: number) => {
    // The other column keeps its width; this one takes what the stage can spare.
    const other = column === "conversation" ? (shown.hideInspector ? 0 : shown.inspector + 1) : shown.hideConversation ? 0 : shown.conversation + 1;
    const room = window.innerWidth - other - STAGE_MIN - 1;
    setLayout({ [column]: Math.round(Math.max(min, Math.min(max, room, next))) });
  };
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={column === "conversation" ? S.titlebar.conversation : S.titlebar.inspector}
      aria-valuenow={width}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      data-testid={`handle-${column}`}
      className={cn(
        "group relative z-10 w-px shrink-0 cursor-col-resize bg-app-line",
        "before:absolute before:inset-y-0 before:-left-1 before:w-2 before:content-['']",
        "hover:bg-app-control focus-visible:bg-app-accent",
      )}
      onPointerDown={(event) => {
        event.preventDefault();
        const from = event.clientX;
        const start = width;
        const target = event.currentTarget;
        target.setPointerCapture(event.pointerId);
        const move = (moved: PointerEvent) => {
          set(start + sign * (moved.clientX - from));
        };
        const up = () => {
          target.removeEventListener("pointermove", move);
          target.removeEventListener("pointerup", up);
          target.removeEventListener("pointercancel", up);
        };
        target.addEventListener("pointermove", move);
        target.addEventListener("pointerup", up);
        target.addEventListener("pointercancel", up);
      }}
      onDoubleClick={() => {
        set(LAYOUT_DEFAULT[column]);
      }}
      onKeyDown={(event) => {
        if (event.key === "ArrowLeft") set(width - sign * 16);
        else if (event.key === "ArrowRight") set(width + sign * 16);
        else return;
        event.preventDefault();
      }}
    />
  );
}

function isText(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return target instanceof HTMLTextAreaElement || target instanceof HTMLInputElement || target.isContentEditable;
}

export function Studio() {
  const { project: projectId = "", stage: stageParam } = useParams();
  const [search, setSearch] = useSearchParams();
  const navigate = useNavigate();
  const location = useLocation();
  const store = useHotendStore();
  const detail = useHotend((state) => state.project);
  const results = useHotend((state) => state.results);
  const look = useHotend((state) => state.look);
  const error = useHotend((state) => state.error);
  const running = useHotend((state) => state.run?.status === "running");
  const runText = useHotend((state) => state.run?.text.length ?? 0);
  const sliceKey = useHotend((state) => `${state.slice?.summary.sha256 ?? ""}:${String(state.sliceStale)}:${state.quality}`);
  const materialId = useHost((state) => state.settings?.material.id ?? null);
  const layout = useShownLayout();
  const scroll = useStudioUi((state) => state.scroll);
  const still = useReducedMotion() === true;
  const viewport = useRef<ViewportHandle | null>(null);
  const inspector = useRef<HTMLDivElement>(null);

  const stage: StageName | null =
    stageParam === "design" || stageParam === "prepare" || stageParam === "print" ? stageParam : null;
  const loaded = detail?.project.id === projectId;
  const current = loaded ? (detail.project.current_version ?? null) : null;
  const currentResult = current === null ? undefined : results[current];
  // What keeps a version out of Prepare is what the engine failed; a requirement that is
  // not met is the maker's to decide about, and the verdict says so.
  const problems = proofOf(currentResult).blocking;
  const hasResult = currentResult !== undefined;

  // The columns are fitted into the window at every size it takes (ui.ts, fitLayout).
  useEffect(() => {
    const measure = () => {
      useStudioUi.getState().setWidth(window.innerWidth);
    };
    measure();
    window.addEventListener("resize", measure);
    return () => {
      window.removeEventListener("resize", measure);
    };
  }, []);

  useEffect(() => {
    useStudioUi.getState().reset();
    void store.getState().openProject(projectId);
    return () => {
      store.getState().closeProject();
    };
  }, [projectId, store]);

  // The message typed on Home is the project's first: sent once the project is open.
  const first = (location.state as { first?: unknown } | null)?.first;
  const sentFirst = useRef<string | null>(null);
  useEffect(() => {
    if (!loaded || typeof first !== "string" || sentFirst.current === location.key) return;
    sentFirst.current = location.key;
    void navigate(location.pathname + location.search, { replace: true, state: null });
    void store.getState().sendMessage(first);
    // The maker was typing on Home: the keyboard stays in the conversation, not on <body>.
    requestAnimationFrame(() => document.getElementById("hotend-composer")?.focus());
  }, [loaded, first, location.key, location.pathname, location.search, navigate, store]);

  // `?v=4` while an older version is being looked at: read once, then written.
  const wanted = search.get("v");
  const applied = useRef<string | null>(null);
  useEffect(() => {
    if (!loaded || applied.current === projectId) return;
    applied.current = projectId;
    const version = wanted === null ? Number.NaN : Number(wanted);
    if (stage === "design" && Number.isInteger(version) && version !== current && detail.versions.some((v) => v.number === version)) {
      void store.getState().lookAt(version);
    }
  }, [loaded, projectId, wanted, stage, current, detail, store]);
  useEffect(() => {
    if (applied.current !== projectId) return;
    if (stage !== "design") return;
    const next = look === null ? null : String(look);
    if (next !== wanted) setSearch(next === null ? {} : { v: next }, { replace: true });
  }, [look, stage, wanted, projectId, setSearch, store]);

  // Entering Prepare lays the pieces out; entering Print asks for the review.
  useEffect(() => {
    if (!loaded || current === null || !hasResult) return;
    if (stage === "prepare" && problems === 0 && materialId !== null && !running) void store.getState().enterPrepare(materialId);
    if (stage === "print") void store.getState().enterPrint();
  }, [loaded, current, hasResult, stage, problems, materialId, running, sliceKey, store]);

  // "Show me" belongs to one version on one stage: another version, an older one or
  // another stage gets every part back.
  useEffect(() => {
    useStudioUi.getState().endShow();
  }, [current, look, stage]);

  // Hotend wrote something while the conversation was hidden.
  useEffect(() => {
    if (layout.hideConversation && runText > 0) useStudioUi.getState().setUnseen(true);
    if (!layout.hideConversation) useStudioUi.getState().setUnseen(false);
  }, [layout.hideConversation, runText]);

  // The verdict bar asks the inspector to show what it speaks about.
  useEffect(() => {
    if (!scroll) return;
    if (shownLayout(useStudioUi.getState()).hideInspector) useStudioUi.getState().showColumn("inspector");
    const timer = setTimeout(() => {
      const target = inspector.current?.querySelector<HTMLElement>(`[data-scroll="${scroll.to}"]`);
      target?.scrollIntoView({ block: "nearest", behavior: "auto" });
      if (scroll.to !== "measure") target?.querySelector<HTMLElement>("button")?.focus({ preventScroll: true });
    }, 0);
    return () => {
      clearTimeout(timer);
    };
  }, [scroll]);

  const input = useVerdictInput();
  const verdict = stage && loaded ? verdictFor(stage, input) : null;

  const act = useCallback(
    (action: VerdictAction) => {
      const state = store.getState();
      const ui = useStudioUi.getState();
      const now = state.project?.project.current_version ?? null;
      const proof = proofOf(now === null ? undefined : state.results[now]);
      switch (action.do) {
        case "open-settings":
          void navigate("/settings/ai");
          break;
        case "open-example":
          void state.openExample(S.home.exampleName, EXAMPLE_CODE, S.home.exampleNote).then((project) => {
            if (project) void navigate(`/hotend/p/${project.id}/design`);
          });
          break;
        case "stop":
          void state.cancelRun();
          // The Stop button goes away with the run: the keyboard goes to the message box.
          setTimeout(() => document.getElementById("hotend-composer")?.focus(), 0);
          break;
        case "dismiss-refused":
          state.clearRefused();
          break;
        case "send-again": {
          const text = state.lastMessage ?? state.project?.transcript.findLast((entry) => entry.role === "user")?.text;
          if (text) void state.sendMessage(text);
          ui.showColumn("conversation");
          break;
        }
        case "open-slicer":
          void navigate("/settings/slicer");
          break;
        case "look-current":
          void state.lookAt(null);
          break;
        case "use-version":
          void state.useVersion(action.version);
          break;
        case "undo-change":
          void state.undo();
          break;
        case "ask-fix": {
          const line = proof.firstProblem;
          if (line) void state.sendMessage(S.proof.fixMessage(line.summary ?? line.entries[0]?.message ?? line.title));
          ui.showColumn("conversation");
          break;
        }
        case "show-me": {
          const line = proof.firstProblem;
          const part = line?.spot?.part ?? (line?.parts.length === 1 ? (line.parts[0] ?? null) : null);
          if (line && part !== null) {
            if (ui.solo === part && ui.spot?.line === line.key) ui.showOnly(null);
            else {
              // The line opens with it: what "Show me" hides comes back when the line closes.
              ui.toggleLine(line.key, true);
              ui.showOnly(part, { line: line.key, part, at: line.spot?.at ?? null, label: line.spot?.label ?? null });
            }
          }
          ui.scrollTo("problem");
          break;
        }
        case "measure": {
          const first = sizesOf(now === null ? undefined : state.results[now]).find((size) => size.group === "measure");
          ui.openSizeRow(first?.name ?? null, true);
          ui.scrollTo("measure");
          break;
        }
        case "read-note":
          ui.scrollTo("note");
          break;
        case "go":
          void navigate(`/hotend/p/${projectId}/${action.stage}`);
          break;
        case "slice":
          void state.sliceNow();
          break;
        case "plate":
          // The next plate of a job of several: it is shown in Prepare, where it is sliced.
          state.setPlate(action.plate);
          void navigate(`/hotend/p/${projectId}/prepare`);
          break;
        case "review-again":
          void state.reviewAgain();
          break;
      }
    },
    [navigate, projectId, store],
  );

  // Keys that work from anywhere in the studio (6.3). No key starts or stops a print.
  const primary = verdict?.primary?.action;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      const ui = useStudioUi.getState();
      const state = store.getState();
      const key = event.key;
      if (key === "1" || key === "2" || key === "3") {
        void navigate(`/hotend/p/${projectId}/${(["design", "prepare", "print"] as const)[Number(key) - 1] ?? "design"}`);
      } else if (key === "Enter") {
        if (!primary) return;
        act(primary);
      } else if (key === "l" || key === "L") {
        ui.showColumn("conversation");
        setTimeout(() => document.getElementById("hotend-composer")?.focus(), 0);
      } else if (key === ".") {
        void state.cancelRun();
      } else if (key === "z" || key === "Z") {
        if (isText(event.target)) return;
        if (event.shiftKey) void state.redoNext();
        else void state.undo();
      } else if (key === "\\" || key === "|") {
        ui.toggleColumn(event.shiftKey ? "inspector" : "conversation");
      } else if (key === "0") {
        viewport.current?.focus();
      } else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [act, navigate, primary, projectId, store]);

  if (stage === null) return <Navigate to={`/hotend/p/${projectId}/design`} replace />;

  if (!loaded) {
    return (
      <div className="grid h-full place-items-center p-8" data-testid="studio-loading">
        {error ? (
          <ApiErrorCard
            // A project that is gone is said in the window's words, not with its id.
            error={error.kind === "not_found" ? { ...error, message: S.studio.gone } : error}
            className="max-w-[440px]"
            action={
              <Link to="/hotend" className={buttonVariants({ variant: "outline", size: "sm" })}>
                {S.titlebar.home}
              </Link>
            }
          />
        ) : null}
      </div>
    );
  }

  const onStage = stage === "design" ? (look ?? current) : current;
  const result = onStage === null ? undefined : results[onStage];

  const showInspector = !(layout.hideInspector || (stage === "design" && current === null));
  // A column slides open in 240 ms and shut in 160 ms (DESIGN_SPEC 9); its content keeps
  // its width meanwhile, so nothing inside it is laid out again.
  const slide = {
    initial: { width: 0 },
    animate: { width: "auto", transition: { duration: still ? 0 : 0.24, ease: EASE } },
    exit: { width: 0, transition: { duration: still ? 0 : 0.16, ease: EASE_OUT } },
  };

  return (
    <div className="flex h-full min-h-0 overflow-hidden" data-testid="studio">
      <AnimatePresence initial={false}>
        {layout.hideConversation ? null : (
          <motion.div key="conversation" className="flex min-h-0 shrink-0 overflow-hidden" {...slide}>
            <aside
              aria-label={S.titlebar.conversation}
              className="flex min-h-0 shrink-0 flex-col bg-app-panel"
              style={{ width: layout.conversation }}
              data-testid="conversation-column"
            >
              <Conversation projectId={projectId} />
            </aside>
          </motion.div>
        )}
      </AnimatePresence>
      {layout.hideConversation ? null : <Handle column="conversation" />}
      <section className="flex min-h-0 min-w-0 flex-1 flex-col" data-testid="stage-column">
        <Stage stage={stage} verdict={verdict} onAction={act} viewport={viewport} />
        {current !== null ? <VersionRail stage={stage} projectId={projectId} /> : null}
      </section>
      {showInspector ? <Handle column="inspector" /> : null}
      <AnimatePresence initial={false}>
        {showInspector ? (
          <motion.div key="inspector" className="flex min-h-0 shrink-0 overflow-hidden" {...slide}>
            <aside
              ref={inspector}
              aria-label={S.titlebar.inspector}
              className="app-scroll min-h-0 shrink-0 overflow-x-hidden overflow-y-auto bg-app-panel px-4 pt-3 pb-6 [scrollbar-gutter:stable]"
              style={{ width: layout.inspector }}
              data-testid="inspector"
            >
              {/* The numbers of another stage come in with a short fade and 4 px of rise. */}
              <div key={stage} className="app-enter">
                {stage === "design" ? (
                  <div className="space-y-4">
                    <Sizes result={result} readOnly={look} />
                    <Proof result={result} />
                  </div>
                ) : stage === "prepare" ? (
                  <PreparePanel />
                ) : (
                  <PrintPanel />
                )}
              </div>
            </aside>
          </motion.div>
        ) : null}
      </AnimatePresence>
    </div>
  );
}
