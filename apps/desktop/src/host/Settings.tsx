// Settings (DESIGN_SPEC 4.8): a section list on the left, one column of content, and a
// Back button that returns to where the maker was. Changes apply at once; only the key
// has a Save button.
import { ArrowLeft, Monitor, Moon, Sun } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Link, Navigate, useLocation, useNavigate, useParams } from "react-router";

import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { NumberField } from "@/components/ui/number-field";
import { Segmented } from "@/components/ui/segmented";
import { Select } from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import {
  toApiError,
  type ApiError,
  type Calibration,
  type CalibrationUpdate,
  type GaugeExport,
  type GaugeReading,
  type HandOffStatus,
  type ProviderSettings,
  type SlicerInstallation,
} from "@/lib/core";
import { cn } from "@/lib/utils";
import { AmsList } from "@/hotend/Ams";
import { ApiErrorCard, ErrorCard, PathText, SimTag, StatusIcon } from "@/hotend/bits";
import { useHotend } from "@/hotend/context";
import { calibrationState, quantityWords, sharedEnding } from "@/hotend/calibration";
import { plain } from "@/hotend/format";
import { handoffFolderOf, windowsForm, withHandoffFolder } from "@/hotend/handoff";

import { useCore, useHost, useHostStore } from "./core-context";
import { PrinterLanSection, ResearchSection } from "./pending-sections";
import { AiSection, EffortRow, firstModel, healthCounts, HealthRows, MaterialSelect, PlateSelect, Row } from "./sections";
import { H } from "./strings";
import { useTheme, type ThemeChoice } from "./theme";

const SECTIONS = ["ai", "printer", "slicer", "calibration", "appearance", "computer", "shortcuts", "about"] as const;
type Section = (typeof SECTIONS)[number];

function Title({ children }: { children: string }) {
  return <h1 className="t-title pb-4">{children}</h1>;
}

function Sub({ children }: { children: string }) {
  return <h2 className="t-label pt-6 pb-2 text-app-text-2">{children}</h2>;
}

/** The surface every section's rows sit on: the card of Setup (DESIGN_SPEC 4.8). */
const CARD = "rounded-card border border-app-line bg-app-raised p-4";

/** One group of a section: its label over its card. */
function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h2 className="t-label pb-2 text-app-text-2">{title}</h2>
      <div className={CARD}>{children}</div>
    </section>
  );
}

function Ai() {
  const client = useCore();
  const settings = useHost((state) => state.settings);
  const change = useHost((state) => state.change);
  const error = useHost((state) => state.error);
  const projects = useHotend((state) => state.projects);
  const loadProjects = useHotend((state) => state.loadProjects);
  const [draft, setDraft] = useState<ProviderSettings | null>(null);
  useEffect(() => {
    void loadProjects();
  }, [loadProjects]);
  if (!settings) return null;
  const saved = settings.provider;
  const value = draft ?? saved ?? { kind: "anthropic" as const, model: firstModel("anthropic") };
  const switching = saved !== null && value.kind !== saved.kind;
  return (
    <div data-testid="settings-ai">
      <Title>{H.sections.ai}</Title>
      <div className={cn(CARD, "space-y-3")}>
        <AiSection
          value={value}
          scripted={client.transport === "bridge"}
          onChange={(next) => {
            // A provider without a model cannot be saved: it waits for one.
            if (next.model.trim() === "") {
              setDraft(next);
              return;
            }
            setDraft(null);
            void change({ provider: next });
          }}
        />
        <EffortRow
          provider={value}
          value={settings.effort ?? "high"}
          onChange={(effort) => {
            void change({ effort });
          }}
        />
        <Row label={H.ai.effort}>
          <div className="flex items-center gap-3">
            <NumberField
              label={H.ai.effort}
              stepper
              min={1}
              max={200}
              step={1}
              value={settings.max_turns}
              onValueCommitted={(turns) => {
                if (turns !== null && turns !== settings.max_turns) void change({ max_turns: Math.round(turns) });
              }}
            />
            <span className="t-ui text-app-text-2">{H.ai.effortUnit}</span>
          </div>
        </Row>
        <p className="t-caption max-w-[68ch] text-app-text-2" data-testid="work-limit-note">
          {H.ai.effortNote(settings.max_turns)}
        </p>
      </div>
      {saved && (switching || (projects?.length ?? 0) > 0) ? (
        <p className="t-ui max-w-[68ch] pt-5 text-app-text-2">{H.ai.switching(H.providers[saved.kind])}</p>
      ) : null}
      {client.transport === "bridge" && value.kind === "ollama" && value.model === "scripted" ? null : (
        <p className="t-ui max-w-[68ch] pt-3 text-app-text-2">
          {value.kind === "ollama" ? H.ai.sendsOllama : H.ai.sends(H.providers[value.kind])}
        </p>
      )}
      {value.kind === "ollama" ? null : <p className="t-ui max-w-[68ch] pt-1 text-app-text-2">{H.ai.never}</p>}
      {error && value.kind === "ollama" ? <ApiErrorCard error={error} className="mt-4" /> : null}
      <WebResearch />
    </div>
  );
}

/**
 * Web research in Settings, AI model. The conversation's "Turn on web research" opens this
 * page with `focus: "web-research"` when no usable source is chosen: the section is then
 * brought into view.
 */
function WebResearch() {
  const location = useLocation();
  const focus = (location.state as { focus?: unknown } | null)?.focus === "web-research";
  const [section, setSection] = useState<HTMLDivElement | null>(null);
  useEffect(() => {
    if (focus && section) section.scrollIntoView?.({ block: "start" });
  }, [focus, section]);
  return (
    <div id="web-research" ref={setSection} data-testid="web-research" data-focus={focus || undefined}>
      <ResearchSection />
    </div>
  );
}

/**
 * The folder the checked file (and an editable Bambu Studio project) is saved in for a
 * hand-off. It is picked in the system's folder dialog; the window shows it as Windows
 * reads it, and the core's reason when a hand-off cannot be made with it
 * (`handoff_status`).
 */
function HandOffFolder() {
  const client = useCore();
  const chosen = useHost((state) => handoffFolderOf(state.settings));
  const change = useHost((state) => state.change);
  const connect = useHotend((state) => state.printers?.[0]?.deliveries.find((delivery) => delivery.kind === "bambu_connect") ?? null);
  const loadPrinters = useHotend((state) => state.loadPrinters);
  const [error, setError] = useState<ApiError | null>(null);
  // The core's account of the hand-off for the folder in the settings now.
  const [status, setStatus] = useState<{ chosen: string | null; value: HandOffStatus } | null>(null);
  useEffect(() => {
    let gone = false;
    client.handoff_status().then(
      (value) => {
        if (!gone) setStatus({ chosen, value });
      },
      () => undefined,
    );
    return () => {
      gone = true;
    };
  }, [client, chosen]);
  const current = status?.chosen === chosen ? status.value : null;
  const choose = async () => {
    setError(null);
    try {
      const folder = await client.folder_pick(H.handoff.pickTitle);
      if (folder === null) return;
      if (await change(withHandoffFolder(folder))) await loadPrinters();
    } catch (thrown) {
      setError(toApiError(thrown));
    }
  };
  const windows = chosen === null ? null : (current?.windows_folder ?? windowsForm(chosen));
  const problem = current?.problem ?? (connect !== null && !connect.enabled ? connect.disabled_reason : null);
  const using = chosen === null && current?.folder ? (current.windows_folder ?? current.folder) : null;
  return (
    <>
      <Sub>{H.handoff.title}</Sub>
      <div className={cn(CARD, "space-y-2")} data-testid="handoff-folder-setting">
        <p className="t-ui max-w-[68ch] text-app-text-2">{H.handoff.why}</p>
        <p className="t-ui max-w-[68ch] text-app-text-2">{H.handoff.projects}</p>
        {chosen === null ? (
          <div className="space-y-0.5">
            <p className="t-ui">{H.handoff.none}</p>
            {using !== null ? (
              <p className="t-ui text-app-text-2" data-testid="handoff-using">
                {H.handoff.using(using)}
              </p>
            ) : null}
          </div>
        ) : (
          <div className="space-y-0.5">
            <PathText path={chosen} />
            {windows !== null ? <p className="t-ui text-app-text-2">{H.handoff.windows(windows)}</p> : null}
          </div>
        )}
        {problem ? (
          <p className="t-ui flex gap-2" data-testid="handoff-problem">
            <StatusIcon kind="problem" className="mt-px" />
            <span className="min-w-0 flex-1">{problem}</span>
          </p>
        ) : null}
        {current !== null && !current.opens ? (
          <p className="t-ui text-app-text-2" data-testid="handoff-no-opener">
            {H.handoff.noOpener}
          </p>
        ) : null}
        <Button variant="outline" size="sm" onClick={() => void choose()} data-testid="handoff-choose">
          {H.handoff.choose}
        </Button>
        {error ? <ApiErrorCard error={error} /> : null}
      </div>
    </>
  );
}

function Printer() {
  const printer = useHotend((state) => state.printers?.[0] ?? null);
  const loadPrinters = useHotend((state) => state.loadPrinters);
  useEffect(() => {
    void loadPrinters();
  }, [loadPrinters]);
  return (
    <div data-testid="settings-printer">
      <Title>{H.sections.printer}</Title>
      <div className={cn(CARD, "space-y-2")}>
        <div className="flex items-center justify-between gap-3">
          <p className="t-heading">{H.printer.card}</p>
          <SimTag>{H.printer.tag}</SimTag>
        </div>
        <p className="t-ui text-app-text-2">{H.printer.real}</p>
        {printer ? (
          <>
            <p className="t-ui num">
              {printer.state.headline || H.printer.idle} · <span className="text-app-text-2">{H.printer.serial(printer.serial)}</span>
            </p>
            <AmsList units={printer.state.ams} />
          </>
        ) : null}
        <p className="t-ui flex gap-2 pt-1 text-app-text-2">
          <StatusIcon kind="note" className="mt-px" />
          {H.printer.unverified}
        </p>
      </div>

      <Sub>{H.printer.material}</Sub>
      <div className={CARD}>
        <MaterialSelect />
        <p className="t-caption pt-1.5 text-app-text-2">{H.printer.materialNote}</p>
      </div>

      <Sub>{H.printer.plate}</Sub>
      <PlateSelect />
      <p className="t-caption max-w-[68ch] pt-1.5 text-app-text-2">{H.printer.plateNote}</p>

      <HandOffFolder />
      <PrinterLanSection />

      <Sub>{H.printer.ways}</Sub>
      <ul className="space-y-3">
        {printer?.deliveries.map((delivery) => (
          <li key={delivery.kind} className={cn(CARD, !delivery.enabled && "text-app-text-2")}>
            <p className="t-ui-strong flex flex-wrap items-center gap-2">
              {delivery.name}
              {delivery.kind === "simulated_printer" ? <SimTag>{H.printer.tag}</SimTag> : null}
              {!delivery.enabled ? (
                <span className="t-caption font-normal text-app-text-2">
                  {/* A way of this version that waits for a setting is not ready; it is in the build. */}
                  {delivery.kind === "bambu_connect" ? H.printer.notReady : H.printer.notInBuild}
                </span>
              ) : null}
            </p>
            {!delivery.enabled && delivery.disabled_reason ? <p className="t-ui pt-1 text-app-text-2">{delivery.disabled_reason}</p> : null}
            <dl className="t-ui grid grid-cols-[56px_minmax(0,1fr)] gap-x-3 gap-y-1 pt-2">
              {delivery.can.length > 0 ? (
                <>
                  <dt className="text-app-text-2">{H.printer.can}</dt>
                  <dd>
                    <ul>
                      {delivery.can.map((line) => (
                        <li key={line}>{line}</li>
                      ))}
                    </ul>
                  </dd>
                </>
              ) : null}
              {delivery.cannot.length > 0 ? (
                <>
                  <dt className="text-app-text-2">{H.printer.cannot}</dt>
                  <dd>
                    <ul>
                      {delivery.cannot.map((line) => (
                        <li key={line}>{line}</li>
                      ))}
                    </ul>
                  </dd>
                </>
              ) : null}
              {delivery.needs.length > 0 ? (
                <>
                  <dt className="text-app-text-2">{H.printer.needs}</dt>
                  <dd>
                    <ul>
                      {delivery.needs.map((line) => (
                        <li key={line}>{line}</li>
                      ))}
                    </ul>
                  </dd>
                </>
              ) : null}
            </dl>
          </li>
        ))}
      </ul>
    </div>
  );
}

function Slicer() {
  const client = useCore();
  const refresh = useHost((state) => state.refresh);
  const [slicers, setSlicers] = useState<SlicerInstallation[] | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const read = useMemo(
    () => () =>
      client.slicers_list().then(setSlicers, (thrown: unknown) => {
        setError(toApiError(thrown));
      }),
    [client],
  );
  useEffect(() => {
    void read();
  }, [read]);

  const choose = async (slicer: SlicerInstallation) => {
    setError(null);
    try {
      const path = await client.file_pick(H.slicer.chooseTitle(slicer.name));
      if (path === null) return;
      setBusy(slicer.kind);
      await client.slicer_choose(slicer.kind, path);
      await Promise.all([refresh(), read()]);
    } catch (thrown) {
      setError(toApiError(thrown));
    } finally {
      setBusy(null);
    }
  };

  const anyStandIn = slicers?.some((slicer) => slicer.stand_in) ?? false;
  const none = slicers !== null && !slicers.some((slicer) => slicer.usable);
  return (
    <div data-testid="settings-slicer">
      <Title>{H.sections.slicer}</Title>
      <p className="t-ui max-w-[68ch] pb-4 text-app-text-2">{H.slicer.intro}</p>
      {none ? <p className="t-ui pb-3">{H.slicer.none}</p> : null}
      <ul className="space-y-3">
        {slicers?.map((slicer) => (
          <li key={slicer.kind} className={cn(CARD, "space-y-1")} data-testid={`slicer-${slicer.kind}`}>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="t-ui-strong flex min-w-0 flex-wrap items-center gap-2">
                {slicer.stand_in ? H.slicer.standInFor(slicer.kind) : slicer.name}
                {slicer.stand_in ? <SimTag>{H.slicer.standIn}</SimTag> : null}
              </p>
              <Button variant="outline" size="sm" disabled={busy !== null} onClick={() => void choose(slicer)}>
                {busy === slicer.kind ? <Spinner /> : null}
                {H.slicer.choose}
              </Button>
            </div>
            <p className="t-ui flex items-center gap-2">
              <StatusIcon kind={slicer.verdict === "ok" ? "ok" : slicer.verdict === "warn" ? "note" : slicer.path === null ? "skipped" : "problem"} />
              {slicer.path === null
                ? H.slicer.notFound
                : slicer.stand_in
                  ? H.slicer.standInState
                  : (H.slicer.verdicts[slicer.verdict] ?? slicer.verdict)}
              {slicer.version ? <span className="num text-app-text-2">· {H.slicer.version(slicer.version)}</span> : null}
            </p>
            {slicer.path ? <PathText path={slicer.path} /> : null}
            {slicer.reason && !slicer.stand_in ? <p className="t-ui text-app-text-2">{slicer.reason.replace(/^./, (first) => first.toUpperCase())}.</p> : null}
            {slicer.fix ? <p className="t-ui">{slicer.fix}</p> : null}
          </li>
        ))}
      </ul>
      {anyStandIn ? <p className="t-ui max-w-[68ch] pt-4 text-app-text-2">{H.slicer.standInNote}</p> : null}
      {error ? <ApiErrorCard error={error} className="mt-4" /> : null}
    </div>
  );
}

/** How far a reading may lie from the drawn size before it is taken for a slip, in mm. */
const GAUGE_TOLERANCE = 1;

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** The core's refusal of a gauge for a part without fits: nothing to gauge. */
function nothingToGauge(error: ApiError): boolean {
  return "code" in error && error.code === "bad_request";
}

/** "holes come out 0.09 mm smaller": what a change of the table means for a maker. */
function changeWords(change: CalibrationUpdate["changes"][number]): string {
  // An offset is per side; across a hole or a peg it counts twice.
  const across = `${plain(Math.abs(change.after) * 2, 2)}\u00a0mm`;
  if (change.quantity === "hole_offset") {
    return Math.abs(change.after) < 0.005 ? H.calibration.asDrawn("Holes") : H.calibration.holes(across, change.after > 0);
  }
  if (change.quantity === "peg_offset") {
    return Math.abs(change.after) < 0.005 ? H.calibration.asDrawn("Pegs and rails") : H.calibration.pegs(across, change.after > 0);
  }
  return H.calibration.change(change.quantity.replaceAll("_", " "), `${plain(change.before, 3)} mm`, `${plain(change.after, 3)} mm`);
}

/** The gauge's own words for a feature, without the workshop's: a "boss" is the post a screw goes into. */
function plainWords(what: string): string {
  return what.replace(/\bboss\b/g, "post");
}

function CalibrationSection() {
  const client = useCore();
  const projects = useHotend((state) => state.projects);
  const loadProjects = useHotend((state) => state.loadProjects);
  const [calibration, setCalibration] = useState<Calibration | null>(null);
  const [projectId, setProjectId] = useState<string | null>(null);
  // Which projects have a fit to gauge: a snap, a slide, a screw hole. Read once per project.
  const [fits, setFits] = useState<Record<string, boolean>>({});
  const [gauge, setGauge] = useState<{ folder: string; result: GaugeExport } | null>(null);
  const [measured, setMeasured] = useState<Record<string, number | null>>({});
  const [busy, setBusy] = useState<"export" | "record" | "rebuild" | null>(null);
  const [update, setUpdate] = useState<CalibrationUpdate | null>(null);
  // The two things that can fail are said where they happen: the export under step 1,
  // the readings under step 2.
  const [exportError, setExportError] = useState<ApiError | null>(null);
  const [recordError, setRecordError] = useState<ApiError | null>(null);
  const [readError, setReadError] = useState<ApiError | null>(null);
  const [rebuilt, setRebuilt] = useState<{ version: number } | "refused" | null>(null);

  useEffect(() => {
    void loadProjects();
    client.calibration_get().then(setCalibration, (thrown: unknown) => {
      setReadError(toApiError(thrown));
    });
  }, [client, loadProjects]);

  const withPart = useMemo(() => (projects ?? []).filter((project) => project.current_version !== null), [projects]);
  useEffect(() => {
    let left = false;
    for (const project of withPart) {
      if (project.current_version === null) continue;
      client.version_get(project.id, project.current_version).then(
        (detail) => {
          // The engine skips its fit check when the model declares no fit.
          const has = (detail.result.checks ?? []).some((check) => check.id === "fit_pairs" && check.status !== "skipped");
          if (!left) setFits((before) => (before[project.id] === has ? before : { ...before, [project.id]: has }));
        },
        () => undefined,
      );
    }
    return () => {
      left = true;
    };
  }, [client, withPart]);
  const known = withPart.every((project) => fits[project.id] !== undefined);
  const withFits = withPart.filter((project) => fits[project.id] === true);
  const chosen = projectId !== null && withFits.some((project) => project.id === projectId) ? projectId : (withFits[0]?.id ?? null);
  const chosenName = withFits.find((project) => project.id === chosen)?.name ?? "";

  const exportGauge = async () => {
    if (chosen === null) return;
    setExportError(null);
    setUpdate(null);
    setRebuilt(null);
    try {
      const folder = await client.folder_pick(H.calibration.exportTitle);
      if (folder === null) return;
      setBusy("export");
      setGauge({ folder, result: await client.gauge_export(chosen, null, null, folder) });
      setMeasured({});
      setRecordError(null);
    } catch (thrown) {
      setExportError(toApiError(thrown));
    } finally {
      setBusy(null);
    }
  };

  const features = gauge?.result.gauges.flatMap((entry) => entry.dimensions.map((dimension) => ({ ...dimension, key: `${entry.name}:${dimension.id}` }))) ?? [];
  const readings: GaugeReading[] = features.flatMap((feature) => {
    const value = measured[feature.key];
    return value == null ? [] : [{ feature: feature.feature === "peg" ? "peg" : "hole", drawn: feature.drawn, measured: value }];
  });
  // A reading more than a millimetre from the drawing is a slip of the caliper, the unit
  // or the row: it is said at the field, before anything is sent.
  // Where on a feature to measure is the same for all of them: it is said once, not in every row.
  const rowWords = sharedEnding(features.map((feature) => plainWords(feature.what)));
  const off = (feature: { key: string; drawn: number }): number | null => {
    const value = measured[feature.key];
    return value != null && Math.abs(value - feature.drawn) > GAUGE_TOLERANCE ? Math.abs(value - feature.drawn) : null;
  };
  const anyOff = features.some((feature) => off(feature) !== null);

  const record = async () => {
    if (readings.length === 0 || anyOff) return;
    setRecordError(null);
    setRebuilt(null);
    setBusy("record");
    try {
      const answer = await client.calibration_record(readings, null);
      setUpdate(answer);
      setCalibration({ printer_id: answer.printer_id, table: answer.table });
    } catch (thrown) {
      setRecordError(toApiError(thrown));
    } finally {
      setBusy(null);
    }
  };

  // A version knows the calibration it was built with: the project is built again, as it
  // is, so that its fits use what was just measured.
  const rebuild = async () => {
    if (chosen === null) return;
    setBusy("rebuild");
    try {
      const detail = await client.project_get(chosen);
      const current = detail.versions.find((version) => version.number === detail.project.current_version);
      const [name, value] = Object.entries(current?.params ?? {})[0] ?? [];
      if (name === undefined || value === undefined) {
        setRebuilt("refused");
        return;
      }
      const report = await client.parameter_set(chosen, name, value);
      setRebuilt(report.version ? { version: report.version.number } : "refused");
      void loadProjects();
    } catch {
      setRebuilt("refused");
    } finally {
      setBusy(null);
    }
  };

  const state = calibrationState(calibration?.table ?? null);
  const featuresSaved = update?.changes.reduce((sum, change) => sum + change.readings, 0) ?? 0;
  const words = (quantities: readonly string[]) => quantityWords(quantities, H.calibration.quantities);
  return (
    <div data-testid="settings-calibration">
      <Title>{H.calibration.title}</Title>
      <div className="space-y-4">
        <div className={CARD}>
          <p className="t-ui max-w-[68ch] text-app-text-2">{H.calibration.intro}</p>
          <p className="t-ui max-w-[68ch] pt-1.5 text-app-text-2">{H.calibration.caliper}</p>
        </div>

        <Group title={H.calibration.state}>
          <div className="space-y-1.5" data-testid="calibration-state" data-measured={state?.measured.length ?? 0}>
            {calibration === null ? null : state === null || state.at === null ? (
              <p className="t-ui flex items-start gap-2">
                <StatusIcon kind="question" className="mt-px" />
                <span className="min-w-0 flex-1">{H.calibration.none}</span>
              </p>
            ) : (
              <>
                <p className="t-ui flex items-start gap-2">
                  {/* Never the green tick: what was measured does not yet prove a fit. */}
                  <StatusIcon kind="note" className="mt-px" />
                  <span className="min-w-0 flex-1">
                    {featuresSaved > 0
                      ? H.calibration.fromGauge(featuresSaved)
                      : H.calibration.measuredOn(words(state.measured), new Date(state.at).toLocaleDateString())}
                  </span>
                </p>
                {state.guessed.length > 0 ? <p className="t-ui pl-6 text-app-text-2">{H.calibration.stillGeneral(words(state.guessed))}</p> : null}
                <p className="t-ui max-w-[68ch] pl-6 text-app-text-2">{H.calibration.notProven}</p>
              </>
            )}
          </div>
          {readError ? <ApiErrorCard error={readError} className="mt-3" /> : null}
        </Group>

        <Group title={H.calibration.step1}>
          {projects !== null && withPart.length === 0 ? (
            <p className="t-ui max-w-[68ch]">{H.calibration.noProject}</p>
          ) : known && withFits.length === 0 ? (
            <p className="t-ui max-w-[68ch]" data-testid="gauge-no-fits">
              {H.calibration.noFits}
            </p>
          ) : (
            <div className="space-y-3">
              <p className="t-ui max-w-[68ch] text-app-text-2">{H.calibration.step1Line}</p>
              <Row label={H.calibration.project}>
                <Select
                  label={H.calibration.project}
                  value={chosen ?? ""}
                  onChange={(id) => {
                    setProjectId(id);
                    setExportError(null);
                  }}
                  // Only projects with something to gauge: a part without fits has no gauge.
                  options={withFits.map((project) => ({ value: project.id, label: project.name }))}
                  className="w-[280px]"
                />
              </Row>
              <p className="t-caption text-app-text-2">{H.calibration.projectNote}</p>
              <Button variant="outline" disabled={busy !== null || chosen === null} onClick={() => void exportGauge()} data-testid="gauge-export">
                {busy === "export" ? <Spinner /> : null}
                {busy === "export" ? H.calibration.exporting : H.calibration.export}
              </Button>
              {exportError && nothingToGauge(exportError) ? (
                <p className="t-ui flex items-start gap-2" role="alert" data-testid="gauge-nothing">
                  <StatusIcon kind="problem" className="mt-px" />
                  <span className="min-w-0 flex-1">{H.calibration.nothingToGauge}</span>
                </p>
              ) : exportError ? (
                <ApiErrorCard error={exportError} />
              ) : null}
              {gauge ? (
                <div role="status" data-testid="gauge-exported">
                  <p className="t-ui">{H.calibration.exported(gauge.result.files.length)}</p>
                  <PathText path={gauge.folder} />
                  {/* Every file by its name, and what it is for. */}
                  <ul className="space-y-0.5 pt-1.5" data-testid="gauge-files">
                    {gauge.result.files.map((file) => (
                      <li key={file.path} className="t-ui flex flex-wrap items-baseline gap-x-2">
                        <span className="t-mono text-app-text">{fileName(file.path)}</span>
                        <span className="text-app-text-2" data-testid={file.kind === "3mf" ? "gauge-print" : undefined}>
                          {file.kind === "3mf" ? H.calibration.printThis : H.calibration.picture}
                        </span>
                      </li>
                    ))}
                  </ul>
                  {gauge.result.warnings.map((warning) => (
                    <p key={warning} className="t-ui text-app-text-2">
                      {warning}
                    </p>
                  ))}
                </div>
              ) : null}
            </div>
          )}
        </Group>

        <Group title={H.calibration.step2}>
          {features.length === 0 ? (
            <p className="t-ui max-w-[68ch] text-app-text-2">{H.calibration.step2Waiting}</p>
          ) : (
            <div className="space-y-3">
              <p className="t-ui max-w-[68ch] text-app-text-2">
                {H.calibration.step2Line}
                {rowWords.shared ? <span data-testid="gauge-every"> {H.calibration.onEvery(rowWords.shared)}</span> : null}
              </p>
              <table className="w-full border-separate border-spacing-y-1" data-testid="gauge-features">
                <thead>
                  <tr className="t-caption text-left text-app-text-2">
                    <th className="pr-3 font-normal">{H.calibration.feature}</th>
                    <th className="w-[84px] pr-3 text-right font-normal">{H.calibration.drawn}</th>
                    <th className="w-[132px] text-right font-normal">{H.calibration.measured}</th>
                  </tr>
                </thead>
                <tbody>
                  {features.map((feature, index) => (
                    <tr key={feature.key} className="align-top">
                      <td className="t-ui pr-3 pt-[5px]">
                        <span className="num text-app-text-2">{index + 1} · </span>
                        <span className="t-ui-strong">{feature.feature === "peg" ? H.calibration.peg : H.calibration.hole}</span>
                        <span className="text-app-text-2"> · {rowWords.lines[index] ?? plainWords(feature.what)}</span>
                        {off(feature) !== null ? (
                          <span className="t-caption block pt-0.5 text-app-fail" role="alert" data-testid="gauge-off">
                            {H.calibration.off(`${plain(off(feature) ?? 0, 2)} mm`, `${plain(feature.drawn, 2)} mm`)}
                          </span>
                        ) : null}
                      </td>
                      <td className="t-ui num pr-3 pt-[5px] text-right whitespace-nowrap">{plain(feature.drawn, 2)} mm</td>
                      <td className="text-right whitespace-nowrap">
                        <span className="inline-flex items-center gap-1.5">
                          <NumberField
                            label={`${H.calibration.measured}: ${plainWords(feature.what)}`}
                            min={0}
                            step={0.01}
                            format={{ maximumFractionDigits: 3 }}
                            value={measured[feature.key] ?? null}
                            onValueChange={(value) => {
                              setMeasured((before) => ({ ...before, [feature.key]: value }));
                            }}
                            inputClassName="w-[72px]"
                            invalid={off(feature) !== null}
                          />
                          <span className="t-ui text-app-text-2">mm</span>
                        </span>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <div className="flex flex-wrap items-center gap-3">
                <Button variant="outline" disabled={readings.length === 0 || anyOff || busy !== null} onClick={() => void record()} data-testid="calibration-record">
                  {busy === "record" ? <Spinner /> : null}
                  {busy === "record" ? H.calibration.recording : H.calibration.record}
                </Button>
                {readings.length === 0 ? <span className="t-ui text-app-text-2">{H.calibration.nothing}</span> : null}
              </div>
            </div>
          )}
          {update ? (
            <div className="space-y-2 pt-4" role="status" data-testid="calibration-result">
              <p className="t-ui">{update.changes.length === 0 ? H.calibration.noChange : H.calibration.result}</p>
              <ul className="space-y-1">
                {update.changes.map((change) => (
                  <li key={change.quantity} className="t-ui num">
                    {changeWords(change)}
                  </li>
                ))}
              </ul>
              {/* What was built before does not know these values: the way to use them is one button. */}
              {chosen !== null && rebuilt === null ? (
                <Button disabled={busy !== null} onClick={() => void rebuild()} data-testid="calibration-rebuild">
                  {busy === "rebuild" ? <Spinner /> : null}
                  {busy === "rebuild" ? H.calibration.rebuilding : H.calibration.rebuild(chosenName)}
                </Button>
              ) : null}
              {rebuilt === "refused" ? (
                <p className="t-ui flex items-start gap-2" role="alert">
                  <StatusIcon kind="problem" className="mt-px" />
                  <span className="min-w-0 flex-1">{H.calibration.rebuildRefused}</span>
                </p>
              ) : rebuilt !== null && chosen !== null ? (
                <p className="t-ui flex flex-wrap items-center gap-x-3 gap-y-1" data-testid="calibration-rebuilt">
                  <span>{H.calibration.rebuilt(chosenName, rebuilt.version)}</span>
                  <Link to={`/hotend/p/${chosen}/design`} className="inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline">
                    {H.calibration.openProject(chosenName)}
                  </Link>
                </p>
              ) : null}
            </div>
          ) : null}
          {recordError && "code" in recordError && recordError.code === "bad_request" ? (
            // The engine refused the readings: what to do is to measure again, not to run the
            // health check, and its own sentence is a developer's.
            <ErrorCard title={H.calibration.refused} className="mt-4" />
          ) : recordError ? (
            <ApiErrorCard error={recordError} className="mt-4" />
          ) : null}
        </Group>
      </div>
    </div>
  );
}

function Appearance() {
  const choice = useTheme((state) => state.choice);
  const choose = useTheme((state) => state.choose);
  return (
    <div data-testid="settings-appearance">
      <Title>{H.sections.appearance}</Title>
      <div className={CARD}>
      <Row label={H.appearance.theme}>
        <Segmented<ThemeChoice>
          label={H.appearance.theme}
          value={choice}
          onChange={choose}
          segments={[
            { value: "dark", label: (<><Moon strokeWidth={1.5} />{H.appearance.dark}</>) },
            { value: "light", label: (<><Sun strokeWidth={1.5} />{H.appearance.light}</>) },
            { value: "system", label: (<><Monitor strokeWidth={1.5} />{H.appearance.system}</>) },
          ]}
        />
      </Row>
      </div>
    </div>
  );
}

function Computer() {
  const store = useHostStore();
  const doctor = useHost((state) => state.doctor);
  const busy = useHost((state) => state.doctorBusy);
  const [copied, setCopied] = useState(false);
  // The quick check may be all there is: this page shows the full one.
  useEffect(() => {
    void store.getState().checkComputer();
  }, [store]);
  return (
    <div data-testid="settings-computer">
      <div className="flex items-start justify-between gap-3">
        <Title>{H.sections.computer}</Title>
        {doctor && !busy ? <span className="t-caption num pt-1.5 text-app-text-2">{healthCounts(doctor)}</span> : null}
      </div>
      <div className={CARD}>
        <HealthRows report={doctor} busy={busy} />
      </div>
      <div className="flex flex-wrap gap-2 pt-4">
        <Button variant="outline" size="sm" disabled={busy} onClick={() => void store.getState().checkComputer()}>
          {H.health.again}
        </Button>
        {doctor ? (
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void navigator.clipboard.writeText(JSON.stringify(doctor, null, 2)).then(
                () => {
                  setCopied(true);
                  setTimeout(() => {
                    setCopied(false);
                  }, 2000);
                },
                () => undefined,
              );
            }}
          >
            {copied ? H.health.copied : H.health.copy}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function Keys({ keys }: { keys: string }) {
  const mod = /Mac/.test(navigator.platform) ? "⌘" : "Ctrl";
  return (
    <span className="flex flex-wrap items-center gap-1">
      {keys.split(" / ").map((combo, index) => (
        <span key={combo} className="flex items-center gap-1">
          {index > 0 ? <span className="text-app-text-3">/</span> : null}
          <Kbd>{combo.replaceAll("Mod", mod).replaceAll("+", " ")}</Kbd>
        </span>
      ))}
    </span>
  );
}

function Shortcuts() {
  const table = (rows: readonly [string, string][]) => (
    <table className="w-full">
      <thead>
        <tr className="t-caption text-left text-app-text-2">
          <th className="w-[240px] pb-1 font-normal">{H.shortcuts.keys}</th>
          <th className="pb-1 font-normal">{H.shortcuts.action}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map(([keys, action]) => (
          <tr key={keys} className="border-t border-app-line">
            <td className="py-1.5">
              <Keys keys={keys} />
            </td>
            <td className="t-ui py-1.5">{action}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
  return (
    <div data-testid="settings-shortcuts">
      <Title>{H.shortcuts.title}</Title>
      <div className={CARD}>{table(H.shortcuts.rows)}</div>
      <Sub>{H.shortcuts.inView}</Sub>
      <div className={CARD}>{table(H.shortcuts.viewRows)}</div>
      <p className="t-ui pt-4 text-app-text-2">{H.shortcuts.none}</p>
    </div>
  );
}

function About() {
  const doctor = useHost((state) => state.doctor);
  const store = useHostStore();
  useEffect(() => {
    void store.getState().quickCheck();
  }, [store]);
  return (
    <div data-testid="settings-about">
      <Title>{H.sections.about}</Title>
      <div className={CARD}>
        <p className="t-heading">{H.about.version(doctor?.hotend ?? "")}</p>
        {doctor?.debug_build ? <p className="t-ui text-app-text-2">{H.about.build}</p> : null}
        <p className="t-ui max-w-[68ch] pt-2 text-app-text-2">{H.about.dataNote}</p>
      </div>
      <Sub>{H.about.licences}</Sub>
      <p className="t-ui max-w-[68ch] pb-3 text-app-text-2">{H.about.licencesLine}</p>
      <div className={CARD}>
      <table className="w-full" data-testid="licences">
        <thead>
          <tr className="t-caption text-left text-app-text-2">
            <th className="pb-1 font-normal">{H.about.name}</th>
            <th className="w-[240px] pb-1 font-normal">{H.about.licence}</th>
          </tr>
        </thead>
        <tbody>
          {__HOTEND_LICENCES__.map(([name, version, licence]) => (
            <tr key={name} className="border-t border-app-line align-top">
              <td className="t-ui py-1.5 pr-3">
                {name} <span className="num text-app-text-2">{version}</span>
              </td>
              <td className="t-ui py-1.5 text-app-text-2">{licence}</td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
      <p className="t-ui max-w-[68ch] pt-4 text-app-text-2">{H.about.slicers}</p>
      <p className="t-caption pt-2 text-app-text-2">{H.about.full}</p>
    </div>
  );
}

const CONTENT: Record<Section, () => React.ReactNode> = {
  ai: Ai,
  printer: Printer,
  slicer: Slicer,
  calibration: CalibrationSection,
  appearance: Appearance,
  computer: Computer,
  shortcuts: Shortcuts,
  about: About,
};

export function SettingsScreen() {
  const { section } = useParams();
  const navigate = useNavigate();
  const location = useLocation();
  const from = (location.state as { from?: unknown } | null)?.from;
  const back = typeof from === "string" ? from : "/hotend";
  if (!SECTIONS.includes(section as Section)) return <Navigate to="/settings/ai" replace />;
  const Content = CONTENT[section as Section];
  return (
    // One block in the middle of the window: the list and what it controls stay side by side
    // at any width (220 + 48 + 640).
    <div className="app-scroll h-full min-h-0 overflow-y-auto [scrollbar-gutter:stable_both-edges]" data-testid="settings">
      <div className="mx-auto flex w-full max-w-[956px] items-start gap-12 px-6 pt-6 pb-12" data-testid="settings-block">
        <nav aria-label={H.settings} className="sticky top-6 w-[220px] shrink-0" data-testid="settings-nav">
          <Button
            variant="ghost"
            size="sm"
            className="mb-3 -ml-1 gap-1.5 text-app-text-2"
            onClick={() => {
              void navigate(back);
            }}
            data-testid="settings-back"
          >
            <ArrowLeft strokeWidth={1.5} />
            {H.back}
          </Button>
        <p className="t-heading px-2 pb-2">{H.settings}</p>
        <ul className="space-y-0.5">
          {SECTIONS.map((name) => (
            <li key={name}>
              <Link
                to={`/settings/${name}`}
                state={location.state}
                replace
                aria-current={name === section ? "page" : undefined}
                className={cn(
                  "t-ui flex h-7 items-center rounded-control px-2 text-app-text-2 hover:bg-app-hover hover:text-app-text",
                  name === section && "bg-app-active text-app-text hover:bg-app-active",
                )}
              >
                {H.sections[name]}
              </Link>
            </li>
          ))}
        </ul>
        </nav>
        <div className="w-full max-w-[640px] min-w-0 flex-1 pt-2" data-testid="settings-content">
          <Content />
        </div>
      </div>
    </div>
  );
}
