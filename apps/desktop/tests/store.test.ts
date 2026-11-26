import { describe, expect, it, vi } from "vitest";

import type { ApiError, BuildReport, Project, ShellEvent } from "@/lib/core";
import { createHotendStore } from "@/hotend/store";

import { cableBoxClient, PROJECT, settled, v2 } from "./cable-box";
import { fakeClient } from "./fake-client";

const project = (current_version: number | null): Project => ({
  id: "p1",
  name: "Cable box",
  created_at_ms: 1,
  updated_at_ms: 2,
  current_version,
});

const run = (event: Extract<ShellEvent, { channel: "run" }>["event"], project_id = PROJECT): ShellEvent => ({
  channel: "run",
  run_id: "r1",
  project_id,
  event,
});

describe("the Hotend store", () => {
  it("lists projects and creates one", async () => {
    const projects_list = vi.fn().mockResolvedValueOnce([]).mockResolvedValueOnce([project(null)]);
    const project_create = vi.fn().mockResolvedValue(project(null));
    const print_jobs = vi.fn().mockResolvedValue([]);
    const store = createHotendStore(fakeClient({ projects_list, project_create, print_jobs }));

    expect(store.getState().projects).toBeNull();
    await store.getState().loadProjects();
    expect(store.getState().projects).toEqual([]);
    const made = await store.getState().createProject("Cable box");
    expect(made?.id).toBe("p1");
    expect(project_create).toHaveBeenCalledWith("Cable box");
  });

  it("keeps the core's own words when the project list cannot be read", async () => {
    const refusal: ApiError = { kind: "storage", message: "The folder is gone.", next_step: "Check the disk." };
    const store = createHotendStore(fakeClient({ projects_list: vi.fn().mockRejectedValue(refusal) }));
    await store.getState().loadProjects();
    expect(store.getState().projectsError).toEqual(refusal);
    expect(store.getState().projects).toBeNull();
  });

  it("opens a project with the result of every version", async () => {
    const store = createHotendStore(cableBoxClient());
    await store.getState().openProject(PROJECT);
    const state = store.getState();
    expect(state.project?.project.current_version).toBe(2);
    expect(Object.keys(state.results)).toEqual(["1", "2"]);
    // The mesh could not be read: the stage says so, the numbers still show.
    expect(state.meshes[2]).toBeNull();
  });

  it("rebuilds one size at a time and lets the latest value of a size win", async () => {
    let release: (report: BuildReport) => void = () => undefined;
    const client = cableBoxClient();
    const real = client.parameter_set;
    const parameter_set = vi.fn((projectId: string, name: string, value: number) =>
      parameter_set.mock.calls.length === 1
        ? new Promise<BuildReport>((resolve) => {
            release = resolve;
          })
        : real(projectId, name, value),
    );
    const store = createHotendStore({ ...client, parameter_set });
    await store.getState().openProject(PROJECT);

    store.getState().setSize("height", 44);
    expect(store.getState().rebuild).toMatchObject({ name: "height", label: "Height", unit: "mm", value: 44 });
    store.getState().setSize("height", 45);
    store.getState().setSize("height", 46);
    expect(store.getState().queued).toEqual({ height: 46 });
    expect(parameter_set).toHaveBeenCalledTimes(1);

    release(await real(PROJECT, "height", 44));
    await settled();
    await settled();
    await settled();
    // 45 was never built: only the first value and the last.
    expect(parameter_set.mock.calls.map((call) => call[2])).toEqual([44, 46]);
    expect(store.getState().rebuild).toBeNull();
    expect(store.getState().project?.project.current_version).toBe(4);
    expect(store.getState().project?.versions.at(-1)?.params.height).toBe(46);
  });

  it("goes back to the last good value when a size does not build, and says why", async () => {
    const refusedReport: BuildReport = {
      version: null,
      result: { ...v2.result, status: "model_error", error: { type: "ValueError", message: "The lid no longer fits.", line: 3, traceback: "" } },
    };
    const store = createHotendStore(cableBoxClient({ parameter_set: vi.fn().mockResolvedValue(refusedReport) }));
    await store.getState().openProject(PROJECT);
    store.getState().setSize("height", 80);
    await settled();
    expect(store.getState().refused).toEqual({
      name: "height",
      label: "Height",
      unit: "mm",
      value: 80,
      still: 40,
      reason: "The lid no longer fits.",
    });
    expect(store.getState().rebuild).toBeNull();
    expect(store.getState().project?.project.current_version).toBe(2);
  });

  it("undoes to the version a version was made from, and goes forward again", async () => {
    const client = cableBoxClient();
    const store = createHotendStore(client);
    await store.getState().openProject(PROJECT);
    await store.getState().undo();
    expect(client.version_revert).toHaveBeenCalledWith(PROJECT, 1);
    expect(store.getState().project?.project.current_version).toBe(1);
    expect(store.getState().redo).toEqual([2]);
    await store.getState().redoNext();
    expect(store.getState().project?.project.current_version).toBe(2);
    expect(store.getState().redo).toEqual([]);
  });

  it("streams a run of the open project and ignores another project's", async () => {
    const client = cableBoxClient({ message_send: vi.fn().mockResolvedValue({ run_id: "r1" }) });
    const store = createHotendStore(client);
    await store.getState().openProject(PROJECT);
    await store.getState().sendMessage("Make it taller");
    expect(store.getState().sent).toBe("Make it taller");

    store.getState().receive(run({ kind: "agent", event: { type: "text_delta", text: "Sure." } }, "another"));
    expect(store.getState().run?.text).toBe("");
    store.getState().receive(run({ kind: "agent", event: { type: "text_delta", text: "Sure." } }));
    store.getState().receive(run({ kind: "finished", stop_reason: "done", error: null }));
    await settled();
    expect(store.getState().run).toMatchObject({ status: "done", text: "Sure." });
  });

  it("keeps the message and shows the core's refusal when a message cannot start", async () => {
    const refusal: ApiError = { kind: "not_configured", message: "No AI provider is chosen.", next_step: "Open Settings." };
    const store = createHotendStore(cableBoxClient({ message_send: vi.fn().mockRejectedValue(refusal) }));
    await store.getState().openProject(PROJECT);
    await store.getState().sendMessage("Hello");
    expect(store.getState().sendError).toEqual(refusal);
    expect(store.getState().lastMessage).toBe("Hello");
    expect(store.getState().run).toBeNull();
  });
});
