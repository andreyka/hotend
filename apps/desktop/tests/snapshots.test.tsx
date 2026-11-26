// Snapshots beside the version rail: the name check (the core's rule), the list, and what
// each row does, against stand-in answers of the snapshot commands; that the panel reaches
// the shell's own commands through the client; and the plain disabled state for a window
// whose shell does not carry them.
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CoreContext, PendingContext } from "@/host/core-context";
import type { CoreClient } from "@/lib/core";
import { NOT_AVAILABLE, type PendingCommands, type Snapshot, type SnapshotExport } from "@/lib/core/pending";
import { SnapshotsPanel } from "@/hotend/Snapshots";
import { checkSnapshotName, MAX_SNAPSHOT_NAME } from "@/hotend/snapshots";
import { S } from "@/hotend/strings";

import { settled } from "./cable-box";
import { fakeClient } from "./fake-client";
import { standInPending } from "./fake-pending";

afterEach(cleanup);

const PROJECT = "p-1";

const snap = (name: string, version: number, printed = false, id = name): Snapshot => ({
  id,
  project_id: PROJECT,
  name,
  version,
  note: "",
  created_at_ms: Date.now() - 10 * 60_000,
  printed_job_id: printed ? "job-1" : null,
  printed_sha256: printed ? "ab".repeat(32) : null,
});

async function show(pending: PendingCommands | null, client: Partial<CoreClient> = {}, current: number | null = 3) {
  const onRestored = vi.fn();
  const core = fakeClient(client);
  render(
    <CoreContext value={core}>
      <PendingContext value={pending}>
        <SnapshotsPanel projectId={PROJECT} current={current} onRestored={onRestored} />
      </PendingContext>
    </CoreContext>,
  );
  await act(async () => {
    await settled();
  });
  return { onRestored, core };
}

async function click(testId: string, index = 0) {
  await act(async () => {
    fireEvent.click(screen.getAllByTestId(testId)[index] as HTMLElement);
    await settled();
  });
}

describe("a snapshot name is checked as the core checks it", () => {
  it("takes one plain line of 1 to 80 characters, trimmed", () => {
    expect(checkSnapshotName("  Lid fits  ", [])).toEqual({ name: "Lid fits", problem: null });
    expect(checkSnapshotName("Ünïcode lid, v2 (wide)", []).problem).toBeNull();
    expect(checkSnapshotName("x".repeat(MAX_SNAPSHOT_NAME), []).problem).toBeNull();
    // Characters, not UTF-16 units: 80 emoji are 80 characters.
    expect(checkSnapshotName("\u{1F600}".repeat(MAX_SNAPSHOT_NAME), []).problem).toBeNull();
    expect(checkSnapshotName("x".repeat(MAX_SNAPSHOT_NAME + 1), []).problem).toBe("long");
    expect(checkSnapshotName("   ", []).problem).toBe("empty");
  });

  it("refuses line breaks, tabs, wide spaces and invisible or direction-changing characters", () => {
    for (const name of ["a\nb", "a\tb", "a b", "a　b", "a​b", "a‮b", "﻿lid", "a\u{E0041}b", "a­b"]) {
      expect(checkSnapshotName(name, []).problem, JSON.stringify(name)).toBe("plain");
    }
  });

  it("refuses a name the project has, without regard to ASCII case", () => {
    expect(checkSnapshotName("lid FITS", [snap("Lid fits", 2)]).problem).toBe("taken");
    // Only ASCII letters are folded, as in the core.
    expect(checkSnapshotName("ÜBER", [snap("über", 2)]).problem).toBeNull();
  });
});

describe("the snapshots list", () => {
  it("calls the shell's snapshot commands through the client", async () => {
    // No stand-in: the commands of the client, as the window has them.
    const list = vi.fn(() => Promise.resolve([snap("Lid fits", 2)]));
    const exportRun = vi.fn(() =>
      Promise.resolve<SnapshotExport>({ snapshot: snap("Lid fits", 2), folder: "/o/x", files: [], printed: null, printed_missing: null }),
    );
    const folderPick = vi.fn(() => Promise.resolve("/home/me/out"));
    const { core } = await show(null, { snapshots_list: list, snapshot_export: exportRun, folder_pick: folderPick });
    expect(list).toHaveBeenCalledWith(PROJECT);
    expect(screen.getAllByTestId("snapshot-row").map((row) => row.getAttribute("data-name"))).toEqual(["Lid fits"]);
    await click("snapshot-export");
    expect(core.folder_pick).toHaveBeenCalledWith("Save snapshot files");
    expect(exportRun).toHaveBeenCalledWith(PROJECT, "Lid fits", "/home/me/out");
  });

  it("says plainly that snapshots are not available in a window without them, with its buttons off", async () => {
    await show(standInPending({ snapshots_list: vi.fn(() => Promise.reject(NOT_AVAILABLE)) }));
    expect(screen.getByTestId("snapshots").getAttribute("data-state")).toBe("unavailable");
    expect(screen.getByTestId("snapshots-unavailable").textContent).toBe(S.snapshots.notAvailable);
    expect((screen.getByTestId("snapshot-take") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByTestId("snapshot-list")).toBeNull();
  });

  it("lists the snapshots newest first with their version, date and a printed badge", async () => {
    const pending = standInPending({
      snapshots_list: vi.fn(() => Promise.resolve([snap("First try", 1), snap("Lid fits", 3, true)])),
    });
    await show(pending);
    const rows = screen.getAllByTestId("snapshot-row");
    expect(rows.map((row) => row.getAttribute("data-name"))).toEqual(["Lid fits", "First try"]);
    expect(rows[0]?.textContent).toContain("v3");
    expect(rows[0]?.textContent).toMatch(/v3\d+ min ago/);
    expect(rows[0]?.querySelector("[data-testid=snapshot-printed]")?.textContent).toBe(S.snapshots.printed);
    expect(rows[1]?.querySelector("[data-testid=snapshot-printed]")).toBeNull();
    // The current version needs no going back to.
    expect(rows[0]?.querySelector("[data-testid=snapshot-restore]")).toBeNull();
    expect(rows[1]?.querySelector("[data-testid=snapshot-restore]")?.textContent).toBe(S.snapshots.restore);
    expect(rows[0]?.querySelector("[data-testid=snapshot-delete]")?.textContent).toBe(S.snapshots.remove);
  });

  it("says when there are none", async () => {
    await show(standInPending({ snapshots_list: vi.fn(() => Promise.resolve([])) }));
    expect(screen.getByTestId("snapshots-empty").textContent).toBe(S.snapshots.none);
  });

  it("asks for a name, checks it, and makes the snapshot of the current version", async () => {
    let list = [snap("Lid fits", 2)];
    const create = vi.fn((_project: string, name: string, version: number | null) => {
      const made = snap(name, version ?? 0);
      list = [...list, made];
      return Promise.resolve(made);
    });
    const pending = standInPending({ snapshots_list: vi.fn(() => Promise.resolve(list)), snapshot_create: create });
    await show(pending);
    await click("snapshot-take");
    const field = screen.getByTestId<HTMLInputElement>("snapshot-name");
    expect(field.type).toBe("text");
    // Empty: said only once the maker tries.
    expect(screen.getByTestId("snapshot-name-note").textContent).toBe(S.snapshots.nameHint(80));
    await click("snapshot-save");
    expect(screen.getByTestId("snapshot-name-note").textContent).toBe(S.snapshots.invalid.empty);
    fireEvent.change(field, { target: { value: "LID FITS" } });
    expect(screen.getByTestId("snapshot-name-note").textContent).toBe(S.snapshots.invalid.taken);
    expect((screen.getByTestId("snapshot-save") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(field, { target: { value: "x".repeat(81) } });
    expect(screen.getByTestId("snapshot-name-note").textContent).toBe(S.snapshots.invalid.long(80));
    fireEvent.change(field, { target: { value: "  Before the hinge  " } });
    await click("snapshot-save");
    expect(create).toHaveBeenCalledWith(PROJECT, "Before the hinge", 3, "");
    expect(screen.queryByTestId("snapshot-form")).toBeNull();
    expect(screen.getAllByTestId("snapshot-row").map((row) => row.getAttribute("data-name"))).toEqual(["Before the hinge", "Lid fits"]);
  });

  it("exports into a folder picked under its own title, and shows what was written", async () => {
    const result: SnapshotExport = {
      snapshot: snap("Lid fits", 2, true),
      folder: "/home/me/out/Box - Lid fits (v2)",
      files: [
        { kind: "step", source: "exported", path: "/o/model.step", bytes: 10, sha256: "a" },
        { kind: "3mf", source: "copied", path: "/o/model.3mf", bytes: 10, sha256: "b" },
        { kind: "manifest", source: "written", path: "/o/snapshot.json", bytes: 10, sha256: "c" },
      ],
      printed: null,
      printed_missing: { job_id: "job-1", sha256: "ab", message: "printed file missing: ..." },
    };
    const exportRun = vi.fn(() => Promise.resolve(result));
    const folderPick = vi.fn(() => Promise.resolve("/home/me/out"));
    await show(standInPending({ snapshots_list: vi.fn(() => Promise.resolve([snap("Lid fits", 2, true)])), snapshot_export: exportRun }), {
      folder_pick: folderPick,
    });
    await click("snapshot-export");
    expect(folderPick).toHaveBeenCalledWith("Save snapshot files");
    expect(S.snapshots.exportTitle).toBe("Save snapshot files");
    expect(exportRun).toHaveBeenCalledWith(PROJECT, "Lid fits", "/home/me/out");
    const shown = screen.getByTestId("snapshot-export-result");
    expect(shown.textContent).toContain("/home/me/out/Box - Lid fits (v2)");
    expect(screen.getByTestId("snapshot-export-sources").textContent).toBe("3 files · 1 copied as stored, 1 made now");
    expect(screen.getAllByTestId("snapshot-export-file").map((li) => li.textContent)).toEqual([
      "STEP · exported",
      "3MF · copied",
      "snapshot.json · written",
    ]);
    expect(screen.getByTestId("snapshot-printed-missing").textContent).toBe(S.snapshots.printedMissing);
  });

  it("exports nothing when the folder picker is cancelled", async () => {
    const exportRun = vi.fn();
    await show(standInPending({ snapshots_list: vi.fn(() => Promise.resolve([snap("Lid fits", 2)])), snapshot_export: exportRun }), {
      folder_pick: vi.fn(() => Promise.resolve(null)),
    });
    await click("snapshot-export");
    expect(exportRun).not.toHaveBeenCalled();
    expect(screen.queryByTestId("snapshot-export-result")).toBeNull();
  });

  it("goes back to a snapshot and has the window read the project again", async () => {
    const restore = vi.fn(() => Promise.resolve({} as never));
    const { onRestored } = await show(standInPending({ snapshots_list: vi.fn(() => Promise.resolve([snap("First try", 1)])), snapshot_restore: restore }));
    await click("snapshot-restore");
    expect(restore).toHaveBeenCalledWith(PROJECT, "First try");
    expect(onRestored).toHaveBeenCalledTimes(1);
  });

  it("deletes the name only after asking", async () => {
    let list = [snap("First try", 1)];
    const remove = vi.fn(() => {
      const gone = list[0] as Snapshot;
      list = [];
      return Promise.resolve(gone);
    });
    await show(standInPending({ snapshots_list: vi.fn(() => Promise.resolve(list)), snapshot_delete: remove }));
    await click("snapshot-delete");
    expect(remove).not.toHaveBeenCalled();
    expect(screen.getByRole("group", { name: S.snapshots.removeSure }).textContent).toContain(S.snapshots.removeHint);
    await click("snapshot-delete-sure");
    expect(remove).toHaveBeenCalledWith(PROJECT, "First try");
    expect(screen.getByTestId("snapshots-empty")).toBeTruthy();
  });

  it("shows the core's refusal in its own words", async () => {
    const refused = { kind: "invalid_input", message: "the name is taken by v2", next_step: "Choose another name." };
    await show(
      standInPending({
        snapshots_list: vi.fn(() => Promise.resolve([])),
        snapshot_create: vi.fn(() => Promise.reject(refused)),
      }),
    );
    await click("snapshot-take");
    fireEvent.change(screen.getByTestId("snapshot-name"), { target: { value: "Lid" } });
    await click("snapshot-save");
    expect(screen.getByRole("alert").textContent).toContain("the name is taken by v2");
    // The form stays, with the name, to be changed.
    expect(screen.getByTestId<HTMLInputElement>("snapshot-name").value).toBe("Lid");
  });
});
