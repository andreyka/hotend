// A placeholder of the stand-in slicer states no time, layers or grams in its file (the
// stricter stand-in claims nothing it does not do). The review shows the stand-in's rough
// estimates from the slice record kept for that very file instead, as the confirmation
// dialog does; any other file is shown exactly as it states itself.
import { describe, expect, it } from "vitest";

import type { PrintReview, SliceOutcome } from "@/lib/core";
import { shownSummary } from "@/hotend/inspector/PrintPanel";

const SHA = "ab".repeat(32);

function review(): PrintReview {
  return {
    summary: {
      file_name: "plate_1.gcode.3mf",
      sha256: SHA,
      size_bytes: 2084,
      plate: 1,
      estimated_seconds: 0,
      layers: null,
      filaments: [
        { slot: 1, material: "PLA", colour: null, grams: 0, metres: 0, nozzle_id: 1, selected_in_gcode: false },
      ],
      max_nozzle_c: null,
      max_bed_c: null,
      max_chamber_c: null,
      extent: null,
    },
  } as unknown as PrintReview;
}

function slice(sha256: string, warning = "fake_slicer"): SliceOutcome {
  return {
    summary: {
      sha256: sha256.toUpperCase(),
      time_s: 3779.6,
      layers: 37,
      filament: [{ slot: 1, grams: 28.25, metres: 9.4, nozzle_id: 1 }],
      warnings: [{ code: warning, message: "" }],
    },
  } as unknown as SliceOutcome;
}

describe("the numbers of a stand-in review", () => {
  it("come from the slice record of that very file", () => {
    const shown = shownSummary(review(), true, [null, slice(SHA)]);
    expect(shown.estimated_seconds).toBe(3780);
    expect(shown.layers).toBe(37);
    expect(shown.filaments[0]?.grams).toBe(28.25);
    // Everything the file states that is not an estimate stays the file's.
    expect(shown.sha256).toBe(SHA);
    expect(shown.max_nozzle_c).toBeNull();
  });

  it("are the file's own when the record is of another file", () => {
    expect(shownSummary(review(), true, [slice("cd".repeat(32))])).toEqual(review().summary);
  });

  it("are the file's own when the record of that file is not the stand-in's", () => {
    expect(shownSummary(review(), true, [slice(SHA, "slicer_version")])).toEqual(review().summary);
  });

  it("are the file's own for a file that is not a placeholder", () => {
    expect(shownSummary(review(), false, [slice(SHA)])).toEqual(review().summary);
  });
});
