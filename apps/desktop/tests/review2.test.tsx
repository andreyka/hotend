// Regression tests: each test names the defect it stands for. The
// mesh and the content policy have files of their own (mesh.test.ts, csp.test.ts).
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { useEffect } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, HostProvider } from "@/host/core-context";
import { healthRows } from "@/host/health";
import { HealthRows } from "@/host/sections";
import { H } from "@/host/strings";
import type { CoreClient, DoctorReport, PrintJob, PrintReview, SliceOutcome, Value } from "@/lib/core";
import { commands } from "@/lib/core/bindings";
import { calibrationState, quantityWords } from "@/hotend/calibration";
import { HotendProvider, useHotend, useHotendStore } from "@/hotend/context";
import { PreparePanel } from "@/hotend/inspector/PreparePanel";
import { PrintPanel } from "@/hotend/inspector/PrintPanel";
import { Proof } from "@/hotend/inspector/Proof";
import { slicedBeforePreviews, slicedPlate, type HotendState, type HotendStore } from "@/hotend/store";
import { S } from "@/hotend/strings";
import { useStudioUi } from "@/hotend/ui";
import { prepareVerdict, printVerdict, type VerdictInput } from "@/hotend/verdict";

import { cableBoxClient, PROJECT, settled, v2 } from "./cable-box";

afterEach(cleanup);

const HERE = import.meta.dirname;

/** Opens the cable box and draws `children` inside the providers of the window. */
async function open(answers: Partial<CoreClient>, children: React.ReactNode, state: Partial<HotendState> = {}): Promise<HotendStore> {
  let store: HotendStore | null = null;
  function Grab() {
    const found = useHotendStore();
    useEffect(() => {
      store = found;
    }, [found]);
    const ready = useHotend((s) => s.project !== null);
    return ready ? children : null;
  }
  render(
    <MemoryRouter>
      <CoreContext value={cableBoxClient(answers)}>
        <HostProvider>
          <HotendProvider>
            <Grab />
          </HotendProvider>
        </HostProvider>
      </CoreContext>
    </MemoryRouter>,
  );
  await act(async () => {
    await settled();
    await (store as HotendStore | null)?.getState().openProject(PROJECT);
    await settled();
    (store as HotendStore | null)?.setState(state);
    await settled();
  });
  if (!store) throw new Error("the store was not created");
  return store;
}

const slice = (plate: number, sha = "sha-of-the-slice", codes: string[] = []): SliceOutcome =>
  ({
    record: {
      project_id: PROJECT,
      version: 2,
      profile_hash: "h",
      profile: "demo-0.20mm",
      summary: null,
      job: { plate, printer_id: "bambu-x2d", material: { id: "pla-basic", family: "PLA" }, pieces: ["box"], prepared_at_ms: 1 },
      created_at_ms: 9e15,
    },
    summary: {
      status: "ok",
      output: "plate.gcode.3mf",
      sha256: sha,
      time_s: 3600,
      filament: [{ slot: 0, grams: 28.3, material: "PLA" }],
      layers: 200,
      warnings: codes.map((code) => ({ code, message: code })),
      slicer: { name: "Bambu Studio", version: "2.3.0" },
    },
    preparation: null,
    output_path: "/data/plate.gcode.3mf",
  }) as unknown as SliceOutcome;

const plates = (count: number) =>
  ({ version: 2, created_at_ms: 1, plates: Array.from({ length: count }, (_, index) => ({ index: index + 1, size: [256, 256], items: [] })), unplaced: [] }) as unknown as HotendState["preparation"];

const job = (status: string, change: Partial<PrintJob["detail"]> = {}): PrintJob =>
  ({
    id: "job-1",
    project_id: PROJECT,
    version: 2,
    slice_sha256: "sha-of-the-slice",
    note: "",
    status,
    delivery: status === "pending" ? null : "bambu_connect",
    created_at_ms: 1,
    updated_at_ms: 2,
    detail: { simulated: false, phase: null, headline: null, progress_percent: null, remaining_minutes: null, message: null, problems: [], findings: [], link: null, path: null, ...change },
  }) as unknown as PrintJob;

// "Open in Bambu Connect" was once written on a way that never opened Bambu Connect. Now the
// core opens Bambu Connect's import through its opener after the confirmation, where it has
// a real opener, and records whether it did (`detail.opened`). The window says "open" only
// where Bambu Connect was, or will be, asked to open the file, and "save" otherwise.
describe("the Bambu Connect delivery says \"open\" only where Bambu Connect is opened", () => {
  it("is named for what it does: it saves the checked file and asks Bambu Connect to import it", () => {
    expect(S.print.connect.button).toBe("Save and open in Bambu Connect…");
    expect(S.print.connect.under).toBe(
      "Hotend saves the checked file in your hand-off folder and asks Bambu Connect to import it. You choose the printer and press Print there; Hotend does not control the print.",
    );
    // With the stand-in opener nothing is opened, and nothing says it is.
    expect(S.print.connect.buttonNoOpener).toBe("Save for Bambu Connect…");
    expect(S.print.connect.underNoOpener).toContain("does not open Bambu Connect");
    expect(S.print.connect.underNoOpener).not.toContain("asks Bambu Connect");
    expect(S.verdict.handedOff(true)).toBe("Saved, and Bambu Connect was asked to import it. Choose the printer and press Print there.");
    expect(S.verdict.handedOff(false)).toBe("Saved for Bambu Connect. Import it there, choose the printer and press Print.");
    expect(S.verdict.handedOff(false)).not.toMatch(/asked|opened/);
    // The person presses Print in Bambu Connect: no word of the window says Hotend prints.
    for (const words of [S.print.connect.button, S.print.connect.buttonNoOpener]) {
      expect(words).not.toMatch(/Print|Start|Send/);
    }
  });

  it("says after a hand-off with the stand-in that Bambu Connect was not opened, and where the file is", async () => {
    const handed = job("handed_off", {
      message: "saved to the hand-off folder; Bambu Connect was not opened (stand-in opener: nothing was opened). ...",
      link: "bambu-connect://import-file?path=%2Fdata%2Fhandoff%2FPrint_2-v2.gcode.3mf",
      path: "/data/handoff/Print_2-v2.gcode.3mf",
      ...({ opened: false, opener: "stand-in opener: nothing was opened", windows_path: null } as object),
    });
    // No printer on the network is set up for reading its status.
    const noPrinter = {
      address: null,
      trust: "unconfirmed",
      ca_file: null,
      pin: null,
      access_code: "missing",
      access_code_store: "keychain",
      watching: false,
      access_code_present: false,
    } as const;
    const store = await open(
      {
        print_jobs: vi.fn(() => Promise.resolve([handed])),
        printer_lan_get: vi.fn(() => Promise.resolve(noPrinter)),
        printer_watch_latest: vi.fn(() => Promise.resolve(null)),
      },
      <PrintPanel />,
      { slice: slice(1), preparation: plates(1) },
    );
    const outcome = screen.getByTestId("outcome");
    expect(outcome.getAttribute("data-status")).toBe("handed_off");
    expect(screen.getByTestId("connect-file").textContent).toBe(
      "Bambu Connect was not opened. Import this file in Bambu Connect, choose the printer and press Print:",
    );
    expect(outcome.textContent).toContain("/data/handoff/Print_2-v2.gcode.3mf");
    expect(outcome.textContent).toContain(S.print.followed.notSetUp);
    expect(outcome.textContent).not.toContain("was asked");
    // The window shows no link.
    expect(outcome.textContent).not.toContain("bambu-connect://");
    const input = { ...store.getState(), canTalk: true, now: 0 } as VerdictInput;
    expect(printVerdict(input)).toMatchObject({ icon: "ok", sentence: S.verdict.handedOff(false) });
  });

  it("says after a hand-off with a real opener that Bambu Connect was asked, and names the Windows path", async () => {
    const handed = job("handed_off", {
      link: "bambu-connect://import-file?path=D%3A%5CModels%5CPrint_2.gcode.3mf&name=Print%202&version=1.0.0",
      path: "/mnt/d/Models/Print_2.gcode.3mf",
      ...({ opened: true, opener: "Windows explorer, from WSL", windows_path: "D:\\Models\\Print_2.gcode.3mf" } as object),
    });
    const store = await open({ print_jobs: vi.fn(() => Promise.resolve([handed])) }, <PrintPanel />, { slice: slice(1), preparation: plates(1) });
    const outcome = screen.getByTestId("outcome");
    expect(screen.getByTestId("connect-file").textContent).toBe(
      "Bambu Connect was asked to import this file. Choose the printer and press Print there:",
    );
    // As Bambu Connect on Windows reads it.
    expect(outcome.textContent).toContain("D:\\Models\\Print_2.gcode.3mf");
    expect(outcome.textContent).not.toContain("bambu-connect://");
    const input = { ...store.getState(), canTalk: true, now: 0 } as VerdictInput;
    expect(printVerdict(input)).toMatchObject({ icon: "ok", sentence: S.verdict.handedOff(true) });
  });

  it("lists Bambu Connect first, then the editable project as a way of its own that says it is outside Hotend's check, and a hand-off that needs a folder says so", async () => {
    const deliveries = (connectEnabled: boolean) =>
      ["simulated_printer", "bambu_connect", "export_to_folder"].map((kind) => ({
        kind,
        name: kind,
        enabled: kind !== "bambu_connect" || connectEnabled,
        disabled_reason: kind === "bambu_connect" && !connectEnabled ? "Choose a folder on a Windows drive for the Bambu Connect hand-off in Settings first." : null,
        sends_file: false,
        starts_print: false,
        controls_print: false,
        live_status: false,
        reports_result: false,
        can: [],
        cannot: [],
        cost: "",
        needs: [],
      }));
    const printer = (connectEnabled: boolean) =>
      ({
        id: "simulated-x2d",
        name: "Simulated X2D",
        model: "X2D",
        serial: "S",
        simulated: true,
        state: { phase: "idle", headline: "", progress_percent: null, remaining_minutes: null, layer: null, total_layers: null, job_name: null, bed_c: null, problems: [], ams: [] },
        deliveries: deliveries(connectEnabled),
      }) as unknown as NonNullable<HotendState["printers"]>[number];
    const review = {
      job: job("pending"),
      delivery: { kind: "simulated_printer" },
      simulated: true,
      printer: null,
      plate: 1,
      pieces: [],
      summary: { file_name: "f", sha256: "sha-of-the-slice", size_bytes: 1, plate: 1, estimated_seconds: 60, layers: 2, filaments: [], max_nozzle_c: 200, max_bed_c: 60, max_chamber_c: null, extent: null },
      review_text: "t",
      warnings: [],
      spools: [],
      web_numbers: [],
    } as unknown as PrintReview;
    await open({ print_jobs: vi.fn(() => Promise.resolve([])) }, <PrintPanel />, {
      slice: slice(1),
      preparation: plates(1),
      review,
      printers: [printer(false)],
    });
    const radios = screen.getAllByRole("radio").map((radio) => radio.closest("label")?.textContent ?? radio.textContent ?? "");
    expect(radios[0]).toContain("Bambu Connect");
    expect(radios[1]).toContain("Bambu Studio project (editable)");
    const project = screen.getByTestId("delivery-project");
    expect(project.textContent).toBe(
      "Bambu Studio project (editable)" +
        "Hotend makes a Bambu Studio project of this plate with its print settings. You change it and print it from Bambu Studio." +
        "A project opened this way is sliced again by Bambu Studio, so it is outside Hotend's check.",
    );
    // It can be chosen (it does not need the hand-off folder to be listed), and nothing in
    // it says the project was checked.
    const row = screen.getAllByRole("radio")[1];
    expect(row?.getAttribute("aria-disabled")).not.toBe("true");
    expect(project.textContent).not.toMatch(/\bchecked\b/);
    const send = screen.getByTestId("send-to").textContent;
    expect(send.indexOf("Bambu Connect")).toBeLessThan(send.indexOf("Bambu Studio project"));
    expect(send.indexOf("Bambu Studio project")).toBeLessThan(send.indexOf("A simulated print"));
    expect(screen.getByTestId("connect-under").textContent).toContain("Windows drive");
    expect(screen.getByTestId("handoff-settings").textContent).toBe("Choose the hand-off folder in Settings");
  });
});

describe("the plate of a print (the review said \"plate 1\" for every plate)", () => {
  const review = (plate: number, chamber: number | null): PrintReview =>
    ({
      job: job("pending"),
      delivery: { kind: "simulated_printer" },
      simulated: true,
      printer: null,
      plate,
      pieces: ["box"],
      // The plate inside the sliced file is 1 in every file.
      summary: { plate: 1, estimated_seconds: 3600, layers: 200, filaments: [], max_nozzle_c: 220, max_bed_c: 55, max_chamber_c: chamber, sha256: "sha-of-the-slice" },
      warnings: [],
      spools: [],
    }) as unknown as PrintReview;

  it("is the plate of the layout that the core recorded with the slice, not the one on the stage", async () => {
    expect(slicedPlate(slice(3))).toBe(3);
    expect(slicedPlate(null)).toBeNull();
    const store = await open({}, null, { slice: slice(3), preparation: plates(4), plate: 1 });
    const input = { ...store.getState(), canTalk: true, now: 0 } as VerdictInput;
    expect(prepareVerdict(input)).toMatchObject({ sentence: "Plate 3 of 4 sliced: 1 h · 28 g", primary: { label: "Review plate 3" } });
  });

  it("shows the review's plate of the layout, and the chamber when the file heats one", async () => {
    await open({}, <PrintPanel />, { slice: slice(3), preparation: plates(4), plate: 1, review: review(3, 60), wantReview: true });
    const facts = screen.getByTestId("review-facts");
    expect(facts.textContent).toContain("v2, plate 3 of 4");
    expect(facts.textContent).toContain("Chamber up to60 °C");
    expect(screen.getByTestId("start-print").textContent).toBe("Start simulated print…");
  });

  it("leaves the chamber out for a file that heats none", async () => {
    await open({}, <PrintPanel />, { slice: slice(1), preparation: plates(1), review: review(1, null), wantReview: true });
    expect(screen.getByTestId("review-facts").textContent).toContain("Bed up to55 °C");
    expect(screen.getByTestId("review-facts").textContent).not.toContain("Chamber");
  });

  it("names a finished job by the plate its slice is of", async () => {
    const done = { ...job("exported", { path: "/usb/plate.gcode.3mf" }), delivery: "export_to_folder" } as PrintJob;
    await open({ print_jobs: vi.fn(() => Promise.resolve([done])) }, <PrintPanel />, { slice: slice(2), preparation: plates(4), plate: 4 });
    expect(screen.getByTestId("print-facts").textContent).toContain("v2, plate 2 of 4");
  });
});

describe("the slicer row of This computer (a missing slicer was said to give \"rough estimates\")", () => {
  const check = (id: string, status: "ok" | "warn" | "info", detail: string) => ({ id, label: id, status, required: false, detail, fix: null });
  const installation = (usable: boolean, standIn: boolean) => ({
    kind: "bambu-studio",
    name: "Bambu Studio",
    path: usable ? "/opt/bambu-studio" : null,
    found: null,
    version: usable && !standIn ? "2.3.0" : null,
    stand_in: standIn,
    verdict: usable ? "ok" : "missing",
    reason: null,
    fix: null,
    usable,
    printers: ["bambu-x2d"],
  });
  const report = (slicers: unknown[]): DoctorReport =>
    ({
      ok: true,
      hotend: "0.1.0",
      debug_build: false,
      sandbox: null,
      // What `hotend doctor` reports on a machine without Bambu Studio.
      checks: [check("slicer.bambu-studio", "warn", "not found"), check("slicer.usable", "warn", "none: no real slicer can be run on this machine")],
      slicers,
    }) as unknown as DoctorReport;
  const slicerRow = (slicers: unknown[]) => healthRows(report(slicers)).find((row) => row.id === "slicer");

  it("says that slicing needs a slicer, and where to choose one", () => {
    const row = slicerRow([installation(false, false)]);
    expect(row?.text).toBe("None found. You can design and check parts; slicing needs Bambu Studio.");
    expect(row?.text).not.toMatch(/rough|estimate/i);
    expect(row?.link).toEqual({ to: "/settings/slicer", label: "Open Settings, Slicer" });
    expect(row?.status).toBe("warn");
  });

  it("speaks of rough numbers only where the stand-in really is in use", () => {
    expect(slicerRow([installation(true, true)])).toMatchObject({ text: H.health.slicerStandIn, link: null });
    expect(slicerRow([installation(true, false)])).toMatchObject({ text: "Bambu Studio 2.3.0 found.", link: null });
  });

  it("draws the link in the row", () => {
    render(
      <MemoryRouter>
        <HealthRows report={report([installation(false, false)])} busy={false} />
      </MemoryRouter>,
    );
    const link = screen.getByTestId("health-link-slicer");
    expect(link.textContent).toBe("Open Settings, Slicer");
    expect(link.getAttribute("href")).toBe("/settings/slicer");
  });
});

describe("the Proof of a printer that was partly measured (it said \"measured hole and peg sizes\" after one hole)", () => {
  const entry = (measured: string[]) =>
    Object.fromEntries(
      ["hole_offset", "peg_offset", "xy_shrink", "z_shrink", "min_clearance"].map((quantity) => [
        quantity,
        measured.includes(quantity) ? { value: 0.05, provenance: "measured", measured_at: "2026-01-05T10:00:00Z" } : { value: 0.1, provenance: "guessed" },
      ]),
    );
  const table = (measured: string[]): Value => ({ entries: [entry(measured)] }) as Value;
  const words = (measured: string[]) => {
    const state = calibrationState(table(measured));
    if (!state) throw new Error("the table has an entry");
    return S.proof.calibratedPartly(
      1,
      quantityWords(state.measured, H.calibration.quantities),
      state.guessed.length > 0 ? quantityWords(state.guessed, H.calibration.quantities) : null,
    );
  };

  it("names only what was measured, and what is still a general value", () => {
    expect(words(["hole_offset"])).toBe(
      "This version uses your printer's measured hole sizes. 1 fit is still not sure to work on every print: one gauge print says where that print landed, not how much the printer varies, and Hotend still uses general values for peg sizes, shrinkage across the plate, shrinkage in height and the smallest gap that still comes apart.",
    );
    expect(words(["hole_offset", "peg_offset"])).toContain("measured hole sizes and peg sizes. ");
    expect(words(["peg_offset"])).not.toContain("measured hole");
    const all = words(["hole_offset", "peg_offset", "xy_shrink", "z_shrink", "min_clearance"]);
    expect(all).not.toContain("general values");
    expect(all.endsWith("not how much the printer varies.")).toBe(true);
    expect(S.proof.calibratedTitle(true)).toBe("Fits partly measured on this printer, not yet proven");
    expect(S.proof.calibratedTitle(false)).toBe("Fits measured on this printer, not yet proven");
  });

  it("says so in the Proof of a version built after one hole was measured", async () => {
    await open({ calibration_get: vi.fn(() => Promise.resolve({ printer_id: "bambu-x2d", table: table(["hole_offset"]) })) as never }, <Proof result={v2.result} />);
    const line = screen.getByText("Fits partly measured on this printer, not yet proven");
    const key = line.closest("[data-testid=proof-line]")?.getAttribute("data-key") ?? "";
    act(() => {
      useStudioUi.getState().toggleLine(key, true);
    });
    const state = screen.getByTestId("fits-state");
    expect(state.getAttribute("data-fits")).toBe("measured");
    expect(state.textContent).toContain("This version uses your printer's measured hole sizes.");
    expect(state.textContent).not.toContain("hole and peg");
    expect(state.textContent).toContain("general values for peg sizes");
  });
});

describe("the window's permissions (six commands were granted that no screen calls)", () => {
  const capability = JSON.parse(readFileSync(join(HERE, "..", "src-tauri", "capabilities", "main.json"), "utf8")) as { permissions: string[] };
  const granted = capability.permissions.map((permission) => permission.replace(/^allow-/, "").replaceAll("-", "_"));
  // The interface's own code: everything under src/ but the generated binding.
  const source = (readdirSync(join(HERE, "..", "src"), { recursive: true }) as string[])
    .filter((file) => /\.tsx?$/.test(file) && !file.replaceAll("\\", "/").endsWith("lib/core/bindings.ts"))
    .map((file) => readFileSync(join(HERE, "..", "src", file), "utf8"))
    .join("\n");
  const called = Object.keys(commands).filter((name) => new RegExp(`(?:\\.|\\b)${name}\\(`).test(source));
  /**
   * Granted today and called by no screen. The capability is the shell's file: these are
   * to be taken out of it there (the facade and the `hotend` command keep them). This
   * list may only shrink; a command that gets a screen leaves it.
   */
  const NOT_CALLED = ["audit_log", "checks_run", "measure_run", "questions_list", "versions_list", "views_render"];

  it("grants every command the interface calls", () => {
    expect(called.length).toBeGreaterThan(30);
    expect(called.filter((name) => !granted.includes(name))).toEqual([]);
  });

  it("grants no command without a call site, beyond the six that are known", () => {
    const unused = granted.filter((name) => !called.includes(name));
    expect(unused.filter((name) => !NOT_CALLED.includes(name))).toEqual([]);
    // A command of the list that a screen now calls is no longer an exception.
    expect(NOT_CALLED.filter((name) => called.includes(name))).toEqual([]);
  });
});

describe("the package's scripts (two could not run on Windows)", () => {
  const scripts = (JSON.parse(readFileSync(join(HERE, "..", "package.json"), "utf8")) as { scripts: Record<string, string> }).scripts;

  it("run through pnpm's shell emulator, which reads NAME=value before a command on every system", () => {
    const setting = Object.entries(scripts).filter(([, line]) => /(^|&&\s*)[A-Z_][A-Z0-9_]*=\S+\s/.test(line));
    expect(setting.map(([name]) => name).sort()).toEqual(["bindings", "screenshots"]);
    const npmrc = readFileSync(join(HERE, "..", ".npmrc"), "utf8");
    expect(npmrc.split("\n").map((line) => line.trim())).toContain("shell-emulator=true");
  });

  it("looks for the bridge under the name cargo gives it on Windows", () => {
    const bridge = readFileSync(join(HERE, "..", "e2e", "bridge.ts"), "utf8");
    expect(bridge).toContain('process.platform === "win32" ? "hotend-bridge.exe" : "hotend-bridge"');
  });
});

describe("a review after the version was laid out again (the core answers slice_changed)", () => {
  it("reads the layout again, so that Prepare offers to slice again", async () => {
    const pending = job("pending");
    const refusal = { kind: "print", code: "slice_changed", message: "The version was laid out again after this file was sliced.", next_step: "Slice it again.", findings: [] };
    let layout = plates(1);
    const store = await open(
      {
        print_jobs: vi.fn(() => Promise.resolve([pending])),
        print_review: vi.fn(() => Promise.reject(refusal)) as never,
        preparation_get: vi.fn(() => Promise.resolve(layout)) as never,
        slice_get: vi.fn(() => Promise.resolve(slice(1))) as never,
      },
      null,
    );
    const before = { ...store.getState(), canTalk: true, now: 0 } as VerdictInput;
    expect(prepareVerdict(before)).toMatchObject({ primary: { label: "Review print" } });
    // The layout was made again elsewhere, after the slice.
    layout = { ...layout, created_at_ms: 9.5e15 } as typeof layout;
    await act(async () => {
      await store.getState().reviewAgain();
      await settled();
    });
    const after = { ...store.getState(), canTalk: true, now: 0 } as VerdictInput;
    expect(store.getState().reviewError).toMatchObject({ code: "slice_changed" });
    expect(prepareVerdict(after)).toMatchObject({ sentence: "Changed since the last slice.", primary: { label: "Slice again", action: { do: "slice" } } });
    expect(printVerdict(after)).toMatchObject({ sentence: "Slice this version first.", primary: { label: "Go to Prepare" } });
  });
});

describe("a review after the plate's print settings changed (the core answers plan_changed, not slice_changed)", () => {
  const refusal = {
    kind: "print",
    code: "plan_changed",
    message: "the print settings of this plate changed after this print was sliced",
    next_step: "Slice the version again and propose a new print.",
    findings: [],
  };

  it("reads the slice and the layout again", async () => {
    const pending = job("pending");
    const sliceGet = vi.fn(() => Promise.resolve(slice(1)));
    const preparationGet = vi.fn(() => Promise.resolve(plates(1)));
    const store = await open(
      {
        print_jobs: vi.fn(() => Promise.resolve([pending])),
        print_review: vi.fn(() => Promise.reject(refusal)) as never,
        preparation_get: preparationGet as never,
        slice_get: sliceGet as never,
      },
      null,
    );
    const slices = sliceGet.mock.calls.length;
    const layouts = preparationGet.mock.calls.length;
    await act(async () => {
      await store.getState().reviewAgain();
      await settled();
    });
    expect(store.getState().reviewError).toMatchObject({ code: "plan_changed" });
    expect(sliceGet.mock.calls.length).toBeGreaterThan(slices);
    expect(preparationGet.mock.calls.length).toBeGreaterThan(layouts);
  });

  it("says that the print settings changed, and leads back to Prepare", async () => {
    expect(S.print.blocked.plan_changed).toBe("The print settings of this plate changed since it was sliced. Slice it again.");
    await open({}, <PrintPanel />, { slice: slice(1), preparation: plates(1), wantReview: true, reviewError: refusal as never });
    const blocked = screen.getByTestId("review-blocked");
    expect(blocked.textContent).toContain("The print settings of this plate changed since it was sliced. Slice it again.");
    expect(blocked.textContent).not.toContain("The sliced file has changed");
    expect(blocked.textContent).toContain(S.verdict.backPrepare);
  });
});

// A Bambu Connect hand-off of a slice made before Hotend-made plate previews were switched
// on was once refused, and the refusal pointed at Bambu Studio first: it was easy to miss
// that pressing "Slice again" is all it takes.
describe("a slice made before Hotend-made plate previews were switched on", () => {
  const settingsWith = (previews: "slicer" | "hotend_made") =>
    vi.fn(() =>
      Promise.resolve({
        provider: { kind: "ollama" as const, model: "scripted" },
        printer_id: "bambu-x2d",
        material: { id: "pla-basic", family: "PLA" },
        max_turns: 24,
        slicer_executables: {},
        plate_type: null,
        nozzle_diameter_mm: null,
        spool: { slot: "1", colour: null, silk: false },
        previews,
      }),
    ) as never;
  const outdated = {
    kind: "print",
    code: "not_importable",
    reason: "slice_before_previews",
    message:
      "This slice was made before Hotend drew plate previews. Slice again, then hand it off. Bambu Connect cannot import it as it is: it lacks the plate preview `Metadata/plate_1.png`. Nothing was saved or sent.",
    next_step: "Slice again, then hand it to Bambu Connect. Other ways: make the editable Bambu Studio project, or save the file to a folder or a USB drive.",
    findings: [],
  };
  const withPreviews = (one: SliceOutcome, previews: "slicer" | "hotend_made"): SliceOutcome =>
    ({ ...one, record: { ...one.record, job: { ...one.record.job, previews } } }) as SliceOutcome;

  it("is refused with Slice again right in the card, which slices as Prepare does", async () => {
    const sliceRun = vi.fn(() => Promise.resolve(slice(1, "sha-with-previews", ["hotend_made_previews"])));
    const store = await open(
      { slice_run: sliceRun as never, settings_get: settingsWith("hotend_made") },
      <PrintPanel />,
      { slice: slice(1), preparation: plates(1), wantReview: true, reviewError: outdated as never },
    );
    const blocked = screen.getByTestId("review-blocked");
    expect(blocked.textContent).toContain("This slice was made before Hotend drew plate previews. Slice again, then hand it off.");
    expect(blocked.textContent).toContain("Other ways: make the editable Bambu Studio project");
    const again = screen.getByTestId("blocked-slice-again");
    expect(again.textContent).toBe(S.verdict.sliceAgain);
    expect(again.textContent).toBe("Slice again");
    expect(blocked.textContent).not.toContain(S.verdict.reviewAgain);
    await act(async () => {
      fireEvent.click(again);
      await settled();
    });
    expect(sliceRun).toHaveBeenCalledWith(PROJECT, store.getState().quality, 1);
    expect(store.getState().slice?.summary.sha256).toBe("sha-with-previews");
  });

  it("offers no Slice again where slicing again alone would not help", async () => {
    const off = { ...outdated, reason: "previews_off", message: "Bambu Connect cannot import this slice: ...", next_step: "Open Settings, choose Hotend-made plate previews, and slice again." };
    await open({}, <PrintPanel />, { slice: slice(1), preparation: plates(1), wantReview: true, reviewError: off as never });
    expect(screen.queryByTestId("blocked-slice-again")).toBeNull();
    expect(screen.getByTestId("review-blocked").textContent).toContain(S.verdict.reviewAgain);
  });

  it("is told apart from the slice record and the settings, with nothing new asked of the core", () => {
    expect(slicedBeforePreviews(slice(1), "hotend_made")).toBe(true);
    expect(slicedBeforePreviews(withPreviews(slice(1), "slicer"), "hotend_made")).toBe(true);
    // Made with previews asked for (also by the stand-in, which draws none), or carrying
    // Hotend's previews in an older record: not outdated.
    expect(slicedBeforePreviews(withPreviews(slice(1), "hotend_made"), "hotend_made")).toBe(false);
    expect(slicedBeforePreviews(slice(1, "sha", ["hotend_made_previews"]), "hotend_made")).toBe(false);
    // With previews off nothing is outdated; without settings nothing is said.
    expect(slicedBeforePreviews(slice(1), "slicer")).toBe(false);
    expect(slicedBeforePreviews(slice(1), null)).toBe(false);
    expect(slicedBeforePreviews(null, "hotend_made")).toBe(false);
  });

  it("is marked outdated in Prepare, and the verdict offers Slice again", async () => {
    expect(S.prepare.previewsOutdated).toBe("Sliced before Hotend drew plate previews. Slice again to hand it to Bambu Connect.");
    const store = await open({ settings_get: settingsWith("hotend_made") }, <PreparePanel />, { slice: slice(1), preparation: null });
    expect(screen.getByTestId("slice-previews-outdated").textContent).toBe(S.prepare.previewsOutdated);
    const input = { ...store.getState(), preparation: plates(1), canTalk: true, now: 0, previews: "hotend_made" } as VerdictInput;
    expect(prepareVerdict(input)).toMatchObject({
      sentence: "Sliced before Hotend drew plate previews.",
      primary: { label: "Slice again", action: { do: "slice" } },
      quiet: { label: "Review print", action: { do: "go", stage: "print" } },
    });
    expect(prepareVerdict({ ...input, previews: "slicer" })).toMatchObject({ primary: { label: "Review print" } });
  });

  it("is not marked once the slice has the previews, or while previews are off", async () => {
    await open({ settings_get: settingsWith("hotend_made") }, <PreparePanel />, { slice: withPreviews(slice(1), "hotend_made"), preparation: null });
    expect(screen.getByTestId("slice-summary")).toBeTruthy();
    expect(screen.queryByTestId("slice-previews-outdated")).toBeNull();
    cleanup();
    await open({ settings_get: settingsWith("slicer") }, <PreparePanel />, { slice: slice(1), preparation: null });
    expect(screen.queryByTestId("slice-previews-outdated")).toBeNull();
  });
});
