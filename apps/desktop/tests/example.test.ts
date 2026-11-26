// The example the window ships is the model the engine's own tests build.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(fileURLToPath(new URL(path, import.meta.url)), "utf8");

describe("the bundled example", () => {
  it("is the engine's cable box model, unchanged", () => {
    expect(read("../src/hotend/data/cable_box.py")).toBe(read("../../../python/tests/models/cable_box.py"));
  });
});
