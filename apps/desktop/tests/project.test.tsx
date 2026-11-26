// The editable Bambu Studio project in the Print stage: which made project belongs to the
// slice in view, where it can go, and that the window hands it over only through the
// shell's confirmation command, with nothing but the project and the destination.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, HostProvider } from "@/host/core-context";
import type { CoreClient, EditableProject, PrintJob, PrintReview, ProjectReview, SliceOutcome } from "@/lib/core";
import { HotendProvider, useHotend, useHotendStore } from "@/hotend/context";
import { PrintPanel } from "@/hotend/inspector/PrintPanel";
import { projectButton, projectDestination, projectFor, projectGoesTo, projectOutcome } from "@/hotend/project";
import type { HotendState, HotendStore } from "@/hotend/store";
import { S } from "@/hotend/strings";

import { cableBoxClient, PROJECT, settled } from "./cable-box";

afterEach(cleanup);

const SHA = "sha-of-the-slice";

const editable = (change: Partial<EditableProject> = {}): EditableProject =>
  ({
    id: "export-1",
    project_id: PROJECT,
    version: 2,
    plate: 1,
    plan_sha256: "plan",
    slice_sha256: SHA,
    path: "/data/projects/p/versions/2/slicer-project/1-plan/box.3mf",
    sha256: "c0ffee",
    keys: { process: [], filament: [], printer: [] },
    test_slice: {},
    proof: "passed",
    notice: S.print.project.outside,
    delivery: null,
    detail: { simulated: false, phase: null, headline: null, progress_percent: null, remaining_minutes: null, message: null, problems: [], findings: [], link: null, path: null, windows_path: null, opened: null, opener: null },
    created_at_ms: 1,
    updated_at_ms: 1,
    ...change,
  }) as unknown as EditableProject;

const projectReview = (destination: ProjectReview["destination"], opens: boolean): ProjectReview => ({
  project: editable(),
  destination,
  file_name: "Cable_box-v2-c0ffee.3mf",
  handoff:
    destination.kind === "bambu_studio"
      ? { folder: "/mnt/d/Models/Hotend", windows_folder: "D:\\Models\\Hotend", opener: opens ? "Windows explorer, from WSL" : "stand-in opener", opens }
      : null,
  review_text: "the printer layer's text",
  warnings: [],
  notice: S.print.project.outside,
});

describe("the project of the slice in view", () => {
  it("is the one made for this version, this plate and this checked slice", () => {
    const slice = { version: 2, plate: 1, sha256: SHA };
    const other = editable({ id: "other", plate: 2 });
    const older = editable({ id: "older", slice_sha256: "an-earlier-slice" });
    expect(projectFor([other, older, editable()], slice)?.id).toBe("export-1");
    expect(projectFor([other, older], slice)).toBeNull();
    expect(projectFor([editable({ slice_sha256: SHA.toUpperCase() })], slice)?.id).toBe("export-1");
    expect(projectFor([editable()], null)).toBeNull();
  });

  it("goes to Bambu Studio, or to a folder only once one was chosen", () => {
    expect(projectDestination("bambu_studio", null)).toEqual({ kind: "bambu_studio" });
    expect(projectDestination("folder", null)).toBeNull();
    expect(projectDestination("folder", "/media/usb")).toEqual({ kind: "folder", folder: "/media/usb" });
  });

  it("says \"open\" only where Bambu Studio is opened, and never that anything is printed", () => {
    expect(projectButton("bambu_studio", projectReview({ kind: "bambu_studio" }, true))).toBe("Save and open in Bambu Studio…");
    expect(projectButton("bambu_studio", projectReview({ kind: "bambu_studio" }, false))).toBe("Save for Bambu Studio…");
    expect(projectButton("folder", null)).toBe("Save project…");
    for (const words of [S.print.project.studio.button, S.print.project.studio.buttonNoOpener, S.print.project.folder.button]) {
      expect(words).not.toMatch(/Print|Start|Send/);
    }
    expect(projectGoesTo(projectReview({ kind: "bambu_studio" }, true))).toBe("D:\\Models\\Hotend");
    expect(projectGoesTo(projectReview({ kind: "folder", folder: "/media/usb" }, false))).toBe("/media/usb");
  });

  it("says after the hand-over where the project is and whether Bambu Studio was asked to open it", () => {
    const detail = editable().detail;
    const opened = editable({
      delivery: "bambu_studio",
      detail: { ...detail, path: "/mnt/d/Models/Hotend/box.3mf", windows_path: "D:\\Models\\Hotend\\box.3mf", opened: true },
    });
    expect(projectOutcome(opened)).toEqual({ path: "D:\\Models\\Hotend\\box.3mf", sentence: S.print.project.opened });
    const standIn = editable({ delivery: "bambu_studio", detail: { ...detail, path: "/data/handoff/box.3mf", opened: false } });
    expect(projectOutcome(standIn)).toEqual({ path: "/data/handoff/box.3mf", sentence: S.print.project.notOpened });
    const saved = editable({ delivery: "folder", detail: { ...detail, path: "/media/usb/box.3mf" } });
    expect(projectOutcome(saved)).toEqual({ path: "/media/usb/box.3mf", sentence: S.print.project.nothingOpened });
  });
});

const slice = (codes: string[] = []): SliceOutcome =>
  ({
    record: {
      project_id: PROJECT,
      version: 2,
      profile_hash: "h",
      profile: "demo-0.20mm",
      summary: null,
      job: { plate: 1, printer_id: "bambu-x2d", material: { id: "pla-basic", family: "PLA" }, pieces: ["box"], prepared_at_ms: 1 },
      created_at_ms: 9e15,
    },
    summary: {
      status: "ok",
      output: "plate.gcode.3mf",
      sha256: SHA,
      time_s: 3600,
      filament: [],
      layers: 200,
      warnings: codes.map((code) => ({ code, message: code })),
      slicer: { name: "Bambu Studio", version: "2.3.0" },
    },
    preparation: null,
    output_path: "/data/plate.gcode.3mf",
  }) as unknown as SliceOutcome;

const review = {
  job: { id: "job-1", project_id: PROJECT, version: 2, slice_sha256: SHA, note: "", status: "pending", delivery: null, created_at_ms: 1, updated_at_ms: 1, detail: {} } as unknown as PrintJob,
  delivery: { kind: "simulated_printer" },
  simulated: true,
  printer: null,
  plate: 1,
  pieces: [],
  summary: { file_name: "f", sha256: SHA, size_bytes: 1, plate: 1, estimated_seconds: 60, layers: 2, filaments: [], max_nozzle_c: 200, max_bed_c: 60, max_chamber_c: null, extent: null },
  review_text: "t",
  warnings: [],
  spools: [],
  web_numbers: [],
  handoff: null,
} as unknown as PrintReview;

/** The Print stage of the cable box with the editable project chosen. */
async function open(answers: Partial<CoreClient>, state: Partial<HotendState> = {}): Promise<HotendStore> {
  let store: HotendStore | null = null;
  function Grab() {
    const found = useHotendStore();
    useEffect(() => {
      store = found;
    }, [found]);
    const ready = useHotend((s) => s.project !== null);
    return ready ? <PrintPanel /> : null;
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
    (store as HotendStore | null)?.setState({
      slice: slice(),
      preparation: { version: 2, created_at_ms: 1, plates: [{ index: 1, size: [256, 256], items: [] }], unplaced: [] } as unknown as HotendState["preparation"],
      review,
      delivery: "bambu_studio_project",
      ...state,
    });
    await settled();
  });
  if (!store) throw new Error("the store was not created");
  return store;
}

async function click(testId: string) {
  await act(async () => {
    fireEvent.click(screen.getByTestId(testId));
    await settled();
    await settled();
  });
}

describe("the project is handed over only through the shell's confirmation", () => {
  it("is made for the plate, reviewed for Bambu Studio, and saved only after the dialog said yes", async () => {
    const handed = editable({
      delivery: "bambu_studio",
      detail: { ...editable().detail, path: "/data/handoff/box.3mf", opened: false, opener: "stand-in opener" },
    });
    const make = vi.fn(() => Promise.resolve(editable()));
    const reviewProject = vi.fn(() => Promise.resolve(projectReview({ kind: "bambu_studio" }, false)));
    const confirm = vi
      .fn<CoreClient["print_project_confirm"]>()
      .mockResolvedValueOnce({ confirmed: false, project: editable() })
      .mockResolvedValueOnce({ confirmed: true, project: handed });
    const printConfirm = vi.fn();
    await open({
      print_projects: vi.fn(() => Promise.resolve([])),
      print_project_make: make,
      print_project_review: reviewProject,
      print_project_confirm: confirm,
      print_confirm: printConfirm,
    });

    // Before anything is made: what will happen, and that it is outside Hotend's check.
    expect(screen.getByTestId("project-outside").textContent).toBe(S.print.project.outside);
    expect(screen.queryByTestId("start-print")).toBeNull();
    expect(screen.queryByTestId("project-confirm")).toBeNull();
    await click("project-make");
    expect(make).toHaveBeenCalledWith(PROJECT, 1);
    expect(reviewProject).toHaveBeenCalledWith("export-1", { kind: "bambu_studio" });
    expect(screen.getByTestId("project-made").textContent).toBe(S.print.project.made(1));
    expect(screen.getByTestId("project-review").textContent).toContain("It goes to D:\\Models\\Hotend");
    expect(screen.getByTestId("project-outside").textContent).toBe(S.print.project.outside);
    const button = screen.getByTestId("project-confirm");
    expect(button.textContent).toBe("Save for Bambu Studio…");

    // "Cancel" in the dialog: nothing saved, and the window says so.
    await click("project-confirm");
    expect(screen.getByTestId("project-note").textContent).toBe(S.print.project.notConfirmed);
    expect(screen.queryByTestId("project-outcome")).toBeNull();

    // Yes in the dialog: where it was saved, and that Bambu Studio was not opened.
    await click("project-confirm");
    expect(confirm).toHaveBeenCalledTimes(2);
    // The window sends the project and the destination, nothing else.
    for (const call of confirm.mock.calls) expect(call).toEqual(["export-1", { kind: "bambu_studio" }]);
    expect(printConfirm).not.toHaveBeenCalled();
    const outcome = screen.getByTestId("project-outcome");
    expect(outcome.textContent).toContain("/data/handoff/box.3mf");
    expect(screen.getByTestId("project-opened").textContent).toBe(S.print.project.notOpened);
    expect(outcome.textContent).toContain(S.print.project.outside);
  });

  it("finds a project made before, and saves to a folder only once one was picked in the system's dialog", async () => {
    const reviewProject = vi.fn((_id: string, destination: ProjectReview["destination"]) => Promise.resolve(projectReview(destination, false)));
    const folderPick = vi.fn(() => Promise.resolve("/media/usb"));
    await open({
      print_projects: vi.fn(() => Promise.resolve([editable({ id: "other", plate: 2 }), editable()])),
      print_project_review: reviewProject,
      folder_pick: folderPick,
    });
    expect(screen.getByTestId("project-made").textContent).toBe(S.print.project.made(1));
    const where = screen.getAllByRole("radio").filter((radio) => radio.closest("[data-testid=project-handover]"));
    expect(where).toHaveLength(2);
    await act(async () => {
      fireEvent.click(where[1] as HTMLElement);
      await settled();
    });
    expect(screen.getByTestId("project-confirm").textContent).toBe("Save project…");
    expect((screen.getByTestId("project-confirm") as HTMLButtonElement).disabled).toBe(true);
    await click("project-choose-folder");
    expect(folderPick).toHaveBeenCalledWith(S.print.project.pickTitle);
    expect(reviewProject).toHaveBeenLastCalledWith("export-1", { kind: "folder", folder: "/media/usb" });
    expect(screen.getByTestId("project-review").textContent).toContain("It goes to /media/usb");
    expect((screen.getByTestId("project-confirm") as HTMLButtonElement).disabled).toBe(false);
  });

  it("cannot be chosen with the stand-in slicer, which makes no project", async () => {
    const way = (kind: string) => ({ kind, name: kind, enabled: true, disabled_reason: null, sends_file: false, starts_print: false, controls_print: false, live_status: false, reports_result: false, can: [], cannot: [], cost: "", needs: [] });
    const printer = {
      id: "simulated-x2d",
      name: "Simulated X2D",
      model: "X2D",
      serial: "S",
      simulated: true,
      state: { phase: "idle", headline: "", progress_percent: null, remaining_minutes: null, layer: null, total_layers: null, job_name: null, bed_c: null, problems: [], ams: [] },
      deliveries: ["simulated_printer", "bambu_connect"].map(way),
    } as unknown as NonNullable<HotendState["printers"]>[number];
    await open({ print_projects: vi.fn(() => Promise.resolve([])) }, { slice: slice(["fake_slicer"]), delivery: "simulated_printer", printers: [printer] });
    const row = screen.getByTestId("delivery-project");
    expect(row.textContent).toContain(S.print.project.standIn);
    expect(row.textContent).toContain(S.print.project.outside);
    const radio = screen.getAllByRole("radio").find((one) => one.closest("label")?.contains(row));
    expect(radio?.getAttribute("aria-disabled") ?? radio?.getAttribute("data-disabled")).not.toBeNull();
    expect(screen.queryByTestId("project-handover")).toBeNull();
  });
});
