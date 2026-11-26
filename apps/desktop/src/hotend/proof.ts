// The Proof list and the three counts of the verdict (DESIGN_SPEC 5.1, 5.2), computed
// from the engine's result of one version. The window never judges geometry: a line's
// status is the worst status of the engine's results behind it, and the sentences inside
// a line are the engine's own, kept behind "Details". The titles, the grouping and the
// one sentence a line opens with are the window's, written from the typed numbers of the
// result and never from the engine's prose.
import type { CheckResult, CheckStatus, EngineResult, ParamSpec, Value } from "@/lib/core";

import { dimensions, partWords, plain } from "./format";
import { S } from "./strings";

export type ProofGroupId = "must" | "fit" | "prints";
export type ProofIcon = "ok" | "note" | "problem" | "question" | "skipped";

export type ProofEntry = { part: string | null; message: string; status: CheckStatus };

export type ProofLine = {
  /** Stable between versions of one project, so that two results can be compared. */
  key: string;
  group: ProofGroupId;
  title: string;
  status: CheckStatus;
  icon: ProofIcon;
  /** The window's fixed sentence for a requirement that rests on a guess. */
  fixed: string | null;
  /** What the line found, in the window's words and the result's numbers; null when the title says it all. */
  summary: string | null;
  /** The engine's sentences that explain the status, word for word. */
  entries: ProofEntry[];
  /** Guessed sizes the line rests on, by parameter name. */
  guessed: string[];
  /** A guess with no size behind it: the line asks the maker to tell what they measured. */
  asksMeasurement: boolean;
  /** "Fits not proven on this printer": the window adds what that means for now. */
  uncalibrated: boolean;
  /** How many clearances the engine is not sure of, on a line that says fits are not proven. */
  unsure?: number;
  /** The parts the line names, for "Show me". */
  parts: string[];
  /**
   * A requirement that is not met as built, whatever the number behind it is: a
   * guess that does not hold is a problem, and measuring will not make room.
   */
  unmet: boolean;
  /** The line as the verdict says it, a sentence of its own; null where the title does. */
  verdict: string | null;
  /** Where on the part the fault is, for "Show me": a point and the measurement there. */
  spot: ProofSpot | null;
};

export type ProofSpot = { part: string; at: [number, number, number]; label: string };

/** How the lines of a list stand, by what the maker has to do about them. */
export type ProofCounts = { problem: number; pass: number; measure: number; note: number };

export type ProofStanding = keyof ProofCounts | "skipped";

/** One line's place in the counts. */
export function standing(line: ProofLine): ProofStanding {
  if (line.status === "skipped") return "skipped";
  if (line.status === "fail" || line.unmet) return "problem";
  if (line.status === "pass") return "pass";
  return line.icon === "question" ? "measure" : "note";
}

function countsOf(lines: readonly ProofLine[]): ProofCounts {
  const counts: ProofCounts = { problem: 0, pass: 0, measure: 0, note: 0 };
  for (const line of lines) {
    const at = standing(line);
    if (at !== "skipped") counts[at] += 1;
  }
  return counts;
}

/** "1 problem · 10 pass · 1 to measure · 3 notes": never a fraction that reads as failures. */
export function countsText(counts: ProofCounts): string {
  return S.proof.counts(counts);
}

export type ProofGroup = {
  id: ProofGroupId;
  title: string;
  lines: ProofLine[];
  passed: number;
  /** Lines that count: every line but the skipped ones. */
  shown: number;
  counts: ProofCounts;
};

export type Proof = {
  groups: ProofGroup[];
  passed: number;
  shown: number;
  counts: ProofCounts;
  /** What the maker has to get fixed: the engine's failures and the requirements that are not met. */
  problems: number;
  /** The engine's failures alone: these keep a version out of Prepare. */
  blocking: number;
  toMeasure: number;
  notes: number;
  /** Labels of the guessed sizes, in the model's order. */
  guessedLabels: string[];
  /** The first failing line, which the verdict names. */
  firstProblem: ProofLine | null;
  /** The requirements that are not met as built, for the review before a print. */
  unmet: ProofLine[];
  /** The first line that is a note, for "Read it". */
  firstNote: ProofLine | null;
  hasContract: boolean;
};

const RANK: Record<CheckStatus, number> = { fail: 0, warn: 1, pass: 2, skipped: 3 };

function worst(results: readonly CheckResult[]): CheckStatus {
  const counted = results.filter((result) => result.status !== "skipped");
  if (counted.length === 0) return "skipped";
  return counted.reduce<CheckStatus>(
    (status, result) => (RANK[result.status] < RANK[status] ? result.status : status),
    "pass",
  );
}

function record(value: Value | undefined): Record<string, Value> {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value : {};
}

function text(value: Value | undefined): string {
  return typeof value === "string" ? value : "";
}

function strings(value: Value | undefined): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

/** `snap_lid` as words: "Snap lid". */
export function cardWords(card: string): string {
  // "Boss" is a workshop word; a maker calls it the post a screw goes into.
  const words = card.replaceAll("_", " ").replace(/\bboss\b/g, "post").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** `snap_lid_1` is the first snap lid: "Snap lid"; `snap_lid_2` is "Snap lid 2". */
function instanceWords(instance: string, card: string): string {
  const number = /_(\d+)$/.exec(instance)?.[1];
  const words = cardWords(card || instance.replace(/_\d+$/, ""));
  return number === undefined || number === "1" ? words : `${words} ${number}`;
}

/**
 * The engine's sentence made readable for "Details": no back-ticks, no quoted code
 * names, "mm²" for "mm2", "faces" for "face(s)".
 */
export function readable(message: string): string {
  return message
    .replace(/`([^`]*)`/g, (_, name: string) => name.replaceAll("_", " "))
    .replace(/'([A-Za-z0-9_.]+)'/g, (_, name: string) => name.replace(/[_.]+/g, " ").trim())
    .replace(/\bmm2\b/g, "mm²")
    .replace(/\bmm3\b/g, "mm³")
    .replace(/(\d) x (?=\d)/g, "$1 × ")
    .replace(/\b(\w+)\(s\)/g, "$1s")
    .replace(/\bNOT\b/g, "not");
}

function numbers(value: Value | undefined): number[] {
  return Array.isArray(value) ? value.filter((item): item is number => typeof item === "number") : [];
}

/** The one sentence of a requirement, from its typed numbers; null when they say nothing. */
function contractSummary(kind: string, data: Record<string, Value>, part: string): string | null {
  const size = numbers(data.size);
  const met = data.met === true ? true : data.met === false ? false : null;
  if (kind === "cavity_min" && size.length === 3 && met !== null) {
    if (met) return S.proof.summary.cavityFits(dimensions(size), part);
    return S.proof.summary.cavityTooSmall(dimensions(size), part, shortfall(data)?.free ?? null);
  }
  if (kind === "outer_max" && size.length === 3 && met !== null) {
    const box = numbers(data.bounding_box);
    return met
      ? S.proof.summary.outerFits(part, dimensions(size))
      : S.proof.summary.outerTooBig(part, dimensions(size), box.length === 3 ? dimensions(box) : null);
  }
  if (kind === "wall_min" && met === false) return wallSummary(data, part);
  return null;
}

/** "A wall of the box is 0.6 mm thick. This printer needs 0.8 mm.", from the typed numbers. */
function wallSummary(data: Record<string, Value>, part: string): string | null {
  const thinnest = data.thinnest;
  const least = typeof data.value === "number" ? data.value : data.min_wall;
  if (typeof thinnest !== "number" || typeof least !== "number") return null;
  return S.proof.summary.wallTooThin(part, plain(thinnest, 2), plain(least, 2));
}

/** The space that is free and the space that is asked for, on the axes that fall short. */
function shortfall(data: Record<string, Value>): { free: string; asked: string } | null {
  const size = numbers(data.size);
  const most = record(data.fits_at_most);
  const short = ["x", "y", "z"]
    .map((axis, index) => ({ at: most[axis], asked: size[index] ?? 0 }))
    .filter((entry): entry is { at: number; asked: number } => typeof entry.at === "number" && entry.at > 0 && entry.at < entry.asked);
  if (short.length === 0) return null;
  return {
    free: short.map((entry) => plain(entry.at, 0)).join(" × "),
    asked: short.map((entry) => plain(entry.asked, 0)).join(" × "),
  };
}

/** Where the thinnest wall is, when the result says so. */
function spotOf(result: CheckResult | undefined, part: string | null): ProofSpot | null {
  if (!result || part === null) return null;
  const data = record(result.data);
  const at = numbers(data.thinnest_at);
  const thinnest = data.thinnest;
  if (at.length !== 3 || typeof thinnest !== "number") return null;
  return { part, at: [at[0] ?? 0, at[1] ?? 0, at[2] ?? 0], label: `${plain(thinnest, 2)} mm` };
}

/** The engine's sentence, whole, as the last resort of a line that has to say what is wrong. */
function engineSentence(message: string): string {
  const sentence = /^.*?[.!?](?=\s|$)/.exec(readable(message))?.[0] ?? readable(message);
  return sentence.replace(/^Requirement .+? \([a-z]+\) is not met: (.)/i, (_, first: string) => first.toUpperCase());
}

function firstSentence(message: string, limit = 60): string {
  const sentence = /^.*?[.!?](?=\s|$)/.exec(message)?.[0] ?? message;
  const trimmed = sentence.replace(/[.!?]$/, "");
  return trimmed.length <= limit ? trimmed : `${trimmed.slice(0, limit - 1).trimEnd()}…`;
}

function entriesOf(results: readonly CheckResult[], status: CheckStatus): ProofEntry[] {
  // What explains the status: the results that are as bad as the line. A passing line
  // shows everything behind it.
  const explaining = status === "pass" ? results : results.filter((r) => r.status === status);
  return explaining.map((r) => ({ part: r.part, message: r.message, status: r.status }));
}

function partsOf(results: readonly CheckResult[], status: CheckStatus): string[] {
  const named = results
    .filter((r) => status === "pass" || r.status === status)
    .flatMap((r) => (r.part === null ? strings(record(r.data).parts) : [r.part]));
  return [...new Set(named)];
}

function iconOf(status: CheckStatus): ProofIcon {
  if (status === "fail") return "problem";
  if (status === "warn") return "note";
  return status === "pass" ? "ok" : "skipped";
}

function plainLine(
  key: string,
  group: ProofGroupId,
  results: readonly CheckResult[],
  passTitle: string,
  otherTitle: string,
): ProofLine {
  const status = worst(results);
  const failing = status === "fail" ? results.filter((r) => r.status === "fail") : [];
  const first = failing[0];
  const wall = first && first.id === "min_wall" && first.part !== null ? wallSummary(record(first.data), partWords(first.part)) : null;
  return {
    key,
    group,
    title: status === "pass" || status === "skipped" ? passTitle : otherTitle,
    status,
    icon: iconOf(status),
    fixed: null,
    // A failing line always opens with one plain sentence.
    summary: wall ?? (first ? engineSentence(first.message) : null),
    unmet: false,
    verdict: null,
    spot: spotOf(first, first?.part ?? null),
    entries:
      status === "skipped"
        ? results.slice(0, 1).map((r) => ({ part: null, message: r.message, status: r.status }))
        : entriesOf(results, status),
    guessed: [],
    asksMeasurement: false,
    uncalibrated: false,
    parts: status === "skipped" ? [] : partsOf(results, status),
  };
}

const PRINT_LINES: { key: string; ids: string[]; pass: string; other: string }[] = [
  {
    key: "solid",
    ids: ["valid_solid", "manifold", "single_solid_per_part"],
    pass: S.proof.solid.pass,
    other: S.proof.solid.other,
  },
  { key: "fits_build_volume", ids: ["fits_build_volume"], pass: S.proof.fits.pass, other: S.proof.fits.other },
  { key: "min_wall", ids: ["min_wall"], pass: S.proof.wall.pass, other: S.proof.wall.other },
  { key: "overhangs", ids: ["overhangs"], pass: S.proof.overhangs.pass, other: S.proof.overhangs.other },
  { key: "bed_contact", ids: ["bed_contact"], pass: S.proof.bed.pass, other: S.proof.bed.other },
  { key: "small_features", ids: ["small_features"], pass: S.proof.small.pass, other: S.proof.small.other },
];

const KNOWN = new Set([
  ...PRINT_LINES.flatMap((line) => line.ids),
  "parts_do_not_intersect",
  "fit_pairs",
  "contract",
  "parameter_ranges",
]);

function contractLine(result: CheckResult, specs: Record<string, ParamSpec>): ProofLine {
  const data = record(result.data);
  const requirement = text(data.requirement) || result.message;
  const kind = text(data.requirement_kind);
  const parts = strings(data.parts);
  const first = partWords(parts[0] ?? result.part ?? "part");
  const second = partWords(parts[1] ?? "part");
  const cards = Array.isArray(data.cards) ? data.cards.map(record) : [];
  const card = text(cards[0]?.instance).replace(/_\d+$/, "");
  const question = data.question === null || data.question === undefined ? null : record(data.question);
  const guessed = question
    ? strings(question.guessed_params).filter((name) => name in specs)
    : [];
  // A size the engine cannot find on the part is called by the size's own name.
  const sizeLabel = guessed.length === 1 ? (specs[guessed[0] ?? ""]?.label ?? null) : null;
  const title =
    kind === "cavity_min"
      ? S.proof.contract.cavity(first)
      : kind === "outer_max"
        ? S.proof.contract.outer(first)
        : kind === "wall_min"
          ? S.proof.contract.wall(first)
          : kind === "dimension"
            ? (sizeLabel ?? S.proof.contract.dimension(first))
            : kind === "fit"
              ? S.proof.contract.fit(first, second, card ? cardWords(card).toLowerCase() : null)
              : firstSentence(readable(result.message));
  let icon = iconOf(result.status);
  let fixed: string | null = null;
  let asksMeasurement = false;
  // Not met is not met, whoever the number came from: measuring the thing will not
  // make the part larger.
  const unmet = data.met === false && result.status !== "fail" && result.status !== "skipped";
  const short = kind === "cavity_min" && data.met === false ? shortfall(data) : null;
  if (unmet) {
    icon = "problem";
    fixed = kind === "cavity_min" ? S.proof.unmet.cavity : S.proof.unmet.other;
  } else if (question && result.status !== "fail") {
    if (guessed.length === 0 && kind === "fit") {
      icon = "note";
      fixed = S.proof.guess.fit;
    } else {
      icon = "question";
      fixed =
        data.met === true
          ? S.proof.guess.met
          : guessed.length > 0
              ? S.proof.guess.notChecked
              : S.proof.guess.unknown;
      asksMeasurement = guessed.length === 0;
    }
  }
  return {
    key: `contract:${requirement}`,
    group: "must",
    title,
    status: result.status,
    icon,
    fixed,
    summary:
      contractSummary(kind, data, first) ??
      (result.status === "fail" || unmet ? engineSentence(result.message) : null),
    entries: [{ part: null, message: result.message, status: result.status }],
    guessed,
    asksMeasurement,
    uncalibrated: false,
    parts,
    unmet,
    verdict:
      data.met === false && kind === "cavity_min"
        ? short
          ? S.proof.verdict.cavity(first, short.free, short.asked)
          : S.proof.verdict.cavityPlain(first)
        : null,
    spot: result.status === "fail" ? spotOf(result, parts[0] ?? result.part) : null,
  };
}

function fitLines(results: readonly CheckResult[]): ProofLine[] {
  const lines: ProofLine[] = [];
  const instances = new Map<string, CheckResult[]>();
  const others: CheckResult[] = [];
  for (const result of results) {
    const instance = text(record(result.data).instance);
    if (instance) instances.set(instance, [...(instances.get(instance) ?? []), result]);
    else others.push(result);
  }
  for (const [instance, group] of instances) {
    const status = worst(group);
    const name = instanceWords(instance, text(record(group[0]?.data).card));
    const joints = group.filter((r) => record(r.data).kind === "joint");
    const jointFails = joints.some((r) => r.status === "fail");
    lines.push({
      key: `fit:${instance}`,
      group: "fit",
      title:
        status === "pass" || status === "skipped"
          ? S.proof.card.pass(name)
          : jointFails
            ? S.proof.card.joint(name)
            : S.proof.card.off(name),
      status,
      icon: iconOf(status),
      fixed: null,
      summary: status === "fail" ? engineSentence(group.find((r) => r.status === "fail")?.message ?? "") : null,
      unmet: false,
      verdict: null,
      spot: null,
      // A passing card shows how it goes together; its clearances are summed up in the title.
      entries: status === "pass" ? entriesOf(joints.length > 0 ? joints : group.slice(0, 1), "pass") : entriesOf(group, status),
      guessed: [],
      asksMeasurement: false,
      uncalibrated: false,
      parts: partsOf(group, status),
    });
  }
  others.forEach((result, index) => {
    const kind = text(record(result.data).kind);
    const title =
      result.status === "pass" || result.status === "skipped"
        ? firstSentence(readable(result.message))
        : kind === "robustness"
          ? S.proof.fitFinding.robustness
          : kind === "unverified"
            ? S.proof.fitFinding.unverified
            : kind === "declaration"
              ? S.proof.fitFinding.declaration
              : kind === "undeclared"
                ? S.proof.fitFinding.undeclared
                : firstSentence(readable(result.message));
    lines.push({
      ...plainLine(`fit:${kind || "other"}:${index}`, "fit", [result], title, title),
      uncalibrated: kind === "robustness" && result.status !== "pass",
      ...(kind === "robustness" && Array.isArray(record(result.data).unproven)
        ? { unsure: (record(result.data).unproven as unknown[]).length }
        : {}),
    });
  });
  return lines;
}

/** Failures first, then what is not met, then the rest by status. */
function order(line: ProofLine): number {
  if (line.status === "fail") return 0;
  return line.unmet ? 1 : RANK[line.status] + 1;
}

function sorted(lines: ProofLine[]): ProofLine[] {
  return lines
    .map((line, index) => ({ line, index }))
    .sort((a, b) => order(a.line) - order(b.line) || a.index - b.index)
    .map(({ line }) => line);
}

function group(id: ProofGroupId, title: string, lines: ProofLine[]): ProofGroup {
  const counted = lines.filter((line) => line.status !== "skipped");
  return {
    id,
    title,
    lines: sorted(lines),
    passed: counted.filter((line) => line.status === "pass").length,
    shown: counted.length,
    counts: countsOf(lines),
  };
}

/** The Proof list of one engine result. */
export function proofOf(result: EngineResult | null | undefined): Proof {
  const checks = result?.checks ?? [];
  const specs = result?.param_specs ?? {};
  const byId = (ids: readonly string[]) => checks.filter((check) => ids.includes(check.id));

  // "Skipped: the model declares no CONTRACT" is the absence of a contract, not a line of it.
  const contract = byId(["contract"]).filter(
    (check) => check.status !== "skipped" || text(record(check.data).requirement) !== "",
  );
  const must = contract.map((check) => contractLine(check, specs));

  const collide = byId(["parts_do_not_intersect"]);
  const fit = [
    ...(collide.length > 0
      ? [plainLine("parts_do_not_intersect", "fit", collide, S.proof.collide.pass, S.proof.collide.other)]
      : []),
    ...fitLines(byId(["fit_pairs"])),
  ];

  // A wall the model promised and the printer needs is one measurement: where a
  // requirement already fails on it, the print check says nothing new.
  const told = new Set(
    contract
      .filter((check) => check.status === "fail" && record(check.data).requirement_kind === "wall_min")
      .flatMap((check) => {
        const data = record(check.data);
        return [...strings(data.parts), ...(check.part === null ? [] : [check.part])].map((part) => `${part}:${String(data.thinnest)}`);
      }),
  );
  const repeats = (check: CheckResult) =>
    check.id === "min_wall" && check.status === "fail" && told.has(`${check.part ?? ""}:${String(record(check.data).thinnest)}`);
  const prints = PRINT_LINES.flatMap((line) => {
    const all = byId(line.ids);
    const results = all.filter((check) => !repeats(check));
    if (results.length === 0) return [];
    // Folded away entirely when nothing but the repeated failure is wrong.
    if (results.length < all.length && worst(results) !== "fail") return [];
    return [plainLine(line.key, "prints", results, line.pass, line.other)];
  });
  checks
    .filter((check) => !KNOWN.has(check.id))
    .forEach((check, index) => {
      const title = firstSentence(readable(check.message));
      prints.push(plainLine(`other:${check.id}:${index}`, "prints", [check], title, title));
    });

  const groups = [
    group("must", S.proof.groups.must, must),
    group("fit", S.proof.groups.fit, fit),
    group("prints", S.proof.groups.prints, prints),
  ].filter((entry) => entry.id === "must" || entry.shown > 0);
  const lines = groups.flatMap((entry) => entry.lines);

  const guessedNames = Object.entries(specs)
    .filter(([, spec]) => spec.provenance === "guessed")
    .map(([name]) => name);
  const failing = lines.filter((line) => line.status === "fail");
  const unmet = lines.filter((line) => line.unmet);
  const problems = [...failing, ...unmet];
  const asking = lines.filter((line) => line.status === "warn" && line.asksMeasurement && !line.unmet);
  const notes = lines.filter((line) => line.status === "warn" && line.icon === "note");

  return {
    groups,
    passed: groups.reduce((sum, entry) => sum + entry.passed, 0),
    shown: groups.reduce((sum, entry) => sum + entry.shown, 0),
    counts: countsOf(lines),
    problems: problems.length,
    blocking: failing.length,
    toMeasure: guessedNames.length + asking.length,
    notes: notes.length,
    guessedLabels: guessedNames.map((name) => specs[name]?.label ?? name),
    firstProblem: problems[0] ?? null,
    unmet,
    firstNote: notes[0] ?? null,
    hasContract: contract.length > 0,
  };
}

export type VerdictKind = "none" | "problems" | "measure" | "notes" | "ready";

/** Worst first: problems, then numbers to measure, then notes, then ready (5.1). */
export function verdictKind(proof: Proof, hasVersion: boolean): VerdictKind {
  if (!hasVersion) return "none";
  if (proof.problems > 0) return "problems";
  if (proof.toMeasure > 0) return "measure";
  return proof.notes > 0 ? "notes" : "ready";
}

/** The status line of a project card and of a first version's marker (4.2). */
export function statusLine(proof: Proof): { kind: VerdictKind; text: string } {
  const kind = verdictKind(proof, true);
  switch (kind) {
    case "problems":
      return { kind, text: S.card.problems(proof.problems) };
    case "measure":
      return { kind, text: S.card.toMeasure(proof.toMeasure) };
    case "notes":
      return { kind, text: S.card.notes(proof.notes) };
    default:
      return { kind, text: S.card.ready };
  }
}
