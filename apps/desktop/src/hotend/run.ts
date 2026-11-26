// What a run of the agent looks like while it streams (DESIGN_SPEC 4.3.1). `reduceRun` is
// pure: the store feeds it every run event of the open project, in order. The tool's own
// `summary` is written for the AI and is never shown; the window writes each finished
// step from the tool's name, its `ok` flag and the project as it is after the call.
import { agentEvent, type RunEvent } from "@/lib/core";
import type { ResearchNeeded } from "@/lib/core/agent-events";

import { compact } from "./format";
import { S } from "./strings";

/** What the project showed after a tool call, filled in by the store. */
export type StepAfter = {
  /** The version the call made. */
  version?: number;
  /** For a changed size: its label and value as written. */
  size?: { label: string; value: string };
  /** For a slice: time and weight as written. */
  slice?: { time: string; grams: string };
};

export type ToolStep = {
  id: string;
  name: string;
  /** The reason the agent gave for the call, when it gave one. */
  note: string | null;
  state: "running" | "ok" | "refused";
  after: StepAfter | null;
};

export type RunStatus = "running" | "done" | "budget" | "cancelled" | "error";

export type RunState = {
  runId: string;
  projectId: string;
  status: RunStatus;
  /** The assistant's text so far. */
  text: string;
  /** True between a `thinking` event and the next text or tool call. */
  thinking: boolean;
  tools: ToolStep[];
  /** The run's last `usage` event. */
  usage: { toolCalls: number; tokens: number } | null;
  /** Why the run ended badly: the core's code and sentence. */
  error: { code: string; message: string } | null;
  /** True once the maker asked to stop. */
  stopping: boolean;
  /** The versions this run made, each with how much text had streamed when it arrived. */
  marks: { version: number; offset: number }[];
  /**
   * A run this window picked up after a reload: how much of its text had streamed
   * before, and the steps that were finished then. Zero and empty for a run seen whole.
   */
  offset: number;
  earlier: string[];
  /** True for a run this window took up after a reload. */
  resumed: boolean;
  /**
   * What the run would have looked up on the web and why it had no research: the agent's
   * structured `research_needed` marker, never read from its text. Null when it has none.
   */
  research: ResearchNeeded | null;
};

export function startRun(runId: string, projectId: string): RunState {
  return {
    runId,
    projectId,
    status: "running",
    text: "",
    thinking: false,
    tools: [],
    usage: null,
    error: null,
    stopping: false,
    marks: [],
    offset: 0,
    earlier: [],
    resumed: false,
    research: null,
  };
}

/**
 * A run that was going when the window was loaded again: what the window remembers of
 * it, and nothing of its text, which the core stores when the run ends.
 */
export function resumeRun(runId: string, projectId: string, memory: RunMemory | null): RunState {
  return {
    ...startRun(runId, projectId),
    marks: memory?.marks ?? [],
    offset: memory?.chars ?? 0,
    earlier: memory?.steps ?? [],
    resumed: true,
  };
}

/** How a run that could not reach the AI failed, in the four ways a maker can do something about. */
export type ProviderFailure = "key_rejected" | "rate_limited" | "provider_unreachable" | "model_not_found";

const PROVIDER_FAILURES: readonly string[] = ["key_rejected", "rate_limited", "provider_unreachable", "model_not_found"];

/**
 * The core's code for a failed run where it has one of the four; until the core tells
 * them apart (it reports "upstream_error" or "agent_error" with the provider's own text)
 * the text is read for the status the provider answered with. Null when it says nothing
 * the window knows: the card then stays general.
 */
export function providerFailure(code: string | null, message: string): ProviderFailure | null {
  if (code !== null && PROVIDER_FAILURES.includes(code)) return code as ProviderFailure;
  if (code !== null && !["agent_error", "upstream_error", "provider_error", "egress_error"].includes(code)) return null;
  if (/\b401\b|\b403\b|authentication|unauthori[sz]ed|(invalid|incorrect|wrong|bad).{0,16}(api[ _-]?key|key|token)|permission_error/i.test(message)) return "key_rejected";
  if (/\b404\b|model.{0,60}(not found|does not exist|not exist|unknown)|not_found_error|no such model/i.test(message)) return "model_not_found";
  if (/\b429\b|\b402\b|rate.?limit|quota|credit|billing|overloaded|too many requests/i.test(message)) return "rate_limited";
  if (/fetch failed|error sending request|connection (refused|reset|closed)|ECONN|ENOTFOUND|EAI_AGAIN|ETIMEDOUT|network|dns|timed? ?out|unreachable|\b50[234]\b|upstream/i.test(message) || code === "upstream_error") {
    return "provider_unreachable";
  }
  return null;
}

export function reduceRun(run: RunState, event: RunEvent): RunState {
  switch (event.kind) {
    case "finished":
      return {
        ...run,
        status: event.stop_reason,
        thinking: false,
        error:
          event.stop_reason === "error"
            ? { code: "agent_error", message: event.error ?? "" }
            : run.error,
      };
    case "failed":
      return { ...run, status: "error", thinking: false, error: { code: event.code, message: event.message } };
    case "agent": {
      const agent = agentEvent(event.event);
      switch (agent.type) {
        case "text_delta":
          return {
            ...run,
            text: run.text + agent.text,
            thinking: false,
            research: agent.researchNeeded ?? run.research,
          };
        case "thinking":
          return { ...run, thinking: true };
        case "tool_start":
          return {
            ...run,
            thinking: false,
            tools: [
              ...run.tools,
              { id: agent.toolCallId, name: agent.name, note: agent.note, state: "running", after: null },
            ],
          };
        case "tool_end":
          return {
            ...run,
            tools: run.tools.map((tool) =>
              tool.id === agent.toolCallId ? { ...tool, state: agent.ok ? "ok" : "refused" } : tool,
            ),
          };
        case "usage":
          return {
            ...run,
            usage: { toolCalls: agent.toolCalls, tokens: agent.inputTokens + agent.outputTokens },
          };
        default:
          return run;
      }
    }
  }
}

/** Fills in what the project showed after a call. */
export function withStepAfter(run: RunState, toolCallId: string, after: StepAfter): RunState {
  return {
    ...run,
    tools: run.tools.map((tool) => (tool.id === toolCallId ? { ...tool, after } : tool)),
  };
}

/** True when an event says the stored project may have changed and should be read again. */
export function changesProject(event: RunEvent): boolean {
  if (event.kind !== "agent") return true;
  return agentEvent(event.event).type === "tool_end";
}

function oneLine(text: string | null): string | null {
  const line = text?.split("\n")[0]?.trim() ?? "";
  return line === "" ? null : line.replace(/[.:]$/, "");
}

/**
 * The work line: what Hotend is doing now. `labelOf` gives a size's label for a
 * parameter name the call's note mentions; without one the line stays general.
 */
export function workLine(run: RunState): string {
  const running = [...run.tools].reverse().find((tool) => tool.state === "running");
  if (!running) return S.work.thinking;
  switch (running.name) {
    case "write_model_code":
      return S.work.writing(oneLine(running.note));
    case "set_parameter":
      return S.work.changing(null);
    case "run_checks":
      return S.work.checking;
    case "render_views":
      return S.work.looking;
    case "measure":
      return S.work.measuring;
    case "read_docs":
    case "list_cards":
    case "list_components":
      return S.work.reading;
    case "slice":
      return S.work.slicing;
    case "propose_print":
      return S.work.proposing;
    default:
      return S.work.other;
  }
}

/** The finished steps of a run, in order, as the lines of "Show steps". */
export function finishedSteps(run: RunState): { id: string; text: string }[] {
  const earlier = run.earlier.map((text, index) => ({ id: `earlier-${index}`, text }));
  return [...earlier, ...liveSteps(run)];
}

function liveSteps(run: RunState): { id: string; text: string }[] {
  return run.tools.flatMap((tool) => {
    if (tool.state === "running") return [];
    const ok = tool.state === "ok";
    const after = tool.after;
    let text: string;
    switch (tool.name) {
      case "write_model_code":
        text = ok && after?.version !== undefined ? S.steps.wrote(after.version) : ok ? S.steps.wrote(null) : S.steps.wroteFailed;
        break;
      case "set_parameter":
        text = !ok
          ? S.steps.setFailed
          : after?.size && after.version !== undefined
            ? S.steps.set(after.size.label, after.size.value, after.version)
            : S.steps.setPlain;
        break;
      case "run_checks":
        text = S.steps.checked;
        break;
      case "render_views":
        text = S.steps.looked;
        break;
      case "measure":
        text = S.steps.measured;
        break;
      case "slice":
        text = !ok ? S.steps.sliceFailed : after?.slice ? S.steps.sliced(after.slice.time, after.slice.grams) : S.steps.slicedPlain;
        break;
      case "propose_print":
        text = ok ? S.steps.proposed : S.steps.proposeFailed;
        break;
      case "read_docs":
      case "list_cards":
      case "list_components":
        text = S.steps.read;
        break;
      default:
        text = S.steps.other;
    }
    return [{ id: tool.id, text }];
  });
}

/**
 * What a finished run says about its work: nothing where "Show steps (5)" already
 * counts them, and one sentence where it took no step at all. The count is the count of
 * the lines the disclosure shows, so that the two can never disagree.
 */
export function runCaption(run: RunState): string | null {
  return finishedSteps(run).length > 0 ? null : S.talk.noSteps;
}

/** "38k tokens used", for the caption's tooltip; null when the run counted none. */
export function runTokens(run: RunState): string | null {
  return run.usage && run.usage.tokens > 0 ? S.talk.tokens(compact(run.usage.tokens)) : null;
}

/** True when the run made a print proposal that worked. */
export function proposedPrint(run: RunState): ToolStep | null {
  return run.tools.find((tool) => tool.name === "propose_print" && tool.state === "ok") ?? null;
}

/** Notes the versions that arrived during the run, at the text streamed so far. */
export function withMarks(run: RunState, versions: readonly number[]): RunState {
  const known = new Set(run.marks.map((mark) => mark.version));
  const added = versions.filter((version) => !known.has(version));
  if (added.length === 0) return run;
  return {
    ...run,
    marks: [...run.marks, ...added.map((version) => ({ version, offset: run.offset + run.text.length }))],
  };
}

export type RunSegment = { type: "text"; text: string } | { type: "version"; version: number };

/**
 * The run's text with its versions in between, each before the paragraph that was
 * streaming when it arrived, so that a marker never splits a sentence.
 */
export function runSegments(run: Pick<RunState, "text" | "marks"> & { offset?: number }): RunSegment[] {
  const segments: RunSegment[] = [];
  let from = 0;
  const before = run.offset ?? 0;
  for (const mark of [...run.marks].sort((a, b) => a.offset - b.offset || a.version - b.version)) {
    // The text before the offset never changes, so the marker never moves once drawn.
    const cut = Math.max(from, run.text.lastIndexOf("\n\n", Math.max(0, mark.offset - before)));
    const text = run.text.slice(from, cut).trim();
    if (text !== "") segments.push({ type: "text", text });
    segments.push({ type: "version", version: mark.version });
    from = cut;
  }
  const rest = run.text.slice(from).trim();
  if (rest !== "") segments.push({ type: "text", text: rest });
  return segments;
}

/**
 * What this window saw of a run and the core does not store: where in the text each
 * version was made, and whether the run was stopped. The stored conversation is drawn
 * with it after a reload, so that it reads as it did live. It is a memory for display,
 * kept in the window's own storage for the newest 200 runs; without it a run's versions
 * are listed before its text and a stopped run does not say so.
 */
export type RunMemory = {
  status: RunStatus;
  marks: { version: number; offset: number }[];
  /** The finished steps as they were written, so that "Show steps (5)" reads the same after a reload. */
  steps: string[];
  /** How much of the run's text had streamed. */
  chars: number;
  /** What the run would have looked up and why it had no research (its `research_needed` marker). */
  research?: ResearchNeeded | null;
};

const RUNS_KEY = "hotend.runs";
const RUNS_KEPT = 200;

function readRuns(): Record<string, RunMemory> {
  try {
    const stored: unknown = JSON.parse(window.localStorage.getItem(RUNS_KEY) ?? "{}");
    return typeof stored === "object" && stored !== null && !Array.isArray(stored) ? (stored as Record<string, RunMemory>) : {};
  } catch {
    return {};
  }
}

export function rememberRun(run: RunState): void {
  try {
    const memory: RunMemory = {
      status: run.status,
      marks: run.marks,
      steps: finishedSteps(run).map((step) => step.text),
      chars: run.offset + run.text.length,
      research: run.research,
    };
    const all = { ...readRuns(), [run.runId]: memory };
    const runs = Object.fromEntries(Object.entries(all).slice(-RUNS_KEPT));
    window.localStorage.setItem(RUNS_KEY, JSON.stringify(runs));
  } catch {
    // The conversation then reads a little differently after a reload.
  }
}

function researchOf(stored: unknown): ResearchNeeded | null {
  if (typeof stored !== "object" || stored === null) return null;
  const { lookup, state, source } = stored as Record<string, unknown>;
  if (typeof lookup !== "string") return null;
  const short = (value: unknown) => (typeof value === "string" ? value.slice(0, 40) : null);
  return { lookup: lookup.slice(0, 200), state: short(state), source: short(source) };
}

export function rememberedRun(runId: string): RunMemory | null {
  const found = readRuns()[runId];
  if (!found || !Array.isArray(found.marks)) return null;
  return {
    status: found.status,
    marks: found.marks.filter((mark) => Number.isInteger(mark.version) && Number.isInteger(mark.offset)),
    steps: Array.isArray(found.steps) ? found.steps.filter((step): step is string => typeof step === "string") : [],
    chars: Number.isInteger(found.chars) ? found.chars : 0,
    research: researchOf(found.research),
  };
}

/** A stored text with its versions in between, cut as `runSegments` cuts a live run. */
export function textSegments(text: string, marks: readonly { version: number; offset: number }[]): RunSegment[] {
  return runSegments({ text, marks: [...marks] });
}

/**
 * The run that is going in each project, for this session of the window: a reload reads
 * it back and takes the run up again. It is the session's, not the computer's, because a
 * run does not outlive the core, and a new session starts with a new core.
 */
const LIVE_KEY = "hotend.live";

function readLive(): Record<string, string> {
  try {
    const stored: unknown = JSON.parse(window.sessionStorage.getItem(LIVE_KEY) ?? "{}");
    return typeof stored === "object" && stored !== null && !Array.isArray(stored) ? (stored as Record<string, string>) : {};
  } catch {
    return {};
  }
}

export function noteLiveRun(projectId: string, runId: string | null): void {
  try {
    const rest = Object.fromEntries(Object.entries(readLive()).filter(([id]) => id !== projectId));
    window.sessionStorage.setItem(LIVE_KEY, JSON.stringify(runId === null ? rest : { ...rest, [projectId]: runId }));
  } catch {
    // A reload then does not take the run up again; its result is read when it ends.
  }
}

export function liveRunOf(projectId: string): string | null {
  const runId = readLive()[projectId];
  return typeof runId === "string" ? runId : null;
}
