// Home (DESIGN_SPEC 4.2): the prompt is the page; under it the projects, most recently
// changed first, each with the picture of its part as it last looked in Design.
import { ArrowUp, Box, Ellipsis, Search } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { useHost, useHostStore } from "@/host/core-context";
import { canTalk, healthFailed } from "@/host/store";
import { H } from "@/host/strings";
import { useTheme } from "@/host/theme";
import type { Project } from "@/lib/core";

import exampleDark from "./assets/example-dark.png";
import exampleLight from "./assets/example-light.png";
import { ApiErrorCard, ErrorCard, IconButton, StatusIcon, type StatusKind } from "./bits";
import { useHotend, useNow } from "./context";
import { EXAMPLE_CODE } from "./example";
import { ago } from "./format";
import type { VerdictKind } from "./proof";
import type { CardInfo } from "./store";
import { S } from "./strings";
import { readThumb } from "./thumbs";
import { ExportPopover } from "./TitleBar";

const NAME_LIMIT = 40;

/**
 * A project is named after the first clause of its first message: up to the first comma,
 * full stop or line end. A clause longer than 40 characters ends on its last whole word
 * within them, with "…" to say that it was cut.
 */
export function projectName(text: string): string {
  const clause = (/^[^,.;:!?\n]*/.exec(text.trim())?.[0] ?? "").replace(/\s+/g, " ").trim();
  const name = clause === "" ? text.trim().replace(/\s+/g, " ") : clause;
  if (name.length <= NAME_LIMIT) return name;
  const cut = name.slice(0, NAME_LIMIT + 1);
  const end = cut.lastIndexOf(" ");
  return `${(end > 0 ? cut.slice(0, end) : name.slice(0, NAME_LIMIT)).trimEnd()}…`;
}

const ICONS: Record<VerdictKind, StatusKind> = {
  none: "none",
  problems: "problem",
  measure: "question",
  notes: "note",
  ready: "ok",
};

function Picture({ project }: { project: Project }) {
  const [picture, setPicture] = useState<string | null>(null);
  const theme = useTheme((state) => state.theme);
  const version = project.current_version;
  useEffect(() => {
    let left = false;
    if (version === null) return;
    // The picture of the current version, or of the newest one before it that was on the
    // stage: a size changed a moment ago is still the same part to look at.
    void (async () => {
      for (let number = version; number >= Math.max(1, version - 8); number -= 1) {
        const found = await readThumb(project.id, number, theme);
        if (found !== null || number === Math.max(1, version - 8)) {
          if (!left) setPicture(found);
          return;
        }
      }
    })();
    return () => {
      left = true;
    };
  }, [project.id, version, theme]);
  // The picture has nothing behind the part: the card draws the stage's own ground.
  return (
    <div className="grid aspect-[3/2] w-full place-items-center overflow-hidden rounded-t-[7px] bg-linear-to-b from-hotend-stage-top to-hotend-stage-bottom">
      {picture ? (
        <img src={picture} alt="" className="size-full object-cover" data-testid="card-picture" data-theme-of={theme} />
      ) : (
        <Box className="size-8 text-app-text-3" strokeWidth={1.25} aria-hidden />
      )}
    </div>
  );
}

function Card({ project, card, now }: { project: Project; card: CardInfo | undefined; now: number }) {
  // A print that runs, as the print events say it; the card's own reading is from when Home opened.
  const live = useHotend((state) => Object.values(state.live).find((job) => job.project_id === project.id) ?? null);
  const working = useHotend((state) => state.runs[project.id]?.status === "running");
  const printing = live ?? card?.printing ?? null;
  const to = `/hotend/p/${project.id}/${printing && !working ? "print" : "design"}`;
  return (
    <li className="group relative" data-testid="project-card">
      <Link
        to={to}
        aria-label={S.home.open(project.name)}
        className="block h-full rounded-card border border-app-line bg-app-panel hover:border-app-control hover:bg-app-raised"
      >
        <Picture project={project} />
        <div className="space-y-0.5 px-3 pt-2.5 pb-3">
          <p className="t-ui-strong truncate text-app-text" title={project.name}>
            {project.name}
          </p>
          {/* One height for every card, whatever the row holds. */}
          <p
            className="t-ui flex h-5 items-center gap-1.5 text-app-text-2"
            data-testid="card-status"
            data-state={working ? "working" : printing ? "printing" : undefined}
          >
            {working ? (
              <>
                <StatusIcon kind="busy" />
                <span className="truncate">{S.card.working}</span>
              </>
            ) : printing ? (
              printing.detail.simulated ? (
                <>
                  <StatusIcon kind="sim" />
                  <span className="num truncate text-app-sim">{S.card.simulatedPrint(printing.detail.progress_percent)}</span>
                </>
              ) : (
                <>
                  <StatusIcon kind="busy" />
                  <span className="num truncate">{S.card.printing(printing.detail.progress_percent)}</span>
                </>
              )
            ) : card === undefined ? null : card.status === null ? (
              S.card.nothing
            ) : (
              <>
                <StatusIcon kind={ICONS[card.status.kind]} />
                <span className="num truncate">
                  {card.status.text}
                  {card.version === null ? "" : ` · v${card.version}`}
                </span>
              </>
            )}
          </p>
          <p className="t-caption text-app-text-2">{ago(project.updated_at_ms, now)}</p>
        </div>
      </Link>
      {project.current_version !== null ? (
        <div className="absolute top-2 right-2 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 has-[[data-popup-open]]:opacity-100">
          <ExportPopover
            project={project}
            align="end"
            trigger={
              <IconButton label={S.project.export} className="border border-app-line bg-app-raised shadow-1">
                <Ellipsis strokeWidth={1.5} />
              </IconButton>
            }
          />
        </div>
      ) : null}
    </li>
  );
}

/** The bundled example, as a card like any project's: what Hotend makes, before anything is made. */
function ExampleCard({ busy, disabled, onOpen }: { busy: boolean; disabled: boolean; onOpen(): void }) {
  const theme = useTheme((state) => state.theme);
  return (
    <li className="relative" data-testid="example-card">
      <button
        type="button"
        disabled={disabled}
        onClick={onOpen}
        data-testid="open-example"
        className="block h-full w-full rounded-card border border-app-line bg-app-panel text-left hover:border-app-control hover:bg-app-raised disabled:opacity-60"
      >
        <div className="relative grid aspect-[3/2] w-full place-items-center overflow-hidden rounded-t-[7px] bg-linear-to-b from-hotend-stage-top to-hotend-stage-bottom">
          <img src={theme === "light" ? exampleLight : exampleDark} alt="" className="size-full object-cover" />
          <Badge className="absolute top-2 left-2">{S.home.exampleTag}</Badge>
        </div>
        <div className="space-y-0.5 px-3 pt-2.5 pb-3">
          <p className="t-ui-strong truncate text-app-text">{S.home.exampleTitle}</p>
          <p className="t-ui flex h-5 items-center gap-1.5 text-app-text-2">
            {busy ? <Spinner /> : null}
            {busy ? S.home.exampleBuilding : S.home.exampleLine}
          </p>
          <p className="t-caption text-app-text-2">{S.home.exampleUnder}</p>
        </div>
      </button>
    </li>
  );
}

export function Home() {
  const navigate = useNavigate();
  const location = useLocation();
  const hostStore = useHostStore();
  const projects = useHotend((state) => state.projects);
  const projectsError = useHotend((state) => state.projectsError);
  const cards = useHotend((state) => state.cards);
  const loadProjects = useHotend((state) => state.loadProjects);
  const createProject = useHotend((state) => state.createProject);
  const openExample = useHotend((state) => state.openExample);
  const error = useHotend((state) => state.error);
  const dismissError = useHotend((state) => state.dismissError);
  const talk = useHost(canTalk);
  const settingsRead = useHost((state) => state.settings !== null);
  const unhealthy = useHost(healthFailed);
  const now = useNow(true);

  const [text, setText] = useState("");
  const [needKey, setNeedKey] = useState(false);
  const [busy, setBusy] = useState<"send" | "example" | null>(null);
  const [query, setQuery] = useState("");
  const field = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    void loadProjects();
    void hostStore.getState().quickCheck();
  }, [loadProjects, hostStore]);

  // Focused when the page opens, and again on Mod+N.
  useEffect(() => {
    field.current?.focus();
  }, [location.state]);

  // One to six lines.
  useLayoutEffect(() => {
    const area = field.current;
    if (!area) return;
    area.style.height = "auto";
    area.style.height = `${Math.min(area.scrollHeight + 2, 6 * 21 + 26)}px`;
  }, [text]);

  const send = async () => {
    const message = text.trim();
    if (message === "" || busy !== null || unhealthy || projectsError) return;
    if (!talk) {
      setNeedKey(true);
      return;
    }
    setBusy("send");
    const project = await createProject(projectName(message));
    setBusy(null);
    if (project) void navigate(`/hotend/p/${project.id}/design`, { state: { first: message } });
  };

  const example = async () => {
    if (busy !== null) return;
    setBusy("example");
    const project = await openExample(S.home.exampleName, EXAMPLE_CODE, S.home.exampleNote);
    setBusy(null);
    if (project) void navigate(`/hotend/p/${project.id}/design`);
  };

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return needle === "" ? (projects ?? []) : (projects ?? []).filter((project) => project.name.toLowerCase().includes(needle));
  }, [projects, query]);
  const none = projects !== null && projects.length === 0 && !projectsError;

  return (
    <div className="app-scroll h-full overflow-y-auto [scrollbar-gutter:stable_both-edges]" data-testid="home">
      {/* One container for the page: the prompt, the ideas and the projects share its two edges. */}
      <div className="mx-auto flex min-h-full w-full max-w-[976px] flex-col px-6 pb-10" data-testid="home-column">
        <section className="w-full pt-14 pb-10" aria-labelledby="home-question">
          {unhealthy ? (
            <ErrorCard
              className="mb-5"
              title={H.setup.failsClosed}
              action={
                <Button variant="outline" size="sm" onClick={() => void navigate("/settings/computer")}>
                  {S.home.healthFailed}
                </Button>
              }
            />
          ) : null}
          <h1 id="home-question" className="t-display pb-5 text-center text-app-text">
            {S.home.question}
          </h1>
          <div className="relative">
            <textarea
              ref={field}
              rows={1}
              value={text}
              disabled={unhealthy || busy === "send" || projectsError !== null}
              aria-labelledby="home-question"
              placeholder={S.home.placeholder}
              data-testid="home-prompt"
              onChange={(event) => {
                setText(event.target.value);
                setNeedKey(false);
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  void send();
                }
              }}
              className="t-body block min-h-12 w-full resize-none rounded-card border border-app-control bg-app-raised py-[12.5px] pr-12 pl-4 text-app-text shadow-1 placeholder:text-app-text-3 disabled:opacity-60"
            />
            <div className="absolute right-2 bottom-2">
              <Button
                size="icon-sm"
                variant={text.trim() === "" ? "ghost" : "default"}
                aria-label={S.home.send}
                title={S.home.send}
                disabled={text.trim() === "" || unhealthy || busy !== null || projectsError !== null}
                onClick={() => void send()}
                data-testid="home-send"
              >
                {busy === "send" ? <Spinner /> : <ArrowUp strokeWidth={2} />}
              </Button>
            </div>
          </div>
          {needKey && settingsRead ? (
            <p className="t-ui flex flex-wrap items-center gap-2 pt-2 text-app-text" role="status" data-testid="need-key">
              <StatusIcon kind="note" />
              {S.home.noKey}
              <Button variant="outline" size="sm" onClick={() => void navigate("/settings/ai")}>
                {S.verdict.openSettings}
              </Button>
            </p>
          ) : null}
          {/* Three ideas of equal width in one row: a tap puts the words in the field. */}
          <div className="grid grid-cols-3 gap-6 pt-4" data-testid="suggestions">
            {S.suggestions.map((suggestion) => (
              <button
                key={suggestion}
                type="button"
                disabled={unhealthy || projectsError !== null}
                onClick={() => {
                  setText(suggestion);
                  field.current?.focus();
                }}
                className="t-ui min-h-7 rounded-card border border-app-line px-3 py-2 text-left [text-wrap:balance] text-app-text-2 hover:border-app-control hover:bg-app-hover hover:text-app-text disabled:opacity-40"
              >
                {suggestion}
              </button>
            ))}
          </div>
          {/* One cause is said once: the card under Projects says it when the list failed too. */}
          {error && error.message !== projectsError?.message ? (
            <ApiErrorCard
              error={error}
              className="mt-4"
              action={
                <Button variant="outline" size="sm" onClick={dismissError}>
                  {H.error.dismiss}
                </Button>
              }
            />
          ) : null}
        </section>

        {none ? (
          <section aria-labelledby="home-example" className="w-full" data-testid="home-empty">
            <div className="flex min-h-9 items-center pb-3">
              <h2 id="home-example" className="t-heading text-app-text">
                {S.home.projects}
              </h2>
            </div>
            <ul className="grid grid-cols-2 gap-6 min-[900px]:grid-cols-3">
              <ExampleCard busy={busy === "example"} disabled={busy !== null} onOpen={() => void example()} />
            </ul>
            <p className="t-ui pt-4 text-app-text-2">{S.home.empty}</p>
          </section>
        ) : (
          <section aria-labelledby="home-projects" className="w-full">
            <div className="flex min-h-9 items-center justify-between gap-4 pb-3">
              <h2 id="home-projects" className="t-heading text-app-text">
                {S.home.projects}
              </h2>
              {projects !== null && projects.length > 8 ? (
                <div className="relative w-[240px]">
                  <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-app-text-2" strokeWidth={1.5} aria-hidden />
                  <Input
                    type="search"
                    value={query}
                    aria-label={S.home.search}
                    placeholder={S.home.search}
                    onChange={(event) => {
                      setQuery(event.target.value);
                    }}
                    className="pl-8"
                  />
                </div>
              ) : null}
            </div>
            {projectsError ? (
              <ErrorCard
                title={S.home.folderFailed}
                detail={projectsError.message}
                nextStep={projectsError.next_step}
                action={
                  <Button variant="outline" size="sm" onClick={() => void loadProjects()}>
                    {S.home.tryAgain}
                  </Button>
                }
              />
            ) : projects === null ? (
              <ul className="grid grid-cols-2 gap-6 min-[900px]:grid-cols-3" aria-hidden data-testid="cards-loading">
                {[0, 1, 2, 3].map((index) => (
                  <li key={index} className="rounded-card border border-app-line opacity-40">
                    <div className="aspect-[3/2]" />
                    <div className="h-[82px]" />
                  </li>
                ))}
              </ul>
            ) : shown.length === 0 ? (
              <p className="t-ui text-app-text-2">{S.home.noMatch}</p>
            ) : (
              <ul className="grid grid-cols-2 gap-6 min-[900px]:grid-cols-3" data-testid="cards">
                {shown.map((project) => (
                  <Card key={project.id} project={project} card={cards[project.id]} now={now} />
                ))}
                {/* The maker's own work comes first; the example follows it until it has been opened once. */}
                {query.trim() === "" && !projects.some((project) => project.name === S.home.exampleName) ? (
                  <ExampleCard busy={busy === "example"} disabled={busy !== null} onOpen={() => void example()} />
                ) : null}
              </ul>
            )}
          </section>
        )}
      </div>
    </div>
  );
}
