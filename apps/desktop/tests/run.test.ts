// The streaming reducer (DESIGN_SPEC 4.3.1): text, tool steps, version markers.
import { describe, expect, it } from "vitest";

import type { RunEvent } from "@/lib/core";
import {
  changesProject,
  finishedSteps,
  reduceRun,
  runCaption,
  runTokens,
  runSegments,
  startRun,
  withMarks,
  withStepAfter,
  workLine,
} from "@/hotend/run";

const agent = (event: Record<string, string | number | boolean>): RunEvent => ({ kind: "agent", event });

describe("a run as it streams", () => {
  it("joins the text, tracks each tool call and ends", () => {
    const events: RunEvent[] = [
      agent({ type: "thinking", text: "hm" }),
      agent({ type: "text_delta", text: "Here is " }),
      agent({ type: "text_delta", text: "the plan." }),
      agent({ type: "tool_start", name: "write_model_code", tool_call_id: "c1", note: "first version" }),
      agent({ type: "tool_end", name: "write_model_code", tool_call_id: "c1", ok: true, summary: "status: ok", images: 0 }),
      agent({ type: "tool_start", name: "render_views", tool_call_id: "c2", args_omitted: true }),
      agent({ type: "tool_end", name: "render_views", tool_call_id: "c2", ok: false, summary: "error (no_model)", images: 0 }),
      agent({ type: "usage", requests: 3, input_tokens: 1900, output_tokens: 300, tool_calls: 2 }),
      { kind: "finished", stop_reason: "done", error: null },
    ];
    const run = events.reduce(reduceRun, startRun("r1", "p1"));
    expect(run.status).toBe("done");
    expect(run.text).toBe("Here is the plan.");
    expect(run.thinking).toBe(false);
    expect(run.tools.map((tool) => [tool.id, tool.name, tool.state])).toEqual([
      ["c1", "write_model_code", "ok"],
      ["c2", "render_views", "refused"],
    ]);
    expect(run.usage).toEqual({ toolCalls: 2, tokens: 2200 });
    // One count for the work of a run: the lines "Show steps (2)" opens. No second
    // caption that counts the tool calls another way.
    expect(runCaption(run)).toBeNull();
    expect(finishedSteps(run)).toHaveLength(2);
    expect(runCaption(reduceRun(startRun("r2", "p1"), { kind: "finished", stop_reason: "done", error: null }))).toBe(
      "Answered without changing the part",
    );
    // The token count is a detail: it is offered in a tooltip, not in the line.
    expect(runTokens(run)).toBe("2.2k tokens used");
    // The tool's own summary is written for the AI and is never kept.
    expect(JSON.stringify(run)).not.toContain("status: ok");
  });

  it("says what Hotend is doing in plain words, never the tool's name or arguments", () => {
    let run = startRun("r1", "p1");
    expect(workLine(run)).toBe("Thinking…");
    run = reduceRun(run, agent({ type: "tool_start", name: "write_model_code", tool_call_id: "c1", note: "Wall 2 mm.\nmore" }));
    expect(workLine(run)).toBe("Writing the model: Wall 2 mm");
    run = reduceRun(run, agent({ type: "tool_end", name: "write_model_code", tool_call_id: "c1", ok: true, summary: "", images: 0 }));
    run = reduceRun(run, agent({ type: "tool_start", name: "list_cards", tool_call_id: "c2" }));
    expect(workLine(run)).toBe("Reading the manual…");
    expect(workLine(run)).not.toMatch(/list_cards|\{/);
  });

  it("lists every finished step, with the version a call made", () => {
    let run = startRun("r1", "p1");
    for (const [id, name, ok] of [
      ["c1", "list_cards", true],
      ["c2", "write_model_code", true],
      ["c3", "write_model_code", false],
      ["c4", "render_views", true],
      ["c5", "something_new", true],
    ] as const) {
      run = reduceRun(run, agent({ type: "tool_start", name, tool_call_id: id }));
      run = reduceRun(run, agent({ type: "tool_end", name, tool_call_id: id, ok, summary: "", images: 0 }));
    }
    run = withStepAfter(run, "c2", { version: 1 });
    run = reduceRun(run, agent({ type: "tool_start", name: "run_checks", tool_call_id: "c6" }));
    expect(finishedSteps(run).map((step) => step.text)).toEqual([
      "Read the manual",
      "Wrote the model · v1",
      "The model did not build, trying again",
      "Looked at it",
      "Did something else",
    ]);
  });

  it("keeps how a run ended badly", () => {
    const failed = reduceRun(startRun("r1", "p1"), { kind: "failed", code: "no_credential", message: "No key." });
    expect(failed.status).toBe("error");
    expect(failed.error).toEqual({ code: "no_credential", message: "No key." });
    const stopped = reduceRun(startRun("r1", "p1"), { kind: "finished", stop_reason: "cancelled", error: null });
    expect(stopped.status).toBe("cancelled");
    expect(stopped.error).toBeNull();
  });

  it("puts a version's marker before the paragraph that was streaming, and never moves it", () => {
    let run = startRun("r1", "p1");
    run = reduceRun(run, agent({ type: "text_delta", text: "The plan.\n\nWriting the first" }));
    run = withMarks(run, [1]);
    const before = runSegments(run);
    expect(before).toEqual([
      { type: "text", text: "The plan." },
      { type: "version", version: 1 },
      { type: "text", text: "Writing the first" },
    ]);
    run = reduceRun(run, agent({ type: "text_delta", text: " version.\n\nIt builds." }));
    run = withMarks(run, [1, 2]);
    const after = runSegments(run);
    // What was drawn before the new text is unchanged: no layout shift above the caret.
    expect(after.slice(0, 2)).toEqual(before.slice(0, 2));
    expect(after).toEqual([
      { type: "text", text: "The plan." },
      { type: "version", version: 1 },
      { type: "text", text: "Writing the first version." },
      { type: "version", version: 2 },
      { type: "text", text: "It builds." },
    ]);
    expect(withMarks(run, [1, 2])).toBe(run);
  });

  it("reads the project again after a tool call and when a run ends, not on every word", () => {
    expect(changesProject(agent({ type: "text_delta", text: "x" }))).toBe(false);
    expect(changesProject(agent({ type: "tool_end", name: "slice", tool_call_id: "c", ok: true, summary: "", images: 0 }))).toBe(true);
    expect(changesProject({ kind: "finished", stop_reason: "done", error: null })).toBe(true);
  });
});
