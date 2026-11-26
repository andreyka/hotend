// What Hotend puts into the host's title bar (DESIGN_SPEC 3.2, 3.3, 3.5): the project's
// name with its menu, the stage switch with its marks, and the printer chip.
import { ChevronDown, CircleCheck, CircleQuestionMark, CircleX, Copy, FileDown, FlaskConical, Printer, TriangleAlert, X } from "lucide-react";
import { useState, type ReactElement } from "react";
import { Link, useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Popover, PopoverClose, PopoverContent, PopoverTitle, PopoverTrigger } from "@/components/ui/popover";
import { Progress } from "@/components/ui/progress";
import { Spinner } from "@/components/ui/spinner";
import { useCore } from "@/host/core-context";
import { toApiError, type ApiError, type ExportedFile, type ExportFormat, type Project } from "@/lib/core";
import { cn } from "@/lib/utils";

import { AmsList } from "./Ams";
import { ApiErrorCard, IconButton, PathText, SimTag } from "./bits";
import { useHotend, useVerdictInput } from "./context";
import { proofOf } from "./proof";
import { S } from "./strings";
import { stageMarks, type Mark, type Stage } from "./verdict";

const STAGES: Stage[] = ["design", "prepare", "print"];

function MarkIcon({ mark }: { mark: Mark }) {
  const common = { className: "size-3.5 shrink-0", strokeWidth: 1.75, "aria-hidden": true } as const;
  if (mark === "busy") return <Spinner className="size-3.5 text-app-text-2" />;
  if (mark === "problem") return <CircleX {...common} className={cn(common.className, "text-app-fail")} />;
  if (mark === "measure") return <CircleQuestionMark {...common} className={cn(common.className, "text-app-warn")} />;
  if (mark === "attention") return <TriangleAlert {...common} className={cn(common.className, "text-app-warn")} />;
  if (mark === "done") return <CircleCheck {...common} className={cn(common.className, "text-app-ok")} />;
  if (mark === "sim") return <FlaskConical {...common} className={cn(common.className, "text-app-sim")} />;
  return null;
}

/** The three stages in the centre of the title bar. A segment is never disabled. */
export function StageSwitch({ projectId, stage }: { projectId: string; stage: Stage }) {
  const input = useVerdictInput();
  const marks = stageMarks(input);
  const current = input.project?.project.current_version ?? null;
  const problems = current === null ? 0 : proofOf(input.results[current]).problems;
  const words = (name: Stage): string => {
    const mark = marks[name];
    if (mark === "none") return S.stages[name];
    const said =
      mark === "problem" ? (name === "design" && problems > 0 ? S.marks.problem(problems) : S.marks.problemPlain) : S.marks[mark];
    return `${S.stages[name]}, ${said}`;
  };
  return (
    <nav aria-label={S.stages.switch} className="flex items-center gap-0.5" data-testid="stage-switch">
      {STAGES.map((name) => (
        <Link
          key={name}
          to={`/hotend/p/${projectId}/${name}`}
          aria-label={words(name)}
          aria-current={stage === name ? "page" : undefined}
          data-testid={`stage-${name}`}
          data-mark={marks[name]}
          className={cn(
            "t-ui flex h-7 items-center gap-1.5 rounded-control border border-transparent px-3 text-app-text-2 hover:bg-app-hover hover:text-app-text",
            stage === name && "app-segment-on",
          )}
        >
          {S.stages[name]}
          <MarkIcon mark={marks[name]} />
        </Link>
      ))}
    </nav>
  );
}

const FORMATS: ExportFormat[] = ["step", "3mf", "stl"];

/** Export files…: the formats, then a folder. The trigger is the caller's. */
export function ExportPopover({
  project,
  trigger,
  align = "start",
}: {
  project: Pick<Project, "id" | "name">;
  trigger: ReactElement;
  align?: "start" | "end";
}) {
  const client = useCore();
  const [open, setOpen] = useState(false);
  const [formats, setFormats] = useState<ExportFormat[]>(["step", "3mf"]);
  const [busy, setBusy] = useState(false);
  const [files, setFiles] = useState<{ folder: string; files: ExportedFile[] } | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [copied, setCopied] = useState(false);

  const run = async () => {
    setError(null);
    setFiles(null);
    setCopied(false);
    try {
      const folder = await client.folder_pick(S.project.exportFolderTitle);
      if (folder === null) return;
      setBusy(true);
      setFiles({ folder, files: await client.export_run(project.id, null, formats, null, folder) });
    } catch (thrown) {
      setError(toApiError(thrown));
    } finally {
      setBusy(false);
    }
  };

  return (
    // The keyboard stays in the open popover: Tab walks its controls, Esc closes it and the
    // focus goes back to what opened it.
    <Popover open={open} onOpenChange={setOpen} modal="trap-focus">
      <PopoverTrigger render={trigger} />
      <PopoverContent align={align} className="w-[320px] space-y-3" data-testid="export">
        <div className="flex items-center justify-between gap-2">
          <PopoverTitle className="t-ui-strong">{S.project.exportTitle}</PopoverTitle>
          <PopoverClose
            render={
              <IconButton label={S.project.exportClose} className="-my-1 -mr-1" data-testid="export-close">
                <X strokeWidth={1.5} />
              </IconButton>
            }
          />
        </div>
        <div className="space-y-1">
          {FORMATS.map((format) => (
            <label key={format} className="flex min-h-7 items-center gap-2">
              <Checkbox
                checked={formats.includes(format)}
                onCheckedChange={(checked) => {
                  setFormats(checked ? [...formats, format] : formats.filter((other) => other !== format));
                }}
              />
              <span className="t-ui">{S.project.formats[format]}</span>
              <span className="t-caption text-app-text-2">{S.project.formatNotes[format]}</span>
            </label>
          ))}
        </div>
        <Button variant="outline" size="sm" disabled={busy || formats.length === 0} onClick={() => void run()} data-testid="export-go">
          {busy ? <Spinner /> : <FileDown strokeWidth={1.5} />}
          {busy ? S.project.exporting : S.project.exportGo}
        </Button>
        {error ? <ApiErrorCard error={error} /> : null}
        {files ? (
          <div className="space-y-1.5" role="status" data-testid="exported">
            <p className="t-ui">{S.project.exported(files.files.length)}</p>
            {/* Which files: by their names, not only how many. */}
            <ul className="t-mono text-app-text" data-testid="exported-files">
              {files.files.map((file) => (
                <li key={file.path} className="[overflow-wrap:anywhere]">
                  {file.path.split(/[\\/]/).pop()}
                </li>
              ))}
            </ul>
            <PathText path={files.folder} />
            <Button
              variant="ghost"
              size="sm"
              className="-ml-2 text-app-accent"
              onClick={() => {
                void navigator.clipboard.writeText(files.folder).then(
                  () => {
                    setCopied(true);
                  },
                  () => undefined,
                );
              }}
            >
              <Copy strokeWidth={1.5} />
              {copied ? S.project.copied : S.project.copyPath}
            </Button>
          </div>
        ) : null}
      </PopoverContent>
    </Popover>
  );
}

/** The project's name; its menu exports the files of the current version. */
export function ProjectMenu() {
  const project = useHotend((state) => state.project?.project ?? null);
  if (!project) return null;
  if (project.current_version === null) {
    return (
      <span className="t-heading min-w-0 truncate px-2 text-app-text" data-testid="project-name" title={project.name}>
        {project.name}
      </span>
    );
  }
  return (
    <ExportPopover
      project={project}
      trigger={
        // A long name gives way to the stage switch: it is cut, and the full name is its title.
        <Button
          variant="ghost"
          size="sm"
          className="max-w-full min-w-0 shrink gap-1 px-2"
          data-testid="project-name"
          title={project.name}
          aria-label={`${S.titlebar.projectMenu}: ${project.name}. ${S.project.export}`}
        >
          <span className="t-heading truncate">{project.name}</span>
          <ChevronDown className="size-3.5 shrink-0 text-app-text-2" strokeWidth={1.5} aria-hidden />
        </Button>
      }
    />
  );
}

/** The printer in the title bar, always with its "Simulated" tag (rule 3.5). */
export function PrinterChip() {
  const navigate = useNavigate();
  const printer = useHotend((state) => state.printers?.[0] ?? null);
  const live = useHotend((state) => state.live);
  const projects = useHotend((state) => state.projects);
  const open = useHotend((state) => state.project?.project ?? null);
  if (!printer) return null;
  const job = Object.values(live)[0] ?? null;
  const percent = job?.detail.progress_percent ?? null;
  const projectName =
    job === null ? null : job.project_id === open?.id ? open.name : (projects?.find((project) => project.id === job.project_id)?.name ?? null);
  return (
    <Popover>
      <PopoverTrigger
        render={
          <Button variant="ghost" size="sm" className="shrink-0 gap-1.5 px-2" data-testid="printer-chip" aria-label={`${S.chip.open}: ${S.chip.name}, ${S.printerTag}${job ? `, ${S.chip.printing(percent)}` : ""}`}>
            {/* The flask is the tag's alone: the chip's own icon is a printer. */}
            <Printer className="size-[18px] text-app-text-2" strokeWidth={1.5} aria-hidden />
            <span className="t-ui">{S.chip.name}</span>
            {printer.simulated ? <SimTag /> : null}
            {job ? <span className="t-caption num text-app-text-2">{percent === null ? "" : `${percent} %`}</span> : null}
          </Button>
        }
      />
      <PopoverContent align="end" className="w-[300px] space-y-2">
        <PopoverTitle className="t-ui-strong">{printer.name}</PopoverTitle>
        {job || !printer.simulated ? null : <p className="t-ui text-app-text-2">{S.chip.nothing}</p>}
        {job ? (
          <div className="space-y-2 pt-1">
            <p className="t-ui num">{S.chip.line(percent, job.detail.remaining_minutes)}</p>
            <Progress value={percent} label={S.print.progress} />
            {job.detail.simulated ? (
              <p className="t-caption text-app-sim" data-testid="chip-sim-note">
                {S.chip.simulatedJob}
              </p>
            ) : null}
            {projectName ? <p className="t-caption text-app-text-2">{projectName}</p> : null}
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void navigate(`/hotend/p/${job.project_id}/print`);
              }}
            >
              {S.chip.go}
            </Button>
          </div>
        ) : (
          <p className="t-ui">{S.chip.idle}</p>
        )}
        {printer.state.ams.length > 0 ? (
          <>
            <hr className="border-app-line" />
            <AmsList units={printer.state.ams} />
          </>
        ) : null}
      </PopoverContent>
    </Popover>
  );
}
