// "Turn on web research" next to an answer that would have looked something up with research
// off. The window learns it from the agent's structured marker (`research_needed` on a
// `text_delta` without text), never from the answer's words, and asks for the agreement in
// the shell's own dialog (`research_consent_ask`), as Settings does; with no usable source
// chosen it opens Settings, AI model, Web research.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, HostProvider, PendingContext } from "@/host/core-context";
import { agentEvent, type RunEvent } from "@/lib/core";
import type { ResearchNeeded } from "@/lib/core/agent-events";
import type { PendingCommands, SearchSource, SearchSourceOption } from "@/lib/core/pending";
import { Conversation, ResearchAction } from "@/hotend/Conversation";
import { HotendProvider, useHotend, useHotendStore } from "@/hotend/context";
import { reduceRun, rememberedRun, rememberRun, startRun } from "@/hotend/run";
import type { HotendStore } from "@/hotend/store";
import { S } from "@/hotend/strings";

import { cableBoxClient, PROJECT, settled } from "./cable-box";
import { fakeClient } from "./fake-client";
import { standInPending } from "./fake-pending";

afterEach(cleanup);

const LOOKUP = "the outline of the phone's camera plateau";

const option = (source: SearchSource, change: Partial<SearchSourceOption> = {}): SearchSourceOption => ({
  source,
  available: true,
  reason: "",
  host: source === "keenable" ? "api.keenable.ai" : "",
  needs_key: false,
  key_present: false,
  key_from_environment: false,
  key_store: "keychain",
  keyless: source === "keenable",
  suggested: false,
  chosen: false,
  agreed: false,
  ...change,
});

const agent = (event: Record<string, unknown>): RunEvent => ({ kind: "agent", event }) as RunEvent;

describe("the research marker", () => {
  it("is read from the event's own field, not from the text", () => {
    expect(agentEvent({ type: "text_delta", text: "", research_needed: { lookup: LOOKUP, state: "needs_consent", source: "keenable" } })).toEqual({
      type: "text_delta",
      text: "",
      researchNeeded: { lookup: LOOKUP, state: "needs_consent", source: "keenable" },
    });
    // An older agent that does not say why: the reason is unknown.
    expect(agentEvent({ type: "text_delta", text: "", research_needed: { lookup: LOOKUP } })).toMatchObject({
      researchNeeded: { lookup: LOOKUP, state: null, source: null },
    });
    // The words alone are not a marker.
    expect(agentEvent({ type: "text_delta", text: `web research is off: ${LOOKUP}` })).toEqual({
      type: "text_delta",
      text: `web research is off: ${LOOKUP}`,
      researchNeeded: null,
    });
    for (const odd of [null, "x", [LOOKUP], 3]) {
      expect(agentEvent({ type: "text_delta", text: "a", research_needed: odd })).toMatchObject({ researchNeeded: null });
    }
    expect(agentEvent({ type: "text_delta", text: "", research_needed: { lookup: "y".repeat(500) } })).toMatchObject({
      researchNeeded: { lookup: "y".repeat(200), state: null, source: null },
    });
  });

  it("stays with the run and leaves its text as it was", () => {
    let run = startRun("r1", PROJECT);
    run = reduceRun(run, agent({ type: "text_delta", text: "The case is ready.\nweb research is off: x" }));
    expect(run.research).toBeNull();
    run = reduceRun(run, agent({ type: "text_delta", text: "", research_needed: { lookup: LOOKUP, state: "needs_consent", source: "built_in" } }));
    run = reduceRun(run, agent({ type: "usage", tool_calls: 3, input_tokens: 1, output_tokens: 1 }));
    run = reduceRun(run, { kind: "finished", stop_reason: "done", error: null });
    expect(run.research).toEqual({ lookup: LOOKUP, state: "needs_consent", source: "built_in" });
    expect(run.text).toBe("The case is ready.\nweb research is off: x");
  });
});

function Where() {
  const location = useLocation();
  return <p data-testid="where">{`${location.pathname} ${JSON.stringify(location.state)}`}</p>;
}

const NEEDS_CONSENT: ResearchNeeded = { lookup: LOOKUP, state: "needs_consent", source: "keenable" };

async function showAction(pending: PendingCommands, need: ResearchNeeded = NEEDS_CONSENT) {
  render(
    <MemoryRouter initialEntries={["/hotend/p/p1/design"]}>
      <CoreContext value={fakeClient({})}>
        <PendingContext value={pending}>
          <Routes>
            <Route path="/hotend/p/p1/design" element={<ResearchAction need={need} />} />
            <Route path="/settings/ai" element={<Where />} />
          </Routes>
        </PendingContext>
      </CoreContext>
    </MemoryRouter>,
  );
  await act(async () => {
    await settled();
  });
}

async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element);
    await settled();
  });
}

describe("Turn on web research", () => {
  it("asks for the agreement in the shell's own dialog for the chosen source", async () => {
    const options = [option("built_in"), option("keenable", { chosen: true }), option("off")];
    const ask = vi.fn(() => Promise.resolve({ agreed: true, options }));
    await showAction(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)), research_consent_ask: ask }));
    expect(screen.getByTestId("research-action").textContent).toContain(S.talk.research.why(LOOKUP));
    await click(screen.getByTestId("research-turn-on-here"));
    expect(ask).toHaveBeenCalledTimes(1);
    expect(ask).toHaveBeenCalledWith("keenable");
    expect(screen.getByRole("status").textContent).toBe(S.talk.research.on);
    expect(screen.queryByTestId("research-turn-on-here")).toBeNull();
  });

  it("says that research stays off when the person does not agree", async () => {
    const options = [option("built_in", { chosen: true }), option("off")];
    const ask = vi.fn(() => Promise.resolve({ agreed: false, options }));
    await showAction(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)), research_consent_ask: ask }));
    await click(screen.getByTestId("research-turn-on-here"));
    expect(ask).toHaveBeenCalledWith("built_in");
    expect(screen.getByRole("status").textContent).toBe(S.talk.research.declined);
    // It can be asked again.
    expect(screen.getByTestId<HTMLButtonElement>("research-turn-on-here").disabled).toBe(false);
  });

  it("never says research is on when it was agreed already: the settings say what stands", async () => {
    const options = [option("keenable", { chosen: true, agreed: true })];
    const ask = vi.fn();
    await showAction(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)), research_consent_ask: ask }));
    await click(screen.getByTestId("research-turn-on-here"));
    expect(ask).not.toHaveBeenCalled();
    expect(screen.queryByText(S.talk.research.on)).toBeNull();
    expect(screen.getByTestId("where").textContent).toContain('"focus":"web-research"');
  });

  it("asks for the agreement too when an older agent does not say why", async () => {
    const options = [option("built_in", { chosen: true })];
    const ask = vi.fn(() => Promise.resolve({ agreed: true, options }));
    await showAction(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)), research_consent_ask: ask }), {
      lookup: LOOKUP,
      state: null,
      source: null,
    });
    await click(screen.getByTestId("research-turn-on-here"));
    expect(ask).toHaveBeenCalledWith("built_in");
  });

  it.each([
    ["the chosen source is off", [option("built_in"), option("off", { chosen: true })]],
    ["the chosen source cannot be used", [option("built_in", { chosen: true, available: false, reason: "no search" })]],
    ["nothing is chosen", [option("built_in"), option("off")]],
  ])("opens Settings, AI model, Web research when %s", async (_, options) => {
    const ask = vi.fn();
    await showAction(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)), research_consent_ask: ask }));
    await click(screen.getByTestId("research-turn-on-here"));
    expect(ask).not.toHaveBeenCalled();
    expect(screen.getByTestId("where").textContent).toBe('/settings/ai {"from":"/hotend/p/p1/design","focus":"web-research"}');
  });

  it("opens the settings when the shell does not carry the commands", async () => {
    await showAction(standInPending({}));
    await click(screen.getByTestId("research-turn-on-here"));
    expect(screen.getByTestId("where").textContent).toContain("/settings/ai");
  });
});

describe("research that is off for another reason", () => {
  it.each([
    ["off", "off", S.talk.research.chooseSource, S.talk.research.reasons.noSource],
    ["off", "built_in", S.talk.research.settings, S.talk.research.reasons.off],
    ["needs_key", "tavily", S.talk.research.settings, S.talk.research.reasons.needs_key],
    ["provider_refused", "built_in", S.talk.research.settings, S.talk.research.reasons.provider_refused],
    ["unsupported_provider", "built_in", S.talk.research.settings, S.talk.research.reasons.unsupported_provider],
  ])("%s (source %s) says why and opens the settings, asking nothing", async (state, source, label, reason) => {
    const ask = vi.fn();
    const options = vi.fn();
    await showAction(standInPending({ research_source_options: options, research_consent_ask: ask }), { lookup: LOOKUP, state, source });
    expect(screen.queryByTestId("research-turn-on-here")).toBeNull();
    expect(screen.getByTestId("research-action").textContent).toContain(reason);
    const button = screen.getByTestId("research-settings-here");
    expect(button.textContent).toBe(label);
    await click(button);
    expect(ask).not.toHaveBeenCalled();
    expect(options).not.toHaveBeenCalled();
    expect(screen.getByTestId("where").textContent).toBe('/settings/ai {"from":"/hotend/p/p1/design","focus":"web-research"}');
  });

  it.each(["budget_used", "stopped"])("%s only says the allowance is used up", async (state) => {
    const ask = vi.fn();
    await showAction(standInPending({ research_consent_ask: ask }), { lookup: LOOKUP, state, source: "built_in" });
    const action = screen.getByTestId("research-action");
    expect(action.textContent).toContain(S.talk.research.reasons.budget);
    expect(action.querySelector("button")).toBeNull();
    expect(screen.queryByText(S.talk.research.on)).toBeNull();
  });

  it("keeps the reason with the run's memory across a reload", () => {
    const need: ResearchNeeded = { lookup: LOOKUP, state: "budget_used", source: "built_in" };
    rememberRun({ ...startRun("r-mem", PROJECT), status: "done", research: need });
    expect(rememberedRun("r-mem")?.research).toEqual(need);
  });
});

describe("the conversation", () => {
  async function open(pending: PendingCommands): Promise<HotendStore> {
    let store: HotendStore | null = null;
    function Grab() {
      const found = useHotendStore();
      useEffect(() => {
        store = found;
      }, [found]);
      const ready = useHotend((s) => s.project !== null);
      return ready ? <Conversation projectId={PROJECT} /> : null;
    }
    render(
      <MemoryRouter>
        <CoreContext value={cableBoxClient({})}>
          <PendingContext value={pending}>
            <HostProvider>
              <HotendProvider>
                <Grab />
              </HotendProvider>
            </HostProvider>
          </PendingContext>
        </CoreContext>
      </MemoryRouter>,
    );
    await act(async () => {
      await settled();
      await (store as HotendStore | null)?.getState().openProject(PROJECT);
      await settled();
    });
    if (!store) throw new Error("the store was not created");
    return store;
  }

  it("shows the action next to the answer that carries the marker, and only there", async () => {
    const options = [option("keenable", { chosen: true })];
    const ask = vi.fn(() => Promise.resolve({ agreed: true, options }));
    const store = await open(standInPending({ research_source_options: vi.fn(() => Promise.resolve(options)), research_consent_ask: ask }));
    let run = reduceRun(startRun("r-phone", PROJECT), agent({ type: "text_delta", text: "The case is built from the maker's drawing." }));
    await act(async () => {
      store.setState({ run });
      await settled();
    });
    // No marker: no action.
    expect(screen.queryByTestId("research-action")).toBeNull();
    run = reduceRun(run, agent({ type: "text_delta", text: "", research_needed: { lookup: LOOKUP, state: "needs_consent", source: "keenable" } }));
    await act(async () => {
      store.setState({ run });
      await settled();
    });
    // Not while the run is still going.
    expect(screen.queryByTestId("research-action")).toBeNull();
    run = reduceRun(run, { kind: "finished", stop_reason: "done", error: null });
    await act(async () => {
      store.setState({ run });
      await settled();
    });
    const live = screen.getByTestId("live-run");
    const action = screen.getByTestId("research-action");
    expect(live.contains(action)).toBe(true);
    await click(screen.getByTestId("research-turn-on-here"));
    expect(ask).toHaveBeenCalledWith("keenable");
  });
});
