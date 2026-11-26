// A fake core that holds the recorded cable box project (the scripted agent's v1 and v2),
// for tests of the store and of components that need an open project.
import { vi } from "vitest";

import type { CoreClient, EngineResult, ProjectDetail, Version } from "@/lib/core";

import { fakeClient, type FakeClient } from "./fake-client";
import detailJson from "./fixtures/cable_box_detail.json";
import v1Json from "./fixtures/cable_box_v1.json";
import v2Json from "./fixtures/cable_box_v2.json";

export const v1 = v1Json as unknown as { version: Version; result: EngineResult };
export const v2 = v2Json as unknown as { version: Version; result: EngineResult };
export const detail = detailJson as unknown as ProjectDetail;
export const PROJECT = detail.project.id;

export const settled = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A core with the cable box in it. `answers` replaces or adds commands. */
export function cableBoxClient(answers: Partial<CoreClient> = {}): FakeClient {
  let versions = [...detail.versions];
  let current = detail.project.current_version;
  const results = new Map<number, EngineResult>([
    [1, v1.result],
    [2, v2.result],
  ]);
  return fakeClient({
    projects_list: vi.fn(() => Promise.resolve([{ ...detail.project, current_version: current }])),
    project_get: vi.fn(() =>
      Promise.resolve({ ...detail, project: { ...detail.project, current_version: current }, versions }),
    ),
    version_get: vi.fn((_project: string, number: number) => {
      const version = versions.find((entry) => entry.number === number);
      const result = results.get(number);
      return version && result
        ? Promise.resolve({ version, result, dir: "/data" })
        : Promise.reject(new Error(`no version ${number}`));
    }),
    version_mesh: vi.fn(() => Promise.reject(new Error("no mesh in this test"))),
    preparation_get: vi.fn(() => Promise.resolve(null)),
    slice_get: vi.fn(() => Promise.resolve(null)),
    print_jobs: vi.fn(() => Promise.resolve([])),
    printers_list: vi.fn(() => Promise.resolve([])),
    settings_get: vi.fn(() =>
      Promise.resolve({
        provider: { kind: "ollama" as const, model: "scripted" },
        printer_id: "bambu-x2d",
        material: { id: "pla-basic", family: "PLA" },
        max_turns: 24,
        slicer_executables: {},
        plate_type: null,
        nozzle_diameter_mm: null,
        spool: { slot: "1", colour: null, silk: false },
      }),
    ),
    // One changed size makes the next version, as the core does.
    parameter_set: vi.fn((_project: string, name: string, value: number) => {
      const parent = versions.find((entry) => entry.number === current);
      if (!parent) return Promise.reject(new Error("no current version"));
      const number = Math.max(...versions.map((entry) => entry.number)) + 1;
      const version: Version = { ...parent, number, parent: parent.number, origin: "set_parameter", params: { ...parent.params, [name]: value } };
      const result = results.get(parent.number) ?? v2.result;
      versions = [...versions, version];
      results.set(number, { ...result, params: { ...result.params, [name]: value } });
      current = number;
      return Promise.resolve({ version, result: results.get(number) ?? result });
    }),
    version_revert: vi.fn((_project: string, number: number) => {
      current = number;
      const version = versions.find((entry) => entry.number === number);
      return version ? Promise.resolve(version) : Promise.reject(new Error("no such version"));
    }),
    ...answers,
  });
}
