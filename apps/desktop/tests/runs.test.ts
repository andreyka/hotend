// A run goes on while the maker is somewhere else (C4): on Home, in another project, or
// while the window loads again. The window keeps it by project and takes it up again.
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ProjectDetail, ShellEvent, TranscriptEntry } from "@/lib/core";
import { finishedSteps, liveRunOf, noteLiveRun, providerFailure, rememberedRun, rememberRun, startRun } from "@/hotend/run";
import { createHotendStore } from "@/hotend/store";

import { cableBoxClient, detail, PROJECT, settled } from "./cable-box";

const RUN = "run-1";

const event = (inner: Extract<ShellEvent, { channel: "run" }>["event"], project_id = PROJECT, run_id = RUN): ShellEvent => ({
  channel: "run",
  run_id,
  project_id,
  event: inner,
});
const agent = (inner: Record<string, unknown>) => ({ kind: "agent" as const, event: inner as never });
const said = (text: string) => event(agent({ type: "text_delta", text }));
const finished = event({ kind: "finished", stop_reason: "done", error: null });

/** The cable box with the maker's new message stored and no answer yet: a run is going. */
function asked(): ProjectDetail {
  const entry: TranscriptEntry = { id: 99, run_id: RUN, role: "user", text: "Make it taller.", at_ms: Date.now(), remembered: true };
  return { ...detail, transcript: [...detail.transcript, entry] };
}

beforeEach(() => {
  window.localStorage.clear();
  window.sessionStorage.clear();
});

describe("a run that goes on while the maker is somewhere else", () => {
  it("is still working, with what it wrote, after Home and back", async () => {
    const message_send = vi.fn().mockResolvedValue({ run_id: RUN });
    const store = createHotendStore(cableBoxClient({ message_send }));
    await store.getState().openProject(PROJECT);
    await store.getState().sendMessage("Make it taller.");
    store.getState().receive(said("Making it "));
    expect(store.getState().run?.status).toBe("running");

    // Home: the project closes, the run does not.
    store.getState().closeProject();
    expect(store.getState().run).toBeNull();
    expect(store.getState().runs[PROJECT]?.status).toBe("running");
    store.getState().receive(said("taller."));
    expect(store.getState().runs[PROJECT]?.text).toBe("Making it taller.");

    // And back: the same run, working, with all of its text, and Stop stops it.
    await store.getState().openProject(PROJECT);
    expect(store.getState().run).toMatchObject({ runId: RUN, status: "running", text: "Making it taller." });
  });

  it("ends in the background and is then read from the stored conversation", async () => {
    const message_send = vi.fn().mockResolvedValue({ run_id: RUN });
    const client = cableBoxClient({ message_send });
    const store = createHotendStore(client);
    await store.getState().loadProjects();
    await store.getState().openProject(PROJECT);
    await store.getState().sendMessage("Make it taller.");
    store.getState().closeProject();
    store.getState().receive(finished);
    await settled();
    expect(store.getState().runs[PROJECT]?.status).toBe("done");
    expect(liveRunOf(PROJECT)).toBeNull();
    expect(rememberedRun(RUN)?.status).toBe("done");
    // Back in the project there is no live run: what Hotend said is stored.
    await store.getState().openProject(PROJECT);
    expect(store.getState().run).toBeNull();
  });

  it("is taken up again after the window was loaded again", async () => {
    // What the window left behind before it was loaded again.
    noteLiveRun(PROJECT, RUN);
    rememberRun({
      ...startRun(RUN, PROJECT),
      text: "First the box.",
      tools: [{ id: "c1", name: "write_model_code", note: null, state: "ok", after: { version: 3 } }],
      marks: [{ version: 3, offset: 14 }],
    });
    const run_cancel = vi.fn().mockResolvedValue(true);
    const store = createHotendStore(cableBoxClient({ project_get: vi.fn(() => Promise.resolve(asked())), run_cancel }));
    await store.getState().openProject(PROJECT);
    const run = store.getState().run;
    expect(run).toMatchObject({ runId: RUN, status: "running", resumed: true, offset: 14 });
    // The steps it had finished are still counted, in the words they were written in.
    expect(finishedSteps(run ?? startRun("", "")).map((step) => step.text)).toEqual(["Wrote the model · v3"]);
    // The message can be sent again from an error card.
    expect(store.getState().lastMessage).toBe("Make it taller.");
    // Stop reaches the core with the run's id.
    await store.getState().cancelRun();
    expect(run_cancel).toHaveBeenCalledWith(RUN);
    expect(store.getState().run?.stopping).toBe(true);
    // Its end clears the live run: the stored conversation has the whole text.
    store.getState().receive(event({ kind: "finished", stop_reason: "cancelled", error: null }));
    await settled();
    await settled();
    expect(store.getState().run).toBeNull();
    expect(rememberedRun(RUN)?.status).toBe("cancelled");
    expect(liveRunOf(PROJECT)).toBeNull();
  });

  it("is not taken up when the run has an answer, an end, or was another session's", async () => {
    const answered: ProjectDetail = {
      ...asked(),
      transcript: [...asked().transcript, { id: 100, run_id: RUN, role: "assistant", text: "Done.", at_ms: Date.now(), remembered: true }],
    };
    noteLiveRun(PROJECT, RUN);
    let store = createHotendStore(cableBoxClient({ project_get: vi.fn(() => Promise.resolve(answered)) }));
    await store.getState().openProject(PROJECT);
    expect(store.getState().run).toBeNull();
    expect(liveRunOf(PROJECT)).toBeNull();

    noteLiveRun(PROJECT, RUN);
    rememberRun({ ...startRun(RUN, PROJECT), status: "error" });
    store = createHotendStore(cableBoxClient({ project_get: vi.fn(() => Promise.resolve(asked())) }));
    await store.getState().openProject(PROJECT);
    expect(store.getState().run).toBeNull();

    // No run of this session: a message without an answer is an old run that failed.
    window.sessionStorage.clear();
    store = createHotendStore(cableBoxClient({ project_get: vi.fn(() => Promise.resolve(asked())) }));
    await store.getState().openProject(PROJECT);
    expect(store.getState().run).toBeNull();
  });

  it("drops a run it took up when the core says no such run is going", async () => {
    noteLiveRun(PROJECT, RUN);
    const project_get = vi.fn(() => Promise.resolve(asked()));
    const store = createHotendStore(cableBoxClient({ project_get, run_cancel: vi.fn().mockResolvedValue(false) }));
    await store.getState().openProject(PROJECT);
    expect(store.getState().run?.status).toBe("running");
    await store.getState().cancelRun();
    expect(store.getState().run).toBeNull();
    expect(store.getState().runs[PROJECT]).toBeUndefined();
  });

  it("says on Home that Hotend is working in a project", async () => {
    noteLiveRun(PROJECT, RUN);
    const store = createHotendStore(cableBoxClient({ project_get: vi.fn(() => Promise.resolve(asked())) }));
    await store.getState().loadProjects();
    expect(store.getState().runs[PROJECT]?.status).toBe("running");
  });
});

describe("a run that could not reach the AI", () => {
  it("is told apart by the core's code, or by what the provider answered", () => {
    expect(providerFailure("key_rejected", "")).toBe("key_rejected");
    expect(providerFailure("agent_error", '401 {"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}')).toBe("key_rejected");
    expect(providerFailure("agent_error", "429 rate_limit_error: This request would exceed your rate limit")).toBe("rate_limited");
    expect(providerFailure("agent_error", "Your credit balance is too low to access the API.")).toBe("rate_limited");
    expect(providerFailure("upstream_error", "error sending request for url (https://api.example.com/v1/messages)")).toBe("provider_unreachable");
    expect(providerFailure("agent_error", "fetch failed")).toBe("provider_unreachable");
    expect(providerFailure("agent_error", '404 {"type":"error","error":{"type":"not_found_error","message":"model: claude-nope"}}')).toBe("model_not_found");
    // Anything else stays general, and a code that means something else is left alone.
    expect(providerFailure("agent_error", "The model returned an empty answer.")).toBeNull();
    expect(providerFailure("sandbox_unavailable", "401")).toBeNull();
  });
});
