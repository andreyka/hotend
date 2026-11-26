// The editable Bambu Studio project of a plate, as the Print stage offers it: which made
// project belongs to the slice in view, where it can go, and what the window says before and
// after the shell's confirmation dialog. The core makes, checks and hands over the project;
// nothing here decides anything the core decides.
import type { EditableProject, ProjectDestination, ProjectReview } from "@/lib/core";

import { handoffDetail } from "./handoff";
import { S } from "./strings";

/** Where the person wants the project: the hand-off to Bambu Studio, or a folder. */
export type ProjectWhere = "bambu_studio" | "folder";

/**
 * The newest project made for exactly this slice: the same version, the same plate of the
 * layout, and the sliced file whose check it was made from. `projects` is newest first, as
 * `print_projects` answers.
 */
export function projectFor(
  projects: readonly EditableProject[],
  slice: { version: number; plate: number; sha256: string } | null,
): EditableProject | null {
  if (!slice) return null;
  return (
    projects.find(
      (project) =>
        project.version === slice.version &&
        project.plate === slice.plate &&
        project.slice_sha256.toLowerCase() === slice.sha256.toLowerCase(),
    ) ?? null
  );
}

/** The destination for `print_project_review` and `print_project_confirm`; null while a folder is still to be chosen. */
export function projectDestination(where: ProjectWhere, folder: string | null): ProjectDestination | null {
  if (where === "bambu_studio") return { kind: "bambu_studio" };
  return folder === null ? null : { kind: "folder", folder };
}

/** The button says what happens: a save, and the opening of Bambu Studio only where this Hotend opens it. */
export function projectButton(where: ProjectWhere, review: Pick<ProjectReview, "handoff"> | null): string {
  if (where === "folder") return S.print.project.folder.button;
  return review?.handoff?.opens === false ? S.print.project.studio.buttonNoOpener : S.print.project.studio.button;
}

/** Where a reviewed project goes, as the program that reads it names the folder. */
export function projectGoesTo(review: Pick<ProjectReview, "destination" | "handoff">): string {
  if (review.destination.kind === "folder") return review.destination.folder;
  return review.handoff?.windows_folder ?? review.handoff?.folder ?? "";
}

/** After a hand-over: where the project was saved, and whether Bambu Studio was asked to open it. */
export function projectOutcome(project: EditableProject): { path: string | null; sentence: string } {
  const detail = handoffDetail(project);
  const path = detail.windows_path ?? project.detail.path;
  if (project.delivery === "folder") return { path, sentence: S.print.project.nothingOpened };
  return { path, sentence: detail.opened ? S.print.project.opened : S.print.project.notOpened };
}
