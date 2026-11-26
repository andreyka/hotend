// More regression tests: what was once broken, written down as what must hold. Each test
// names the defect it stands for. Proof, runs and the number
// field have files of their own (proof.test.ts, runs.test.ts, number-field.test.tsx).
import { describe, expect, it } from "vitest";

import type { EngineResult, PrintJob, ProjectDetail, SliceOutcome, Value } from "@/lib/core";
import { nearColour } from "@/hotend/Ams";
import { calibrationState, sharedEnding } from "@/hotend/calibration";
import { duration, partWords } from "@/hotend/format";
import { cardWords } from "@/hotend/proof";
import { createHotendStore, printedVersion } from "@/hotend/store";
import { S } from "@/hotend/strings";
import { designVerdict, printVerdict, stageMarks, type VerdictInput } from "@/hotend/verdict";
import { versionLabel } from "@/hotend/versions";

import durations from "../src-tauri/tests/fixtures/durations.json";
import { cableBoxClient, detail, PROJECT, settled, v1, v2 } from "./cable-box";

async function input(change: Partial<VerdictInput> = {}): Promise<VerdictInput> {
  const store = createHotendStore(cableBoxClient());
  await store.getState().openProject(PROJECT);
  return { ...store.getState(), canTalk: true, now: 1_000_000, ...change };
}

const slice = (version: number, sha: string, codes: string[] = []): SliceOutcome =>
  ({
    record: { project_id: PROJECT, version, profile_hash: "h", profile: "demo-0.20mm", summary: null, job: { plate: 1, printer_id: "bambu-x2d", material: { id: "pla-basic", family: "PLA" }, pieces: ["box", "lid"], prepared_at_ms: 1 }, created_at_ms: 9e15 },
    summary: {
      status: "ok",
      output: "plate.gcode",
      sha256: sha,
      time_s: 3600,
      filament: [{ slot: 0, grams: 28.3, material: "PLA" }],
      layers: 200,
      warnings: codes.map((code) => ({ code, message: code })),
      slicer: { name: "s", version: "1" },
    },
    preparation: null,
    output_path: "/tmp/plate.gcode",
  }) as unknown as SliceOutcome;

const job = (status: string, version: number, simulated = true): PrintJob =>
  ({
    id: "j1",
    project_id: PROJECT,
    version,
    slice_sha256: "sha-of-the-job",
    note: "",
    status,
    delivery: "simulated_printer",
    created_at_ms: 1,
    updated_at_ms: 2,
    detail: { simulated, phase: "printing", headline: null, progress_percent: 52, remaining_minutes: 102, message: "The simulated print cancelled; nothing was printed.", problems: [], findings: [], link: null, path: null, windows_path: null, opened: null, opener: null },
  }) as PrintJob;

describe("the Print stage while the design moves on (the plate was empty at 52 %)", () => {
  it("shows the version that is printed, not the current one", () => {
    expect(printedVersion(job("printing", 1), 2)).toBe(1);
    expect(printedVersion(job("finished", 1), 2)).toBe(1);
    // Without a job the stage shows the current version.
    expect(printedVersion(null, 2)).toBe(2);
  });

  it("reads the layout, the mesh and the slice of the job's own version", async () => {
    const asked: string[] = [];
    const client = cableBoxClient({
      print_jobs: () => Promise.resolve([job("printing", 1)]),
      preparation_get: (_project, version) => {
        asked.push(`preparation ${version}`);
        return Promise.resolve({ version, plates: [{ index: 1, size: [256, 256], items: [] }], unplaced: [], checks: [], orientations: [], warnings: [] } as never);
      },
      slice_get: (_project, version) => {
        asked.push(`slice ${String(version)}`);
        return Promise.resolve(version === 1 ? slice(1, "sha-of-the-job") : null);
      },
      version_mesh: (_project, version) => {
        asked.push(`mesh ${version}`);
        return Promise.resolve("{}");
      },
    });
    const store = createHotendStore(client);
    await store.getState().openProject(PROJECT);
    await settled();
    const state = store.getState();
    // The cable box is at v2; the job is of v1.
    expect(state.project?.project.current_version).toBe(2);
    expect(state.jobPreparation?.version).toBe(1);
    expect(state.jobSlice?.summary.sha256).toBe("sha-of-the-job");
    expect(asked).toContain("preparation 1");
    expect(asked).toContain("mesh 1");
    expect(state.meshes[1]).toBe("{}");
  });

  it("says which version is printing, in words", () => {
    expect(S.print.jobVersion.printing(3, 6)).toBe("Printing v3. Your design is now v6.");
  });
});

describe("the Print stage of a project that cannot be printed (it said 'Slice this version first')", () => {
  it("says there is nothing to print when nothing was built", async () => {
    const empty = { ...detail, project: { ...detail.project, current_version: null }, versions: [] } as ProjectDetail;
    const verdict = printVerdict(await input({ project: empty, results: {} }));
    expect(verdict).toMatchObject({ sentence: "Nothing to print yet.", primary: { label: "Go to Design", action: { do: "go", stage: "design" } } });
  });

  it("sends the maker to Design when the part has a problem that blocks Prepare", async () => {
    const broken = { ...detail, project: { ...detail.project, current_version: 1 } } as ProjectDetail;
    const verdict = printVerdict(await input({ project: broken, results: { 1: v1.result }, slice: null }));
    expect(verdict).toMatchObject({ icon: "problem", sentence: "Fix 1 problem in Design first.", primary: { label: "Go to Design" } });
  });

  it("asks for a slice only when there is something to slice", async () => {
    const verdict = printVerdict(await input({ slice: null }));
    expect(verdict).toMatchObject({ sentence: "Slice this version first.", primary: { label: "Go to Prepare" } });
  });
});

describe("a stopped print (two 'Review again' buttons, no icon, the core's sentence)", () => {
  it("has the flask in the verdict and one button", async () => {
    const stopped = await input({ slice: slice(2, "sha-of-the-job", ["fake_slicer"]), jobs: [job("cancelled", 2)] });
    expect(printVerdict(stopped)).toMatchObject({ icon: "sim", primary: { label: "Review again" } });
  });

  it("is said in the window's own sentence, with the layer when it is known", () => {
    expect(S.print.stoppedAt(true, 121, 52)).toBe("You stopped the simulated print at layer 121.");
    expect(S.print.stoppedAt(true, null, 52)).toBe("You stopped the simulated print at 52 %.");
    expect(S.print.stoppedAt(false, null, null)).toBe("You stopped the print.");
    expect(S.print.stoppedAt(true, 0, 0)).toBe("You stopped the simulated print before its first layer.");
  });
});

describe("the review (the '?' of a guess over a triangle in the tab)", () => {
  it("uses one mark in the verdict and in the tab: the flask for a stand-in file, none otherwise", async () => {
    const pending = { ...job("pending", 2), slice_sha256: "abc" };
    const review = { job: pending } as VerdictInput["review"];
    const standIn = await input({ slice: slice(2, "abc", ["fake_slicer"]), jobs: [pending], review });
    expect(printVerdict(standIn)?.icon).toBe("sim");
    expect(stageMarks(standIn).print).toBe("sim");
    const real = await input({ slice: slice(2, "abc"), jobs: [pending], review });
    expect(printVerdict(real)?.icon).toBe("none");
    expect(stageMarks(real).print).toBe("none");
  });
});

describe("one way to write a duration (review said 1 h 42 min, the running print 102 min)", () => {
  it("writes every case of the list it shares with the confirmation dialog", () => {
    // The shell's dialog (src-tauri/src/confirm.rs) is tested against the same file:
    // a person compares the dialog with the window's review before a print starts.
    expect(durations.cases.length).toBeGreaterThan(10);
    for (const [seconds, written] of durations.cases) expect(duration(Number(seconds)), `${String(seconds)} s`).toBe(written);
  });

  it("writes the time left like the slice time", () => {
    expect(S.print.left(duration(102 * 60))).toBe("About 1 h 42 min left");
    expect(S.print.left(duration(44 * 60))).toBe("About 44 min left");
    expect(S.chip.line(25, 102)).toContain("about 1 h 42 min left");
  });
});

describe("a size change and the problems of the part (the change was blamed for an old problem)", () => {
  it("offers to undo the change only when the change made the problem", async () => {
    // The recorded v2 meets its inside size: no problem, its guesses to measure.
    const base = await input();
    expect(designVerdict(base)?.primary?.label).toBe("Measure them");
    // A v2 whose box is too small inside.
    const small: EngineResult = {
      ...v2.result,
      checks: (v2.result.checks ?? []).map((check) =>
        check.id === "contract" && (check.data as { requirement?: string }).requirement === "holds_power_strip"
          ? { ...check, data: { ...(check.data as object), met: false, fits_at_most: { x: 114.0024, y: 54.0024, z: 0 } } as never }
          : check,
      ),
    };
    const sized = {
      ...detail,
      project: { ...detail.project, current_version: 3 },
      versions: [...detail.versions, { ...v2.version, number: 3, parent: 2, origin: "set_parameter", params: { ...v2.version.params, height: 46 } }],
    } as ProjectDetail;
    // The same result before and after: the size did not make the problem.
    const same = designVerdict({ ...base, project: sized, results: { ...base.results, 2: small, 3: small } });
    expect(same?.primary?.label).toBe("Ask Hotend to fix it");
    expect(same?.sentence).toBe("The box is too small inside: 114 × 54 mm free, 120 × 60 mm needed");
  });
});

describe("words a maker reads (raw part ids, 'screw boss', 'Effort')", () => {
  it("writes a part's name with spaces", () => {
    expect(partWords("tray_front_left_for_long_screws")).toBe("tray front left for long screws");
  });

  it("calls a screw boss a screw post", () => {
    expect(cardWords("screw_boss")).toBe("Screw post");
    expect(S.proof.card.pass("Screw post")).toBe("Screw post: goes together, gaps as drawn");
  });

  it("keeps a number and its unit together in the ideas on Home", () => {
    for (const idea of S.suggestions) expect(idea).not.toMatch(/\d mm/);
    expect(S.suggestions.join(" ")).toContain("300 mm");
  });
});

describe("the colour of a spool (a white one looked like a radio button)", () => {
  it("knows when a colour would vanish on the surface it lies on", () => {
    expect(nearColour("#FFFFFF", "#FFFFFF")).toBe(true);
    expect(nearColour("#F4F4F4", "#FFFFFF")).toBe(true);
    expect(nearColour("#1D2024", "#1D2024")).toBe(true);
    expect(nearColour("#D32F2F", "#FFFFFF")).toBe(false);
    expect(nearColour("#FFFFFF", "#1D2024")).toBe(false);
  });
});

describe("what was measured on this printer (Settings said 'recorded', the project 'not calibrated')", () => {
  const quantity = (provenance: string, at: string | null) => ({ value: 0.05, sigma: 0.02, provenance, measured_at: at });
  const table = (hole: string, at: string | null): Value =>
    ({
      protocol: 0,
      printer_id: "bambu-x2d",
      entries: [
        {
          material: "pla-basic",
          nozzle_id: 1,
          nozzle_diameter: 0.4,
          hole_offset: quantity(hole, at),
          peg_offset: quantity(hole, at),
          xy_shrink: quantity("guessed", null),
          z_shrink: quantity("guessed", null),
          min_clearance: quantity("guessed", null),
        },
      ],
    }) as unknown as Value;

  it("is nothing for a printer without a table", () => {
    expect(calibrationState(null)).toBeNull();
  });

  it("names what was measured, when, and what is still a general value", () => {
    const state = calibrationState(table("measured", "2026-11-18T04:38:29Z"));
    expect(state?.measured).toEqual(["hole_offset", "peg_offset"]);
    expect(state?.guessed).toEqual(["xy_shrink", "z_shrink", "min_clearance"]);
    expect(state?.at).toBe(Date.parse("2026-11-18T04:38:29Z"));
  });

  it("counts a table of general values as not measured", () => {
    expect(calibrationState(table("guessed", null))?.at).toBeNull();
  });

  it("is read when a project opens, so the Proof can say it", async () => {
    const store = createHotendStore(
      cableBoxClient({ calibration_get: () => Promise.resolve({ printer_id: "bambu-x2d", table: table("measured", "2026-11-18T04:38:29Z") }) }),
    );
    await store.getState().openProject(PROJECT);
    await settled();
    expect(store.getState().calibration?.measured).toEqual(["hole_offset", "peg_offset"]);
  });
});

describe("a version built again after a calibration", () => {
  it("is not called a changed size", () => {
    const again = { ...v2.version, number: 3, parent: 2, origin: "set_parameter" as const, params: { ...v2.version.params } };
    expect(versionLabel(again, v2.version, v2.result)).toBe("v3 · Built again with your printer's values");
  });
});

describe("the rows of the gauge (four rows ended with the same clause)", () => {
  it("says once what every row says alike", () => {
    const rows = [
      "the inside width of the opening between the two grooved walls, at the rim; measure above the first millimetre from the bed",
      "the inside diameter of the pilot hole in the post; measure above the first millimetre from the bed",
    ];
    expect(sharedEnding(rows)).toEqual({
      lines: ["the inside width of the opening between the two grooved walls, at the rim", "the inside diameter of the pilot hole in the post"],
      shared: "measure above the first millimetre from the bed",
    });
  });

  it("leaves the rows alone when they end differently, or when there is one", () => {
    const rows = ["a slot; measure at the rim", "a hole; measure at the bottom"];
    expect(sharedEnding(rows)).toEqual({ lines: rows, shared: null });
    expect(sharedEnding(["a slot; measure at the rim"])).toEqual({ lines: ["a slot; measure at the rim"], shared: null });
    expect(sharedEnding(["a slot", "a hole"])).toEqual({ lines: ["a slot", "a hole"], shared: null });
  });
});
