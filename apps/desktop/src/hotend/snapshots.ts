// What the window checks of a snapshot name before it asks the core, by the core's own
// rule (`hotend_core::store::snapshot_name`): trimmed, one plain line of 1 to 80
// characters, no control, invisible or direction-changing characters, and not a name the
// project already uses (compared without regard to ASCII case). The core checks again.
import type { Snapshot, SnapshotExport } from "@/lib/core/pending";

/** Longest snapshot name, in characters (`MAX_SNAPSHOT_NAME`). */
export const MAX_SNAPSHOT_NAME = 80;

/** The invisible and bidirectional format characters (`hotend_printer::is_hidden_format`). */
const HIDDEN_FORMAT: readonly (readonly [number, number])[] = [
  [0x00ad, 0x00ad],
  [0x0600, 0x0605],
  [0x061c, 0x061c],
  [0x06dd, 0x06dd],
  [0x070f, 0x070f],
  [0x0890, 0x0891],
  [0x08e2, 0x08e2],
  [0x115f, 0x1160],
  [0x180b, 0x180f],
  [0x200b, 0x200f],
  [0x202a, 0x202e],
  [0x2060, 0x206f],
  [0x2800, 0x2800],
  [0x3164, 0x3164],
  [0xfe00, 0xfe0f],
  [0xfeff, 0xfeff],
  [0xffa0, 0xffa0],
  [0xfff9, 0xfffb],
  [0x110bd, 0x110bd],
  [0x110cd, 0x110cd],
  [0x13430, 0x1343f],
  [0x1bca0, 0x1bca3],
  [0x1d173, 0x1d17a],
  [0xe0000, 0xe0fff],
];

function hiddenFormat(c: string): boolean {
  const code = c.codePointAt(0) ?? 0;
  return HIDDEN_FORMAT.some(([from, to]) => code >= from && code <= to);
}

/** Rust's `char::is_control` and `char::is_whitespace` other than the plain space. */
const REFUSED = /[\p{Cc}]|(?! )\p{White_Space}/u;

/** Trims as Rust's `str::trim` does: Unicode white space only (not U+FEFF, which JavaScript trims). */
function trimmed(text: string): string {
  return text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
}

const asciiLower = (text: string) => text.replace(/[A-Z]/g, (letter) => letter.toLowerCase());

export type NameProblem = "empty" | "long" | "plain" | "taken";

/** The name as the core would store it, or why it would refuse it. */
export function checkSnapshotName(
  name: string,
  existing: readonly Pick<Snapshot, "name">[],
): { name: string; problem: null } | { name: string; problem: NameProblem } {
  const text = trimmed(name);
  if ([...text].some((c) => REFUSED.test(c) || hiddenFormat(c))) return { name: text, problem: "plain" };
  const length = [...text].length;
  if (length === 0) return { name: text, problem: "empty" };
  if (length > MAX_SNAPSHOT_NAME) return { name: text, problem: "long" };
  const lower = asciiLower(text);
  if (existing.some((snapshot) => asciiLower(snapshot.name) === lower)) return { name: text, problem: "taken" };
  return { name: text, problem: null };
}

/** How many files of an export were copied as stored, and how many were made now. */
export function exportSources(result: SnapshotExport): { copied: number; exported: number } {
  return {
    copied: result.files.filter((file) => file.source === "copied").length,
    exported: result.files.filter((file) => file.source === "exported").length,
  };
}
