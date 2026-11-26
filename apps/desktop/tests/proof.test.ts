// Proof, sizes and the version comparison, on the engine's recorded results of the cable
// box demo (the scripted agent's v1 with thin walls, and v2). The example
// meets its 120 x 60 mm inside size (the screw post stands outside an end wall), so a
// requirement that is not met is made from v2 by `unmetRoom`.
import { describe, expect, it } from "vitest";

import type { EngineResult, ProjectDetail, Version } from "@/lib/core";
import { countsText, proofOf, statusLine, verdictKind } from "@/hotend/proof";
import { percentOf, sizesOf, snapped, stepOf } from "@/hotend/sizes";
import { railDots } from "@/hotend/VersionRail";
import { diffVersions, dotName, markerLines, versionLabel } from "@/hotend/versions";

import detailJson from "./fixtures/cable_box_detail.json";
import v1Json from "./fixtures/cable_box_v1.json";
import v2Json from "./fixtures/cable_box_v2.json";

const v1 = v1Json as unknown as { version: Version; result: EngineResult };
const v2 = v2Json as unknown as { version: Version; result: EngineResult };
const detail = detailJson as unknown as ProjectDetail;

/** The room requirement as the engine reports it when something stands in the space. */
const unmetRoom = (result: EngineResult): EngineResult => ({
  ...result,
  checks: (result.checks ?? []).map((check) =>
    check.id === "contract" && (check.data as { requirement?: string }).requirement === "holds_power_strip"
      ? {
          ...check,
          message:
            "Requirement 'holds_power_strip' (guessed) is NOT met: no 120 x 60 x 36 mm cuboid fits inside 'box' (something stands in the free space); with the other two sizes kept, at most about 114.0 mm along X, 54.0 mm along Y fits.",
          data: { ...(check.data as object), met: false, fits_at_most: { x: 114.0024, y: 54.0024, z: 0 } } as never,
        }
      : check,
  ),
});

describe("the Proof list", () => {
  it("counts the lines of the cable box by what there is to do about them, never as a fraction", () => {
    const proof = proofOf(v2.result);
    expect([proof.passed, proof.shown]).toEqual([10, 15]);
    // "10 of 15" read as five failures. The power strip fits as built; its size is a guess.
    expect(proof.counts).toEqual({ problem: 0, pass: 10, measure: 2, note: 3 });
    expect(countsText(proof.counts)).toBe("10 pass · 2 to measure · 3 notes");
    expect(proof.groups.map((group) => countsText(group.counts))).toEqual([
      "2 pass · 2 to measure · 1 note",
      "3 pass · 1 note",
      "5 pass · 1 note",
    ]);
    expect(proof.toMeasure).toBe(3);
    expect(proof.notes).toBe(3);
    expect(proof.guessedLabels).toEqual(["Cable slot", "Inside depth", "Inside width"]);
    expect(proof.groups.map((group) => group.id)).toEqual(["must", "fit", "prints"]);
  });

  it("calls the example's room inside a guess to measure: it is met as built", () => {
    const proof = proofOf(v2.result);
    const room = proof.groups[0]?.lines.find((entry) => entry.key === "contract:holds_power_strip");
    expect(room).toMatchObject({ status: "warn", unmet: false, icon: "question" });
    expect(room?.summary).toBe("A 120 × 60 × 36 mm block fits inside the box.");
    expect(proof.problems).toBe(0);
    expect(proof.unmet).toEqual([]);
    expect(verdictKind(proof, true)).toBe("measure");
    expect(statusLine(proof)).toEqual({ kind: "measure", text: "3 to measure" });
  });

  it("calls a requirement that is not met a problem, whoever the number came from", () => {
    const proof = proofOf(unmetRoom(v2.result));
    const line = proof.groups[0]?.lines[0];
    // The engine marks it "warn" because the size is a guess; the part still does not fit.
    expect(line).toMatchObject({ key: "contract:holds_power_strip", status: "warn", unmet: true, icon: "problem" });
    expect(line?.summary).toBe("A 120 × 60 × 36 mm block does not fit inside the box: something stands in the way, and only about 114 × 54 mm is free.");
    expect(line?.fixed).toBe("Does not fit as built. Measuring will not fix this; ask Hotend to make room.");
    expect(line?.verdict).toBe("The box is too small inside: 114 × 54 mm free, 120 × 60 mm needed");
    expect(proof.problems).toBe(1);
    expect(proof.firstProblem?.key).toBe("contract:holds_power_strip");
    expect(proof.unmet.map((entry) => entry.key)).toEqual(["contract:holds_power_strip"]);
    // It is not what the engine failed: Prepare stays open, and says so before a print.
    expect(proof.blocking).toBe(0);
    expect(verdictKind(proof, true)).toBe("problems");
    expect(statusLine(proof)).toEqual({ kind: "problems", text: "1 problem" });
    // The same requirement, met (the recorded v2): a guess to measure, as before.
    const fits = proofOf(v2.result);
    expect(fits.problems).toBe(0);
    expect(fits.groups[0]?.lines.find((entry) => entry.key === "contract:holds_power_strip")).toMatchObject({ unmet: false, icon: "question" });
    expect(verdictKind(fits, true)).toBe("measure");
    expect(statusLine(fits)).toEqual({ kind: "measure", text: "3 to measure" });
  });

  it("says a thin wall once, in one plain sentence, with the place it is at", () => {
    const proof = proofOf(v1.result);
    const lines = proof.groups.flatMap((group) => group.lines);
    // The requirement and the print check measured the same wall: one line, one problem.
    expect(lines.filter((line) => line.status === "fail").map((line) => line.key)).toEqual(["contract:walls_print"]);
    expect(lines.some((line) => line.key === "min_wall")).toBe(false);
    const wall = lines.find((line) => line.key === "contract:walls_print");
    expect(wall?.summary).toBe("A wall of the box is 0.6 mm thick. This printer needs 0.8 mm.");
    expect(wall?.spot).toEqual({ part: "box", at: [-9.1902, 31, 32.8061], label: "0.6 mm" });
    expect(proof.blocking).toBe(1);
    // A thin wall nothing in the contract speaks of keeps its own line under "How it prints".
    const alone = proofOf({ ...v1.result, checks: (v1.result.checks ?? []).filter((check) => check.id !== "contract") });
    const print = alone.groups.flatMap((group) => group.lines).find((line) => line.key === "min_wall");
    expect(print).toMatchObject({ status: "fail", summary: "A wall of the box is 0.6 mm thick. This printer needs 0.8 mm." });
    expect(print?.spot?.label).toBe("0.6 mm");
  });

  it("has no line for a contract the model did not write", () => {
    const none: EngineResult = {
      ...v2.result,
      checks: [
        ...(v2.result.checks ?? []).filter((check) => check.id !== "contract"),
        { id: "contract", status: "skipped", part: null, message: "Skipped: the model declares no CONTRACT.", data: {} } as never,
      ],
    };
    const proof = proofOf(none);
    expect(proof.hasContract).toBe(false);
    expect(proof.groups[0]).toMatchObject({ id: "must", lines: [], shown: 0 });
    expect(JSON.stringify(proof)).not.toContain("declares no CONTRACT");
  });

  it("puts the problems of the first version first", () => {
    // The thin wall is v1's one problem; its room inside is met.
    const proof = proofOf(v1.result);
    expect(proof.problems).toBe(1);
    expect(proof.firstProblem?.status).toBe("fail");
    expect(verdictKind(proof, true)).toBe("problems");
    expect(statusLine(proof).text).toBe("1 problem");
    const both = proofOf(unmetRoom(v1.result));
    expect(both.problems).toBe(2);
    expect(both.firstProblem?.status).toBe("fail");
    expect(statusLine(both).text).toBe("2 problems");
    expect(proofOf(undefined).shown).toBe(0);
  });
});

describe("Sizes", () => {
  it("groups what to measure, what to choose and what is fixed, with ranges and units", () => {
    const sizes = sizesOf(v2.result);
    const named = (group: string) => sizes.filter((size) => size.group === group).map((size) => size.name);
    // In the order of Hotend's questions (the power strip first, then the cable), not of
    // the alphabet: the first open row is what Hotend asked about first.
    expect(named("measure")).toEqual(["width", "depth", "cable"]);
    // Which side of the box the number is: the row names it, so that two sizes of one
    // cavity can be told apart.
    expect(sizes.find((size) => size.name === "width")?.ask).toEqual({ kind: "inside", part: "box", axis: 0, side: "long" });
    expect(sizes.find((size) => size.name === "depth")?.ask).toMatchObject({ kind: "inside", part: "box", axis: 1, side: "short" });
    expect(sizes.find((size) => size.name === "cable")?.ask).toEqual({ kind: "opening", part: "box", axis: null, side: null });
    expect(sizes.find((size) => size.name === "height")?.ask).toBeNull();
    expect(named("choices")).toEqual(["height", "wall"]);
    const height = sizes.find((size) => size.name === "height");
    expect(height).toMatchObject({ label: "Height", unit: "mm", value: 40, min: 26, max: 80 });
    const wall = sizes.find((size) => size.name === "wall");
    expect(wall).toMatchObject({ value: 2, min: 1.6, max: 3 });
  });

  it("steps and snaps inside the range", () => {
    const wall = { unit: "mm", min: 1.6, max: 3 };
    expect(stepOf(wall)).toBeLessThanOrEqual(0.1);
    expect(snapped(9, wall)).toBe(3);
    expect(snapped(0, wall)).toBe(1.6);
    expect(percentOf(40, 26, 80)).toBeCloseTo(25.93, 1);
    expect(percentOf(5, 5, 5)).toBe(0);
  });
});

describe("the version comparison", () => {
  const before = { version: v1.version, result: v1.result };
  const after = { version: v2.version, result: v2.result };

  it("says which sizes, parts and proof lines changed between v1 and v2", () => {
    const diff = diffVersions(before, after);
    expect(diff.sizes).toEqual([{ name: "wall", label: "Wall thickness", unit: "mm", from: 1, to: 2 }]);
    expect(diff.parts.map((part) => part.name)).toEqual(["box", "lid"]);
    // The screw post stands outside one end wall, so the box is longer than its inside plus walls.
    expect(diff.parts[0]?.size).toBe("129 × 62.5 × 40 → 130 × 64 × 40 mm");
    expect(diff.parts[0]?.mass).toBe("38 → 55 g");
    expect(diff.nowFails).toEqual([]);
    // One wall, said once (the print check of the same wall is folded into the requirement).
    expect(diff.nowPasses.sort()).toEqual(["Wall of the box"]);
    expect(diff.sameProof).toBeNull();
  });

  it("sums up what did not change instead of listing it", () => {
    const same = diffVersions(after, after);
    expect(same.sizes).toEqual([]);
    expect(same.parts).toEqual([]);
    expect(same.sameProof).toBe("Same proof: 10 pass · 2 to measure · 3 notes.");
    expect(diffVersions(null, before)).toEqual({ sizes: [], parts: [], nowPasses: [], nowFails: [], sameProof: null });
  });

  it("labels versions and writes the lines under a marker", () => {
    expect(versionLabel(v2.version, v1.version, v2.result)).toBe("v2 · Hotend");
    expect(markerLines(null, before)).toEqual(["1 problem"]);
    expect(markerLines(before, after)).toEqual(["Wall thickness 1 → 2 mm", "Now passes: Wall of the box"]);
    const height: Version = { ...v2.version, number: 3, parent: 2, origin: "set_parameter", params: { ...v2.version.params, height: 46 } };
    expect(versionLabel(height, v2.version, v2.result)).toBe("v3 · Height set to 46 mm");
    expect(markerLines(after, { version: height, result: v2.result })).toEqual(["Same proof: 10 pass · 2 to measure · 3 notes."]);
    expect(dotName(v1.version, v1.result)).toBe("Version 1, written by Hotend, has 1 problem");
    expect(dotName(height, unmetRoom(v2.result))).toBe("Version 3, one size changed, has 1 problem");
  });

  it("collapses a run of changes to one size into its last dot", () => {
    const [first, second] = detail.versions as [Version, Version];
    const step = (number: number, parent: number, params: Record<string, number>): Version => ({
      ...second,
      number,
      parent,
      origin: "set_parameter",
      params: { ...second.params, ...params },
    });
    const versions = [
      first,
      second,
      step(3, 2, { height: 44 }),
      step(4, 3, { height: 46 }),
      step(5, 4, { height: 48 }),
      step(6, 5, { height: 48, wall: 2.4 }),
    ];
    expect(railDots(versions).map((dot) => [dot.version.number, dot.run])).toEqual([
      [1, 1],
      [2, 1],
      [5, 3],
      [6, 1],
    ]);
  });
});
