// The hand-off as the core reports it: with the print review, the job's detail, an editable
// project's detail and the settings (`HandOffTarget`, `PrintJobDetail`,
// `Settings::handoff_folder` in hotend-core, mirrored in the generated binding).
import type { HandOffTarget, PrintJobDetail, PrintReview, Settings } from "@/lib/core";

export type { HandOffTarget };

/** What the core recorded about a handed-off job or project, besides its link and path. */
export interface HandOffDetail {
  windows_path: string | null;
  /** True only when a real opener asked the program (Bambu Connect, Bambu Studio) to open it. */
  opened: boolean;
  opener: string | null;
}

export function handoffOf(review: Pick<PrintReview, "handoff"> | null): HandOffTarget | null {
  return review?.handoff ?? null;
}

export function handoffDetail(of: { detail: PrintJobDetail }): HandOffDetail {
  const { detail } = of;
  return { windows_path: detail.windows_path ?? null, opened: detail.opened === true, opener: detail.opener ?? null };
}

/** The hand-off folder the settings name; `null` for the default. */
export function handoffFolderOf(settings: Settings | null): string | null {
  return settings?.handoff_folder ?? null;
}

/** Settings with another hand-off folder, for `settings_set`. */
export function withHandoffFolder(folder: string): Partial<Settings> {
  return { handoff_folder: folder };
}

/**
 * How Windows names a folder that WSL shows under `/mnt/<drive letter>/`: `/mnt/d/Models`
 * is `D:\Models`. For showing only; the core translates the path it hands over itself and
 * refuses what Windows cannot read. `null` for any other path.
 */
export function windowsForm(path: string): string | null {
  const match = /^\/mnt\/([A-Za-z])\/(.+)$/.exec(path);
  if (!match) return null;
  const names = (match[2] ?? "").split("/").filter((name) => name !== "");
  if (names.length === 0 || names.some((name) => name === "." || name === "..")) return null;
  return `${(match[1] ?? "").toUpperCase()}:\\${names.join("\\")}`;
}
