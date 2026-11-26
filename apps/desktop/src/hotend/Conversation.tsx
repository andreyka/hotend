// The conversation column (DESIGN_SPEC 4.3.1, 4.3.5, 6.4): the stored text, a marker per
// version, the streaming run with its one work line, and the composer. No bubbles, no
// typewriter effect, no raw tool output: the AI's work is shown as work on the part.
import { ArrowUp, ChevronDown, ChevronRight, Square } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useLocation, useNavigate } from "react-router";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import { useHost, usePending } from "@/host/core-context";
import { canTalk } from "@/host/store";
import { H } from "@/host/strings";
import type { ApiError, EngineResult, ProjectDetail, TranscriptEntry, Version } from "@/lib/core";
import type { ResearchNeeded } from "@/lib/core/agent-events";
import { cn } from "@/lib/utils";

import { ErrorCard, StatusIcon } from "./bits";
import { useHotend } from "./context";
import { Markdown } from "./markdown";
import { proofOf, statusLine } from "./proof";
import {
  finishedSteps,
  proposedPrint,
  providerFailure,
  rememberedRun,
  runCaption,
  runSegments,
  runTokens,
  textSegments,
  workLine,
  type RunMemory,
  type RunSegment,
  type RunState,
} from "./run";
import { versionOf } from "./store";
import { S } from "./strings";
import { useStudioUi } from "./ui";
import { markerLines, versionLabel } from "./versions";

type Item =
  | { type: "entry"; entry: TranscriptEntry; segments: RunSegment[] | null; steps: string[]; research?: ResearchNeeded }
  | { type: "version"; version: Version }
  | { type: "stopped"; key: string; at: number | null };

/**
 * The stored conversation in the order it was lived. A version the maker made (a changed
 * size, the example) sits where it was made. The versions of one of Hotend's runs sit
 * inside that run's text where they arrived, as they did while it streamed, when this
 * window remembers the run (`remember`, see run.ts); otherwise they are listed before the
 * text, so that the column still ends with what Hotend said last. A run the maker
 * stopped ends with a line that says so. The entries and versions of the run that is on
 * screen are left out; the live section draws them.
 */
export function storedItems(
  detail: ProjectDetail,
  liveRunId: string | null,
  liveVersions: ReadonlySet<number>,
  remember: (runId: string) => (Pick<RunMemory, "status" | "marks"> & Partial<RunMemory>) | null = rememberedRun,
): Item[] {
  const entries = detail.transcript.filter((entry) => !(entry.role !== "user" && entry.run_id === liveRunId));
  const versions = detail.versions.filter((version) => !liveVersions.has(version.number));
  const items: Item[] = [];
  let next = 0;
  const takeVersionsBefore = (atMs: number): Version[] => {
    const taken: Version[] = [];
    while (next < versions.length && (versions[next]?.created_at_ms ?? 0) < atMs) {
      const version = versions[next++];
      if (version) taken.push(version);
    }
    return taken;
  };
  entries.forEach((entry, index) => {
    const made = takeVersionsBefore(entry.at_ms);
    const memory = remember(entry.run_id);
    if (entry.role === "user") {
      for (const version of made) items.push({ type: "version", version });
      items.push({ type: "entry", entry, segments: null, steps: [] });
    } else {
      // The versions of this run, at the place in its text where each one arrived.
      const marks = (memory?.marks ?? []).filter((mark) => made.some((version) => version.number === mark.version));
      const marked = new Set(marks.map((mark) => mark.version));
      for (const version of made) if (!marked.has(version.number)) items.push({ type: "version", version });
      items.push({
        type: "entry",
        entry,
        segments: marks.length > 0 ? textSegments(entry.text, marks) : null,
        // The steps as they were written while the run streamed: the same lines, the same count.
        steps: memory?.steps ?? [],
        // The last line of the run offers web research, when the run asked for it.
        ...(memory?.research && entries[index + 1]?.run_id !== entry.run_id ? { research: memory.research } : {}),
      });
    }
    const last = entries[index + 1]?.run_id !== entry.run_id;
    if (last && memory?.status === "cancelled" && entry.run_id !== liveRunId) {
      for (const version of takeVersionsBefore(entries[index + 1]?.at_ms ?? Number.POSITIVE_INFINITY).filter(
        (version) => version.origin !== "set_parameter" && memory.marks.some((mark) => mark.version === version.number),
      )) {
        items.push({ type: "version", version });
      }
      const before = detail.versions.filter((version) => version.created_at_ms <= (entries[index + 1]?.at_ms ?? Number.POSITIVE_INFINITY));
      items.push({ type: "stopped", key: entry.run_id, at: before.at(-1)?.number ?? null });
    }
  });
  for (const version of takeVersionsBefore(Number.POSITIVE_INFINITY)) items.push({ type: "version", version });
  return items;
}

function Speaker({ children }: { children: ReactNode }) {
  return <p className="t-caption mb-1 text-app-text-3">{children}</p>;
}

function VersionMarker({
  version,
  detail,
  results,
  onStage,
  onLook,
}: {
  version: Version;
  detail: ProjectDetail;
  results: Record<number, EngineResult>;
  onStage: boolean;
  onLook(): void;
}) {
  const parent = versionOf(detail, version.parent);
  const result = results[version.number];
  const lines = markerLines(
    parent ? { version: parent, result: results[parent.number] } : null,
    { version, result },
  );
  const first = !parent && result ? statusLine(proofOf(result)) : null;
  return (
    <button
      type="button"
      onClick={onLook}
      aria-label={S.marker.look(version.number)}
      data-testid={`marker-${version.number}`}
      className="group block w-full rounded-control py-1 text-left"
    >
      <span className="flex items-center gap-2">
        <span className={cn("h-px w-3 bg-app-line", onStage && "bg-app-accent")} />
        <span className={cn("t-caption font-medium text-app-text-2 group-hover:text-app-text", onStage && "text-app-accent")}>
          {versionLabel(version, parent, result)}
        </span>
        <span className="h-px flex-1 bg-app-line" />
      </span>
      {version.origin !== "set_parameter" && version.note ? (
        <span className="t-caption mt-1 block pl-5 text-app-text-2">“{version.note}”</span>
      ) : null}
      {lines.map((line, index) => (
        <span key={index} className="t-caption mt-0.5 flex items-start gap-1.5 pl-5 text-app-text-2">
          {first && index === 0 ? (
            <StatusIcon
              kind={first.kind === "problems" ? "problem" : first.kind === "measure" ? "question" : first.kind === "notes" ? "note" : "ok"}
              className="size-3.5"
            />
          ) : null}
          <span>{line}</span>
        </span>
      ))}
    </button>
  );
}

/** "Show steps (5)" and the lines behind it: one count, the count of the lines. */
function Steps({ steps, caption = false, title }: { steps: readonly { id: string; text: string }[]; caption?: boolean; title?: string }) {
  const [open, setOpen] = useState(false);
  if (steps.length === 0) return null;
  return (
    <>
      <button
        type="button"
        aria-expanded={open}
        title={title}
        data-testid={caption ? "run-caption" : "show-steps"}
        onClick={() => {
          setOpen(!open);
        }}
        className="t-caption -ml-1 inline-flex h-7 items-center gap-1 rounded-control px-1 text-app-text-3 hover:text-app-text"
      >
        {open ? <ChevronDown className="size-3.5" strokeWidth={1.5} /> : <ChevronRight className="size-3.5" strokeWidth={1.5} />}
        {open ? S.talk.hideSteps(steps.length) : S.talk.showSteps(steps.length)}
      </button>
      {open ? (
        <ol className="t-caption space-y-1 pb-1 pl-5 text-app-text-2" data-testid="steps">
          {steps.map((step) => (
            <li key={step.id}>{step.text}</li>
          ))}
        </ol>
      ) : null}
    </>
  );
}

/**
 * What to offer next to an answer whose run would have looked something up without web
 * research, from the agent's `research_needed` marker (kept in the run or its memory):
 * - not agreed yet (or the core did not say why): "Turn on web research", which asks for the
 *   agreement in the shell's own dialog for the chosen source, as Settings does;
 * - no source chosen, switched off for the project, a key needed, the provider refusing or
 *   having no research: the reason and a button to Settings, AI model, Web research;
 * - the project's allowance used up: the reason alone.
 * It says research is on only after the person agreed in that dialog.
 */
export function researchOffer(need: ResearchNeeded): { kind: "consent" | "settings" | "none"; reason: string | null } {
  const reasons = S.talk.research.reasons;
  switch (need.state) {
    case null:
    case "needs_consent":
      return { kind: "consent", reason: null };
    case "off":
      return { kind: "settings", reason: need.source === "off" ? reasons.noSource : reasons.off };
    case "needs_key":
    case "provider_refused":
    case "unsupported_provider":
      return { kind: "settings", reason: reasons[need.state] };
    case "budget_used":
    case "stopped":
      return { kind: "none", reason: reasons.budget };
    default:
      return { kind: "settings", reason: null };
  }
}

export function ResearchAction({ need }: { need: ResearchNeeded }) {
  const pending = usePending();
  const navigate = useNavigate();
  const location = useLocation();
  const [state, setState] = useState<"idle" | "busy" | "on" | "declined">("idle");
  const offer = researchOffer(need);
  const lookup = need.lookup;
  const toSettings = () => {
    void navigate("/settings/ai", { state: { from: location.pathname, focus: "web-research" } });
  };
  const turnOn = async () => {
    setState("busy");
    try {
      const options = await pending.research_source_options();
      const chosen = options.find((option) => option.chosen) ?? null;
      if (chosen === null || chosen.source === "off" || !chosen.available || chosen.agreed) {
        // Nothing to agree to here (or it was agreed since): the settings say what stands.
        setState("idle");
        toSettings();
      } else {
        const answer = await pending.research_consent_ask(chosen.source);
        setState(answer.agreed ? "on" : "declined");
      }
    } catch {
      // A shell without these commands, or a failure: the settings show what is wrong.
      setState("idle");
      toSettings();
    }
  };
  if (offer.kind !== "consent") {
    return (
      <div className="mt-2 flex flex-wrap items-center gap-2" data-testid="research-action" data-kind={offer.kind}>
        {offer.kind === "settings" ? (
          <Button variant="outline" size="sm" onClick={toSettings} data-testid="research-settings-here">
            {need.state === "off" && need.source === "off" ? S.talk.research.chooseSource : S.talk.research.settings}
          </Button>
        ) : null}
        <span className="t-caption text-app-text-2">{[offer.reason, S.talk.research.why(lookup)].filter(Boolean).join(" ")}</span>
      </div>
    );
  }
  return (
    <div className="mt-2 flex flex-wrap items-center gap-2" data-testid="research-action" data-kind="consent" data-state={state}>
      {state === "on" ? null : (
        <Button variant="outline" size="sm" disabled={state === "busy"} onClick={() => void turnOn()} data-testid="research-turn-on-here">
          {S.talk.research.turnOn}
        </Button>
      )}
      <span className="t-caption text-app-text-2" role={state === "on" || state === "declined" ? "status" : undefined}>
        {state === "on" ? S.talk.research.on : state === "declined" ? S.talk.research.declined : S.talk.research.why(lookup)}
      </span>
    </div>
  );
}

function WorkLine({ run }: { run: RunState }) {
  const steps = finishedSteps(run);
  const running = run.status === "running";
  const caption = runCaption(run);
  return (
    <div className="mt-2">
      {running ? (
        <div className="flex min-h-7 items-start gap-2 py-1" data-testid="work-line">
          <Spinner className="mt-px size-3.5 text-app-text-2" />
          <span className="t-caption text-app-text-2">{workLine(run)}</span>
        </div>
      ) : caption !== null ? (
        <p className="t-caption py-1 text-app-text-3" data-testid="run-caption" title={runTokens(run) ?? undefined}>
          {caption}
        </p>
      ) : null}
      {/* One number for the work of a run: the lines of the disclosure, counted. */}
      <Steps steps={steps} caption={!running} title={runTokens(run) ?? undefined} />
    </div>
  );
}

function RunError({
  run,
  sendErrorKind,
  onRetry,
}: {
  run: RunState | null;
  sendErrorKind: boolean;
  onRetry(text?: string): void;
}) {
  const navigate = useNavigate();
  const sendError = useHotend((state) => state.sendError);
  const cancelRun = useHotend((state) => state.cancelRun);
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const settings = useHost((state) => state.settings);
  const provider = settings?.provider ? H.providers[settings.provider.kind] : "your provider";
  const code = sendErrorKind && sendError ? ("code" in sendError ? sendError.code : sendError.kind) : (run?.error?.code ?? null);
  const message = sendErrorKind && sendError ? sendError.message : (run?.error?.message ?? "");

  // The message itself can always be sent again from the card: nothing typed is lost.
  const again = (
    <Button variant="outline" size="sm" onClick={() => { onRetry(); }} data-testid="send-again">
      {S.runError.sendAgain}
    </Button>
  );
  const pair = (first: ReactNode) => (
    <span className="flex flex-wrap gap-2">
      {first}
      {again}
    </span>
  );
  const settingsButton = (
    <Button variant="outline" size="sm" onClick={() => void navigate("/settings/ai")}>
      {S.runError.openSettings}
    </Button>
  );
  const retry = (
    <Button variant="outline" size="sm" onClick={() => { onRetry(); }}>
      {S.runError.tryAgain}
    </Button>
  );
  const proceed = (
    <Button variant="outline" size="sm" onClick={() => { onRetry(S.runError.continueText); }}>
      {S.runError.continue}
    </Button>
  );

  // The part as the run left it: "unchanged" is not true of a run that made versions.
  const left = run && run.marks.length > 0 && current !== null ? S.runError.partAt(current) : S.runError.unchanged;

  if (run?.status === "budget" && !sendErrorKind) {
    return <ErrorCard title={S.runError.budget(settings?.max_turns ?? 24, current)} action={proceed} code="budget" />;
  }
  switch (code) {
    case "no_credential":
    case "not_configured":
      return <ErrorCard title={S.runError.noKey(provider)} action={pair(settingsButton)} code={code} />;
    case "busy":
      return (
        <ErrorCard
          title={S.runError.busy}
          code={code}
          action={pair(
            <Button variant="outline" size="sm" onClick={() => void cancelRun()}>
              {S.runError.stopIt}
            </Button>,
          )}
        />
      );
    case "cap_exceeded":
    case "timeout":
      return <ErrorCard title={S.runError.limit(current)} action={proceed} code={code} />;
    case "too_many_refusals":
      return <ErrorCard title={S.runError.refusals} action={retry} code={code} />;
    case "sandbox_unavailable":
      return (
        <ErrorCard
          title={S.runError.sandbox}
          code={code}
          action={pair(
            <Button variant="outline" size="sm" onClick={() => void navigate("/settings/computer")}>
              {S.runError.openComputer}
            </Button>,
          )}
        />
      );
    default: {
      // What the provider answered, said in the window's words with the one thing to do;
      // the provider's own text is for a bug report and stays behind "Details".
      const raw = message.length > 600 ? `${message.slice(0, 599).trimEnd()}…` : message;
      const failure = providerFailure(code, message);
      if (failure !== null) {
        return (
          <ErrorCard
            title={S.runError.provider[failure](provider, settings?.provider?.model ?? null)}
            nextStep={left}
            action={failure === "key_rejected" || failure === "model_not_found" ? settingsButton : again}
            code={code}
            raw={raw}
            data-failure={failure}
          />
        );
      }
      return <ErrorCard title={S.runError.other} nextStep={left} action={retry} code={code} raw={raw} />;
    }
  }
}

function Composer({ empty, disabledReason }: { empty: boolean; disabledReason: ReactNode }) {
  const sendMessage = useHotend((state) => state.sendMessage);
  const cancelRun = useHotend((state) => state.cancelRun);
  const run = useHotend((state) => state.run);
  const look = useHotend((state) => state.look);
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const sent = useHotend((state) => state.sent);
  const sendError = useHotend((state) => state.sendError);
  const lastMessage = useHotend((state) => state.lastMessage);
  const draft = useStudioUi((state) => state.draft);
  const [text, setText] = useState("");
  const field = useRef<HTMLTextAreaElement>(null);
  const running = run?.status === "running";
  const disabled = disabledReason !== null;

  // Grows from one line to eight (40 px when empty).
  useLayoutEffect(() => {
    const area = field.current;
    if (!area) return;
    area.style.height = "auto";
    // An empty box is as high as its placeholder, which the browser measures (field-sizing)
    // and scrollHeight does not: with large text the placeholder takes two lines.
    area.style.height = area.value === "" ? "" : `${Math.min(area.scrollHeight + 2, 8 * 21 + 18)}px`;
  }, [text, running]);

  // Text the window puts in for the maker to finish (a picked face, a measurement).
  const taken = useRef(0);
  useEffect(() => {
    if (!draft || draft.tick === taken.current) return;
    taken.current = draft.tick;
    setText((before) => (before.trim() === "" ? draft.text : `${before.trimEnd()} ${draft.text}`));
    requestAnimationFrame(() => {
      const area = field.current;
      if (!area) return;
      area.focus();
      area.setSelectionRange(area.value.length, area.value.length);
    });
  }, [draft]);

  // A message the core refused to start was not sent: the box takes it back. One that is
  // sent again from a card leaves the box when it goes.
  const [seenError, setSeenError] = useState<ApiError | null>(null);
  if (sendError !== seenError) {
    setSeenError(sendError);
    if (sendError !== null && lastMessage !== null && text.trim() === "") setText(lastMessage);
  }
  const [seenSent, setSeenSent] = useState<string | null>(null);
  if (sent !== seenSent) {
    setSeenSent(sent);
    if (sent !== null && text.trim() === sent.trim()) setText("");
  }

  const send = () => {
    const message = text.trim();
    if (message === "" || running || disabled) return;
    setText("");
    void sendMessage(message);
  };

  return (
    <div className="shrink-0 border-t border-app-line p-3">
      {disabledReason}
      {look !== null && current !== null && !disabled ? (
        <p className="t-caption mb-2 text-app-text-2">{S.talk.olderVersion(current)}</p>
      ) : null}
      <div className="relative">
        <textarea
          ref={field}
          id="hotend-composer"
          rows={1}
          value={running ? "" : text}
          disabled={disabled}
          // Read-only, not disabled, while Hotend works: the box keeps the focus, so the
          // maker can type the moment the run ends.
          readOnly={running}
          aria-label={empty ? S.talk.placeholderEmpty : S.talk.placeholder}
          placeholder={running ? S.talk.working : empty ? S.talk.placeholderEmpty : S.talk.placeholder}
          onChange={(event) => {
            setText(event.target.value);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              send();
            }
          }}
          className="t-body block [field-sizing:content] min-h-[2.5rem] w-full resize-none rounded-control border border-app-control bg-app-raised py-[8.5px] pr-11 pl-3 text-app-text placeholder:text-app-text-3 disabled:opacity-100 disabled:placeholder:text-app-text-2 read-only:placeholder:text-app-text-2"
        />
        <div className="absolute right-1.5 bottom-1.5">
          {running ? (
            <Tooltip label={run.stopping ? S.verdict.stopping : S.talk.stop} side="top">
              <Button
                variant="outline"
                size="icon-sm"
                aria-label={S.talk.stop}
                data-testid="stop-run"
                disabled={run.stopping}
                onClick={() => {
                  void cancelRun();
                  // The button goes away when the run does: the keyboard stays in the box.
                  field.current?.focus();
                }}
              >
                <Square className="size-4" strokeWidth={1.5} />
              </Button>
            </Tooltip>
          ) : (
            <Tooltip label={S.talk.send} side="top">
              <Button
                variant={text.trim() === "" || disabled ? "ghost" : "secondary"}
                size="icon-sm"
                aria-label={S.talk.send}
                disabled={text.trim() === "" || disabled}
                onClick={send}
                className={cn(text.trim() !== "" && !disabled && "bg-app-text text-app-bg hover:bg-app-text")}
              >
                <ArrowUp strokeWidth={2} />
              </Button>
            </Tooltip>
          )}
        </div>
      </div>
    </div>
  );
}

export function Conversation({ projectId }: { projectId: string }) {
  const navigate = useNavigate();
  const detail = useHotend((state) => state.project);
  const results = useHotend((state) => state.results);
  const run = useHotend((state) => state.run);
  const sent = useHotend((state) => state.sent);
  const sendError = useHotend((state) => state.sendError);
  const lastMessage = useHotend((state) => state.lastMessage);
  const look = useHotend((state) => state.look);
  const lookAt = useHotend((state) => state.lookAt);
  const sendMessage = useHotend((state) => state.sendMessage);
  const talk = useHost(canTalk);
  const fillComposer = useStudioUi((state) => state.fillComposer);
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  // A run taken up after a reload that has ended has only a part of its text: the stored
  // conversation draws it, and the live section keeps what only the run knows (its error).
  const storedOnly = run !== null && run.resumed && run.status !== "running";
  const liveId = run === null || storedOnly ? null : run.runId;
  const liveVersions = useMemo(() => new Set(storedOnly ? [] : run?.marks.map((mark) => mark.version)), [run?.marks, storedOnly]);
  const items = useMemo(() => (detail ? storedItems(detail, liveId, liveVersions) : []), [detail, liveId, liveVersions]);
  const stage = look ?? detail?.project.current_version ?? null;
  const empty = detail !== null && detail.versions.length === 0 && detail.transcript.length === 0 && sent === null && run === null;

  // Stays at the newest line while the maker has not scrolled away from it.
  useLayoutEffect(() => {
    const element = scroller.current;
    if (element && pinned.current) element.scrollTop = element.scrollHeight;
  });

  const userShown = run !== null && detail?.transcript.some((entry) => entry.run_id === run.runId && entry.role === "user");
  const proposal = run ? proposedPrint(run) : null;
  const currentVersion = detail?.project.current_version ?? null;

  const marker = (version: Version) =>
    detail ? (
      <VersionMarker
        key={`v${version.number}`}
        version={version}
        detail={detail}
        results={results}
        onStage={stage === version.number}
        onLook={() => {
          void lookAt(version.number);
          void navigate(`/hotend/p/${projectId}/design`);
        }}
      />
    ) : null;

  return (
    <div className="flex h-full min-h-0 flex-col bg-app-panel" data-testid="conversation">
      <div
        ref={scroller}
        role="log"
        aria-label={S.titlebar.conversation}
        tabIndex={0}
        className="app-scroll app-ring-inset min-h-0 flex-1 space-y-3 overflow-y-auto p-4"
        onScroll={(event) => {
          const element = event.currentTarget;
          pinned.current = element.scrollHeight - element.scrollTop - element.clientHeight < 48;
        }}
      >
        {empty ? (
          <div className="flex flex-col gap-2 pt-1">
            {S.suggestions.map((suggestion) => (
              <Button
                key={suggestion}
                variant="outline"
                className="h-auto justify-start border-app-line px-3 py-2 text-left font-normal whitespace-normal text-app-text-2 hover:text-app-text"
                onClick={() => {
                  fillComposer(suggestion);
                }}
              >
                {suggestion}
              </Button>
            ))}
          </div>
        ) : null}

        {items.map((item) =>
          item.type === "version" ? (
            marker(item.version)
          ) : item.type === "stopped" ? (
            <p key={`s${item.key}`} className="t-ui text-app-text-2" data-testid="stopped-stored">
              {item.at === null ? S.talk.stoppedNothing : S.talk.stoppedAt(item.at)}
            </p>
          ) : item.entry.role === "user" ? (
            <div key={`e${item.entry.id}`}>
              <Speaker>{S.talk.you}</Speaker>
              <p className="t-body rounded-card bg-app-subtle px-3 py-2 break-words whitespace-pre-wrap text-app-text">
                {item.entry.text}
              </p>
            </div>
          ) : (
            <div key={`e${item.entry.id}`} className={cn(!item.entry.remembered && "opacity-60")} data-testid="stored-answer">
              <Speaker>{S.talk.hotend}</Speaker>
              {item.segments ? (
                <div className="space-y-3">
                  {item.segments.map((segment, index) =>
                    segment.type === "text" ? (
                      <Markdown key={index} text={segment.text} />
                    ) : (
                      (() => {
                        const version = versionOf(detail, segment.version);
                        return version ? marker(version) : null;
                      })()
                    ),
                  )}
                </div>
              ) : (
                <Markdown text={item.entry.text} />
              )}
              {item.research && item.entry.run_id !== run?.runId ? <ResearchAction need={item.research} /> : null}
              {item.entry.run_id !== run?.runId ? (
                <Steps steps={item.steps.map((text, index) => ({ id: String(index), text }))} />
              ) : null}
              {!item.entry.remembered ? <p className="t-caption mt-1 text-app-text-2">{S.talk.notRemembered}</p> : null}
            </div>
          ),
        )}

        {sent !== null && !userShown ? (
          <div>
            <Speaker>{S.talk.you}</Speaker>
            <p className="t-body rounded-card bg-app-subtle px-3 py-2 break-words whitespace-pre-wrap text-app-text">{sent}</p>
          </div>
        ) : null}

        {run && !(storedOnly && run.status !== "error" && run.status !== "budget") ? (
          <div data-testid="live-run" data-status={run.status} data-resumed={run.resumed || undefined}>
            <Speaker>{S.talk.hotend}</Speaker>
            <div className="space-y-3">
              {(storedOnly ? [] : runSegments(run)).map((segment, index) =>
                segment.type === "text" ? (
                  <Markdown key={index} text={segment.text} />
                ) : (
                  (() => {
                    const version = versionOf(detail, segment.version);
                    return version ? marker(version) : null;
                  })()
                ),
              )}
            </div>
            {run.research !== null && run.status !== "running" && !storedOnly ? <ResearchAction need={run.research} /> : null}
            <WorkLine run={run} />
            {run.status === "cancelled" ? (
              <p className="t-ui text-app-text-2" data-testid="stopped">
                {currentVersion === null ? S.talk.stoppedNothing : S.talk.stoppedAt(currentVersion)}
              </p>
            ) : null}
            {proposal ? (
              <div className="app-enter mt-2 space-y-2 rounded-card border border-app-line bg-app-raised p-3 shadow-1">
                <p className="t-ui-strong">{S.talk.proposed}</p>
                {proposal.note ? <p className="t-ui text-app-text-2">“{proposal.note}”</p> : null}
                <Button variant="outline" size="sm" onClick={() => void navigate(`/hotend/p/${projectId}/print`)}>
                  {S.talk.goPrint}
                </Button>
              </div>
            ) : null}
          </div>
        ) : null}

        {sendError || run?.status === "error" || run?.status === "budget" ? (
          <RunError
            run={run}
            sendErrorKind={sendError !== null}
            onRetry={(text) => {
              const message = text ?? lastMessage;
              if (message) void sendMessage(message);
            }}
          />
        ) : null}
      </div>

      <Composer
        empty={detail?.versions.length === 0}
        disabledReason={
          talk ? null : (
            <div className="mb-2 flex items-center justify-between gap-2">
              <p className="t-caption text-app-text-2">{S.talk.noKey}</p>
              <Button variant="outline" size="sm" onClick={() => void navigate("/settings/ai")}>
                {S.runError.openSettings}
              </Button>
            </div>
          )
        }
      />
    </div>
  );
}
