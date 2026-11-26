// Inspector of the Print stage (DESIGN_SPEC 4.5 to 4.7): the review of the sliced file,
// where to send it, and the running job. The button under the numbers opens the shell's
// own confirmation dialog; nothing starts from a click in the window.
import { Fragment, useEffect, useRef, useState } from "react";
import { FlaskConical } from "lucide-react";
import { Link, useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { RadioGroup, RadioRow } from "@/components/ui/radio-group";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import type { PrintJob, PrintReview, SliceOutcome } from "@/lib/core";
import { cn } from "@/lib/utils";

import { AmsList, Swatch } from "../Ams";
import { ErrorCard, PathText, SectionLabel, SimTag, StatusIcon } from "../bits";
import { useHotend, useVerdictInput } from "../context";
import { duration, sentence, weightExact } from "../format";
import { handoffDetail, handoffOf } from "../handoff";
import { proofOf } from "../proof";
import { ACTIVE_JOB, currentSlice, slicedPlate, type DeliveryKind } from "../store";
import { S } from "../strings";
import { shownJob, standInSlice } from "../verdict";
import { FollowedStatus } from "./PrinterStatus";
import { ProjectHandOver } from "./ProjectHandOver";

const DELIVERY_KINDS: readonly string[] = ["simulated_printer", "bambu_connect", "export_to_folder", "bambu_studio_project"] satisfies DeliveryKind[];

/** "PLA Basic #FFFFFFFF" as the core names a loaded spool: the product and its colour. */
export function spoolParts(loaded: string): { name: string; colour: string | null } {
  const match = /^(.*?)\s*(#[0-9A-Fa-f]{6}(?:[0-9A-Fa-f]{2})?)?$/.exec(loaded.trim());
  return { name: match?.[1] ?? loaded, colour: match?.[2] ?? null };
}

/**
 * What the core's checker says of a file with nothing to print in it (its `no_moves`
 * warning): true of every placeholder the stand-in slicer writes, and said there in the
 * window's own sentence instead.
 */
const PLACEHOLDER_CODES = new Set(["no_moves"]);

function degrees(value: number | null): string {
  return value === null ? S.print.notStated : `${Math.round(value)} °C`;
}

/** One fact of a print: a quiet label 96 px wide and its value (DESIGN_SPEC 4.5). */
function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="t-ui text-app-text-2">{label}</dt>
      <dd className="t-ui num min-w-0 font-medium text-app-text">{children}</dd>
    </>
  );
}

const FACTS = "grid grid-cols-[96px_minmax(0,1fr)] gap-x-3 gap-y-2";

/** "v2, plate 1 of 4" where the layout has several plates, "v2, plate 1" where it has one. */
function usePlateLine(version: number, plate: number): string {
  const project = useHotend((state) => state.project?.project ?? null);
  // The layout of the version that is printed, which may no longer be the current one.
  const plates = useHotend(
    (state) => (state.jobPreparation?.version === version ? state.jobPreparation : state.preparation)?.plates.length ?? 1,
  );
  return S.print.what(project?.name ?? "", version, plate, plates);
}

/** The slice a job is of: the one in view, or the one of the job's own version. */
function useJobSlice(job: PrintJob): SliceOutcome | null {
  const slice = useHotend((state) => state.slice);
  const jobSlice = useHotend((state) => state.jobSlice);
  return [slice, jobSlice].find((one) => one?.summary.sha256 === job.slice_sha256) ?? null;
}

function What({ job }: { job: PrintJob }) {
  const remembered = useHotend((state) => state.jobPlates[job.id]);
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const mine = useJobSlice(job);
  const grams = mine ? mine.summary.filament.reduce((sum, use) => sum + use.grams, 0) : null;
  // The plate the job's slice is of, as the core recorded it; else the one its review named.
  const line = usePlateLine(job.version, slicedPlate(mine) ?? remembered ?? 1);
  const active = ACTIVE_JOB.has(job.status);
  return (
    <div className="space-y-2">
      <dl className={FACTS} data-testid="print-facts">
        <Fact label={S.print.facts.what}>{line}</Fact>
        {grams !== null ? <Fact label={S.print.facts.material}>{weightExact(grams)}</Fact> : null}
      </dl>
      {current !== null && job.version !== current ? (
        // The design has moved on since: the plate on the stage is the one of the print.
        <p className="t-ui flex gap-2 text-app-text-2" data-testid="job-version-note">
          <StatusIcon kind="history" className="mt-px" />
          <span className="min-w-0 flex-1">
            {active ? S.print.jobVersion.printing(job.version, current) : S.print.jobVersion.printed(job.version, current)}
          </span>
        </p>
      ) : null}
    </div>
  );
}

/** The one link out of the stand-in: Settings, Slicer. */
function UseStudio({ className }: { className?: string }) {
  return (
    <Link
      to="/settings/slicer"
      className={cn("t-ui inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline", className)}
      data-testid="use-studio"
    >
      {S.print.useStudio}
    </Link>
  );
}

/**
 * The numbers a review shows. A placeholder of the stand-in slicer states no time, layers
 * or grams in its file (it prints nothing, and claims nothing it does not do); its rough
 * estimates are in the slice record the core keeps for that very file, and those are
 * shown, marked as the stand-in's estimates. The confirmation dialog does the same
 * (`confirm::with_stand_in_estimates`). Any other file is shown as it states itself.
 */
export function shownSummary(review: PrintReview, standIn: boolean, slices: (SliceOutcome | null)[]): PrintReview["summary"] {
  const { summary } = review;
  // Only the stand-in's own record of this very file: the same hash and the stand-in's mark.
  const slice = slices.find(
    (one) => one?.summary.sha256.toLowerCase() === summary.sha256.toLowerCase() && standInSlice(one),
  );
  if (!standIn || !slice) return summary;
  const grams = (slot: number) => slice.summary.filament.find((use) => use.slot === slot)?.grams ?? null;
  return {
    ...summary,
    estimated_seconds: Math.round(slice.summary.time_s),
    layers: slice.summary.layers,
    filaments: summary.filaments.map((filament) => ({ ...filament, grams: grams(filament.slot) ?? filament.grams })),
  };
}

function ReviewFacts({ review, standIn }: { review: PrintReview; standIn: boolean }) {
  const slice = useHotend((state) => state.slice);
  const jobSlice = useHotend((state) => state.jobSlice);
  const summary = shownSummary(review, standIn, [slice, jobSlice]);
  const time = summary.estimated_seconds === null ? null : duration(summary.estimated_seconds);
  const total = summary.filaments.reduce((sum, filament) => sum + (filament.grams ?? 0), 0);
  const stated = summary.filaments.some((filament) => filament.grams !== null);
  // The plate of the layout, as the confirmation dialog names it: `summary.plate` is the
  // plate inside the sliced file, which is 1 in every file.
  const line = usePlateLine(review.job.version, review.plate);
  // A stand-in file has nothing to print in it, and says so in the slicer's words; the
  // window says what that means instead.
  const warnings = standIn ? review.warnings.filter((warning) => !PLACEHOLDER_CODES.has(warning.code)) : review.warnings;
  return (
    <div data-testid="review-facts">
      <p className="t-ui text-app-text-2">{line}</p>
      <p className="t-numeric-lg num pt-1" data-testid="review-time">
        {[standIn && time ? S.print.about(time) : (time ?? S.print.timeNotStated), stated ? weightExact(total) : null].filter(Boolean).join(" · ")}
      </p>
      {standIn ? (
        <div className="space-y-1 pt-1.5" data-testid="review-stand-in">
          <SimTag>{S.prepare.standInTag}</SimTag>
          <p className="t-caption text-app-sim">{S.print.placeholder}</p>
        </div>
      ) : null}
      <dl className={cn(FACTS, "pt-3")}>
        <Fact label={S.print.facts.layers}>{summary.layers === null ? S.print.notStated : String(summary.layers)}</Fact>
        {summary.filaments.map((filament) => {
          const spool = review.spools.find((choice) => choice.slot === filament.slot);
          const loaded = spool ? spoolParts(spool.loaded) : null;
          return (
            <Fragment key={filament.slot}>
              <Fact label={S.print.facts.material}>
                {[filament.grams === null ? null : weightExact(filament.grams), filament.material ?? spool?.material ?? null]
                  .filter(Boolean)
                  .join(" ") || S.print.notStated}
              </Fact>
              <Fact label={S.print.facts.nozzle}>{S.print.nozzle(filament.nozzle_id)}</Fact>
              {spool && loaded ? (
                <Fact label={S.print.facts.spool}>
                  <span className="flex flex-wrap items-center gap-x-1.5">
                    <Swatch colour={loaded.colour} />
                    <span>{loaded.name}</span>
                    <span className="font-normal text-app-text-2">{S.print.spool(spool.ams_unit + 1, spool.tray + 1)}</span>
                  </span>
                </Fact>
              ) : null}
            </Fragment>
          );
        })}
        {/* A stand-in file states no temperatures: the rows are left out, not "not stated". */}
        {summary.max_nozzle_c === null && standIn ? null : <Fact label={S.print.facts.nozzleUpTo}>{degrees(summary.max_nozzle_c)}</Fact>}
        {summary.max_bed_c === null && standIn ? null : <Fact label={S.print.facts.bedUpTo}>{degrees(summary.max_bed_c)}</Fact>}
        {/* Most files heat no chamber: the row is there when the file does, as in the dialog. */}
        {summary.max_chamber_c === null ? null : <Fact label={S.print.facts.chamberUpTo}>{degrees(summary.max_chamber_c)}</Fact>}
      </dl>
      {warnings.length > 0 ? (
        <div className="space-y-1 pt-4">
          {warnings.map((warning) => (
            <p key={`${warning.code}:${warning.line}`} className="t-ui flex gap-2 text-app-text-2">
              <StatusIcon kind="note" className="mt-px" />
              <span className="min-w-0 flex-1">{S.print.finding(warning.line, warning.message)}</span>
            </p>
          ))}
        </div>
      ) : null}
      {standIn ? null : <p className="t-caption pt-3 text-app-text-2">{S.print.readFromFile}</p>}
    </div>
  );
}

/** The button says what it starts: on the simulated printer, a simulated print; for Bambu
 * Connect, a save, and the opening of Bambu Connect only where this Hotend opens it. */
function buttonLabel(delivery: DeliveryKind, review: PrintReview | null): string {
  switch (delivery) {
    case "simulated_printer":
      return S.print.simulated.button;
    case "bambu_connect":
      return handoffOf(review)?.opens === false ? S.print.connect.buttonNoOpener : S.print.connect.button;
    case "export_to_folder":
      return S.print.folder.button;
    case "bambu_studio_project":
      // Not shown: the project has a button of its own (ProjectHandOver).
      return S.print.project.studio.button;
  }
}

/**
 * The editable Bambu Studio project of the plate: a way of its own, under Bambu Connect.
 * It always says that the project is outside Hotend's check; the stand-in makes none.
 */
function ProjectRow({ standIn }: { standIn: boolean }) {
  return (
    <RadioRow value="bambu_studio_project" disabled={standIn}>
      <span className="block" data-testid="delivery-project">
        <span className={cn("block", standIn && "text-app-text-2")}>{S.print.project.name}</span>
        <span className="block text-app-text-2">{standIn ? S.print.project.standIn : S.print.project.under}</span>
        <span className="block text-app-text-2">{S.print.project.outside}</span>
      </span>
    </RadioRow>
  );
}

function SendTo({ standIn }: { standIn: boolean }) {
  const deliveries = useHotend((state) => state.printers?.[0]?.deliveries ?? null);
  const delivery = useHotend((state) => state.delivery);
  const folder = useHotend((state) => state.folder);
  const review = useHotend((state) => state.review);
  const setDelivery = useHotend((state) => state.setDelivery);
  const pickFolder = useHotend((state) => state.pickFolder);
  const reviewing = useHotend((state) => state.reviewing);
  const entry = (kind: DeliveryKind) => deliveries?.find((one) => one.kind === kind) ?? null;
  const offered = (kind: DeliveryKind) => entry(kind)?.enabled ?? false;
  // Bambu Connect is listed when the core lists it, also while it cannot be chosen (under
  // WSL until a Windows drive folder is chosen), with the reason.
  const connect = entry("bambu_connect");
  const connectBlocked = connect !== null && !connect.enabled;
  const target = handoffOf(review);
  const connectUnder = standIn
    ? S.print.standIn
    : connectBlocked
      ? (connect.disabled_reason ?? S.print.connect.settings)
      : target?.opens === false
        ? S.print.connect.underNoOpener
        : S.print.connect.under;
  return (
    <section aria-labelledby="send-title" data-testid="send-to">
      <SectionLabel id="send-title">{S.print.sendTo}</SectionLabel>
      <RadioGroup
        aria-labelledby="send-title"
        value={delivery}
        disabled={reviewing}
        onValueChange={(value) => {
          if (typeof value === "string" && DELIVERY_KINDS.includes(value)) void setDelivery(value as DeliveryKind);
        }}
        className="-mx-2"
      >
        {/* Bambu Connect first: the way a Bambu printer gets a print without Developer Mode. */}
        {connect !== null ? (
          <RadioRow value="bambu_connect" disabled={standIn || connectBlocked}>
            <span className={cn((standIn || connectBlocked) && "text-app-text-2")}>{S.print.connect.name}</span>
            <span className="block text-app-text-2" data-testid="connect-under">
              {connectUnder}
            </span>
          </RadioRow>
        ) : null}
        {connect !== null ? <ProjectRow standIn={standIn} /> : null}
        {offered("simulated_printer") ? (
          <RadioRow value="simulated_printer">
            <span className="flex items-center gap-2">
              {S.print.simulated.name} <SimTag />
            </span>
            <span className="block text-app-text-2">{S.print.simulated.under}</span>
          </RadioRow>
        ) : null}
        {offered("export_to_folder") ? (
          <RadioRow value="export_to_folder" disabled={standIn}>
            <span className={cn(standIn && "text-app-text-2")}>{S.print.folder.name}</span>
            <span className="block text-app-text-2">{standIn ? S.print.standIn : S.print.folder.under}</span>
          </RadioRow>
        ) : null}
      </RadioGroup>
      {standIn && (connect !== null || offered("export_to_folder")) ? <UseStudio className="ml-6" /> : null}
      {delivery === "bambu_studio_project" && !standIn ? (
        <div className="pt-2 pl-6">
          <ProjectHandOver />
        </div>
      ) : null}
      {connectBlocked && !standIn ? (
        <Link
          to="/settings/printer"
          className="t-ui ml-6 inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline"
          data-testid="handoff-settings"
        >
          {S.print.connect.settings}
        </Link>
      ) : null}
      {delivery === "bambu_connect" && target !== null && !standIn ? (
        <p className="t-ui pl-6 text-app-text-2" data-testid="handoff-folder">
          {S.print.connect.folder(target.windows_folder ?? target.folder)}
        </p>
      ) : null}
      {delivery === "export_to_folder" && !standIn ? (
        <div className="space-y-1 pt-1 pl-6">
          {folder ? <PathText path={folder} /> : null}
          <Button variant="outline" size="sm" onClick={() => void pickFolder(S.print.folder.pickTitle)}>
            {S.print.folder.choose}
          </Button>
        </div>
      ) : null}
    </section>
  );
}

function Blocked() {
  const navigate = useNavigate();
  const error = useHotend((state) => state.reviewError);
  const projectId = useHotend((state) => state.project?.project.id ?? "");
  const reviewAgain = useHotend((state) => state.reviewAgain);
  const sliceNow = useHotend((state) => state.sliceNow);
  const slicing = useHotend((state) => state.slicing);
  if (!error) return null;
  const code = "code" in error ? error.code : null;
  const known = code ? S.print.blocked[code] : undefined;
  const finding = "findings" in error ? (error.findings?.[0] ?? null) : null;
  // A slice made before Hotend-made previews were switched on: slicing again is all it
  // takes, so that is offered right here (the core's `reason`).
  const sliceAgain = code === "not_importable" && "reason" in error && error.reason === "slice_before_previews";
  const backToPrepare =
    code === "gcode_rejected" || code === "no_spool" || code === "slice_changed" || code === "plan_changed";
  return (
    <section aria-labelledby="review-title" data-testid="review-blocked">
      <SectionLabel id="review-title">{S.print.review}</SectionLabel>
      <ErrorCard
        title={known ?? error.message}
        detail={finding ? S.print.finding(finding.line, finding.message) : known ? error.message : null}
        nextStep={known ? null : error.next_step}
        code={[code ?? error.kind, finding?.code].filter(Boolean).join(" · ")}
        action={
          sliceAgain ? (
            <Button variant="outline" size="sm" disabled={slicing} data-testid="blocked-slice-again" onClick={() => void sliceNow()}>
              {S.verdict.sliceAgain}
            </Button>
          ) : backToPrepare ? (
            <Button variant="outline" size="sm" onClick={() => void navigate(`/hotend/p/${projectId}/prepare`)}>
              {S.verdict.backPrepare}
            </Button>
          ) : (
            <Button variant="outline" size="sm" onClick={() => void reviewAgain()}>
              {S.verdict.reviewAgain}
            </Button>
          )
        }
      />
    </section>
  );
}

/** How many numbers from the web get a line each, as in the confirmation dialog. */
const WEB_LINES = 6;

/**
 * Sizes of the version taken from a web page that nobody has confirmed, listed as the
 * confirmation dialog lists them (`confirm.rs`): one line each, at most six, the rest
 * counted. They do not stop the print; the person decides knowing them.
 */
function WebNumbers({ review }: { review: PrintReview }) {
  const unconfirmed = (review.web_numbers ?? []).filter((number) => number.state !== "confirmed");
  if (unconfirmed.length === 0) return null;
  const more = unconfirmed.length - WEB_LINES;
  return (
    <section className="space-y-1" data-testid="review-web-numbers">
      <p className="t-ui-strong flex gap-2">
        <StatusIcon kind="question" className="mt-px" />
        <span className="min-w-0 flex-1">{S.print.web.title}</span>
      </p>
      <ul className="t-ui space-y-0.5 pl-6 text-app-text-2">
        {unconfirmed.slice(0, more > 0 ? WEB_LINES : unconfirmed.length).map((number) => (
          <li key={`${number.note}:${number.key}`}>{S.print.web.line(number.label, number.value.toFixed(1), number.unit, number.host)}</li>
        ))}
        {more > 0 ? <li>{S.print.web.more(more)}</li> : null}
      </ul>
    </section>
  );
}

function ReviewPanel() {
  const review = useHotend((state) => state.review);
  const reviewing = useHotend((state) => state.reviewing);
  const reviewError = useHotend((state) => state.reviewError);
  const reviewNote = useHotend((state) => state.reviewNote);
  const confirming = useHotend((state) => state.confirming);
  const delivery = useHotend((state) => state.delivery);
  const folder = useHotend((state) => state.folder);
  const confirmPrint = useHotend((state) => state.confirmPrint);
  const slice = useHotend((state) => currentSlice(state));
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const result = useHotend((state) => (current === null ? undefined : state.results[current]));
  const standIn = slice?.summary.warnings.some((warning) => warning.code === "fake_slicer") ?? false;
  const proof = proofOf(result);
  const guessed = proof.guessedLabels;
  const plates = useHotend((state) => state.preparation?.plates.length ?? 1);

  if (reviewError) return <Blocked />;
  if (!review) {
    return (
      <section aria-labelledby="review-title">
        <SectionLabel id="review-title">{S.print.review}</SectionLabel>
        {reviewing ? (
          <p className="t-ui flex items-center gap-2 py-1 text-app-text-2">
            <Spinner /> {S.verdict.checking}
          </p>
        ) : null}
      </section>
    );
  }
  const needsFolder = delivery === "export_to_folder" && folder === null;
  const locked = needsFolder ? S.print.folder.needFolder : null;
  return (
    <div className="relative space-y-4" data-testid="review">
      {review.job.note ? (
        <div data-testid="ai-note">
          <p className="t-caption text-app-text-2">{S.print.aiNote}</p>
          <p className="t-ui whitespace-pre-wrap">“{review.job.note}”</p>
        </div>
      ) : null}
      <section aria-labelledby="review-title">
        <SectionLabel id="review-title">{S.print.review}</SectionLabel>
        <ReviewFacts review={review} standIn={standIn} />
      </section>
      <hr className="border-app-line" />
      <SendTo standIn={standIn} />
      {/* What the part does not do is said once more where the print is started. */}
      {proof.unmet.map((line) => (
        <p key={line.key} className="t-ui flex gap-2 text-app-text" data-testid="review-unmet">
          <StatusIcon kind="problem" label={S.proof.status.problem} className="mt-px" />
          <span className="min-w-0 flex-1">{sentence(line.verdict ?? line.summary ?? line.title)}</span>
        </p>
      ))}
      {guessed.length > 0 ? (
        <p className="t-ui flex gap-2 text-app-text-2">
          <StatusIcon kind="question" className="mt-px" />
          <span className="min-w-0 flex-1">{S.print.stillGuesses(guessed.length, guessed.join(", "))}</span>
        </p>
      ) : null}
      <WebNumbers review={review} />
      {plates > 1 ? (
        <p className="t-ui flex gap-2 text-app-text" data-testid="more-plates">
          <StatusIcon kind="note" className="mt-px" />
          <span className="min-w-0 flex-1">{S.print.morePlates(plates - 1)}</span>
        </p>
      ) : null}
      {reviewNote === "not_confirmed" && delivery !== "bambu_studio_project" ? (
        <p className="t-ui text-app-text-2" role="status" data-testid="review-note">
          {S.print.notConfirmed}
        </p>
      ) : null}
      {/* The editable project has its own button, under Send to. */}
      {delivery === "bambu_studio_project" ? null : (
        <Tooltip label={locked} wrap side="top">
          <Button
            className="w-full"
            disabled={reviewing || confirming || locked !== null}
            onClick={() => void confirmPrint()}
            data-testid="start-print"
          >
            {buttonLabel(delivery, review)}
          </Button>
        </Tooltip>
      )}
      {confirming ? <div className="absolute -inset-x-4 -top-3 -bottom-6 bg-app-scrim" data-testid="scrim" aria-hidden /> : null}
    </div>
  );
}

/**
 * One click turns the button into "Stop for good?". The question stays until it is
 * answered: by one of its two buttons, by Esc, or by the focus leaving it. When it closes
 * without a stop, the focus goes back to Stop.
 */
function StopButton({ jobId }: { jobId: string }) {
  const stopPrint = useHotend((state) => state.stopPrint);
  const [asking, setAsking] = useState(false);
  const [stopping, setStopping] = useState(false);
  const stop = useRef<HTMLButtonElement>(null);
  const back = useRef(false);
  useEffect(() => {
    if (asking || !back.current) return;
    back.current = false;
    stop.current?.focus();
  }, [asking]);
  const close = () => {
    back.current = true;
    setAsking(false);
  };
  return (
    <div>
      {asking ? (
        <div
          className="flex flex-wrap items-center gap-2"
          role="group"
          aria-label={S.print.stopSure}
          data-testid="stop-asking"
          onKeyDown={(event) => {
            if (event.key !== "Escape") return;
            event.stopPropagation();
            close();
          }}
          onBlur={(event) => {
            // The focus left the question for somewhere else in the window.
            if (event.relatedTarget instanceof Node && !event.currentTarget.contains(event.relatedTarget)) setAsking(false);
          }}
        >
          <Button
            variant="destructive"
            size="sm"
            autoFocus
            onClick={() => {
              close();
              setStopping(true);
              void stopPrint(jobId).finally(() => {
                setStopping(false);
              });
            }}
            data-testid="stop-print-sure"
          >
            {S.print.stopSure}
          </Button>
          <Button variant="ghost" size="sm" onClick={close}>
            {S.print.keep}
          </Button>
        </div>
      ) : (
        <Button
          ref={stop}
          variant="outline"
          size="sm"
          disabled={stopping}
          onClick={() => {
            setAsking(true);
          }}
          data-testid="stop-print"
        >
          {S.print.stop}
        </Button>
      )}
      {/* Said once when the question appears, for someone who does not see the swap. */}
      <p className="sr-only" aria-live="polite">
        {asking ? S.print.stopAsk : ""}
      </p>
    </div>
  );
}

function Running({ job }: { job: PrintJob }) {
  const printer = useHotend((state) => state.printers?.[0] ?? null);
  const spools = useHotend((state) => state.jobSpools[job.id]);
  const state = printer?.state;
  const percent = job.detail.progress_percent;
  const offline = job.detail.phase === "offline";
  const starting = job.status === "starting";
  return (
    <div className="space-y-4" data-testid="running" data-status={job.status}>
      <section aria-labelledby="printing-title">
        <SectionLabel id="printing-title">{starting ? S.print.starting : S.print.printing}</SectionLabel>
        {starting ? (
          <p className="t-ui py-1 text-app-text-2">{S.print.sent}</p>
        ) : (
          <div className={cn("space-y-1.5", offline && "opacity-60")}>
            <p className="t-numeric-lg num font-mono" data-testid="print-percent">
              {percent === null ? "–" : `${percent} %`}
            </p>
            <Progress value={percent} label={S.print.progress} />
            <div className="min-h-[54px]">
              {job.detail.remaining_minutes !== null ? (
                <p className="t-ui num font-mono" data-testid="print-left">
                  {S.print.left(duration(job.detail.remaining_minutes * 60))}
                </p>
              ) : null}
              {state?.layer != null && state.total_layers != null ? (
                <p className="t-ui num font-mono">{S.print.layer(state.layer, state.total_layers)}</p>
              ) : null}
              {state?.bed_c != null ? <p className="t-ui num font-mono">{S.print.bed(state.bed_c)}</p> : null}
            </div>
          </div>
        )}
        {offline ? <p className="t-ui py-1 text-app-text-2">{S.print.mayRun}</p> : null}
        <div className="pt-2">
          <StopButton jobId={job.id} />
        </div>
      </section>
      {state && state.ams.length > 0 ? (
        <>
          <hr className="border-app-line" />
          <AmsList units={state.ams} inUse={spools} />
        </>
      ) : null}
      <hr className="border-app-line" />
      <What job={job} />
    </div>
  );
}

function Outcome({ job }: { job: PrintJob }) {
  const navigate = useNavigate();
  const reviewAgain = useHotend((state) => state.reviewAgain);
  const stoppedAt = useHotend((state) => state.jobStops[job.id]);
  const standIn = standInSlice(useJobSlice(job));
  const projectId = job.project_id;
  const [copied, setCopied] = useState(false);
  const again = (label: string) => (
    <Button variant="outline" size="sm" onClick={() => void reviewAgain()} data-testid="review-again">
      {label}
    </Button>
  );
  const handoff = handoffDetail(job);
  // A handed-off file is named as Bambu Connect reads it: its Windows path where it has one.
  const path = job.status === "handed_off" ? (handoff.windows_path ?? job.detail.path) : job.detail.path;
  const pathLine =
    path !== null ? (
      <div className="space-y-1">
        <PathText path={path} />
        <Button
          variant="ghost"
          size="sm"
          className="-ml-2 text-app-accent"
          onClick={() => {
            void navigator.clipboard.writeText(path).then(
              () => {
                setCopied(true);
              },
              () => undefined,
            );
          }}
        >
          {copied ? S.project.copied : S.project.copyPath}
        </Button>
      </div>
    ) : null;

  return (
    <div className="space-y-3" data-testid="outcome" data-status={job.status}>
      <SectionLabel>{S.print.last}</SectionLabel>
      {job.status === "finished" ? (
        <>
          <What job={job} />
          {job.detail.simulated ? (
            <>
              <p className="t-ui flex items-start gap-2 rounded-tag bg-app-sim-wash px-2 py-1.5 text-app-sim" data-testid="sim-outcome">
                <FlaskConical className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} aria-hidden />
                <span>{S.print.finishedNote}</span>
              </p>
              {standIn ? (
                // Nothing was made, and nothing could have been: the way to a real print.
                <div className="space-y-2 rounded-card border border-app-line bg-app-raised p-4" data-testid="for-real">
                  <p className="t-ui-strong">{S.print.forReal.title}</p>
                  <ol className="t-ui space-y-1 text-app-text-2">
                    {S.print.forReal.steps.map((step, index) => (
                      <li key={step} className="flex gap-2">
                        <span className="num w-3 shrink-0 text-app-text">{index + 1}</span>
                        <span className="min-w-0 flex-1">{step}</span>
                      </li>
                    ))}
                  </ol>
                  <Button size="sm" onClick={() => void navigate("/settings/slicer")} data-testid="for-real-button">
                    {S.print.forReal.button}
                  </Button>
                </div>
              ) : null}
            </>
          ) : (
            // "Print again" is the verdict bar's button; it is not said twice.
            <Button variant="outline" size="sm" onClick={() => void navigate(`/hotend/p/${projectId}/design`)}>
              {S.print.backDesign}
            </Button>
          )}
        </>
      ) : job.status === "exported" ? (
        <>
          <What job={job} />
          {pathLine}
          {again(S.verdict.reviewAgain)}
        </>
      ) : job.status === "handed_off" ? (
        // The file was placed for Bambu Connect. The window says whether Bambu Connect was
        // asked to import it (the core's record: a real opener started), where the file is
        // as Bambu Connect reads it, and what to do there. The link itself is not shown.
        <>
          <What job={job} />
          {path !== null ? (
            <p className="t-ui" data-testid="connect-file">
              {handoff.opened ? S.print.connectAsked : S.print.connectNotOpened}
            </p>
          ) : null}
          {pathLine}
          <FollowedStatus job={job} />
          {again(S.verdict.reviewAgain)}
        </>
      ) : job.status === "rejected" ? (
        <ErrorCard
          title={S.print.blocked.gcode_rejected ?? S.verdict.cannotSend}
          detail={job.detail.findings[0] ? S.print.finding(job.detail.findings[0].line, job.detail.findings[0].message) : job.detail.message}
          code={["gcode_rejected", job.detail.findings[0]?.code].filter(Boolean).join(" · ")}
          action={
            <Button variant="outline" size="sm" onClick={() => void navigate(`/hotend/p/${projectId}/prepare`)}>
              {S.verdict.backPrepare}
            </Button>
          }
        />
      ) : (
        <>
          <p className="t-ui-strong" data-testid="outcome-line">
            {S.print.outcome[job.status]?.(job.detail.simulated) ?? S.print.outcomePlain}
          </p>
          {job.status === "cancelled" ? (
            // The window's own sentence; the verdict bar offers "Review again", once.
            <p className="t-ui text-app-text-2" data-testid="stopped-at">
              {S.print.stoppedAt(job.detail.simulated, stoppedAt ?? null, job.detail.progress_percent)}
            </p>
          ) : job.detail.message ? (
            <p className="t-ui text-app-text-2">{sentence(job.detail.message)}</p>
          ) : null}
          {job.detail.problems.map((problem) => (
            <div key={problem.raw_code}>
              <p className="t-ui">{problem.message}</p>
              <p className="t-mono text-app-text-2">{problem.raw_code}</p>
            </div>
          ))}
          <What job={job} />
          {job.status === "cancelled" ? null : again(S.verdict.reviewAgain)}
        </>
      )}
    </div>
  );
}

export function PrintPanel() {
  const input = useVerdictInput();
  const job = shownJob(input);
  if (job && ACTIVE_JOB.has(job.status)) return <Running job={job} />;
  if (job) return <Outcome job={job} />;
  if (!currentSlice(input)) {
    // The same sentence as the verdict bar: what is missing, in the order it is missing.
    const current = input.project?.project.current_version ?? null;
    const blocking = current === null ? 0 : proofOf(input.results[current]).blocking;
    return (
      <p className="t-ui pt-1 text-app-text-2" data-testid="print-empty">
        {current === null ? S.verdict.nothingToPrint : blocking > 0 ? S.verdict.fixFirst(blocking) : S.verdict.sliceFirst}
      </p>
    );
  }
  return <ReviewPanel />;
}
