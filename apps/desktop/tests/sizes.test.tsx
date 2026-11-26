// The parameter control (DESIGN_SPEC 5.3, 6.1): a field and a slider with the range, the
// unit and where the number comes from; one rebuild per committed value.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, HostProvider } from "@/host/core-context";
import type { ApiError, BuildReport, ShellEvent } from "@/lib/core";
import { HotendProvider, useHotend, useHotendStore } from "@/hotend/context";
import { Sizes } from "@/hotend/inspector/Sizes";
import type { HotendStore } from "@/hotend/store";

import { cableBoxClient, PROJECT, settled, v2 } from "./cable-box";
import type { FakeClient } from "./fake-client";

afterEach(cleanup);

function Panel({ grab }: { grab(store: HotendStore): void }) {
  const store = useHotendStore();
  useEffect(() => {
    grab(store);
  }, [grab, store]);
  const current = useHotend((state) => state.project?.project.current_version ?? null);
  const look = useHotend((state) => state.look);
  const result = useHotend((state) => (current === null ? undefined : state.results[look ?? current]));
  return current === null ? null : <Sizes result={result} readOnly={look} />;
}

async function open(client: FakeClient): Promise<HotendStore> {
  let store: HotendStore | null = null;
  render(
    <MemoryRouter>
      <CoreContext value={client}>
        <HostProvider>
          <HotendProvider>
            <Panel
              grab={(found) => {
                store = found;
              }}
            />
          </HotendProvider>
        </HostProvider>
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

const row = (name: string) => screen.getByTestId(`size-${name}`);
const field = (name: string) => row(name).querySelector<HTMLInputElement>('input[inputmode], input[type="text"]') as HTMLInputElement;

describe("a size the maker chooses", () => {
  it("shows its value, unit and range, and marks the built value on the track", async () => {
    await open(cableBoxClient());
    expect(row("height").textContent).toContain("Height");
    expect(field("height").value).toBe("40");
    expect(row("height").textContent).toContain("mm");
    expect(row("height").textContent).toContain("26 to 80");
    const slider = row("height").querySelector('input[type="range"]') as HTMLInputElement;
    expect(slider.getAttribute("aria-label") ?? slider.closest("[aria-label]")?.getAttribute("aria-label")).toBe("Height");
    expect([slider.min, slider.max]).toEqual(["26", "80"]);
    // The hollow tick marks the built value only while the thumb is somewhere else: under
    // the thumb it read as a stray ring.
    expect(screen.queryByTestId("tick-height")).toBeNull();
    expect(row("wall").textContent).toContain("1.6 to 3");
  });

  it("marks the built value on the track while another value is being built", async () => {
    const parameter_set = vi.fn(() => new Promise<BuildReport>(() => undefined));
    const store = await open(cableBoxClient({ parameter_set }));
    act(() => {
      store.getState().setSize("height", 80);
    });
    expect(parseFloat(screen.getByTestId("tick-height").style.left)).toBeCloseTo(25.93, 1);
  });

  it("names where a number comes from and asks for a measurement of a guess", async () => {
    await open(cableBoxClient());
    // The first row is the first thing Hotend asked about, and it is open.
    expect(screen.getByTestId("measure-width").textContent).toContain("Hotend's guess");
    expect(screen.getByTestId("use-measurement-width")).toBeTruthy();
    // It says what to measure, in the window's words.
    expect(screen.getByTestId("measure-ask-width").textContent).toBe(
      "Measure the thing that goes inside the box: the long side, left to right (120 mm now), at its largest. Hotend adds the wall around it.",
    );
    expect(screen.getByTestId("sizes").textContent).toContain("To measure");
    expect(screen.getByTestId("sizes").textContent).toContain("Your choices");
  });

  it("rebuilds once when a typed value is committed, and shows the new version's value", async () => {
    const client = cableBoxClient();
    const store = await open(client);
    await act(async () => {
      fireEvent.focus(field("height"));
      fireEvent.change(field("height"), { target: { value: "46" } });
      fireEvent.blur(field("height"));
      await settled();
      await settled();
    });
    expect(client.parameter_set).toHaveBeenCalledTimes(1);
    expect(client.parameter_set).toHaveBeenCalledWith(PROJECT, "height", 46);
    expect(store.getState().project?.project.current_version).toBe(3);
    expect(field("height").value).toBe("46");
    expect(row("height").getAttribute("data-busy")).toBeNull();
  });

  it("does not rebuild with a typed value outside the range, and says the range", async () => {
    const client = cableBoxClient();
    await open(client);
    await act(async () => {
      fireEvent.focus(field("wall"));
      fireEvent.change(field("wall"), { target: { value: "9" } });
      fireEvent.blur(field("wall"));
      await settled();
      await settled();
    });
    // 9 is not silently turned into 3 and built: the maker asked for 9.
    expect(client.parameter_set).not.toHaveBeenCalled();
    expect(field("wall").value).toBe("9");
    expect(field("wall").getAttribute("aria-invalid")).toBe("true");
    expect(screen.getByTestId("size-beyond").textContent).toBe("Wall thickness goes from 1.6 to 3 mm. Ask Hotend if you need more.");
    // A value inside the range clears it and rebuilds.
    await act(async () => {
      fireEvent.focus(field("wall"));
      fireEvent.change(field("wall"), { target: { value: "2.4" } });
      fireEvent.blur(field("wall"));
      await settled();
      await settled();
    });
    expect(screen.queryByTestId("size-beyond")).toBeNull();
    expect(client.parameter_set).toHaveBeenCalledWith(PROJECT, "wall", 2.4);
  });

  it("says that the computer failed, not the size, when the rebuild could not run", async () => {
    const sandbox: ApiError = {
      kind: "engine",
      code: "sandbox_unavailable",
      message: "The sandbox cannot be set up on this computer.",
      next_step: "Open Settings, This computer, and follow the steps there.",
    } as ApiError;
    const store = await open(cableBoxClient({ parameter_set: vi.fn(() => Promise.reject(sandbox)) }));
    await act(async () => {
      store.getState().setSize("wall", 2.8);
      await settled();
      await settled();
    });
    expect(store.getState().refused).toBeNull();
    expect(screen.queryByTestId("size-refused")).toBeNull();
    const card = screen.getByTestId("size-rebuild-error");
    expect(card.textContent).toContain("Wall thickness 2.8 mm was not built. Still at 2 mm.");
    expect(card.textContent).toContain("The sandbox cannot be set up on this computer.");
    expect(card.textContent).toContain("Open Settings, This computer, and follow the steps there.");
    expect(screen.getByRole("button", { name: "Open This computer" })).toBeTruthy();
  });

  it("refuses a measurement of 0 or less", async () => {
    const message_send = vi.fn().mockResolvedValue({ run_id: "r1" });
    await open(cableBoxClient({ message_send }));
    const input = screen.getByTestId("measure-width").querySelector("input") as HTMLInputElement;
    await act(async () => {
      fireEvent.focus(input);
      fireEvent.change(input, { target: { value: "-5" } });
      fireEvent.blur(input);
      await settled();
    });
    expect((screen.getByTestId("use-measurement-width") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("measure-wrong").textContent).toBe("A measurement is more than 0.");
    expect(message_send).not.toHaveBeenCalled();
  });

  it("says when a measurement was not applied, with the model's value back in the field", async () => {
    const message_send = vi.fn().mockResolvedValue({ run_id: "r1" });
    const store = await open(cableBoxClient({ message_send }));
    const finished: ShellEvent = { channel: "run", run_id: "r1", project_id: PROJECT, event: { kind: "finished", stop_reason: "done", error: null } };
    await act(async () => {
      await store.getState().sendMessage("I measured Inside width: 185 mm. Use this value and mark it as measured.", { name: "width", value: 185 });
      store.getState().receive(finished);
      await settled();
      await settled();
    });
    // The run ended without a new version: the part is still 120 mm wide.
    expect(store.getState().notApplied).toEqual({ name: "width", value: 185 });
    const note = screen.getByTestId("not-applied");
    expect(note.textContent).toContain("Not applied: the part is still 120 mm. Send it again or ask Hotend why.");
    expect((screen.getByTestId("measure-width").querySelector("input") as HTMLInputElement).value).toBe("120");
    expect(screen.getByRole("button", { name: "Send again" })).toBeTruthy();
  });

  it("shows the busy state while the part rebuilds and the reason when it does not build", async () => {
    let release: (report: BuildReport) => void = () => undefined;
    const parameter_set = vi.fn(
      () =>
        new Promise<BuildReport>((resolve) => {
          release = resolve;
        }),
    );
    const store = await open(cableBoxClient({ parameter_set }));
    act(() => {
      store.getState().setSize("height", 80);
    });
    expect(row("height").getAttribute("data-busy")).toBe("true");
    expect(field("height").value).toBe("80");
    await act(async () => {
      release({
        version: null,
        result: { ...v2.result, status: "model_error", error: { type: "ValueError", message: "The lid no longer fits.", line: 1, traceback: "" } },
      });
      await settled();
    });
    expect(row("height").getAttribute("data-busy")).toBeNull();
    expect(field("height").value).toBe("40");
    expect(screen.getByTestId("size-refused").textContent).toBe("Height 80 mm does not build: The lid no longer fits. Still at 40 mm.");
  });

  it("is read-only, with the reason, while an older version is on the stage", async () => {
    const store = await open(cableBoxClient());
    await act(async () => {
      await store.getState().lookAt(1);
      await settled();
    });
    expect(field("wall").value).toBe("1");
    expect(field("wall").disabled).toBe(true);
    expect((row("wall").querySelector('input[type="range"]') as HTMLInputElement).disabled).toBe(true);
  });
});
