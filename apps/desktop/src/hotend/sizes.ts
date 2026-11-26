// Sizes (DESIGN_SPEC 5.3, 6.1): the model's parameters grouped by where each number comes
// from, and the step a control moves by.
import type { EngineResult, ParamSpec, Value } from "@/lib/core";

export type SizeGroupId = "measure" | "choices" | "fixed";

export type Size = {
  name: string;
  label: string;
  unit: string;
  value: number;
  min: number | null;
  max: number | null;
  provenance: string;
  group: SizeGroupId;
  /** What a guessed size stands for, for the sentence that says what to measure. */
  ask: SizeAsk | null;
};

/**
 * What the requirement behind a guessed size is about, and on which part. Where the
 * requirement is a block of three sizes and this size is one of them, `axis` says which
 * (0 left to right, 1 front to back, 2 bottom to top) and `side` whether that is the
 * long or the short side of the block as it lies.
 */
export type SizeAsk = {
  kind: "inside" | "outside" | "opening" | "other";
  part: string | null;
  axis: 0 | 1 | 2 | null;
  side: "long" | "short" | null;
};

/** A dimension to draw on the part: from one point to another, in model millimetres. */
export type SizeSpan = { from: [number, number, number]; to: [number, number, number] };

function record(value: Value | undefined): Record<string, Value> {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value : {};
}

function names(value: Value | undefined): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

/** The guessed sizes in the order Hotend asks about them, each with what it is for. */
function asked(result: EngineResult | null | undefined): Map<string, SizeAsk> {
  const found = new Map<string, SizeAsk>();
  const kinds = new Map<string, Omit<SizeAsk, "axis" | "side"> & { size: number[] }>();
  const params = result?.params ?? {};
  for (const check of result?.checks ?? []) {
    if (check.id !== "contract") continue;
    const data = record(check.data);
    const kind = typeof data.requirement_kind === "string" ? data.requirement_kind : "";
    const requirement = typeof data.requirement === "string" ? data.requirement : "";
    kinds.set(requirement, {
      kind: kind === "cavity_min" ? "inside" : kind === "outer_max" ? "outside" : kind === "dimension" ? "opening" : "other",
      part: names(data.parts)[0] ?? check.part ?? null,
      size: Array.isArray(data.size) ? data.size.filter((item): item is number => typeof item === "number") : [],
    });
  }
  for (const question of result?.questions ?? []) {
    for (const name of question.guessed_params) {
      if (found.has(name)) continue;
      const about = kinds.get(question.requirement);
      if (!about) {
        found.set(name, { kind: "other", part: null, axis: null, side: null });
        continue;
      }
      // The size is one side of the requirement's block when exactly one side has its value.
      const value = params[name] ?? result?.param_specs?.[name]?.default;
      const same = about.size.length === 3 && value !== undefined ? about.size.flatMap((side, index) => (Math.abs(side - value) < 1e-6 ? [index] : [])) : [];
      const axis = same.length === 1 ? (same[0] as 0 | 1 | 2) : null;
      const [x = 0, y = 0] = about.size;
      const side = axis === null || axis === 2 || x === y ? null : (axis === 0) === x > y ? "long" : "short";
      found.set(name, { kind: about.kind, part: about.part, axis, side });
    }
  }
  return found;
}

function groupOf(provenance: string): SizeGroupId {
  if (provenance === "guessed") return "measure";
  return provenance === "style" ? "choices" : "fixed";
}

/**
 * The sizes of a result. What is to measure comes first, in the order of Hotend's
 * questions, so that the first row is what Hotend asked about first; the rest follow in
 * the order the core lists them.
 */
export function sizesOf(result: EngineResult | null | undefined): Size[] {
  const specs: Record<string, ParamSpec> = result?.param_specs ?? {};
  const params = result?.params ?? {};
  const asks = asked(result);
  const order = [...asks.keys()];
  const rank = (name: string) => (order.includes(name) ? order.indexOf(name) : order.length);
  const entries = Object.entries(specs)
    .map((entry, index) => ({ entry, index }))
    .sort((a, b) => rank(a.entry[0]) - rank(b.entry[0]) || a.index - b.index)
    .map(({ entry }) => entry);
  return entries.map(([name, spec]) => ({
    name,
    label: spec.label || name,
    unit: spec.unit,
    value: params[name] ?? spec.default,
    min: spec.min ?? null,
    max: spec.max ?? null,
    provenance: spec.provenance,
    group: groupOf(spec.provenance),
    ask: spec.provenance === "guessed" ? (asks.get(name) ?? { kind: "other", part: null, axis: null, side: null }) : null,
  }));
}

/**
 * Where a guessed size is on the part, for the line drawn while its row is in use: along
 * its axis, as long as the size, through the middle of the part and a little above its
 * top. Null for a size that is no side of a block (an opening, a fit).
 */
export function spanOf(size: Size, result: EngineResult | null | undefined): SizeSpan | null {
  const axis = size.ask?.axis ?? null;
  const part = result?.parts?.find((candidate) => candidate.name === size.ask?.part);
  if (axis === null || !part) return null;
  const middle = [0, 1, 2].map((index) => ((part.bbox.min[index] ?? 0) + (part.bbox.max[index] ?? 0)) / 2);
  const top = part.bbox.max[2] ?? 0;
  const at = (offset: number): [number, number, number] => {
    const point: [number, number, number] = [middle[0] ?? 0, middle[1] ?? 0, axis === 2 ? (middle[2] ?? 0) : top];
    point[axis] = (middle[axis] ?? 0) + offset;
    // An upright size is drawn beside the part, at its front left corner.
    if (axis === 2) {
      point[0] = part.bbox.min[0] ?? 0;
      point[1] = part.bbox.min[1] ?? 0;
    }
    return point;
  };
  return { from: at(-size.value / 2), to: at(size.value / 2) };
}

/** 0.5 mm (0.1 mm when the range is under 20 mm), 1°, 0.01 for a ratio, 1 for a count. */
export function stepOf(size: Pick<Size, "unit" | "min" | "max">): number {
  if (size.unit === "deg" || size.unit === "°" || size.unit === "count") return 1;
  if (size.unit === "ratio") return 0.01;
  if (size.min !== null && size.max !== null && size.max - size.min < 20) return 0.1;
  return 0.5;
}

/** A value brought onto the control's steps and into its range. */
export function snapped(value: number, size: Pick<Size, "unit" | "min" | "max">): number {
  const step = stepOf(size);
  const origin = size.min ?? 0;
  let next = origin + Math.round((value - origin) / step) * step;
  if (size.min !== null) next = Math.max(size.min, next);
  if (size.max !== null) next = Math.min(size.max, next);
  return Number(next.toFixed(4));
}

/** Where a value sits on its range, 0 to 100. */
export function percentOf(value: number, min: number, max: number): number {
  if (max <= min) return 0;
  return Math.min(100, Math.max(0, ((value - min) / (max - min)) * 100));
}

/** True when a typed value lies outside the range the model gives the size. */
export function outOfRange(value: number, size: Pick<Size, "min" | "max">): boolean {
  return (size.min !== null && value < size.min) || (size.max !== null && value > size.max);
}
