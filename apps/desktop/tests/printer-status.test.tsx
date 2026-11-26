// A handed-off print as the printer reports it (read-only), in the Print stage's
// inspector: against stand-in answers of the printer commands, through the shell's own
// commands and event stream, and in its disabled state for a window without them.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, PendingContext } from "@/host/core-context";
import type { CoreClient, PrintJob } from "@/lib/core";
import { NOT_AVAILABLE, type LanPrinterStatus, type PendingCommands, type PrinterLanSetup } from "@/lib/core/pending";
import { FollowedStatus } from "@/hotend/inspector/PrinterStatus";
import { S } from "@/hotend/strings";

import { settled } from "./cable-box";
import { fakeClient } from "./fake-client";
import { standInPending } from "./fake-pending";

afterEach(cleanup);

const JOB = {
  id: "job-7",
  project_id: "p-1",
  version: 2,
  slice_sha256: "s",
  note: "",
  status: "handed_off",
  delivery: "bambu_connect",
  created_at_ms: 1,
  updated_at_ms: 2,
  detail: { simulated: false, phase: null, headline: null, progress_percent: null, remaining_minutes: null, message: null, problems: [], findings: [], link: null, path: null },
} as unknown as PrintJob;

const setup = (change: Partial<PrinterLanSetup> = {}): PrinterLanSetup => ({
  address: { ip: "192.168.1.40", serial: "01P00A123456789" },
  trust: "pinned",
  ca_file: null,
  pin: null,
  access_code: "secret_store",
  access_code_store: "keychain",
  watching: true,
  access_code_present: true,
  ...change,
});

const status = (change: Partial<LanPrinterStatus> = {}): LanPrinterStatus => ({
  state: {
    phase: "printing",
    headline: "Printing",
    progress_percent: 37,
    remaining_minutes: 95,
    layer: 12,
    total_layers: 200,
    job_name: "Print_2-v2",
    bed_c: 55,
    problems: [],
    ams: [],
  },
  nozzles: [{ id: 0, current_c: 219.6, target_c: 220 }],
  bed: { current_c: 54.8, target_c: 55 },
  chamber: { current_c: null, target_c: null },
  reports: 4,
  job_id: JOB.id,
  matched_name: "Print_2-v2",
  connected: true,
  ended: null,
  ...change,
});

async function show(pending: PendingCommands | null, client: Partial<CoreClient> = {}) {
  const core = fakeClient(client);
  render(
    <MemoryRouter>
      <CoreContext value={core}>
        <PendingContext value={pending}>
          <FollowedStatus job={JOB} />
        </PendingContext>
      </CoreContext>
    </MemoryRouter>,
  );
  await act(async () => {
    await settled();
  });
  return core;
}

describe("the printer status of a handed-off print", () => {
  it("reads the shell's printer commands and follows the printer events of the stream", async () => {
    // No stand-in: the commands and the event stream of the client, as the window has them.
    const start = vi.fn(() => Promise.resolve(null));
    const core = await show(null, {
      printer_lan_get: vi.fn(() => Promise.resolve(setup({ watching: false }))),
      printer_watch_latest: vi.fn(() => Promise.resolve(null)),
      printer_watch_start: start,
    });
    expect(core.printer_lan_get).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("followed").getAttribute("data-state")).toBe("not-following");
    await act(async () => {
      fireEvent.click(screen.getByTestId("followed-start"));
      await settled();
    });
    expect(start).toHaveBeenCalledTimes(1);
    expect(core.subscribers()).toBe(1);
    await act(async () => {
      // Events of other channels are not the panel's.
      core.emit({ channel: "lagged", missed: 2 });
      core.emit({ channel: "printer", status: status({ state: { ...status().state, layer: 70 } }) });
      await settled();
    });
    expect(screen.getByTestId("followed-layer").textContent).toBe("70 of 200");
    cleanup();
    expect(core.subscribers()).toBe(0);
  });

  it("says what it always said in a window that cannot read the printer", async () => {
    await show(
      standInPending({
        printer_lan_get: vi.fn(() => Promise.reject(NOT_AVAILABLE)),
        printer_watch_latest: vi.fn(() => Promise.reject(NOT_AVAILABLE)),
      }),
    );
    const shown = screen.getByTestId("followed");
    expect(shown.getAttribute("data-state")).toBe("unavailable");
    expect(shown.textContent).toContain(S.print.cannotFollow);
    expect(shown.textContent).toContain(S.print.followed.notAvailable);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("says plainly when no printer is set up, and where to set one up", async () => {
    await show(
      standInPending({
        printer_lan_get: vi.fn(() => Promise.resolve(setup({ address: null, watching: false }))),
        printer_watch_latest: vi.fn(() => Promise.resolve(null)),
      }),
    );
    expect(screen.getByTestId("followed").textContent).toContain("Printer status: not set up.");
    expect(screen.getByTestId("followed-settings").getAttribute("href")).toBe("/settings/printer");
  });

  it("shows phase, layer, progress, time left, temperatures and problems of this job", async () => {
    const latest = status({
      state: {
        ...status().state,
        problems: [{ message: "HMS_0300_0100_0001_0002", raw_code: "0300_0100_0001_0002", meaning: "the nozzle temperature is abnormal", blocking: true, help_url: null }],
      },
    });
    await show(
      standInPending({
        printer_lan_get: vi.fn(() => Promise.resolve(setup())),
        printer_watch_latest: vi.fn(() => Promise.resolve(latest)),
      }),
    );
    expect(screen.getByTestId("followed").getAttribute("data-state")).toBe("following");
    expect(screen.getByTestId("followed-layer").textContent).toBe("12 of 200");
    expect(screen.getByTestId("followed-percent").textContent).toBe("37 %");
    expect(screen.getByTestId("followed-left").textContent).toBe("1 h 35 min");
    const facts = screen.getByTestId("followed-facts").textContent;
    expect(facts).toContain("Printing");
    expect(facts).toContain("220 °C → 220 °C");
    expect(facts).toContain("55 °C → 55 °C");
    expect(facts).not.toContain("Chamber");
    expect(screen.getByTestId("followed-problems").textContent).toContain("The nozzle temperature is abnormal");
    expect(screen.getByTestId("followed-problems").textContent).toContain("0300_0100_0001_0002");
    expect(facts).toContain(S.print.followed.fromPrinter);
  });

  it("follows the printer's reports as they come, and says when a report is not this job's", async () => {
    const pending = standInPending({
      printer_lan_get: vi.fn(() => Promise.resolve(setup())),
      printer_watch_latest: vi.fn(() => Promise.resolve(null)),
    });
    await show(pending);
    expect(screen.getByTestId("followed").getAttribute("data-state")).toBe("waiting");
    expect(screen.getByTestId("followed").textContent).toContain(S.print.followed.waiting);
    await act(async () => {
      pending.report(status({ state: { ...status().state, layer: 50 } }));
      await settled();
    });
    expect(screen.getByTestId("followed-layer").textContent).toBe("50 of 200");
    await act(async () => {
      pending.report(status({ job_id: null, matched_name: null, state: { ...status().state, job_name: "Someone else's part" } }));
      await settled();
    });
    expect(screen.getByTestId("followed").getAttribute("data-state")).toBe("other-job");
    expect(screen.getByTestId("followed").textContent).toContain("Printer status: not this job.");
    expect(screen.getByTestId("followed").textContent).toContain("Someone else's part");
    expect(screen.queryByTestId("followed-facts")).toBeNull();
  });

  it("starts listening only when the person asks for it", async () => {
    let watching = false;
    const start = vi.fn(() => {
      watching = true;
      return Promise.resolve(null);
    });
    await show(
      standInPending({
        printer_lan_get: vi.fn(() => Promise.resolve(setup({ watching }))),
        printer_watch_latest: vi.fn(() => Promise.resolve(null)),
        printer_watch_start: start,
      }),
    );
    expect(start).not.toHaveBeenCalled();
    expect(screen.getByTestId("followed").textContent).toContain(S.print.followed.notFollowing);
    await act(async () => {
      fireEvent.click(screen.getByTestId("followed-start"));
      await settled();
    });
    expect(start).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("followed").getAttribute("data-state")).toBe("waiting");
    expect(screen.queryByTestId("followed-start")).toBeNull();
  });
});
