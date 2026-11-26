// The verdict bar and the marks on the stage switch (DESIGN_SPEC 3.3, 3.4): pure
// functions of the project's state. Rows are tried top to bottom; the first that matches
// wins. Nothing here is stored.
import type { PreviewsMode, PrintJob } from "@/lib/core";

import { clock, duration, sized, weight } from "./format";
import { handoffDetail } from "./handoff";
import { proofOf, verdictKind, type Proof } from "./proof";
import { workLine } from "./run";
import { ACTIVE_JOB, currentSlice, newestJob, slicedBeforePreviews, slicedPlate, versionOf, type HotendState } from "./store";
import { S } from "./strings";

export type Stage = "design" | "prepare" | "print";
export type Mark = "none" | "busy" | "problem" | "measure" | "attention" | "done" | "sim";
export type VerdictIcon = "none" | "busy" | "ok" | "note" | "problem" | "question" | "history" | "sim";

export type VerdictAction =
  | { do: "open-settings" }
  | { do: "open-example" }
  | { do: "stop" }
  | { do: "look-current" }
  | { do: "use-version"; version: number }
  | { do: "undo-change" }
  | { do: "dismiss-refused" }
  | { do: "send-again" }
  | { do: "open-slicer" }
  | { do: "ask-fix" }
  | { do: "show-me" }
  | { do: "measure" }
  | { do: "read-note" }
  | { do: "go"; stage: Stage }
  | { do: "slice" }
  | { do: "plate"; plate: number }
  | { do: "review-again" };

export type Verdict = {
  icon: VerdictIcon;
  sentence: string;
  /** `calm` draws the button as an outline: the way on, but not what the stage is for. */
  primary?: { label: string; action: VerdictAction; calm?: boolean };
  quiet?: { label: string; action: VerdictAction };
  /** What a click on the sentence scrolls the inspector to. */
  about?: "problem" | "measure" | "note";
};

export type VerdictInput = Pick<
  HotendState,
  | "project"
  | "results"
  | "look"
  | "run"
  | "rebuild"
  | "refused"
  | "rebuildError"
  | "preparation"
  | "preparing"
  | "prepareError"
  | "plate"
  | "quality"
  | "slice"
  | "sliceStale"
  | "slicing"
  | "sliceError"
  | "jobs"
  | "review"
  | "reviewing"
  | "reviewError"
  | "confirming"
  | "wantReview"
  | "printers"
> & {
  /** A provider is chosen and has its key. */
  canTalk: boolean;
  /** The maker's last message was stopped before Hotend built anything. */
  stoppedEmpty?: boolean;
  /** Milliseconds, for "taking longer than usual" and the waiting clock. */
  now: number;
  /** The settings' `previews`: a slice made without Hotend-made previews while they are on is outdated. */
  previews?: PreviewsMode | null;
};

function current(input: VerdictInput): { number: number | null; proof: Proof } {
  const number = input.project?.project.current_version ?? null;
  return { number, proof: proofOf(number === null ? null : input.results[number]) };
}

function running(input: VerdictInput): Verdict | null {
  const run = input.run;
  if (run?.status !== "running") return null;
  return {
    icon: "busy",
    sentence: S.verdict.working(workLine(run).replace(/…$/, "")),
    primary: { label: run.stopping ? S.verdict.stopping : S.verdict.stop, action: { do: "stop" } },
  };
}

export function designVerdict(input: VerdictInput): Verdict | null {
  const { number, proof } = current(input);
  const busy = running(input);
  if (number === null) {
    if (busy) return busy;
    if (!input.canTalk) {
      return {
        icon: "none",
        sentence: S.verdict.noKey,
        primary: { label: S.verdict.openSettings, action: { do: "open-settings" } },
        quiet: { label: S.verdict.openExample, action: { do: "open-example" } },
      };
    }
    if (input.stoppedEmpty) {
      return { icon: "none", sentence: S.verdict.stoppedEmpty, primary: { label: S.verdict.sendItAgain, action: { do: "send-again" } } };
    }
    return null;
  }
  if (busy) return busy;
  if (input.rebuild) {
    const slow = input.now - input.rebuild.since > 10_000;
    const value = sized(input.rebuild.value, input.rebuild.unit);
    // The news comes first: a sentence that runs out of room loses its end.
    const sentence = input.rebuild.again ? S.verdict.rebuildingAgain : S.verdict.rebuilding(input.rebuild.label, value);
    return { icon: "busy", sentence: slow ? S.verdict.slow(sentence) : sentence };
  }
  const total = input.project?.versions.length ?? 0;
  if (input.look !== null) {
    return {
      icon: "history",
      sentence: S.verdict.looking(input.look, total),
      primary: { label: S.verdict.backTo(number), action: { do: "look-current" } },
      quiet: { label: S.verdict.use(input.look), action: { do: "use-version", version: input.look } },
    };
  }
  if (input.refused) {
    const { label, unit, value, still } = input.refused;
    // The row under the size says why; "OK" puts the verdict back to what the part is.
    return {
      icon: "problem",
      sentence: S.verdict.refused(label, sized(value, unit), sized(still, unit)),
      quiet: { label: S.verdict.ok, action: { do: "dismiss-refused" } },
    };
  }
  if (input.rebuildError) {
    // The size is not what failed: the computer or the core is. The row under "To measure"
    // says what and what to do.
    const { unit, still } = input.rebuildError;
    return {
      icon: "problem",
      sentence: S.verdict.rebuildFailed(sized(still, unit)),
      quiet: { label: S.verdict.ok, action: { do: "dismiss-refused" } },
      about: "measure",
    };
  }
  const version = versionOf(input.project, number);
  const kind = verdictKind(proof, true);
  if (kind === "problems") {
    const first = proof.firstProblem;
    // A requirement that is not met says what is missing: "will not fit: 114 × 54 mm is free…".
    const title = first?.title ?? "";
    if (version?.origin === "set_parameter" && version.parent !== null) {
      const parent = versionOf(input.project, version.parent);
      const changed = Object.entries(version.params).find(([name, value]) => parent?.params[name] !== value);
      const spec = changed ? input.results[number]?.param_specs?.[changed[0]] : undefined;
      // The size is to blame only for a problem it made: one that was there before the
      // change is not undone by undoing it.
      const before = input.results[version.parent];
      if (changed && spec && before !== undefined && proofOf(before).problems < proof.problems) {
        return {
          icon: "problem",
          sentence: S.verdict.sizeProblem(spec.label, sized(changed[1], spec.unit), proof.problems, title),
          primary: { label: S.verdict.undo, action: { do: "undo-change" } },
          quiet: { label: S.verdict.askFix, action: { do: "ask-fix" } },
          about: "problem",
        };
      }
    }
    return {
      icon: "problem",
      sentence: proof.problems === 1 && first?.verdict ? first.verdict : S.verdict.problem(proof.problems, title),
      primary: { label: S.verdict.askFix, action: { do: "ask-fix" } },
      // "Show me" where there is a place to show; a part that is too small has none.
      ...(proof.problems === 1 && first?.verdict ? {} : { quiet: { label: S.verdict.showMe, action: { do: "show-me" } as VerdictAction } }),
      about: "problem",
    };
  }
  if (kind === "measure") {
    return {
      icon: "question",
      sentence: S.verdict.guesses(proof.toMeasure),
      primary: { label: S.verdict.measure(proof.toMeasure), action: { do: "measure" } },
      quiet: { label: S.verdict.prepareAnyway, action: { do: "go", stage: "prepare" } },
      about: "measure",
    };
  }
  if (kind === "notes") {
    return {
      icon: "note",
      sentence: S.verdict.readyNotes(proof.notes),
      primary: { label: S.verdict.prepare, action: { do: "go", stage: "prepare" } },
      quiet: { label: S.verdict.readIt, action: { do: "read-note" } },
      about: "note",
    };
  }
  return {
    icon: "ok",
    sentence: S.verdict.ready,
    primary: { label: S.verdict.prepare, action: { do: "go", stage: "prepare" } },
  };
}

function noSlicer(input: VerdictInput): boolean {
  const error = input.sliceError;
  if (!error) return false;
  return error.kind === "not_configured" || (error.kind === "slicer" && error.code === "slicer_not_installed");
}

/**
 * One sentence of the window's own for a slice that failed, by the core's code, and what
 * the core itself said, which belongs in the Slice section and never in the verdict bar.
 */
export function sliceFailure(input: Pick<VerdictInput, "sliceError">): { sentence: string; said: string; code: string | null } | null {
  const error = input.sliceError;
  if (!error) return null;
  const code = "code" in error ? error.code : null;
  return { sentence: (code && S.prepare.sliceFailed[code]) || S.prepare.sliceFailedPlain, said: error.message, code };
}

/** True for a slice made by the stand-in slicer: its numbers are rough (DESIGN_SPEC 11). */
export function standInSlice(slice: VerdictInput["slice"]): boolean {
  return slice?.summary.warnings.some((warning) => warning.code === "fake_slicer") ?? false;
}

/** How many plates the layout needs, and which one the slice in view is of. */
export function platesOf(input: Pick<VerdictInput, "preparation" | "plate" | "slice">): { count: number; sliced: number } {
  return { count: input.preparation?.plates.length ?? 1, sliced: slicedPlate(input.slice) ?? input.plate };
}

export function sliceTotals(input: Pick<VerdictInput, "slice">): { time: string; grams: string } | null {
  const summary = input.slice?.summary;
  if (!summary) return null;
  const grams = summary.filament.reduce((sum, use) => sum + use.grams, 0);
  return { time: duration(summary.time_s), grams: weight(grams) };
}

export function prepareVerdict(input: VerdictInput): Verdict | null {
  const busy = running(input);
  if (busy) return busy;
  const { number, proof } = current(input);
  if (number === null) {
    return { icon: "none", sentence: S.verdict.nothingToPrepare, primary: { label: S.verdict.goDesign, action: { do: "go", stage: "design" } } };
  }
  if (proof.blocking > 0) {
    return {
      icon: "problem",
      sentence: S.verdict.fixFirst(proof.blocking),
      primary: { label: S.verdict.goDesign, action: { do: "go", stage: "design" } },
    };
  }
  if (input.preparing || (!input.preparation && !input.prepareError)) {
    return { icon: "busy", sentence: S.verdict.layingOut };
  }
  if (input.prepareError) return { icon: "problem", sentence: input.prepareError.message };
  const unplaced = input.preparation?.unplaced[0];
  if (unplaced) return { icon: "problem", sentence: S.verdict.doesNotFit(unplaced.part) };
  if (input.slicing) return { icon: "busy", sentence: S.verdict.slicing(input.plate) };
  if (noSlicer(input)) {
    return { icon: "note", sentence: S.verdict.noSlicer, primary: { label: S.verdict.openSettings, action: { do: "open-settings" } } };
  }
  const failure = sliceFailure(input);
  if (failure) {
    return { icon: "problem", sentence: failure.sentence, primary: { label: S.verdict.sliceAgain, action: { do: "slice" } } };
  }
  const sliceNow = currentSlice(input);
  if (input.slice && !sliceNow && input.slice.record.version === number) {
    return { icon: "note", sentence: S.verdict.changed, primary: { label: S.verdict.sliceAgain, action: { do: "slice" } } };
  }
  if (!sliceNow) {
    const plates = input.preparation?.plates.length ?? 1;
    return {
      icon: "ok",
      sentence: plates <= 1 ? S.verdict.fitsOne : S.verdict.fitsMany(plates, input.plate),
      primary: { label: S.verdict.slice, action: { do: "slice" } },
    };
  }
  const plates = platesOf(input);
  if (slicedBeforePreviews(sliceNow, input.previews)) {
    // Bambu Connect cannot import it; a folder still can take it, so the review stays.
    return {
      icon: "note",
      sentence: S.verdict.previewsOutdated,
      primary: { label: S.verdict.sliceAgain, action: { do: "slice" } },
      quiet: { label: plates.count > 1 ? S.verdict.reviewPlate(plates.sliced) : S.verdict.review, action: { do: "go", stage: "print" } },
    };
  }
  const totals = sliceTotals(input);
  const rough = standInSlice(sliceNow);
  return {
    icon: rough ? "sim" : "ok",
    sentence:
      plates.count > 1
        ? S.verdict.slicedPlate(plates.sliced, plates.count, totals?.time ?? "", totals?.grams ?? "", rough)
        : S.verdict.sliced(totals?.time ?? "", totals?.grams ?? "", rough),
    primary: { label: plates.count > 1 ? S.verdict.reviewPlate(plates.sliced) : S.verdict.review, action: { do: "go", stage: "print" } },
  };
}

/** The job the Print stage shows: one that runs, or the outcome of the newest one. */
export function shownJob(input: Pick<VerdictInput, "jobs" | "wantReview" | "slice" | "sliceStale" | "quality" | "project" | "preparation">): PrintJob | null {
  const newest = newestJob(input.jobs);
  if (!newest || newest.status === "pending") return null;
  if (ACTIVE_JOB.has(newest.status)) return newest;
  if (input.wantReview) return null;
  const slice = currentSlice(input);
  // An outcome stays while it is the outcome of the slice in view.
  return slice === null || slice.summary.sha256 === newest.slice_sha256 ? newest : null;
}

export function printVerdict(input: VerdictInput): Verdict | null {
  const busy = running(input);
  if (busy) return busy;
  const job = shownJob(input);
  if (!job && !currentSlice(input)) {
    // The same rows as Prepare: there is nothing to slice before there is a part without problems.
    const { number, proof } = current(input);
    const toDesign = { label: S.verdict.goDesign, action: { do: "go", stage: "design" } as VerdictAction };
    if (number === null) return { icon: "none", sentence: S.verdict.nothingToPrint, primary: toDesign };
    if (proof.blocking > 0) return { icon: "problem", sentence: S.verdict.fixFirst(proof.blocking), primary: toDesign };
    return { icon: "none", sentence: S.verdict.sliceFirst, primary: { label: S.verdict.goPrepare, action: { do: "go", stage: "prepare" } } };
  }
  if (input.confirming) return { icon: "busy", sentence: S.verdict.waitingDialog };
  if (job) {
    const printer = input.printers?.[0]?.state;
    const again = { label: S.verdict.reviewAgain, action: { do: "review-again" } as VerdictAction };
    switch (job.status) {
      case "starting":
        return { icon: "busy", sentence: S.verdict.waitingStart(clock((input.now - job.updated_at_ms) / 1000)) };
      case "printing":
        if (job.detail.phase === "offline") return { icon: "note", sentence: S.verdict.offline };
        if (job.detail.phase === "preparing" || job.detail.phase === "heating") {
          const what = (job.detail.headline ?? "").replace(/^Getting ready:\s*/i, "").replace(/\.$/, "").toLowerCase();
          return { icon: "busy", sentence: S.verdict.gettingReady(what || "heating") };
        }
        return {
          icon: "busy",
          sentence: S.verdict.printing(printer?.layer ?? null, printer?.total_layers ?? null, job.detail.progress_percent),
        };
      case "finished": {
        // A simulated print made nothing: no green tick, and the way on is back to the part.
        if (job.detail.simulated) {
          return {
            icon: "sim",
            sentence: S.verdict.finishedSim,
            // Nothing was made: the way on is said in the inspector ("To print it for real"),
            // and going back to the part is offered without the weight of a next step.
            primary: { label: S.verdict.backDesign, action: { do: "go", stage: "design" }, calm: true },
            quiet: { label: S.verdict.runAgain, action: { do: "review-again" } },
          };
        }
        const plates = platesOf(input);
        if (plates.count > 1 && plates.sliced < plates.count) {
          return {
            icon: "ok",
            sentence: S.verdict.finishedPlate(plates.sliced, plates.count),
            primary: { label: S.verdict.goPlate(plates.sliced + 1), action: { do: "plate", plate: plates.sliced + 1 } },
            quiet: { label: S.verdict.printAgain, action: { do: "review-again" } },
          };
        }
        return { icon: "ok", sentence: S.verdict.finished, primary: { label: S.verdict.printAgain, action: { do: "review-again" } } };
      }
      case "not_started":
        return { icon: "problem", sentence: S.verdict.notStarted, primary: again };
      case "failed":
        return { icon: "problem", sentence: S.verdict.failed, primary: again };
      case "rejected":
        return { icon: "problem", sentence: S.verdict.cannotSend, primary: { label: S.verdict.backPrepare, action: { do: "go", stage: "prepare" } } };
      case "cancelled":
        return { icon: job.detail.simulated ? "sim" : "none", sentence: job.detail.simulated ? S.verdict.cancelledSim : S.verdict.cancelled, primary: again };
      case "exported":
        return { icon: "ok", sentence: S.verdict.saved };
      case "handed_off":
        return { icon: "ok", sentence: S.verdict.handedOff(handoffDetail(job).opened) };
      default:
        return { icon: "none", sentence: job.detail.message ?? job.status, primary: again };
    }
  }
  if (input.reviewError) {
    return { icon: "problem", sentence: S.verdict.cannotSend, primary: { label: S.verdict.backPrepare, action: { do: "go", stage: "prepare" } } };
  }
  if (input.reviewing || !input.review) return { icon: "busy", sentence: S.verdict.checking };
  // No button on purpose: the button that starts a print is under the numbers. The "?" is
  // for numbers that are guesses; a review is a step, marked only where it is a stand-in.
  return { icon: standInSlice(currentSlice(input)) ? "sim" : "none", sentence: S.verdict.checkNumbers };
}

export function verdictFor(stage: Stage, input: VerdictInput): Verdict | null {
  if (stage === "design") return designVerdict(input);
  return stage === "prepare" ? prepareVerdict(input) : printVerdict(input);
}

/** The marks after the three labels of the stage switch. */
export function stageMarks(input: VerdictInput): Record<Stage, Mark> {
  const { number, proof } = current(input);
  const runBusy = input.run?.status === "running";
  let design: Mark = "none";
  if (runBusy || input.rebuild) design = "busy";
  else if (number !== null) {
    const kind = verdictKind(proof, true);
    design = kind === "problems" ? "problem" : kind === "measure" ? "measure" : kind === "notes" ? "attention" : "done";
  }

  let prepare: Mark = "none";
  const sliceNow = currentSlice(input);
  if (input.preparing || input.slicing) prepare = "busy";
  else if ((input.preparation?.unplaced.length ?? 0) > 0 || (input.sliceError && !noSlicer(input))) prepare = "problem";
  else if (input.slice && !sliceNow && input.slice.record.version === number) prepare = "attention";
  // With several plates one slice is one plate of them: the others are still to do.
  else if (sliceNow) prepare = platesOf(input).count > 1 ? "attention" : "done";

  let print: Mark = "none";
  const newest = newestJob(input.jobs);
  if (newest) {
    if (ACTIVE_JOB.has(newest.status)) print = "busy";
    // The same mark as the verdict of the review (see printVerdict).
    else if (newest.status === "pending") print = sliceNow && standInSlice(sliceNow) ? "sim" : "none";
    else if (["rejected", "not_started", "failed"].includes(newest.status)) print = "problem";
    else if (newest.status === "finished" && newest.detail.simulated) print = newest.version === number ? "sim" : "none";
    else if (["finished", "exported", "handed_off"].includes(newest.status) && newest.version === number) print = "done";
  }
  return { design, prepare, print };
}
