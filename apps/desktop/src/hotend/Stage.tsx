// The stage (DESIGN_SPEC 3.2, 4.3.2, 10): the 3D view with Hotend's furniture drawn as
// ordinary, opaque HTML over it. Nothing ever opens over the part, and an old part stays
// until its replacement is ready.
import { Box, FileDown, MousePointerClick, Scan, Scissors, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Kbd } from "@/components/ui/kbd";
import { Menu, MenuContent, MenuItem, MenuTrigger } from "@/components/ui/menu";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Segmented } from "@/components/ui/segmented";
import { Slider } from "@/components/ui/slider";
import { Tooltip } from "@/components/ui/tooltip";
import { useTheme } from "@/host/theme";
import { cn } from "@/lib/utils";

import { ApiErrorCard, Flash, IconButton, SimTag } from "./bits";
import { useHotend } from "./context";
import { dimensions, partWords, plain, weight } from "./format";
import { pickedPlace } from "./pick";
import { ACTIVE_JOB, newestJob, printedVersion, slicedPlate } from "./store";
import { S } from "./strings";
import { ExportPopover } from "./TitleBar";
import { saveThumb, scaleThumb } from "./thumbs";
import { useStudioUi, type CutAxis } from "./ui";
import { shownJob, type Stage as StageName, type Verdict, type VerdictAction } from "./verdict";
import { VerdictBar } from "./VerdictBar";
import { namedParts, parseMesh, type MeshDocument } from "./viewport/mesh";
import { designScene, plateScene, type SceneDocument } from "./viewport/scene";
import { canShow3d, Viewport, type Clip, type Picked, type StandardView, type ViewMark, type ViewportHandle } from "./viewport/Viewport";

/** The bed the main nozzle of the X2D reaches, until a layout says its own size. */
const PLATE: readonly [number, number] = [256, 256];

const VIEWS: { direction: StandardView; label: string; key: string }[] = [
  { direction: "iso", label: S.stage.viewNames.iso, key: "1" },
  { direction: "front", label: S.stage.viewNames.front, key: "2" },
  { direction: "top", label: S.stage.viewNames.top, key: "3" },
  { direction: "right", label: S.stage.viewNames.right, key: "4" },
];

type Bounds = { xmin: number; xmax: number; ymin: number; ymax: number; zmin: number; zmax: number };

/** As many part chips as fit in six tenths of the stage, in one row; the rest are behind "+5 parts". */
function chipsThatFit(names: readonly string[], stageWidth: number): number {
  if (names.length <= 2) return names.length;
  const room = stageWidth * 0.6 - 88;
  let used = 0;
  let count = 0;
  for (const name of names) {
    used += Math.min(16, name.length) * 7.2 + 42;
    if (used > room) break;
    count += 1;
  }
  return count === names.length ? count : Math.max(1, count);
}

function where(point: readonly number[]): string {
  const [x = 0, y = 0, z = 0] = point;
  return `x ${plain(x, 1)}, y ${plain(y, 1)}, z ${plain(z, 1)} mm`;
}

export function Stage({
  stage,
  verdict,
  onAction,
  viewport,
}: {
  stage: StageName;
  verdict: Verdict | null;
  onAction(action: VerdictAction): void;
  viewport: React.RefObject<ViewportHandle | null>;
}) {
  const navigate = useNavigate();
  const theme = useTheme((state) => state.theme);
  const detail = useHotend((state) => state.project);
  const look = useHotend((state) => state.look);
  const meshes = useHotend((state) => state.meshes);
  const results = useHotend((state) => state.results);
  const rebuild = useHotend((state) => state.rebuild);
  const ownPreparation = useHotend((state) => state.preparation);
  const jobPreparation = useHotend((state) => state.jobPreparation);
  const ownPlate = useHotend((state) => state.plate);
  const jobPlates = useHotend((state) => state.jobPlates);
  const slice = useHotend((state) => state.slice);
  const jobSlice = useHotend((state) => state.jobSlice);
  // The print in view on the Print stage: one that runs, or the outcome of the newest one.
  const printJob = useHotend((state) => (stage === "print" ? shownJob(state) : null));
  const slicing = useHotend((state) => state.slicing);
  const preparing = useHotend((state) => state.preparing);
  const jobs = useHotend((state) => state.jobs);
  const printer = useHotend((state) => state.printers?.[0] ?? null);
  const error = useHotend((state) => state.error);
  const dismissError = useHotend((state) => state.dismissError);
  const lookAt = useHotend((state) => state.lookAt);
  const ui = useStudioUi();
  const [webgl] = useState(canShow3d);

  const project = detail?.project ?? null;
  const current = project?.current_version ?? null;
  // Print shows the version that is being printed, or was: a design that moved on since
  // does not empty the plate.
  const printed = stage === "print" ? printedVersion(printJob, current) : current;
  const onStage = stage === "design" ? (look ?? current) : printed;
  const ofJob = stage === "print" && printJob !== null && printJob.version !== current;
  const preparation = ofJob ? (jobPreparation?.version === printJob.version ? jobPreparation : null) : ownPreparation;
  const plateIndex =
    stage === "print" && printJob !== null
      ? // The plate the job's own slice is of, as the core recorded it.
        (slicedPlate([slice, jobSlice].find((one) => one?.summary.sha256 === printJob.slice_sha256) ?? null) ??
        jobPlates[printJob.id] ??
        (ofJob ? 1 : ownPlate))
      : ownPlate;
  const meshText = onStage === null ? undefined : meshes[onStage];
  const result = onStage === null ? undefined : results[onStage];
  const problems = result?.status === "check_error";
  const asPlate = stage !== "design" && !(stage === "prepare" && problems);

  const mesh = useMemo<MeshDocument | null>(() => {
    if (typeof meshText !== "string") return null;
    try {
      return parseMesh(meshText);
    } catch {
      return null;
    }
  }, [meshText]);
  const unreadable = onStage !== null && (meshText === null || (typeof meshText === "string" && mesh === null));

  const plate = preparation?.plates.find((candidate) => candidate.index === plateIndex) ?? preparation?.plates[0] ?? null;
  const scene = useMemo<SceneDocument | null>(() => {
    if (onStage === null) return plateScene(null, PLATE, []);
    if (!asPlate) return mesh ? designScene(mesh) : null;
    if (!mesh) return null;
    return plateScene(mesh, plate?.size ?? PLATE, plate?.items ?? []);
  }, [onStage, asPlate, mesh, plate]);

  // The old part stays on the stage until its replacement is ready.
  const [lastScene, setLastScene] = useState<SceneDocument | null>(scene);
  if (scene && scene !== lastScene) setLastScene(scene);
  const shown = scene ?? (unreadable ? null : lastScene);

  const partNames = useMemo(() => (mesh && !asPlate ? namedParts(mesh).map((part) => part.name) : []), [mesh, asPlate]);
  const root = useRef<HTMLDivElement>(null);
  const [stageWidth, setStageWidth] = useState(0);
  useEffect(() => {
    const element = root.current;
    if (!element) return;
    const observer = new ResizeObserver(() => {
      setStageWidth(element.clientWidth);
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, []);
  const chipCount = chipsThatFit(partNames, stageWidth || 800);
  const hidden = useMemo(() => {
    if (asPlate) return [];
    return ui.solo !== null ? partNames.filter((name) => name !== ui.solo) : ui.hidden;
  }, [asPlate, ui.solo, ui.hidden, partNames]);

  // A running print cuts the plate at the printed height; Cut open cuts the part.
  const job = newestJob(jobs);
  const printing = stage === "print" && job !== null && ACTIVE_JOB.has(job.status);
  const clip = useMemo<Clip | null>(() => {
    const bounds = shown?.shapes.bb as Bounds | undefined;
    if (!bounds) return null;
    if (printing && job?.status === "printing") {
      const state = printer?.state;
      const fraction =
        state?.layer != null && state.total_layers ? state.layer / state.total_layers : (job.detail.progress_percent ?? 0) / 100;
      const top = Math.max(0, ...(plate?.items.map((item) => item.bbox.max[2] ?? 0) ?? [0]));
      // Nothing is cut before the first layer is down (the parts are a ghost until then),
      // and the cut is a hair above the printed height so that it never lies in the bed.
      const printed = fraction * top;
      const first = state?.total_layers ? top / state.total_layers : 0.2;
      return printed >= first && printed > 0 ? { axis: "z", at: printed + 0.05 } : null;
    }
    if (stage !== "design" || !ui.cut.on) return null;
    const [min, max] =
      ui.cut.axis === "x" ? [bounds.xmin, bounds.xmax] : ui.cut.axis === "y" ? [bounds.ymin, bounds.ymax] : [bounds.zmin, bounds.zmax];
    return { axis: ui.cut.axis, at: min + (max - min) * ui.cut.at };
  }, [shown, printing, job, printer, plate, stage, ui.cut]);
  // A print that has not laid its first layer: the parts are drawn whole, as a ghost.
  const ghost = printing && clip === null;

  // From 150 ms into a rebuild the view drops to 55 %; a fast build does not flicker.
  const [dim, setDim] = useState(false);
  useEffect(() => {
    if (!rebuild) return;
    const timer = setTimeout(() => {
      setDim(true);
    }, 150);
    return () => {
      clearTimeout(timer);
      setDim(false);
    };
  }, [rebuild]);

  // A picture for the project's card, once the version has been on the stage for a second.
  const projectId = project?.id;
  useEffect(() => {
    if (stage !== "design" || !projectId || onStage === null || !mesh || rebuild || ui.cut.on || hidden.length > 0) return;
    const timer = setTimeout(() => {
      void viewport.current?.capture().then(async (picture) => {
        for (const [of, taken] of Object.entries(picture ?? {})) {
          const scaled = await scaleThumb(taken);
          if (scaled) await saveThumb(projectId, onStage, of as "dark" | "light", scaled);
        }
      });
    }, 1000);
    return () => {
      clearTimeout(timer);
    };
  }, [stage, projectId, onStage, mesh, rebuild, ui.cut.on, hidden.length, theme, viewport]);

  // What Hotend points at on the part: where a fault is, and the size whose row is in use.
  const spot = ui.spot;
  const dimension = ui.dimension;
  const piece = ui.piece;
  const marks = useMemo<ViewMark[]>(() => {
    if (stage === "prepare" && piece !== null) {
      // The pieces of the part whose row is in use, outlined where they lie on the plate.
      const plate = preparation?.plates.find((entry) => entry.index === plateIndex);
      const [x, y] = [(plate?.size[0] ?? 0) / 2, (plate?.size[1] ?? 0) / 2];
      return (plate?.items ?? [])
        .filter((item) => item.part === piece)
        .map((item) => ({
          kind: "box" as const,
          min: [item.bbox.min[0] - x, item.bbox.min[1] - y, item.bbox.min[2]] as const,
          max: [item.bbox.max[0] - x, item.bbox.max[1] - y, item.bbox.max[2]] as const,
        }));
    }
    if (stage !== "design") return [];
    const drawn: ViewMark[] = [];
    if (spot?.at) drawn.push({ kind: "spot", at: spot.at, label: spot.label });
    if (dimension && look === null) drawn.push({ kind: "span", from: dimension.from, to: dimension.to, label: dimension.label });
    return drawn;
  }, [stage, spot, dimension, look, piece, preparation, plateIndex]);
  // The newest model Hotend wrote, up to the current version: another one is another shape.
  const fresh = (detail?.versions ?? []).findLast((version) => version.origin !== "set_parameter" && version.number <= (current ?? 0))?.number ?? 0;

  const setPicked = ui.setPicked;
  const onPick = useCallback(
    (picked: Picked | null) => {
      setPicked(picked);
    },
    [setPicked],
  );
  useEffect(() => {
    setPicked(null);
  }, [onStage, stage, setPicked]);

  const versions = detail?.versions ?? [];
  const step = (direction: -1 | 1) => {
    if (onStage === null) return;
    const index = versions.findIndex((version) => version.number === onStage) + direction;
    const next = versions[index];
    if (next) void lookAt(next.number);
  };
  const onKey = (event: React.KeyboardEvent) => {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    const view = VIEWS.find((candidate) => candidate.key === event.key);
    if (view) viewport.current?.view(view.direction);
    else if (event.key === "f" || event.key === "F") viewport.current?.fit();
    else if ((event.key === "c" || event.key === "C") && stage === "design") ui.setCut({ on: !ui.cut.on });
    else if (event.key === "[" && stage === "design") step(-1);
    else if (event.key === "]" && stage === "design") step(1);
    else if (event.key === "Escape") {
      if (ui.cut.on) ui.setCut({ on: false });
      else if (look !== null) void lookAt(null);
      else return;
    } else return;
    event.preventDefault();
  };

  const overall = useMemo(() => {
    const parts = result?.parts ?? [];
    if (parts.length === 0) return null;
    const size = [0, 1, 2].map(
      (axis) =>
        Math.max(...parts.map((part) => part.bbox.max[axis] ?? 0)) - Math.min(...parts.map((part) => part.bbox.min[axis] ?? 0)),
    );
    const grams = parts.reduce((sum, part) => sum + (part.mass_estimate_g ?? 0), 0);
    // The weight of the part printed full is not what a print weighs: it is a hint only.
    return { size: dimensions(size), hint: grams > 0 ? S.stage.solidHint(weight(grams)) : null };
  }, [result]);

  const label =
    project === null
      ? S.app
      : onStage === null || partNames.length + (asPlate ? 1 : 0) === 0
        ? S.stage.view3dEmpty(project.name)
        : S.stage.view3d(project.name, (result?.parts ?? []).map((part) => partWords(part.name)).join(" and "), overall?.size ?? "");
  const plateSize = plate?.size ?? PLATE;
  // A run that was stopped before it built anything: the verdict offers to send it again.
  const stoppedEmpty = verdict?.primary?.action.do === "send-again";
  const picked = ui.picked;
  const pickedBox = picked === null ? null : ((result?.parts ?? []).find((part) => part.name === picked.part)?.bbox ?? null);
  const pickedWhere = picked === null ? null : pickedPlace(picked.kind, picked.point, pickedBox);
  const pickedText = picked === null || pickedWhere === null ? null : S.stage.picked(partWords(picked.part), pickedWhere);

  return (
    <div ref={root} className="relative min-h-0 flex-1" data-testid="stage" data-stage={stage} data-version={onStage ?? undefined}>
      {/* First in the document, as in the tab order (6.2): the verdict bar, the view tools and
          what was picked. They are drawn over the view. */}
      <div className={cn("pointer-events-none absolute inset-x-3 top-3 z-10 flex flex-col items-start gap-2", printing && job?.detail.simulated && "top-11")}>
        {/* The view tools keep their corner whatever the verdict says: the bar has the room to their
            left. The tools grow with the text size, so the room they take is counted in rem. */}
        <div
          className="pointer-events-auto max-w-full min-w-0"
          style={{ maxWidth: onStage !== null && webgl ? `calc(100% - ${stage === "design" ? 9.375 : 5.375}rem)` : undefined }}
          data-testid="verdict-room"
        >
          {verdict ? <VerdictBar verdict={verdict} onAction={onAction} onAbout={verdict.about ? () => { if (verdict.about) ui.scrollTo(verdict.about); } : undefined} /> : null}
        </div>
        {onStage !== null && webgl ? (
          <div
            role="toolbar"
            aria-label={S.stage.views}
            data-testid="view-tools"
            className="pointer-events-auto absolute top-0 right-0 flex shrink-0 items-center gap-0.5 rounded-card border border-app-line bg-app-raised p-1 shadow-1"
          >
            <IconButton label={S.stage.fit} hint={<Kbd>F</Kbd>} onClick={() => viewport.current?.fit()}>
              <Scan strokeWidth={1.5} />
            </IconButton>
            <Menu>
              <MenuTrigger
                render={
                  <IconButton label={S.stage.views} hint={<Kbd>1–4</Kbd>}>
                    <Box strokeWidth={1.5} />
                  </IconButton>
                }
              />
              <MenuContent align="end">
                {VIEWS.map((view) => (
                  <MenuItem key={view.direction} onClick={() => viewport.current?.view(view.direction)}>
                    <span className="flex-1">{view.label}</span>
                    <Kbd>{view.key}</Kbd>
                  </MenuItem>
                ))}
              </MenuContent>
            </Menu>
            {stage === "design" ? (
              <Popover
                open={ui.cut.on}
                onOpenChange={(open, details) => {
                  // The cut stays while the maker turns the part: only the button and Esc end it.
                  if (open || details.reason === "trigger-press" || details.reason === "escape-key") ui.setCut({ on: open });
                }}
              >
                <PopoverTrigger
                  render={
                    <IconButton label={S.stage.cut} hint={<Kbd>C</Kbd>} pressed={ui.cut.on}>
                      <Scissors strokeWidth={1.5} />
                    </IconButton>
                  }
                />
                <PopoverContent align="end" className="w-[304px] space-y-3" initialFocus={false}>
                  <Segmented<CutAxis>
                    label={S.stage.cutTitle}
                    size="sm"
                    value={ui.cut.axis}
                    onChange={(axis) => {
                      ui.setCut({ axis });
                    }}
                    segments={[
                      { value: "x", label: S.stage.cutPlanes.x },
                      { value: "y", label: S.stage.cutPlanes.y },
                      { value: "z", label: S.stage.cutPlanes.z },
                    ]}
                  />
                  <Slider
                    label={S.stage.cutSlider}
                    min={0}
                    max={1}
                    step={0.01}
                    value={ui.cut.at}
                    onValueChange={(at) => {
                      ui.setCut({ at });
                    }}
                  />
                </PopoverContent>
              </Popover>
            ) : null}
            {stage === "design" && project !== null && current !== null ? (
              <ExportPopover
                project={project}
                align="end"
                trigger={
                  <IconButton label={S.project.export} data-testid="stage-export">
                    <FileDown strokeWidth={1.5} />
                  </IconButton>
                }
              />
            ) : null}
          </div>
        ) : null}
        {/* Under the verdict bar: what the maker clicked, to mention in a message. */}
        {pickedText !== null && picked !== null ? (
          <div
            className="app-enter pointer-events-auto flex min-h-8 max-w-full items-center gap-1 rounded-card border border-app-line bg-app-raised py-0.5 pr-1 pl-2.5 shadow-1"
            data-testid="picked"
            data-part={picked.part}
            data-kind={picked.kind}
            data-path={picked.path}
            data-point={picked.point?.join(",")}
          >
            <MousePointerClick className="size-4 shrink-0 text-app-text-2" strokeWidth={1.5} aria-hidden />
            <Tooltip label={picked.point ? S.stage.pickedAt(where(picked.point)) : pickedText} side="bottom">
              <span className="t-ui min-w-0 truncate px-1" data-testid="picked-text">
                {pickedText}
              </span>
            </Tooltip>
            <Button
              variant="ghost"
              size="sm"
              className="shrink-0 text-app-accent"
              onClick={() => {
                ui.fillComposer(S.stage.mentionText(partWords(picked.part), pickedWhere ?? "", picked.point ? where(picked.point) : null));
                ui.showColumn("conversation");
              }}
            >
              {S.stage.mention}
            </Button>
            <IconButton label={S.stage.clearPick} side="top" onClick={() => { setPicked(null); }}>
              <X strokeWidth={1.5} />
            </IconButton>
          </div>
        ) : null}
      </div>

      <div
        className={cn("app-fade absolute inset-0 transition-opacity duration-[var(--app-dur-fast)]", dim && "opacity-55")}
        data-testid="stage-view"
        data-dim={dim || undefined}
      >
        {webgl ? (
          <Viewport
            ref={viewport}
            scene={shown}
            cameraKey={asPlate || onStage === null ? "plate" : "design"}
            theme={theme}
            hidden={hidden}
            clip={clip}
            ghost={ghost}
            marked={stage === "design" ? picked : null}
            marks={marks}
            fresh={fresh}
            plate={asPlate || onStage === null ? Math.max(plateSize[0], plateSize[1]) : null}
            onPick={stage === "design" && look === null ? onPick : undefined}
            label={label}
            onKey={onKey}
          />
        ) : (
          <div className="hotend-viewport" />
        )}
      </div>

      {rebuild || slicing || (preparing && stage === "prepare") ? <div className="app-progress-line" data-testid="progress-line" /> : null}

      {printing && job?.detail.simulated ? (
        <div className="absolute inset-x-0 top-0 flex h-8 items-center justify-center gap-2 bg-app-sim-wash text-app-sim" data-testid="sim-band">
          <SimTag className="bg-transparent px-0">{S.stage.simBand}</SimTag>
        </div>
      ) : null}

      {/* Centre text: an empty project, a version without a mesh, no WebGL. */}
      {onStage === null && stage === "design" && (!verdict || stoppedEmpty) ? (
        <div className="pointer-events-none absolute inset-0 grid place-items-center p-8">
          <div
            className="max-w-[440px] space-y-1 rounded-card border border-app-line bg-app-raised px-5 py-4 text-center [text-wrap:balance] shadow-1"
            data-testid="stage-empty"
          >
            {stoppedEmpty ? (
              <p className="t-body text-app-text">{S.stage.stopped}</p>
            ) : (
              <>
                <p className="t-body text-app-text">{S.stage.empty}</p>
                <p className="t-ui text-app-text-2">{S.stage.emptyHint}</p>
              </>
            )}
          </div>
        </div>
      ) : null}
      {unreadable && webgl ? (
        <div className="pointer-events-none absolute inset-0 grid place-items-center p-8">
          <p className="t-body text-app-text-2">{S.stage.noView}</p>
        </div>
      ) : null}
      {!webgl ? (
        <div className="absolute inset-0 grid place-items-center p-8">
          <div className="space-y-3 text-center">
            <p className="t-body text-app-text-2">{S.stage.noWebgl}</p>
            <Button variant="outline" onClick={() => void navigate("/settings/computer")}>
              {S.stage.openComputer}
            </Button>
          </div>
        </div>
      ) : null}

      {/* Bottom right: part chips and the overall size, or the plate's caption. */}
      <div className="pointer-events-none absolute right-3 bottom-3 flex max-w-[70%] flex-col items-end gap-2">
        {!asPlate && partNames.length >= 2 ? (
          // One row: as many chips as fit, the rest behind "+5 parts".
          <div className="pointer-events-auto flex h-8 max-w-full flex-nowrap items-center justify-end gap-1.5" data-testid="part-chips">
            {partNames.slice(0, chipCount).map((name, index) => {
              const off = hidden.includes(name);
              const words = partWords(name);
              return (
                <button
                  key={name}
                  type="button"
                  // The name says what a press does; the state is in the name alone.
                  aria-label={off ? S.stage.show(words) : S.stage.hide(words)}
                  title={`${words}. ${off ? S.stage.partHidden : S.stage.partShown}`}
                  data-off={off || undefined}
                  data-testid="part-chip"
                  onClick={() => {
                    ui.togglePart(name);
                  }}
                  className={cn(
                    "t-ui flex h-7 min-w-0 shrink-0 items-center gap-1.5 rounded-tag border border-app-line bg-app-raised px-2 shadow-1 hover:bg-app-hover",
                    off && "text-app-text-3",
                  )}
                >
                  <span
                    className="size-2.5 shrink-0 rounded-pill border-[1.5px]"
                    style={{
                      borderColor: `var(--hotend-part-${(index % 8) + 1})`,
                      background: off ? "transparent" : `var(--hotend-part-${(index % 8) + 1})`,
                    }}
                  />
                  <span className={cn("max-w-[16ch] truncate", off && "line-through")}>{words}</span>
                </button>
              );
            })}
            {chipCount < partNames.length ? (
              <Popover>
                <PopoverTrigger
                  render={
                    <button
                      type="button"
                      data-testid="more-parts"
                      className="t-ui flex h-7 shrink-0 items-center rounded-tag border border-app-line bg-app-raised px-2 whitespace-nowrap text-app-text-2 shadow-1 hover:bg-app-hover hover:text-app-text"
                    >
                      {S.stage.moreParts(partNames.length - chipCount)}
                    </button>
                  }
                />
                <PopoverContent side="top" align="end" className="app-scroll max-h-[320px] w-[240px] space-y-1 overflow-y-auto" data-testid="parts-popover">
                  {partNames.map((name, index) => {
                    const off = hidden.includes(name);
                    return (
                      <label key={name} className="flex min-h-7 items-center gap-2">
                        <Checkbox
                          checked={!off}
                          onCheckedChange={() => {
                            ui.togglePart(name);
                          }}
                        />
                        <span className="size-2.5 shrink-0 rounded-pill" style={{ background: `var(--hotend-part-${(index % 8) + 1})` }} />
                        <span className="t-ui min-w-0 truncate" title={partWords(name)}>
                          {partWords(name)}
                        </span>
                      </label>
                    );
                  })}
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={hidden.length === 0}
                    onClick={() => {
                      ui.showAllParts();
                    }}
                  >
                    {S.stage.showAllParts}
                  </Button>
                </PopoverContent>
              </Popover>
            ) : null}
          </div>
        ) : null}
        {onStage === null ? (
          <p className="t-caption flex h-6 items-center rounded-tag border border-app-line bg-app-raised px-2 text-app-text-2">{S.stage.plate("Bambu Lab X2D", PLATE[0], PLATE[1])}</p>
        ) : asPlate ? (
          // One chip, one line. What the picture is drawn from is said; that there is no
          // camera is a detail for whoever asks (the tooltip).
          <p
            className="t-caption num pointer-events-auto flex h-6 items-center rounded-tag border border-app-line bg-app-raised px-2 whitespace-nowrap text-app-text-2"
            title={printing ? S.stage.printCaptionHint : undefined}
            data-testid="plate-caption"
          >
            {printing ? S.stage.printCaption(plateSize[0], plateSize[1]) : S.stage.plateShort(plateSize[0], plateSize[1])}
          </p>
        ) : overall ? (
          <Flash
            of={overall.size}
            className="t-caption num font-mono pointer-events-auto flex min-h-6 items-center rounded-tag border border-app-line bg-app-raised px-2 text-app-text-2"
            data-testid="overall-size"
            title={overall.hint ?? undefined}
          >
            {overall.size}
          </Flash>
        ) : null}
      </div>

      {/* Bottom left, above the viewer's axis marker: a failed command. */}
      <div className="pointer-events-none absolute bottom-[104px] left-3 flex max-w-[60%] flex-col items-start gap-2">
        {error ? (
          <ApiErrorCard
            error={error}
            className="pointer-events-auto max-w-[360px] border border-app-line shadow-1"
            action={
              <Button variant="outline" size="sm" onClick={dismissError}>
                {S.stage.clearPick}
              </Button>
            }
          />
        ) : null}
      </div>
    </div>
  );
}
