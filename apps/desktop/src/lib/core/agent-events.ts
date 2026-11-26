// The agent's events (docs/PROTOCOL.md 5.2) arrive as plain JSON inside a run event.
// This is the one place that reads their fields.
import type { Value } from "./bindings";

/**
 * What an answer would look up on the web in a run without web research, and why the run had
 * none: the agent sends it as `research_needed: {lookup, state, source}` on a `text_delta`
 * without text (docs/PROTOCOL.md 5.2), so the window never reads the answer's words to know
 * it. `state` is the core's research state of the run (`needs_consent`, `off`, `budget_used`,
 * ...) and `source` the source chosen in the settings; both null when the core did not say.
 */
export type ResearchNeeded = { lookup: string; state: string | null; source: string | null };

function short(value: Value | undefined): string | null {
  return typeof value === "string" ? value.slice(0, 40) : null;
}

export type AgentEvent =
  | { type: "text_delta"; text: string; researchNeeded: ResearchNeeded | null }
  | { type: "thinking"; text: string }
  | { type: "tool_start"; name: string; toolCallId: string; note: string | null }
  | { type: "tool_end"; name: string; toolCallId: string; ok: boolean; summary: string }
  | { type: "usage"; toolCalls: number; inputTokens: number; outputTokens: number }
  | { type: "unknown" };

function text(value: Value | undefined): string {
  return typeof value === "string" ? value : "";
}

function count(value: Value | undefined): number {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

export function agentEvent(event: Value): AgentEvent {
  if (typeof event !== "object" || event === null || Array.isArray(event)) {
    return { type: "unknown" };
  }
  switch (event.type) {
    case "text_delta": {
      const needed = event.research_needed;
      const researchNeeded =
        typeof needed === "object" && needed !== null && !Array.isArray(needed)
          ? { lookup: text(needed.lookup).slice(0, 200), state: short(needed.state), source: short(needed.source) }
          : null;
      return { type: "text_delta", text: text(event.text), researchNeeded };
    }
    case "thinking":
      return { type: "thinking", text: text(event.text) };
    case "tool_start":
      return {
        type: "tool_start",
        name: text(event.name),
        toolCallId: text(event.tool_call_id),
        note: typeof event.note === "string" ? event.note : null,
      };
    case "tool_end":
      return {
        type: "tool_end",
        name: text(event.name),
        toolCallId: text(event.tool_call_id),
        ok: event.ok === true,
        summary: text(event.summary),
      };
    case "usage":
      return {
        type: "usage",
        toolCalls: count(event.tool_calls),
        inputTokens: count(event.input_tokens),
        outputTokens: count(event.output_tokens),
      };
    default:
      return { type: "unknown" };
  }
}
