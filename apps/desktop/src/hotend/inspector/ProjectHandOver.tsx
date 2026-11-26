// The editable Bambu Studio project of the plate in view, in the Print stage: make it, choose
// where it goes, and hand it over. The button opens the shell's own confirmation dialog
// (`print_project_confirm`); nothing is saved or opened from a click in the window. Every
// state of it says that the project is outside Hotend's check.
import { useEffect, useState } from "react";
import { Link } from "react-router";

import { Button } from "@/components/ui/button";
import { RadioGroup, RadioRow } from "@/components/ui/radio-group";
import { Spinner } from "@/components/ui/spinner";
import { useCore } from "@/host/core-context";
import { toApiError, type ApiError, type EditableProject, type ProjectReview } from "@/lib/core";

import { ApiErrorCard, PathText, StatusIcon } from "../bits";
import { useHotend } from "../context";
import { projectButton, projectDestination, projectFor, projectGoesTo, projectOutcome, type ProjectWhere } from "../project";
import { currentSlice, slicedPlate } from "../store";
import { S } from "../strings";

/** An answer of the core, with the request it answers. */
type Answered<T> = { key: string; value: T };

function Outside() {
  return (
    <p className="t-ui flex gap-2 text-app-text-2" data-testid="project-outside">
      <StatusIcon kind="note" className="mt-px" />
      <span className="min-w-0 flex-1">{S.print.project.outside}</span>
    </p>
  );
}

export function ProjectHandOver() {
  const client = useCore();
  const projectId = useHotend((state) => state.project?.project.id ?? null);
  const slice = useHotend((state) => currentSlice(state));
  const plate = slicedPlate(slice) ?? 1;
  const version = slice?.record.version ?? null;
  const sha256 = slice?.summary.sha256 ?? null;
  // The slice in view: a project is made for it, and for nothing else.
  const sliceKey = projectId === null || version === null || sha256 === null ? null : `${projectId}|${version}|${plate}|${sha256}`;

  const [listed, setListed] = useState<Answered<EditableProject | null> | null>(null);
  const [madeHere, setMadeHere] = useState<Answered<EditableProject> | null>(null);
  const [where, setWhere] = useState<ProjectWhere>("bambu_studio");
  const [folder, setFolder] = useState<string | null>(null);
  const [reviewed, setReviewed] = useState<Answered<{ review: ProjectReview | null; error: ApiError | null }> | null>(null);
  const [making, setMaking] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [done, setDone] = useState<EditableProject | null>(null);

  // The project already made for this slice, if any.
  useEffect(() => {
    if (sliceKey === null || projectId === null || version === null || sha256 === null) return;
    let gone = false;
    client.print_projects(projectId).then(
      (projects) => {
        if (!gone) setListed({ key: sliceKey, value: projectFor(projects, { version, plate, sha256 }) });
      },
      () => {
        // Nothing to show: the person can make the project.
        if (!gone) setListed({ key: sliceKey, value: null });
      },
    );
    return () => {
      gone = true;
    };
  }, [client, sliceKey, projectId, version, plate, sha256]);

  const made = (madeHere?.key === sliceKey ? madeHere.value : null) ?? (listed?.key === sliceKey ? listed.value : null);
  const reading = sliceKey !== null && listed?.key !== sliceKey && madeHere?.key !== sliceKey;

  // What the confirmation will show, for the chosen destination: the core checks the
  // project again and says where it goes.
  const destination = projectDestination(where, folder);
  const reviewKey = made === null || destination === null ? null : `${made.id}|${JSON.stringify(destination)}`;
  useEffect(() => {
    if (made === null || reviewKey === null) return;
    const target = projectDestination(where, folder);
    if (target === null) return;
    let gone = false;
    client.print_project_review(made.id, target).then(
      (review) => {
        if (!gone) setReviewed({ key: reviewKey, value: { review, error: null } });
      },
      (thrown: unknown) => {
        if (!gone) setReviewed({ key: reviewKey, value: { review: null, error: toApiError(thrown) } });
      },
    );
    return () => {
      gone = true;
    };
  }, [client, made, reviewKey, where, folder]);
  const current = reviewed?.key === reviewKey ? reviewed.value : null;
  const review = current?.review ?? null;
  const reviewing = reviewKey !== null && current === null;
  const shownError = error ?? current?.error ?? null;

  const make = async () => {
    if (sliceKey === null || projectId === null) return;
    setMaking(true);
    setError(null);
    setNote(null);
    try {
      setMadeHere({ key: sliceKey, value: await client.print_project_make(projectId, plate) });
    } catch (thrown) {
      setError(toApiError(thrown));
    } finally {
      setMaking(false);
    }
  };

  const pickFolder = async () => {
    try {
      const picked = await client.folder_pick(S.print.project.pickTitle);
      if (picked !== null) setFolder(picked);
    } catch (thrown) {
      setError(toApiError(thrown));
    }
  };

  const confirm = async () => {
    if (made === null || destination === null) return;
    setConfirming(true);
    setError(null);
    setNote(null);
    try {
      const answer = await client.print_project_confirm(made.id, destination);
      if (answer.confirmed) setDone(answer.project);
      else setNote(S.print.project.notConfirmed);
    } catch (thrown) {
      setError(toApiError(thrown));
    } finally {
      setConfirming(false);
    }
  };

  if (done !== null) {
    const outcome = projectOutcome(done);
    return (
      <div className="space-y-2" data-testid="project-outcome">
        <p className="t-ui">{S.print.project.saved}</p>
        {outcome.path !== null ? <PathText path={outcome.path} /> : null}
        <p className="t-ui" data-testid="project-opened">
          {outcome.sentence}
        </p>
        <Outside />
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            setDone(null);
          }}
        >
          {S.print.project.again}
        </Button>
      </div>
    );
  }

  const handoffError = shownError !== null && "code" in shownError && shownError.code === "handoff_folder";
  return (
    <div className="space-y-3" data-testid="project-handover">
      <Outside />
      {made === null ? (
        <div className="space-y-2">
          <p className="t-ui text-app-text-2">{S.print.project.what(plate)}</p>
          {making ? (
            <p className="t-ui flex items-center gap-2 text-app-text-2" role="status">
              <Spinner /> {S.print.project.making}
            </p>
          ) : (
            <Button className="w-full" disabled={reading || sliceKey === null} onClick={() => void make()} data-testid="project-make">
              {S.print.project.make}
            </Button>
          )}
        </div>
      ) : (
        <>
          <p className="t-ui" data-testid="project-made">
            {S.print.project.made(made.plate)}
          </p>
          <section aria-labelledby="project-where" className="space-y-1">
            <p id="project-where" className="t-ui-strong">
              {S.print.project.where}
            </p>
            <RadioGroup
              aria-labelledby="project-where"
              value={where}
              disabled={confirming}
              onValueChange={(value) => {
                if (value === "bambu_studio" || value === "folder") {
                  setWhere(value);
                  setNote(null);
                  setError(null);
                }
              }}
              className="-mx-2"
            >
              <RadioRow value="bambu_studio">
                <span>{S.print.project.studio.name}</span>
                <span className="block text-app-text-2">
                  {review?.handoff?.opens === false ? S.print.project.studio.underNoOpener : S.print.project.studio.under}
                </span>
              </RadioRow>
              <RadioRow value="folder">
                <span>{S.print.project.folder.name}</span>
                <span className="block text-app-text-2">{S.print.project.folder.under}</span>
              </RadioRow>
            </RadioGroup>
            {where === "folder" ? (
              <div className="space-y-1 pt-1 pl-6">
                {folder ? <PathText path={folder} /> : null}
                <Button variant="outline" size="sm" onClick={() => void pickFolder()} data-testid="project-choose-folder">
                  {S.print.folder.choose}
                </Button>
              </div>
            ) : null}
          </section>
          {reviewing ? (
            <p className="t-ui flex items-center gap-2 text-app-text-2">
              <Spinner /> {S.verdict.checking}
            </p>
          ) : null}
          {review !== null ? (
            <div className="space-y-1" data-testid="project-review">
              <p className="t-ui text-app-text-2">{S.print.project.goesTo(projectGoesTo(review))}</p>
              <p className="t-ui text-app-text-2">{S.print.project.fileName(review.file_name)}</p>
              {review.warnings.map((warning) => (
                <p key={`${warning.code}:${warning.line}`} className="t-ui flex gap-2 text-app-text-2">
                  <StatusIcon kind="note" className="mt-px" />
                  <span className="min-w-0 flex-1">{S.print.finding(warning.line, warning.message)}</span>
                </p>
              ))}
            </div>
          ) : null}
          {note !== null ? (
            <p className="t-ui text-app-text-2" role="status" data-testid="project-note">
              {note}
            </p>
          ) : null}
          <Button
            className="w-full"
            disabled={confirming || reviewing || review === null || destination === null}
            onClick={() => void confirm()}
            data-testid="project-confirm"
          >
            {projectButton(where, review)}
          </Button>
        </>
      )}
      {shownError !== null ? (
        <ApiErrorCard
          error={shownError}
          action={
            handoffError ? (
              <Link
                to="/settings/printer"
                className="t-ui inline-flex min-h-7 items-center rounded-tag text-app-accent underline-offset-2 hover:underline"
                data-testid="project-handoff-settings"
              >
                {S.print.connect.settings}
              </Link>
            ) : undefined
          }
        />
      ) : null}
    </div>
  );
}
