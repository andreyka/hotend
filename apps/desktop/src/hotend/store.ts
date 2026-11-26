// The Hotend module's state, shaped around the facade's own types. Everything it holds
// came from the core; nothing here is a second copy of a decision the core makes. The
// verdicts, marks and sentences are computed from this state by pure functions
// (./verdict.ts), never stored.
import { createStore, type StoreApi } from "zustand/vanilla";

import {
  toApiError,
  type ApiError,
  type CoreClient,
  type DeliveryChoice,
  type EngineResult,
  type Preparation,
  type PrepareOptions,
  type PreviewsMode,
  type PrinterInfo,
  type PrintJob,
  type PrintReview,
  type Project,
  type ProjectDetail,
  type ShellEvent,
  type SliceOutcome,
  type Version,
} from "@/lib/core";

import { calibrationState, type CalibrationState } from "./calibration";
import { duration, sized, weight } from "./format";
import { proofOf, statusLine, type VerdictKind } from "./proof";
import {
  changesProject,
  liveRunOf,
  noteLiveRun,
  reduceRun,
  rememberedRun,
  rememberRun,
  resumeRun,
  startRun,
  withMarks,
  withStepAfter,
  type RunState,
} from "./run";
import { sizeChanges } from "./versions";

/**
 * Where the plate goes from the Print stage. `bambu_studio_project` is no delivery of the
 * print job: it is the editable Bambu Studio project of the plate, which the Print stage
 * makes and hands over on its own (`inspector/ProjectHandOver.tsx`).
 */
export type DeliveryKind = "simulated_printer" | "bambu_connect" | "export_to_folder" | "bambu_studio_project";

export type CardInfo = {
  version: number | null;
  status: { kind: VerdictKind; text: string } | null;
  printing: PrintJob | null;
};

/** `again`: no size changed; the version is built again as it is (after a calibration). */
export type Rebuild = { name: string; label: string; unit: string; value: number; since: number; again?: boolean };
export type Refused = { name: string; label: string; unit: string; value: number; still: number; reason: string };
/** A rebuild that could not run at all: the computer's fault, not the size's. */
export type RebuildError = { name: string; label: string; unit: string; value: number; still: number; error: ApiError };
/** A measurement the maker sent that the run did not build into a new version. */
export type NotApplied = { name: string; value: number };

export const ACTIVE_JOB = new Set(["starting", "printing"]);

export type HotendState = {
  /** Null until the list was read. */
  projects: Project[] | null;
  projectsError: ApiError | null;
  cards: Record<string, CardInfo>;
  printers: PrinterInfo[] | null;
  /** Print jobs that are starting or printing, in any project, by job id. */
  live: Record<string, PrintJob>;

  /** The project that is open in the studio. */
  project: ProjectDetail | null;
  /** The engine's result per version of the open project. */
  results: Record<number, EngineResult>;
  /** The text of mesh.json per version. `null` when it could not be read. */
  meshes: Record<number, string | null>;
  /** An older version on the stage, or null for the current one. */
  look: number | null;
  /** Versions undone in this session, for going forward again. */
  redo: number[];
  run: RunState | null;
  /**
   * The run of each project in this session of the window, by project id: a run goes on
   * while the maker is on Home or in another project, and is there again on the way back.
   * `run` is the open project's entry.
   */
  runs: Record<string, RunState>;
  /** The message that was sent and is not in the stored conversation yet. */
  sent: string | null;
  /** The maker's last message, for "Try again". */
  lastMessage: string | null;
  /** A message the core refused to start. */
  sendError: ApiError | null;
  rebuild: Rebuild | null;
  queued: Record<string, number>;
  refused: Refused | null;
  rebuildError: RebuildError | null;
  notApplied: NotApplied | null;

  preparation: Preparation | null;
  preparing: boolean;
  prepareError: ApiError | null;
  /** The plate on the stage, from 1. */
  plate: number;
  quality: string;
  slice: SliceOutcome | null;
  /** The layout changed after `slice` was made. */
  sliceStale: boolean;
  slicing: boolean;
  sliceError: ApiError | null;

  /**
   * What was measured on this printer, read when a project opens; null for a printer that
   * was never calibrated, undefined until it is read.
   */
  calibration: CalibrationState | null | undefined;

  jobs: PrintJob[];
  /**
   * The layout of the version the newest print is of, when that is not the current
   * version: the Print stage draws the plate that is being printed, not the design as it
   * is now.
   */
  jobPreparation: Preparation | null;
  /** The slice of that version, for the weight of the print; null when the core has none. */
  jobSlice: SliceOutcome | null;
  /**
   * The plate of the layout each job this window started is of, as its review said
   * (`PrintReview.plate`): a job's own record does not say, and its slice may have been
   * replaced since.
   */
  jobPlates: Record<string, number>;
  /** The spools each job this window started draws from: AMS unit and tray, counted from 0. */
  jobSpools: Record<string, { unit: number; tray: number }[]>;
  /** The layer the printer was at when the maker stopped a job in this window. */
  jobStops: Record<string, number | null>;
  delivery: DeliveryKind;
  folder: string | null;
  review: PrintReview | null;
  reviewing: boolean;
  reviewError: ApiError | null;
  /** A sentence under the review after a dialog that started nothing. */
  reviewNote: string | null;
  /** The confirmation dialog is open. */
  confirming: boolean;
  /** The maker asked for a new review although the last job has an outcome. */
  wantReview: boolean;

  /** The last command that failed with nothing more specific to show it. */
  error: ApiError | null;

  loadProjects(): Promise<void>;
  loadPrinters(): Promise<void>;
  createProject(name: string): Promise<Project | null>;
  /** Builds the bundled example as a new project. Null when it failed. */
  openExample(name: string, code: string, note: string): Promise<Project | null>;
  openProject(projectId: string): Promise<void>;
  closeProject(): void;
  /** `measured` names the size a measurement in the message is for (Sizes, "To measure"). */
  sendMessage(text: string, measured?: NotApplied): Promise<void>;
  cancelRun(): Promise<void>;
  lookAt(version: number | null): Promise<void>;
  useVersion(version: number): Promise<void>;
  /** Reads the open project again: after something outside the store changed it (a snapshot gone back to). */
  reloadProject(): Promise<void>;
  undo(): Promise<void>;
  redoNext(): Promise<void>;
  setSize(name: string, value: number): void;
  /** Builds the current version again as it is: with the printer's calibration of today. */
  rebuildCurrent(): void;
  clearRefused(): void;
  clearNotApplied(): void;

  enterPrepare(materialId: string): Promise<void>;
  layOut(change: { copies?: Record<string, number>; orientations?: Record<string, string> }): Promise<void>;
  setPlate(plate: number): void;
  setQuality(profile: string): void;
  sliceNow(): Promise<void>;

  enterPrint(): Promise<void>;
  setDelivery(kind: DeliveryKind): Promise<void>;
  pickFolder(title: string): Promise<void>;
  reviewAgain(): Promise<void>;
  confirmPrint(): Promise<void>;
  stopPrint(jobId: string): Promise<void>;

  receive(event: ShellEvent): void;
  dismissError(): void;
};

export type HotendStore = StoreApi<HotendState>;

const EMPTY_PROJECT = {
  project: null,
  results: {},
  meshes: {},
  look: null,
  redo: [],
  run: null,
  sent: null,
  lastMessage: null,
  sendError: null,
  rebuild: null,
  queued: {},
  refused: null,
  rebuildError: null,
  notApplied: null,
  preparation: null,
  preparing: false,
  prepareError: null,
  plate: 1,
  slice: null,
  sliceStale: false,
  slicing: false,
  sliceError: null,
  jobs: [],
  jobPreparation: null,
  jobSlice: null,
  review: null,
  reviewing: false,
  reviewError: null,
  reviewNote: null,
  confirming: false,
  wantReview: false,
} satisfies Partial<HotendState>;

/** The version of the open project with this number. */
export function versionOf(detail: ProjectDetail | null, number: number | null): Version | null {
  if (detail === null || number === null) return null;
  return detail.versions.find((version) => version.number === number) ?? null;
}

/** The slice of the current version, if it still matches the layout and the quality. */
export function currentSlice(state: Pick<HotendState, "slice" | "sliceStale" | "quality" | "project" | "preparation">): SliceOutcome | null {
  const { slice } = state;
  if (!slice || state.sliceStale) return null;
  if (slice.record.version !== state.project?.project.current_version) return null;
  if (slice.record.profile !== state.quality) return null;
  if (state.preparation && state.preparation.created_at_ms > slice.record.created_at_ms) return null;
  return slice;
}

/**
 * True when Hotend-made plate previews are on (`previews`, the settings' value) and the
 * slice was made without them, before they were switched on: Bambu Connect cannot import
 * it, and slicing again adds them. A slice records the settings' `previews` it was made
 * with (`job.previews`); an older record that carries the warning `hotend_made_previews`
 * has them too.
 */
export function slicedBeforePreviews(slice: SliceOutcome | null, previews: PreviewsMode | null | undefined): boolean {
  if (!slice || previews !== "hotend_made") return false;
  if (slice.record.job.previews === "hotend_made") return false;
  return !slice.summary.warnings.some((warning) => warning.code === "hotend_made_previews");
}

/**
 * The plate of the version's layout a slice is of, from 1, as the core recorded it when
 * it sliced; null without a slice. Never the plate that happens to be on the stage.
 */
export function slicedPlate(slice: SliceOutcome | null): number | null {
  return slice?.record.job.plate ?? null;
}

/** The newest job that was sent somewhere, or is waiting for a review. */
export function newestJob(jobs: readonly PrintJob[]): PrintJob | null {
  // A pending job that was withdrawn is `cancelled` without ever having a delivery.
  return jobs.find((job) => !(job.status === "cancelled" && job.delivery === null)) ?? null;
}

/**
 * The version the Print stage shows: the one the print in view is of. A design that was
 * changed while it prints, or after it, does not change what is on the plate.
 */
export function printedVersion(job: PrintJob | null, current: number | null): number | null {
  return job && job.status !== "pending" ? job.version : current;
}

/** True when a failed `parameter_set` says the value is wrong, not that the build could not run. */
export function refusesSize(error: ApiError): boolean {
  if (error.kind === "invalid_input") return true;
  return error.kind === "engine" && (error.code === "model_error" || error.code === "check_error");
}

export function createHotendStore(client: CoreClient): HotendStore {
  return createStore<HotendState>()((set, get) => {
    const fail = (thrown: unknown) => {
      set({ error: toApiError(thrown) });
    };
    const openId = () => get().project?.project.id;
    const isOpen = (projectId: string) => openId() === projectId;

    // The open project's run and its entry in `runs` are one thing.
    const putRun = (projectId: string, run: RunState | null) => {
      set((state) => {
        const rest = Object.fromEntries(Object.entries(state.runs).filter(([id]) => id !== projectId));
        const runs = run === null ? rest : { ...rest, [projectId]: run };
        return state.project?.project.id === projectId || state.run?.projectId === projectId ? { runs, run } : { runs };
      });
    };
    // The versions a project had when the window last read it, for a run that goes on
    // while the project is closed: what comes after is the run's.
    const seenVersions: Record<string, Set<number>> = {};
    /**
     * A run that was going when the window was loaded again (run.ts, `noteLiveRun`): the
     * core appends the maker's message when a run starts and Hotend's answer when it
     * ends, so a last entry that is the maker's, of the run this session started, with
     * no end remembered, is a run that is still going. The core has no command that
     * says so itself; `run_cancel` answering false corrects a wrong guess (cancelRun).
     */
    const resumed = (detail: ProjectDetail): RunState | null => {
      const projectId = detail.project.id;
      const runId = liveRunOf(projectId);
      if (runId === null) return null;
      const last = detail.transcript.at(-1);
      const memory = rememberedRun(runId);
      const going =
        last?.run_id === runId &&
        last.role === "user" &&
        !detail.transcript.some((entry) => entry.run_id === runId && entry.role !== "user") &&
        (memory === null || memory.status === "running");
      if (!going) {
        noteLiveRun(projectId, null);
        return null;
      }
      return resumeRun(runId, projectId, memory);
    };

    // The measurement the running message carries, and how many versions there were.
    let measuring: (NotApplied & { versions: number }) | null = null;
    // True from the first line of `enterPrint` to its end: two calls in one tick must not
    // both propose a job.
    let entering = false;

    let printersAt = 0;
    let printersTimer: ReturnType<typeof setTimeout> | null = null;
    const readPrinters = async () => {
      printersAt = Date.now();
      try {
        set({ printers: await client.printers_list() });
      } catch (thrown) {
        fail(thrown);
      }
    };
    // The printer's layer and bed temperature are not in the job's events: read the
    // printer again, at most twice a second.
    const readPrintersSoon = () => {
      if (printersTimer !== null) return;
      const wait = Math.max(0, 500 - (Date.now() - printersAt));
      printersTimer = setTimeout(() => {
        printersTimer = null;
        void readPrinters();
      }, wait);
    };

    const readResult = async (projectId: string, number: number) => {
      if (get().results[number] !== undefined && isOpen(projectId)) return;
      const detail = await client.version_get(projectId, number);
      if (isOpen(projectId)) set((state) => ({ results: { ...state.results, [number]: detail.result } }));
    };
    const readMesh = async (projectId: string, number: number) => {
      if (get().meshes[number] !== undefined) return;
      let text: string | null;
      try {
        text = await client.version_mesh(projectId, number);
      } catch {
        text = null;
      }
      if (isOpen(projectId)) set((state) => ({ meshes: { ...state.meshes, [number]: text } }));
    };

    const readLayout = async (projectId: string) => {
      const version = get().project?.project.current_version ?? null;
      if (version === null) {
        set({ preparation: null, slice: null, sliceStale: false });
        return;
      }
      const [preparation, slice] = await Promise.all([
        client.preparation_get(projectId, version),
        client.slice_get(projectId, null),
      ]);
      if (!isOpen(projectId) || get().project?.project.current_version !== version) return;
      set((state) => ({
        preparation,
        // A slice made in this session stays on screen, marked as changed, when the
        // layout moved on; the core no longer counts it.
        slice: slice ?? (state.slice?.record.version === version ? state.slice : null),
        sliceStale: slice === null && state.slice?.record.version === version,
        quality: slice?.record.profile ?? state.quality,
      }));
    };

    // The mesh and the layout of the version the newest print is of (see `jobPreparation`).
    const readJobView = async (projectId: string) => {
      const job = newestJob(get().jobs);
      const current = get().project?.project.current_version ?? null;
      if (!job || job.status === "pending" || job.version === current) {
        if ((get().jobPreparation !== null || get().jobSlice !== null) && isOpen(projectId)) set({ jobPreparation: null, jobSlice: null });
        return;
      }
      if (get().jobPreparation?.version === job.version) return;
      await readMesh(projectId, job.version);
      const [preparation, slice] = await Promise.all([
        client.preparation_get(projectId, job.version).catch(() => null),
        client.slice_get(projectId, job.version).catch(() => null),
      ]);
      if (isOpen(projectId)) set({ jobPreparation: preparation, jobSlice: slice });
    };

    const readJobs = async (projectId: string) => {
      const jobs = await client.print_jobs(projectId);
      if (isOpen(projectId)) {
        set({ jobs });
        void readJobView(projectId);
      }
      return jobs;
    };

    // Reads the open project again: its versions, the results that are missing, the mesh
    // of the version on the stage. Calls overlap while a run streams; only answers for
    // the open project are kept.
    const refresh = async (projectId: string) => {
      try {
        const before = get().project;
        const detail = await client.project_get(projectId);
        if (!isOpen(projectId)) return;
        const knew = new Set(before?.versions.map((version) => version.number));
        const made = detail.versions.some((version) => !knew.has(version.number));
        const added = detail.versions.filter((version) => !knew.has(version.number)).map((version) => version.number);
        set((state) => ({
          project: detail,
          // A new version ends the trail of undone versions.
          redo: made && before ? [] : state.redo,
        }));
        const live = get().run;
        if (live?.status === "running" && before) putRun(projectId, withMarks(live, added));
        const current = detail.project.current_version;
        await Promise.all([
          ...detail.versions.map((version) => readResult(projectId, version.number)),
          ...(current === null ? [] : [readMesh(projectId, current)]),
        ]);
        if (current !== before?.project.current_version) {
          set({ plate: 1, review: null, reviewError: null, reviewNote: null, prepareError: null, sliceError: null });
          await readLayout(projectId);
          await readJobView(projectId);
        }
      } catch (thrown) {
        fail(thrown);
      }
    };

    const readCard = async (project: Project) => {
      try {
        const [detail, jobs] = await Promise.all([
          project.current_version === null ? null : client.version_get(project.id, project.current_version),
          client.print_jobs(project.id),
        ]);
        const card: CardInfo = {
          version: project.current_version,
          status: detail ? statusLine(proofOf(detail.result)) : null,
          printing: jobs.find((job) => ACTIVE_JOB.has(job.status)) ?? null,
        };
        set((state) => ({ cards: { ...state.cards, [project.id]: card } }));
      } catch {
        // The card then shows its name and time only.
      }
    };

    // What the project showed after a tool call, for the finished step (./run.ts).
    const describeStep = (toolCallId: string, name: string, knownVersions: number) => {
      const state = get();
      const run = state.run;
      const detail = state.project;
      if (!run || !detail) return;
      const made = detail.versions.filter((version) => version.number > knownVersions).at(-1);
      if (name === "write_model_code" && made) {
        putRun(run.projectId, withStepAfter(run, toolCallId, { version: made.number }));
      } else if (name === "set_parameter" && made) {
        const change = sizeChanges(versionOf(detail, made.parent), made, state.results[made.number])[0];
        putRun(
          run.projectId,
          withStepAfter(run, toolCallId, {
            version: made.number,
            ...(change ? { size: { label: change.label, value: sized(change.to, change.unit) } } : {}),
          }),
        );
      } else if (name === "slice" && state.slice) {
        const grams = state.slice.summary.filament.reduce((sum, use) => sum + use.grams, 0);
        putRun(
          run.projectId,
          withStepAfter(run, toolCallId, {
            slice: { time: duration(state.slice.summary.time_s), grams: weight(grams) },
          }),
        );
      }
    };

    const runSize = async (name: string, value: number, again = false) => {
      const state = get();
      const detail = state.project;
      const current = detail?.project.current_version ?? null;
      if (!detail || current === null) return;
      const projectId = detail.project.id;
      const spec = state.results[current]?.param_specs?.[name];
      const label = spec?.label ?? name;
      const unit = spec?.unit ?? "mm";
      const still = versionOf(detail, current)?.params[name] ?? spec?.default ?? value;
      // A build that starts takes the last refusal with it: one message at a time.
      set({ rebuild: { name, label, unit, value, since: Date.now(), again }, refused: null, rebuildError: null, notApplied: null });
      try {
        const report = await client.parameter_set(projectId, name, value);
        if (!isOpen(projectId)) return;
        if (report.version) {
          set((s) => ({ results: { ...s.results, [report.version?.number ?? 0]: report.result }, look: null }));
          await refresh(projectId);
        } else {
          const reason = report.result.error?.message ?? "";
          set({ refused: { name, label, unit, value, still, reason } });
        }
      } catch (thrown) {
        if (isOpen(projectId)) {
          const error = toApiError(thrown);
          // Only a value the model or the core refuses is the size's fault. A sandbox that
          // cannot start or a core that does not answer is said as what it is.
          if (refusesSize(error)) set({ refused: { name, label, unit, value, still, reason: error.message } });
          else set({ rebuildError: { name, label, unit, value, still, error } });
        }
      } finally {
        if (isOpen(projectId)) {
          const waiting = Object.entries(get().queued);
          const next = waiting[0];
          if (next) {
            set({ queued: Object.fromEntries(waiting.slice(1)) });
            void runSize(next[0], next[1]);
          } else {
            set({ rebuild: null });
          }
        }
      }
    };

    const revert = async (number: number) => {
      const projectId = openId();
      if (projectId === undefined) return false;
      try {
        await client.version_revert(projectId, number);
        set({ look: null, refused: null, rebuildError: null });
        await refresh(projectId);
        return true;
      } catch (thrown) {
        fail(thrown);
        return false;
      }
    };

    const deliveryChoice = (): DeliveryChoice | null => {
      const { delivery, folder } = get();
      if (delivery === "export_to_folder") {
        return folder === null ? null : { kind: "export_to_folder", folder };
      }
      // The project is no delivery of the job: the job is neither reviewed again for it
      // nor confirmed.
      if (delivery === "bambu_studio_project") return null;
      return { kind: delivery };
    };

    // The core refused a review or a confirmation because the version was laid out again
    // since the slice (`slice_changed`), or because the plate's print settings changed
    // since it was sliced (`plan_changed`: another baseline, spool, nozzle or setting),
    // by Hotend's AI or outside this window. The layout and the slice (`slice_get`) are
    // read again, and Prepare offers to slice again.
    const afterRefusal = async (projectId: string, error: ApiError) => {
      if (error.kind !== "print") return;
      if (error.code !== "slice_changed" && error.code !== "plan_changed") return;
      await readLayout(projectId).catch(() => undefined);
    };

    // Makes sure one pending job exists for the current slice, and reads its review.
    const review = async (projectId: string) => {
      const slice = currentSlice(get());
      if (!slice) return;
      set({ reviewing: true, reviewError: null });
      try {
        const jobs = await readJobs(projectId);
        const sha = slice.summary.sha256;
        const pending = jobs.filter((job) => job.status === "pending");
        let job = pending.find((candidate) => candidate.slice_sha256 === sha) ?? null;
        // At most one pending job per project: an older one is withdrawn.
        for (const old of pending) {
          if (old.id !== job?.id) await client.print_cancel(old.id);
        }
        job ??= await client.print_propose(projectId, "");
        const delivery = deliveryChoice() ?? { kind: "simulated_printer" };
        const answer = await client.print_review(job.id, delivery);
        await readJobs(projectId);
        if (isOpen(projectId)) set({ review: answer });
      } catch (thrown) {
        const error = toApiError(thrown);
        if (isOpen(projectId)) {
          set({ review: null, reviewError: error });
          await afterRefusal(projectId, error);
        }
      } finally {
        if (isOpen(projectId)) set({ reviewing: false });
      }
    };

    return {
      projects: null,
      projectsError: null,
      cards: {},
      printers: null,
      live: {},
      runs: {},
      calibration: undefined,
      jobPlates: {},
      jobSpools: {},
      jobStops: {},
      quality: "demo-0.20mm",
      delivery: "simulated_printer",
      folder: null,
      error: null,
      ...EMPTY_PROJECT,

      async loadProjects() {
        try {
          const projects = await client.projects_list();
          set({ projects, projectsError: null });
          await Promise.all([
            ...projects.slice(0, 48).map(readCard),
            // A run this session left going in a project says so on its card after a reload.
            ...projects
              .filter((project) => get().runs[project.id] === undefined && liveRunOf(project.id) !== null)
              .map(async (project) => {
                const run = resumed(await client.project_get(project.id));
                if (run && get().runs[project.id] === undefined) putRun(project.id, run);
              }),
          ]);
        } catch (thrown) {
          set({ projectsError: toApiError(thrown) });
        }
      },

      loadPrinters: readPrinters,

      async createProject(name) {
        try {
          const project = await client.project_create(name);
          set((state) => ({ projects: [project, ...(state.projects ?? [])] }));
          return project;
        } catch (thrown) {
          fail(thrown);
          return null;
        }
      },

      async openExample(name, code, note) {
        try {
          const project = await client.project_create(name);
          const report = await client.model_build(project.id, code, {}, note);
          if (!report.version) {
            fail({
              kind: "engine",
              code: "model_error",
              message: report.result.error?.message ?? "The example did not build.",
              next_step: "Open Settings, This computer, and check the modelling engine.",
            } satisfies ApiError);
          }
          return project;
        } catch (thrown) {
          fail(thrown);
          return null;
        }
      },

      async openProject(projectId) {
        if (!isOpen(projectId)) {
          // A run that is going, or one that ended badly, is there again; a run that ended
          // well is in the stored conversation.
          const known = get().runs[projectId];
          const kept = known && ["running", "error", "budget"].includes(known.status) ? known : null;
          set({ ...EMPTY_PROJECT, run: kept });
        }
        try {
          const detail = await client.project_get(projectId);
          const taken = get().run ?? resumed(detail);
          const asked = taken ? (detail.transcript.findLast((entry) => entry.run_id === taken.runId && entry.role === "user")?.text ?? null) : null;
          set((state) => ({
            project: detail,
            run: taken,
            runs: taken ? { ...state.runs, [projectId]: taken } : state.runs,
            lastMessage: state.lastMessage ?? asked,
          }));
          const current = detail.project.current_version;
          // What the Proof says of fits depends on whether this printer was measured.
          void client.calibration_get().then(
            (calibration) => {
              set({ calibration: calibrationState(calibration.table) });
            },
            () => undefined,
          );
          await Promise.all([
            ...detail.versions.map((version) => readResult(projectId, version.number)),
            ...(current === null ? [] : [readMesh(projectId, current)]),
            readLayout(projectId),
            readJobs(projectId),
          ]);
        } catch (thrown) {
          fail(thrown);
        }
      },

      closeProject() {
        const detail = get().project;
        if (detail) seenVersions[detail.project.id] = new Set(detail.versions.map((version) => version.number));
        // `runs` stays: a run does not end because the maker went to Home.
        set({ ...EMPTY_PROJECT });
      },

      async sendMessage(text, measured) {
        const projectId = openId();
        if (projectId === undefined) return;
        measuring = measured ? { ...measured, versions: get().project?.versions.length ?? 0 } : null;
        set({ sent: text, lastMessage: text, sendError: null, error: null, look: null, refused: null, rebuildError: null, notApplied: null });
        try {
          const { run_id } = await client.message_send(projectId, text);
          noteLiveRun(projectId, run_id);
          // The first events may have arrived before this answer did.
          if (get().runs[projectId]?.runId !== run_id) putRun(projectId, startRun(run_id, projectId));
        } catch (thrown) {
          // The message was not sent: the composer takes it back (Conversation.tsx), so
          // that nothing the maker typed is lost.
          measuring = null;
          set({ sent: null, sendError: toApiError(thrown) });
        }
      },

      async cancelRun() {
        const run = get().run;
        if (run?.status !== "running") return;
        putRun(run.projectId, { ...run, stopping: true });
        try {
          const going = await client.run_cancel(run.runId);
          // A run taken up after a reload that the core does not know had ended while
          // nobody listened: the stored project says how.
          if (!going && run.resumed && get().runs[run.projectId]?.runId === run.runId) {
            noteLiveRun(run.projectId, null);
            putRun(run.projectId, null);
            if (isOpen(run.projectId)) {
              set({ sent: null });
              await refresh(run.projectId);
            }
          }
        } catch (thrown) {
          fail(thrown);
        }
      },

      async lookAt(version) {
        const detail = get().project;
        if (!detail) return;
        const current = detail.project.current_version;
        const look = version === current ? null : version;
        if (look !== null) await readMesh(detail.project.id, look);
        set({ look });
      },

      async useVersion(version) {
        await revert(version);
      },

      async reloadProject() {
        const projectId = openId();
        if (projectId === undefined) return;
        set({ look: null, refused: null, rebuildError: null });
        await refresh(projectId);
      },

      async undo() {
        const detail = get().project;
        const current = versionOf(detail, detail?.project.current_version ?? null);
        if (!current || current.parent === null) return;
        const from = current.number;
        if (await revert(current.parent)) set((state) => ({ redo: [...state.redo, from] }));
      },

      async redoNext() {
        const next = get().redo.at(-1);
        if (next === undefined) return;
        if (await revert(next)) set((state) => ({ redo: state.redo.slice(0, -1) }));
      },

      setSize(name, value) {
        if (get().rebuild) {
          // One build at a time; the latest value of each size wins.
          set((state) => ({ queued: { ...state.queued, [name]: value } }));
          return;
        }
        void runSize(name, value);
      },

      rebuildCurrent() {
        const state = get();
        const current = state.project?.project.current_version ?? null;
        const version = versionOf(state.project, current);
        const specs = current === null ? undefined : state.results[current]?.param_specs;
        // The core rebuilds a version when a size is set, to the value it has too.
        const name = Object.keys(version?.params ?? {})[0] ?? Object.keys(specs ?? {})[0];
        if (name === undefined || state.rebuild) return;
        const value = version?.params[name] ?? specs?.[name]?.default;
        if (value === undefined) return;
        void runSize(name, value, true);
      },

      clearRefused() {
        set({ refused: null, rebuildError: null });
      },

      clearNotApplied() {
        set({ notApplied: null });
      },

      async enterPrepare(materialId) {
        const detail = get().project;
        const version = detail?.project.current_version ?? null;
        if (!detail || version === null || get().preparing) return;
        const projectId = detail.project.id;
        const stored = get().preparation;
        if (stored?.version === version && stored.material_id === materialId) return;
        set({ preparing: true, prepareError: null });
        try {
          let preparation = await client.preparation_get(projectId, version);
          if (!preparation || preparation.material_id !== materialId) {
            preparation = await client.prepare_run(projectId, null, stored?.options ?? { copies: {}, orientations: {} });
          }
          if (isOpen(projectId)) set({ preparation });
          await readLayout(projectId);
        } catch (thrown) {
          if (isOpen(projectId)) set({ prepareError: toApiError(thrown) });
        } finally {
          if (isOpen(projectId)) set({ preparing: false });
        }
      },

      async layOut(change) {
        const projectId = openId();
        const stored = get().preparation;
        if (projectId === undefined || !stored || get().preparing) return;
        const options: PrepareOptions = {
          copies: { ...stored.options.copies, ...change.copies },
          orientations: { ...stored.options.orientations, ...change.orientations },
        };
        set({ preparing: true, prepareError: null });
        try {
          const preparation = await client.prepare_run(projectId, null, options);
          if (isOpen(projectId)) {
            set((state) => ({
              preparation,
              plate: Math.min(state.plate, Math.max(1, preparation.plates.length)),
              sliceStale: state.slice !== null,
              review: null,
            }));
          }
        } catch (thrown) {
          if (isOpen(projectId)) set({ prepareError: toApiError(thrown) });
        } finally {
          if (isOpen(projectId)) set({ preparing: false });
        }
      },

      setPlate(plate) {
        set({ plate });
      },

      setQuality(profile) {
        set({ quality: profile, review: null });
      },

      async sliceNow() {
        const projectId = openId();
        if (projectId === undefined || get().slicing) return;
        set({ slicing: true, sliceError: null });
        try {
          const slice = await client.slice_run(projectId, get().quality, get().plate);
          if (isOpen(projectId)) {
            set({ slice, sliceStale: false, preparation: slice.preparation, review: null, wantReview: true });
          }
        } catch (thrown) {
          if (isOpen(projectId)) set({ sliceError: toApiError(thrown) });
        } finally {
          if (isOpen(projectId)) set({ slicing: false });
        }
      },

      async enterPrint() {
        const projectId = openId();
        if (projectId === undefined || entering || get().reviewing || get().confirming) return;
        entering = true;
        try {
          const jobs = await readJobs(projectId);
          void readPrinters();
          const newest = newestJob(jobs);
          if (newest && ACTIVE_JOB.has(newest.status)) return;
          const slice = currentSlice(get());
          if (!slice) return;
          const hasOutcome =
            newest !== null && newest.status !== "pending" && newest.slice_sha256 === slice.summary.sha256;
          if (hasOutcome && !get().wantReview) return;
          if (get().review?.job.slice_sha256 === slice.summary.sha256 && get().review?.job.status === "pending") return;
          await review(projectId);
        } catch (thrown) {
          fail(thrown);
        } finally {
          entering = false;
        }
      },

      async setDelivery(kind) {
        set({ delivery: kind, reviewNote: null });
        const projectId = openId();
        if (projectId !== undefined && deliveryChoice() !== null && get().review) await review(projectId);
      },

      async pickFolder(title) {
        try {
          const folder = await client.folder_pick(title);
          if (folder === null) return;
          set({ folder });
          const projectId = openId();
          if (projectId !== undefined && get().delivery === "export_to_folder") await review(projectId);
        } catch (thrown) {
          fail(thrown);
        }
      },

      async reviewAgain() {
        const projectId = openId();
        if (projectId === undefined) return;
        set({ wantReview: true, reviewNote: null });
        await review(projectId);
      },

      async confirmPrint() {
        const job = get().review?.job;
        const delivery = deliveryChoice();
        const projectId = openId();
        if (!job || !delivery || projectId === undefined || get().confirming) return;
        set({ confirming: true, reviewNote: null, reviewError: null });
        try {
          const answer = await client.print_confirm(job.id, delivery);
          if (!isOpen(projectId)) return;
          if (answer.confirmed) {
            // The plate the review and the confirmation dialog named.
            const plate = get().review?.plate ?? slicedPlate(get().slice);
            const spools = (get().review?.spools ?? []).map((spool) => ({ unit: spool.ams_unit, tray: spool.tray }));
            set((state) => ({
              review: null,
              wantReview: false,
              jobPlates: plate === null ? state.jobPlates : { ...state.jobPlates, [job.id]: plate },
              jobSpools: { ...state.jobSpools, [job.id]: spools },
            }));
            await readJobs(projectId);
            void readPrinters();
          } else {
            set({ reviewNote: "not_confirmed" });
          }
        } catch (thrown) {
          const error = toApiError(thrown);
          if (isOpen(projectId)) {
            set({ reviewError: error, review: null });
            await afterRefusal(projectId, error);
          }
        } finally {
          if (isOpen(projectId)) set({ confirming: false });
        }
      },

      async stopPrint(jobId) {
        // Where the print was when it was stopped: the core's record of a stopped job
        // does not say.
        const layer = get().printers?.[0]?.state?.layer ?? null;
        set((state) => ({ jobStops: { ...state.jobStops, [jobId]: layer } }));
        try {
          await client.print_cancel(jobId);
          const projectId = openId();
          if (projectId !== undefined) await readJobs(projectId);
        } catch (thrown) {
          fail(thrown);
        }
      },

      receive(event) {
        const projectId = openId();
        if (event.channel === "lagged") {
          if (projectId !== undefined) {
            void refresh(projectId);
            void readJobs(projectId).catch(fail);
          }
          return;
        }
        if (event.channel === "print") {
          readPrintersSoon();
          const patch = (job: PrintJob): PrintJob => {
            const change = event.event;
            if (change.type === "status") return change.job;
            if (change.type === "phase") {
              return { ...job, detail: { ...job.detail, phase: change.phase, headline: change.headline } };
            }
            if (change.type === "progress") {
              return {
                ...job,
                detail: { ...job.detail, progress_percent: change.percent, remaining_minutes: change.remaining_minutes },
              };
            }
            return { ...job, detail: { ...job.detail, problems: [...job.detail.problems, change.problem] } };
          };
          const known = get().live[event.job_id] ?? get().jobs.find((job) => job.id === event.job_id);
          if (!known && event.event.type !== "status") {
            // An event of a job this window has not seen: read it once.
            void client.print_status(event.job_id).then((job) => {
              if (ACTIVE_JOB.has(job.status)) set((state) => ({ live: { ...state.live, [job.id]: job } }));
            }, () => undefined);
            return;
          }
          const next = event.event.type === "status" ? event.event.job : known ? patch(known) : null;
          if (!next) return;
          set((state) => {
            const live = Object.fromEntries(Object.entries(state.live).filter(([id]) => id !== next.id));
            if (ACTIVE_JOB.has(next.status)) live[next.id] = next;
            const card = state.cards[next.project_id];
            return {
              live,
              jobs:
                next.project_id === projectId
                  ? state.jobs.some((job) => job.id === next.id)
                    ? state.jobs.map((job) => (job.id === next.id ? next : job))
                    : [next, ...state.jobs]
                  : state.jobs,
              cards: card
                ? { ...state.cards, [next.project_id]: { ...card, printing: ACTIVE_JOB.has(next.status) ? next : null } }
                : state.cards,
            };
          });
          if (next.project_id === projectId) void readJobView(projectId);
          return;
        }
        // A printer's report is the printer status panel's (PrinterStatus.tsx), not a job's.
        if (event.channel === "printer") return;
        // A run event, of the open project or of one the maker has left.
        const of = event.project_id;
        const known = get().runs[of];
        const memory = known?.runId === event.run_id ? null : rememberedRun(event.run_id);
        const run =
          known?.runId === event.run_id
            ? known
            : memory?.status === "running"
              ? resumeRun(event.run_id, of, memory)
              : startRun(event.run_id, of);
        const next = reduceRun(run, event.event);
        putRun(of, next);
        if (next.status !== "running") noteLiveRun(of, null);
        if (!changesProject(event.event)) return;
        const ended = event.event.kind !== "agent";
        if (of !== projectId) {
          // The project is closed: its versions are read for the run's marks, and its card
          // is read again when the run ends.
          void client.project_get(of).then((detail) => {
            const now = get().runs[of];
            if (now?.runId !== next.runId) return;
            const seen = (seenVersions[of] ??= new Set(detail.versions.map((version) => version.number)));
            const added = detail.versions.map((version) => version.number).filter((number) => !seen.has(number));
            for (const number of added) seen.add(number);
            const marked = withMarks(now, added);
            putRun(of, marked);
            rememberRun(marked);
            if (ended) {
              void readCard(detail.project);
              set((state) => ({
                projects: state.projects?.map((project) => (project.id === of ? detail.project : project)) ?? null,
              }));
            }
          }, () => undefined);
          return;
        }
        const tool = ended ? null : next.tools.findLast((step) => step.state !== "running" && step.after === null);
        const knownVersions = Math.max(0, ...(get().project?.versions.map((version) => version.number) ?? []));
        void refresh(projectId).then(async () => {
          if (tool?.name === "slice" || tool?.name === "propose_print" || ended) {
            await readLayout(projectId).catch(() => undefined);
            await readJobs(projectId).catch(() => undefined);
          }
          if (tool) describeStep(tool.id, tool.name, knownVersions);
          if (ended && get().run?.runId === next.runId) {
            const carried = measuring;
            measuring = null;
            const made = (get().project?.versions.length ?? 0) > (carried?.versions ?? 0);
            set({ sent: null, notApplied: carried && !made ? { name: carried.name, value: carried.value } : null });
          }
          const done = get().run;
          if (done?.runId !== next.runId) return;
          rememberRun(done);
          // A run taken up after a reload has only the end of its text: once it has ended
          // well the stored conversation, which has all of it, is what is drawn.
          if (ended && done.resumed && (done.status === "done" || done.status === "cancelled")) putRun(projectId, null);
        });
      },

      dismissError() {
        set({ error: null });
      },
    };
  });
}
