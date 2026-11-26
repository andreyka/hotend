// How numbers are written (DESIGN_SPEC 5.3, "Numbers"). Every number the window shows
// goes through one of these, so that the same value reads the same everywhere.

// U+202F, the narrow space that does not break: "7 928" never splits over two lines.
const THIN_SPACE = "\u202f";

/** Thousands separated by a narrow no-break space, a point as the decimal mark. */
function grouped(text: string): string {
  const [whole = "", fraction] = text.split(".");
  const sign = whole.startsWith("-") ? "-" : "";
  const digits = sign ? whole.slice(1) : whole;
  const spaced = digits.replace(/\B(?=(\d{3})+(?!\d))/g, THIN_SPACE);
  return fraction === undefined ? `${sign}${spaced}` : `${sign}${spaced}.${fraction}`;
}

/** At most `decimals` decimals and no trailing zeros: 120, 1.6, 42.4. */
export function plain(value: number, decimals = 2): string {
  const rounded = Number(value.toFixed(decimals));
  // -0 would print as "-0".
  return grouped(String(Object.is(rounded, -0) ? 0 : rounded));
}

/** A value with its unit, by the unit the model gave: "120 mm", "45°", "0.35", "3". */
export function sized(value: number, unit: string): string {
  switch (unit) {
    case "deg":
    case "°":
      return `${plain(value, 0)}°`;
    case "ratio":
      return value.toFixed(2);
    case "count":
    case "":
      return plain(value, unit === "count" ? 0 : 2);
    default:
      return `${plain(value)} ${unit}`;
  }
}

/** The unit as it is written after a number field: "mm", "°", or nothing. */
export function unitLabel(unit: string): string {
  if (unit === "deg" || unit === "°") return "°";
  if (unit === "ratio" || unit === "count") return "";
  return unit;
}

/** A clearance: always two decimals, "0.20 mm". */
export function clearance(value: number): string {
  return `${value.toFixed(2)} mm`;
}

/** Sizes joined with "×": "124 × 64 × 42.4 mm". */
export function dimensions(sizes: readonly number[], decimals = 1): string {
  return `${sizes.map((size) => plain(size, decimals)).join(" × ")} mm`;
}

/** An area in whole square millimetres: "7 918 mm²". */
export function area(value: number): string {
  return `${grouped(String(Math.round(value)))} mm²`;
}

/** "1 h 42 min", "59 min" or "55 s"; rounded to the nearest minute from one minute up. */
export function duration(seconds: number): string {
  if (seconds < 60) return `${Math.max(0, Math.round(seconds))} s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours} h` : `${hours} h ${rest} min`;
}

/** Whole grams from 10 g, one decimal under it: "28 g", "8.4 g". */
export function weight(grams: number): string {
  return grams >= 10 ? `${grouped(String(Math.round(grams)))} g` : `${plain(grams, 1)} g`;
}

/** Grams with one decimal, as the review and the dialog keep them: "28.3 g". */
export function weightExact(grams: number): string {
  return `${grams.toFixed(1)} g`;
}

/** "38k tokens" style counts: 950, 1.2k, 38k. */
export function compact(count: number): string {
  if (count < 1000) return String(count);
  const thousands = count / 1000;
  return thousands < 10 ? `${plain(thousands, 1)}k` : `${Math.round(thousands)}k`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "Just now", "12 min ago", "2 hours ago", "Yesterday", "3 Nov", "3 Nov 2025". */
export function ago(atMs: number, nowMs: number): string {
  const seconds = Math.max(0, (nowMs - atMs) / 1000);
  if (seconds < 60) return "Just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const at = new Date(atMs);
  const now = new Date(nowMs);
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (atMs >= startOfToday) {
    const hours = Math.floor(minutes / 60);
    return hours === 1 ? "1 hour ago" : `${hours} hours ago`;
  }
  if (atMs >= startOfToday - 24 * 3600 * 1000) return "Yesterday";
  const day = `${at.getDate()} ${MONTHS[at.getMonth()] ?? ""}`;
  return at.getFullYear() === now.getFullYear() ? day : `${day} ${at.getFullYear()}`;
}

/** The same, inside a sentence: "2 min ago" keeps, "Just now" becomes "just now". */
export function agoInline(atMs: number, nowMs: number): string {
  const text = ago(atMs, nowMs);
  return text === "Just now" || text === "Yesterday" ? text.toLowerCase() : text;
}

/** "0:42" for a count of seconds. */
export function clock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, "0")}`;
}

/** A part's name as words: the model's `cable_box` is "cable box" on every screen. */
export function partWords(part: string): string {
  return part.replaceAll("_", " ").trim();
}

/** "1 note", "2 notes". */
export function count(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** A sentence of the core's, as a sentence: a capital first, "mm²" for "mm2", a full stop. */
export function sentence(text: string): string {
  const tidy = text.trim().replace(/\bmm2\b/g, "mm²").replace(/\bmm3\b/g, "mm³");
  if (tidy === "") return tidy;
  const first = tidy.charAt(0).toUpperCase() + tidy.slice(1);
  return /[.!?…:]$/.test(first) ? first : `${first}.`;
}
