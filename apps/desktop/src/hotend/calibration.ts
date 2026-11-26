// What the calibration table of the printer says, as far as the window needs it: what
// was measured on this printer, when, and what is still Hotend's guess. The table's
// shape is docs/PROTOCOL.md 4.2.
import type { Value } from "@/lib/core";

/** The five quantities of one entry of the table, in the order the window names them. */
export const QUANTITIES = ["hole_offset", "peg_offset", "xy_shrink", "z_shrink", "min_clearance"] as const;
export type Quantity = (typeof QUANTITIES)[number];

export type CalibrationState = {
  /** When the newest measurement was taken, in ms; null when nothing was measured. */
  at: number | null;
  /** The quantities that were measured on this printer. */
  measured: Quantity[];
  /** The quantities that are still general values. */
  guessed: Quantity[];
};

function record(value: Value | undefined): { [key: string]: Value } | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value : null;
}

/**
 * The state of the newest entry of a calibration table; null for no table. A quantity
 * counts as measured when the table says `measured` or `calibrated` and dates it.
 */
export function calibrationState(table: Value | null): CalibrationState | null {
  const entries = record(table ?? undefined)?.entries;
  if (!Array.isArray(entries)) return null;
  let best: CalibrationState | null = null;
  for (const entry of entries) {
    const fields = record(entry);
    if (!fields) continue;
    const state: CalibrationState = { at: null, measured: [], guessed: [] };
    for (const quantity of QUANTITIES) {
      const field = record(fields[quantity]);
      const when = typeof field?.measured_at === "string" ? Date.parse(field.measured_at) : Number.NaN;
      if ((field?.provenance === "measured" || field?.provenance === "calibrated") && Number.isFinite(when)) {
        state.measured.push(quantity);
        state.at = Math.max(state.at ?? 0, when);
      } else {
        state.guessed.push(quantity);
      }
    }
    if (best === null || (state.at ?? 0) > (best.at ?? 0)) best = state;
  }
  return best;
}

/** "hole sizes", "hole sizes and peg sizes", "a, b and c". */
export function listed(words: readonly string[]): string {
  return words.length <= 1 ? (words[0] ?? "") : `${words.slice(0, -1).join(", ")} and ${words.at(-1) ?? ""}`;
}

/** The quantities of a table in words, by the names the window has for them. */
export function quantityWords(quantities: readonly string[], names: Readonly<Record<string, string>>): string {
  return listed(quantities.map((quantity) => names[quantity] ?? quantity.replaceAll("_", " ")));
}

/**
 * What every line of a list says after its last semicolon, taken out when all of them say
 * the same: the gauge ends each feature with where on it to measure, and four rows that
 * end alike are read once, above the list.
 */
export function sharedEnding(lines: readonly string[]): { lines: string[]; shared: string | null } {
  const cut = lines.map((line) => line.lastIndexOf(";"));
  const first = cut[0] === undefined || cut[0] < 0 ? "" : (lines[0] ?? "").slice(cut[0] + 1).trim();
  const same = lines.length > 1 && first !== "" && lines.every((line, index) => (cut[index] ?? -1) >= 0 && line.slice((cut[index] ?? 0) + 1).trim() === first);
  if (!same) return { lines: [...lines], shared: null };
  return { lines: lines.map((line, index) => line.slice(0, cut[index]).trim()), shared: first };
}
