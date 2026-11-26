// Regression tests: what was once broken, written down as what must hold. Each test names
// the defect it stands for.
import { describe, expect, it, vi } from "vitest";

import type { ApiError, PrintJob, SliceOutcome } from "@/lib/core";
import { toApiError } from "@/lib/core";
import { storedItems } from "@/hotend/Conversation";
import { sentence } from "@/hotend/format";
import { projectName } from "@/hotend/Home";
import { pickedPlace } from "@/hotend/pick";
import { proofOf, readable } from "@/hotend/proof";
import { createHotendStore } from "@/hotend/store";
import { thumbKey } from "@/hotend/thumbs";
import { fitLayout, LAYOUT_DEFAULT, STAGE_MIN } from "@/hotend/ui";
import { designVerdict, prepareVerdict, printVerdict, stageMarks, type VerdictInput } from "@/hotend/verdict";

import { cableBoxClient, detail, PROJECT, settled, v2 } from "./cable-box";

describe("the three columns in a window (the inspector was cut off at the minimum size)", () => {
  const shown = (width: number, layout = LAYOUT_DEFAULT, keep: "conversation" | "inspector" = "inspector") => {
    const fitted = fitLayout(layout, width, keep);
    const side = (fitted.hideConversation ? 0 : fitted.conversation + 1) + (fitted.hideInspector ? 0 : fitted.inspector + 1);
    return { fitted, stage: width - side };
  };

  it("never draws columns wider than the window, and keeps the stage at its minimum", () => {
    for (const width of [1100, 1150, 1219, 1220, 1280, 1440, 1920]) {
      for (const keep of ["conversation", "inspector"] as const) {
        const { stage } = shown(width, { ...LAYOUT_DEFAULT, conversation: 520, inspector: 440 }, keep);
        expect(stage).toBeGreaterThanOrEqual(STAGE_MIN);
      }
    }
  });

  it("shows one side column at 1100 px: the one the maker asked for last", () => {
    expect(shown(1100).fitted).toMatchObject({ hideConversation: true, hideInspector: false });
    expect(shown(1100, LAYOUT_DEFAULT, "conversation").fitted).toMatchObject({ hideConversation: false, hideInspector: true });
  });

  it("leaves the chosen widths alone where there is room", () => {
    expect(shown(1280).fitted).toEqual(LAYOUT_DEFAULT);
  });

  it("lets the conversation give way first, then the inspector", () => {
    const { fitted } = shown(1200);
    expect(fitted.hideConversation || fitted.hideInspector).toBe(false);
    expect(fitted.conversation).toBeLessThan(LAYOUT_DEFAULT.conversation);
    expect(fitted.inspector).toBe(LAYOUT_DEFAULT.inspector);
  });
});

describe("a project's name (it was the first six words, cut mid-phrase)", () => {
  it("is the first clause of the first message", () => {
    expect(projectName("A box that hides a power strip, with a snap-on lid")).toBe("A box that hides a power strip");
    expect(projectName("  A wall hook.\nFor a 32 mm pipe")).toBe("A wall hook");
  });

  it("ends on a whole word within 40 characters, and says that it was cut", () => {
    const name = projectName("A wall shelf that carries several potted plants");
    expect(name).toBe("A wall shelf that carries several potted…");
    expect(name.length).toBeLessThanOrEqual(41);
    expect(projectName("Supercalifragilisticexpialidociousnessandmorewithoutanyspace")).toHaveLength(41);
  });
});

describe("what a click on the part is called (it was a row of coordinates)", () => {
  const box = { min: [-62, -32, 0], max: [62, 32, 42] };
  it("names the face in words from where it lies on the part", () => {
    expect(pickedPlace("face", [10, 5, 42], box)).toBe("the top");
    expect(pickedPlace("face", [10, 5, 0], box)).toBe("the bottom");
    expect(pickedPlace("face", [10, -32, 20], box)).toBe("a long side");
    expect(pickedPlace("face", [62, 5, 20], box)).toBe("a short side");
    expect(pickedPlace("face", [10, 5, 2], box)).toBe("an inside face");
    expect(pickedPlace("edge", [62, 32, 20], box)).toBe("an upright edge");
    expect(pickedPlace("vertex", [62, 32, 42], box)).toBe("a corner at the top");
  });
  it("still says something when the point or the part's size is not known", () => {
    expect(pickedPlace("face", null, box)).toBe("a face");
    expect(pickedPlace("edge", [0, 0, 0], null)).toBe("an edge");
  });
});

describe("the proof list (it printed the engine's sentences, ids and coordinates)", () => {
  const proof = proofOf(v2.result);
  const lines = proof.groups.flatMap((group) => group.lines);

  it("says what a requirement found in the window's words and the result's numbers", () => {
    // The example meets its inside size; the unmet wording is in proof.test.ts.
    const room = lines.find((line) => line.title === "Room inside the box");
    expect(room?.summary).toBe("A 120 × 60 × 36 mm block fits inside the box.");
    expect(room?.unmet).toBe(false);
  });

  it("calls a size it cannot find on the part by the size's own name, and says what to do", () => {
    const cable = lines.find((line) => line.title === "Cable slot");
    // The engine reports it as not checkable (with its reason), never as skipped.
    const engine = (v2.result.checks ?? []).find((check) => (check.data as { requirement?: string }).requirement === "cables_pass");
    expect(engine?.data).toMatchObject({ met: null, not_checkable: true });
    expect(cable?.status).toBe("warn");
    expect(cable?.fixed).toBe("Not checked yet: measure it and type what you read above.");
    expect(lines.some((line) => line.title.startsWith("A named size"))).toBe(false);
  });

  it("never puts an id, a back-tick or a quoted code name in a title or a summary", () => {
    for (const line of lines) {
      for (const text of [line.title, line.summary ?? "", line.fixed ?? ""]) {
        expect(text).not.toMatch(/`|'[a-z0-9]+_[a-z0-9_]+'|holds_power_strip|cavity_min|x=-?\d/);
      }
    }
  });

  it("makes the engine's sentence readable for Details", () => {
    expect(readable("Requirement 'holds_power_strip' (guessed) is NOT met: use `cavity_min` on 'box'; 2 face(s), 12 mm2, 120 x 60 mm")).toBe(
      "Requirement holds power strip (guessed) is not met: use cavity min on box; 2 faces, 12 mm², 120 × 60 mm",
    );
    expect(sentence("the simulated print cancelled; nothing was printed")).toBe("The simulated print cancelled; nothing was printed.");
    expect(sentence("support under 12 mm2")).toBe("Support under 12 mm².");
  });
});

describe("an unreachable core (it showed the browser's 'Failed to fetch')", () => {
  it("is said in Hotend's words", () => {
    expect(toApiError(new TypeError("Failed to fetch")).message).toBe("Hotend's core is not answering.");
    expect(toApiError(new TypeError("Load failed")).message).toBe("Hotend's core is not answering.");
    // Anything else keeps its own words.
    expect(toApiError(new Error("The folder is gone.")).message).toBe("The folder is gone.");
  });
});

describe("a card's picture (it was taken in one theme and shown in the other)", () => {
  it("is kept once per theme", () => {
    expect(thumbKey("p1", 3, "dark")).not.toBe(thumbKey("p1", 3, "light"));
  });
});

async function input(change: Partial<VerdictInput> = {}): Promise<VerdictInput> {
  const store = createHotendStore(cableBoxClient());
  await store.getState().openProject(PROJECT);
  return { ...store.getState(), canTalk: true, now: 1_000_000, ...change };
}

const slice = (codes: string[] = []): SliceOutcome =>
  ({
    record: { project_id: PROJECT, version: 2, profile_hash: "h", profile: "demo-0.20mm", summary: null, job: { plate: 1, printer_id: "bambu-x2d", material: { id: "pla-basic", family: "PLA" }, pieces: ["box", "lid"], prepared_at_ms: 1 }, created_at_ms: 9e15 },
    summary: {
      status: "ok",
      output: "plate.gcode",
      sha256: "abc",
      time_s: 3 * 3600 + 180,
      filament: [{ slot: 0, grams: 96, material: "PLA" }],
      layers: 200,
      warnings: codes.map((code) => ({ code, message: code })),
      slicer: { name: "s", version: "1" },
    },
    preparation: null,
    output_path: "/tmp/plate.gcode",
  }) as unknown as SliceOutcome;

const plates = (count: number) =>
  ({ created_at_ms: 1, plates: Array.from({ length: count }, (_, index) => ({ index: index + 1, size: [256, 256], items: [] })), unplaced: [] }) as unknown as VerdictInput["preparation"];

const job = (status: string, simulated: boolean): PrintJob =>
  ({
    id: "j1",
    project_id: PROJECT,
    version: 2,
    slice_sha256: "abc",
    note: "",
    status,
    delivery: "simulated_printer",
    created_at_ms: 1,
    updated_at_ms: 2,
    detail: { simulated, phase: null, headline: null, progress_percent: null, remaining_minutes: null, message: null, problems: [], findings: [], link: null, path: null, windows_path: null, opened: null, opener: null },
  }) as PrintJob;

describe("the verdict bar", () => {
  it("puts the news first when a rebuild is slow (the end of the sentence was cut)", async () => {
    const rebuild = { name: "wall", label: "Wall thickness", unit: "mm", value: 2.6, since: 0 };
    expect(designVerdict(await input({ rebuild, now: 5_000 }))?.sentence).toBe("Rebuilding with Wall thickness 2.6 mm");
    expect(designVerdict(await input({ rebuild, now: 20_000 }))?.sentence).toBe(
      "Taking longer than usual: rebuilding with Wall thickness 2.6 mm",
    );
  });

  it("says the computer could not rebuild, not that the size does not build", async () => {
    const error: ApiError = { kind: "internal", message: "Hotend's core is not answering.", next_step: "Restart Hotend." };
    const verdict = designVerdict(await input({ rebuildError: { name: "wall", label: "Wall thickness", unit: "mm", value: 2.8, still: 2, error } }));
    expect(verdict).toMatchObject({ icon: "problem", sentence: "Could not rebuild. Still at 2 mm." });
  });

  it("uses one sentence of its own for a failed slice, never the slicer's (it was cut mid-quote)", async () => {
    const sliceError = { kind: "slicer", code: "slicer_failed", message: 'The slicer stopped with exit code 1: "Error: object', next_step: "" } as ApiError;
    const verdict = prepareVerdict(await input({ preparation: plates(1), sliceError }));
    expect(verdict?.sentence).toBe("Slicing failed.");
    expect(verdict?.primary?.label).toBe("Slice again");
  });

  it("marks stand-in numbers as rough, with the flask and not the green tick", async () => {
    const real = prepareVerdict(await input({ preparation: plates(1), slice: slice() }));
    expect(real).toMatchObject({ icon: "ok", sentence: "Sliced: 3 h 3 min · 96 g" });
    const rough = prepareVerdict(await input({ preparation: plates(1), slice: slice(["fake_slicer"]) }));
    expect(rough).toMatchObject({ icon: "sim", sentence: "Sliced (rough): about 3 h 3 min · about 96 g" });
  });

  it("says which plate of how many is sliced, and does not tick Prepare until all are", async () => {
    const several = await input({ preparation: plates(4), slice: slice() });
    const verdict = prepareVerdict(several);
    expect(verdict?.sentence).toBe("Plate 1 of 4 sliced: 3 h 3 min · 96 g");
    expect(verdict?.primary?.label).toBe("Review plate 1");
    expect(stageMarks(several).prepare).toBe("attention");
    expect(stageMarks(await input({ preparation: plates(1), slice: slice() })).prepare).toBe("done");
  });

  it("offers the next plate after a finished print of one of several", async () => {
    const verdict = printVerdict(await input({ preparation: plates(4), slice: slice(), jobs: [job("finished", false)] }));
    expect(verdict?.sentence).toBe("Plate 1 of 4 finished.");
    expect(verdict?.primary).toEqual({ label: "Go to plate 2", action: { do: "plate", plate: 2 } });
  });

  it("does not show a green tick or 'Print again' after a simulated print", async () => {
    const done = await input({ preparation: plates(1), slice: slice(["fake_slicer"]), jobs: [job("finished", true)] });
    const verdict = printVerdict(done);
    expect(verdict).toMatchObject({ icon: "sim", sentence: "Simulated print finished. Nothing was made." });
    expect(verdict?.primary?.label).toBe("Back to Design");
    expect(verdict?.quiet?.label).toBe("Run it again");
    expect(stageMarks(done).print).toBe("sim");
    // A real print keeps its tick and its button.
    const real = await input({ preparation: plates(1), slice: slice(), jobs: [job("finished", false)] });
    expect(printVerdict(real)).toMatchObject({ icon: "ok", sentence: "Finished." });
    expect(stageMarks(real).print).toBe("done");
  });
});

describe("the store", () => {
  it("gives a message that could not be sent back to the composer (it disappeared)", async () => {
    const refusal: ApiError = { kind: "internal", message: "Hotend's core is not answering.", next_step: "Restart Hotend." };
    const store = createHotendStore(cableBoxClient({ message_send: vi.fn().mockRejectedValue(refusal) }));
    await store.getState().openProject(PROJECT);
    await store.getState().sendMessage("Make the slot 14 mm wide");
    const state = store.getState();
    // Nothing pretends the message is on its way, and its words are kept for the composer.
    expect(state.sent).toBeNull();
    expect(state.sendError).toEqual(refusal);
    expect(state.lastMessage).toBe("Make the slot 14 mm wide");
    expect(state.run).toBeNull();
  });

  it("proposes one print job when the Print stage is entered twice in one tick", async () => {
    const pending = { ...job("pending", true) };
    let jobs: PrintJob[] = [];
    const print_propose = vi.fn(() => {
      jobs = [pending];
      return new Promise<PrintJob>((resolve) => setTimeout(() => {
        resolve(pending);
      }, 5));
    });
    const print_review = vi.fn(() =>
      Promise.resolve({ job: pending, delivery: "simulated_printer", simulated: true, printer: null, summary: {}, warnings: [], spools: [] }),
    );
    const client = cableBoxClient({
      print_jobs: vi.fn(() => Promise.resolve(jobs)),
      print_propose: print_propose as never,
      print_review: print_review as never,
    });
    const store = createHotendStore(client);
    await store.getState().openProject(PROJECT);
    store.setState({ slice: slice(), quality: "demo-0.20mm", sliceStale: false, wantReview: true });
    const both = [store.getState().enterPrint(), store.getState().enterPrint()];
    await Promise.all(both);
    await settled();
    expect(print_propose).toHaveBeenCalledTimes(1);
  });
});

describe("the stored conversation (after a reload it changed order and lost 'Stopped')", () => {
  const assistant = detail.transcript.find((entry) => entry.role !== "user");
  const runId = assistant?.run_id ?? "";
  const kinds = (items: ReturnType<typeof storedItems>) =>
    items.map((item) => (item.type === "version" ? `v${item.version.number}` : item.type === "stopped" ? "stopped" : item.entry.role));

  it("puts each version where it arrived in Hotend's text, as the live run did", () => {
    const text = assistant?.text ?? "";
    const marks = [
      { version: 1, offset: Math.floor(text.length / 3) },
      { version: 2, offset: Math.floor((2 * text.length) / 3) },
    ];
    const items = storedItems(detail, null, new Set(), (id) => (id === runId ? { status: "done", marks } : null));
    expect(kinds(items)).toEqual(["user", "assistant"]);
    const answer = items[1];
    const segments = answer?.type === "entry" ? (answer.segments ?? []) : [];
    const order = segments.map((segment) => (segment.type === "version" ? `v${segment.version}` : "text"));
    // Text, the first version, text, the second version, and the column ends with text.
    expect(order.filter((part) => part !== "text")).toEqual(["v1", "v2"]);
    expect(order.at(-1)).toBe("text");
    expect(order.indexOf("v1")).toBeGreaterThan(0);
  });

  it("never ends the column with the versions when the window does not remember the run", () => {
    const items = storedItems(detail, null, new Set(), () => null);
    expect(kinds(items)).toEqual(["user", "v1", "v2", "assistant"]);
  });

  it("says that a run was stopped, from what the window remembers of it", () => {
    const items = storedItems(detail, null, new Set(), (id) => (id === runId ? { status: "cancelled", marks: [] } : null));
    expect(kinds(items).at(-1)).toBe("stopped");
    const stopped = items.at(-1);
    expect(stopped?.type === "stopped" ? stopped.at : null).toBe(2);
  });
});
