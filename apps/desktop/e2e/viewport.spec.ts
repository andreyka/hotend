// The 3D view with the cable box of the engine's own tests, in headless Chromium (WebGL
// on SwiftShader), under the window's content policy. It checks what is drawn by
// comparing pictures, and what is picked against the engine's own measurements of the
// parts.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { Page } from "@playwright/test";

import { expect, invoke, test, type Bridge } from "./bridge";

const MODEL = fileURLToPath(new URL("../../../python/tests/models/cable_box.py", import.meta.url));

type Box = { min: [number, number, number]; max: [number, number, number] };
type Built = { projectId: string; boxes: Record<string, Box> };

/** Builds the cable box through the core, as the agent's tool would. */
async function buildCableBox(bridge: Bridge): Promise<Built> {
  const project = await invoke<{ id: string }>(bridge, "project_create", { name: "Cable box" });
  const report = await invoke<{ version: { number: number } | null }>(bridge, "model_build", {
    projectId: project.id,
    code: readFileSync(MODEL, "utf8"),
    params: {},
    note: "viewport test",
  });
  expect(report.version?.number).toBe(1);
  const detail = await invoke<{ result: { parts: { name: string; bbox: Box }[] } }>(
    bridge,
    "version_get",
    { projectId: project.id, number: 1 },
  );
  const boxes = Object.fromEntries(detail.result.parts.map((part) => [part.name, part.bbox]));
  return { projectId: project.id, boxes };
}

async function openDesign(page: Page, bridge: Bridge, projectId: string) {
  const query = new URLSearchParams({ bridge: bridge.url, token: bridge.token });
  await page.goto(`/?${query.toString()}#/hotend/p/${projectId}/design`);
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", "2", {
    timeout: 60_000,
  });
}

/** The share of pixels that differ between two pictures of the same size, from 0 to 1. */
async function difference(page: Page, a: Buffer, b: Buffer): Promise<number> {
  return page.evaluate(
    async ([first, second]) => {
      const pixels = async (png: string) => {
        const image = new Image();
        image.src = `data:image/png;base64,${png}`;
        await image.decode();
        const canvas = document.createElement("canvas");
        canvas.width = image.width;
        canvas.height = image.height;
        const context = canvas.getContext("2d");
        if (!context) throw new Error("no 2D canvas");
        context.drawImage(image, 0, 0);
        return context.getImageData(0, 0, image.width, image.height).data;
      };
      const [one, two] = await Promise.all([pixels(first), pixels(second)]);
      if (one.length !== two.length) throw new Error("pictures of different sizes");
      let differing = 0;
      for (let index = 0; index < one.length; index += 4) {
        const delta =
          Math.abs((one[index] ?? 0) - (two[index] ?? 0)) +
          Math.abs((one[index + 1] ?? 0) - (two[index + 1] ?? 0)) +
          Math.abs((one[index + 2] ?? 0) - (two[index + 2] ?? 0));
        if (delta > 24) differing += 1;
      }
      return differing / (one.length / 4);
    },
    [a.toString("base64"), b.toString("base64")] as const,
  );
}

type Pick = { part: string; kind: string; path: string; point: number[] | null };

/** Clicks a point of the stage and reads what the interface says was picked. */
async function clickAt(page: Page, x: number, y: number): Promise<Pick | null> {
  const host = page.getByTestId("viewport-host");
  const before = Number((await host.getAttribute("data-clicks")) ?? 0);
  await page.mouse.click(x, y);
  await expect(host).toHaveAttribute("data-clicks", String(before + 1));
  const chip = page.getByTestId("picked");
  // The chip follows the store within a frame.
  await page.evaluate(() => new Promise(requestAnimationFrame));
  if ((await chip.count()) === 0) return null;
  const point = await chip.getAttribute("data-point");
  return {
    part: (await chip.getAttribute("data-part")) ?? "",
    kind: (await chip.getAttribute("data-kind")) ?? "",
    path: (await chip.getAttribute("data-path")) ?? "",
    point: point === null ? null : point.split(",").map(Number),
  };
}

function inside(box: Box, point: number[], slack: number): boolean {
  return point.every(
    (value, axis) => value >= (box.min[axis] ?? 0) - slack && value <= (box.max[axis] ?? 0) + slack,
  );
}

test("the cable box is drawn, parts hide, and a click names the right part", async ({
  page,
  bridge,
  violations,
}) => {
  void violations;
  const { projectId, boxes } = await buildCableBox(bridge);
  const box = boxes.box;
  const lid = boxes.lid;
  if (!box || !lid) throw new Error("the cable box has a box and a lid");
  await openDesign(page, bridge, projectId);

  const stage = page.getByTestId("viewport");
  const canvas = stage.locator("canvas").first();
  await expect(canvas).toBeVisible();
  const toggle = (name: string, to: "Hide" | "Show") => page.getByRole("button", { name: `${to} ${name}` }).click();
  /** Moves the pointer off the part: the viewer tints what the pointer is over. */
  const park = async () => {
    const area = await canvas.boundingBox();
    if (!area) throw new Error("the canvas has a size");
    await page.mouse.move(area.x + area.width / 2, area.y + area.height - 3);
    await page.evaluate(() => new Promise(requestAnimationFrame));
  };
  /** The canvas alone: the controls that float over the stage are not the drawing. */
  const picture = () => canvas.screenshot();

  // Nothing of the viewer's own interface is on the stage: no toolbar, no tree, no tabs.
  for (const name of ["tcv_cad_toolbar", "tcv_cad_navigation"]) {
    await expect(stage.locator(`.${name}`)).toHaveCount(1);
    await expect(stage.locator(`.${name}`)).toBeHidden();
  }

  // Drawn: the stage with the parts differs from the stage with both hidden, and hiding
  // only the lid changes the picture again.
  await park();
  const full = await picture();
  await toggle("lid", "Hide");
  await park();
  const withoutLid = await picture();
  await toggle("box", "Hide");
  await park();
  const empty = await picture();
  expect(await difference(page, full, empty)).toBeGreaterThan(0.08);
  expect(await difference(page, full, withoutLid)).toBeGreaterThan(0.02);
  expect(await difference(page, withoutLid, empty)).toBeGreaterThan(0.04);

  // Nothing drawn, nothing picked.
  const area = await canvas.boundingBox();
  if (!area) throw new Error("the canvas has a size");
  const centre = { x: area.x + area.width / 2, y: area.y + area.height / 2 };
  expect(await clickAt(page, centre.x, centre.y)).toBeNull();

  // Both parts back. Lines of clicks across the middle of the stage: every answer names
  // a part, and the point that was hit lies in that part as the engine measured it.
  await toggle("lid", "Show");
  await toggle("box", "Show");
  const picks: Pick[] = [];
  for (let step = 0; step <= 40; step += 1) {
    const x = area.x + area.width * (0.25 + (0.5 * step) / 40);
    for (const y of [centre.y - area.height * 0.1, centre.y, centre.y + area.height * 0.1]) {
      const pick = await clickAt(page, x, y);
      if (pick) picks.push(pick);
    }
  }
  expect(picks.length).toBeGreaterThan(30);
  for (const pick of picks) {
    expect(["box", "lid"]).toContain(pick.part);
    expect(pick.path.startsWith(`/model/${pick.part}/`), pick.path).toBe(true);
    expect(pick.point, "SwiftShader gives the hit point").not.toBeNull();
    const point = pick.point ?? [];
    const own = pick.part === "box" ? box : lid;
    expect(inside(own, point, 0.5), `${pick.part} at ${point.join(", ")}`).toBe(true);
    // Below the lid's lowest point only the box can be hit, above the box only the lid.
    const z = point[2] ?? 0;
    if (z < lid.min[2] - 0.5) expect(pick.part).toBe("box");
    if (z > box.max[2] + 0.5) expect(pick.part).toBe("lid");
  }
  const kinds = (part: string) => new Set(picks.filter((pick) => pick.part === part).map((pick) => pick.kind));
  expect([...kinds("lid")]).toContain("face");
  expect([...kinds("box")]).toContain("face");
  expect(picks.some((pick) => pick.kind === "face" && /\/faces\/faces_\d+$/.test(pick.path))).toBe(true);

  // What was clicked goes into the message box as words, and the chip can be cleared.
  const last = await clickAt(page, centre.x, centre.y);
  expect(last).not.toBeNull();
  await page.getByRole("button", { name: "Mention in message" }).click();
  await expect(page.getByTestId("conversation").getByRole("textbox")).toHaveValue(new RegExp(`^On the ${last?.part ?? ""}, at (the|a|an) `));
  await page.getByTestId("picked").getByRole("button", { name: "Clear" }).click();
  await expect(page.getByTestId("picked")).toHaveCount(0);

  // The viewer's own status line stays off, because Hotend names the pick itself.
  await page.mouse.move(centre.x, centre.y);
  await expect(stage.locator(".tcv_status_line")).toHaveCount(1);
  await expect(stage.locator(".tcv_status_line")).toBeHidden();

  // With the lid hidden the same clicks only ever name the box.
  await toggle("lid", "Hide");
  let boxOnly = 0;
  for (let step = 0; step <= 20; step += 1) {
    const pick = await clickAt(page, area.x + area.width * (0.3 + (0.4 * step) / 20), centre.y);
    if (!pick) continue;
    expect(pick.part).toBe("box");
    boxOnly += 1;
  }
  expect(boxOnly).toBeGreaterThan(5);
  await toggle("lid", "Show");
  await page.getByTestId("picked").getByRole("button", { name: "Clear" }).click();

  // Standard views and Fit move the camera: the picture changes, and comes back.
  await stage.focus();
  await page.keyboard.press("3");
  await park();
  const top = await picture();
  expect(await difference(page, full, top)).toBeGreaterThan(0.05);
  // From the top only the lid can be seen in the middle of the stage.
  expect((await clickAt(page, centre.x, centre.y))?.part).toBe("lid");
  await page.getByTestId("picked").getByRole("button", { name: "Clear" }).click();
  await stage.focus();
  await page.keyboard.press("1");
  await park();
  expect(await difference(page, full, await picture())).toBeLessThan(0.03);

  // Cut open takes part of the drawing away, and gives it back.
  await page.keyboard.press("c");
  await park();
  expect(await difference(page, full, await picture())).toBeGreaterThan(0.03);
  await stage.focus();
  await page.keyboard.press("c");
  await park();
  expect(await difference(page, full, await picture())).toBeLessThan(0.03);

  // Dragging turns the part and is not a pick.
  const clicks = await page.getByTestId("viewport-host").getAttribute("data-clicks");
  await page.mouse.move(centre.x, centre.y);
  await page.mouse.down();
  await page.mouse.move(centre.x + 120, centre.y + 30, { steps: 6 });
  await page.mouse.up();
  await park();
  const turned = await picture();
  expect(await difference(page, full, turned)).toBeGreaterThan(0.05);
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-clicks", clicks ?? "");
  await expect(page.getByTestId("picked")).toHaveCount(0);

  // The viewer marks <body> with its own theme while it is drawn. Away from the design
  // the mark is gone, so the theme of the other screens is the root element's alone.
  await expect(page.locator("body")).toHaveAttribute("data-theme", /dark|light/);
  await page.getByTestId("home-button").click();
  await expect(page.getByTestId("viewport-host")).toHaveCount(0);
  await expect(page.locator("body")).not.toHaveAttribute("data-theme", /.*/);
});

// three-cad-viewer writes the names of a mesh into the page as HTML. The mesh is written
// by a sandboxed worker and is hostile input (IMPLEMENTATION_BRIEF 4, rule 2): a name
// that is not a part's never reaches the viewer, whatever the core handed over.
for (const [what, name] of [
  ["markup", "box<img id=injected-img src=x><a id=injected-a href=https:example.invalid>link"],
  ["a quote", 'box"]<b id="injected-b">'],
] as const) {
  test(`a mesh whose part name holds ${what} is not drawn and adds nothing to the page`, async ({ page, bridge, violations }) => {
    void violations;
    const { projectId } = await buildCableBox(bridge);
    await page.route("**/invoke/version_mesh", async (route) => {
      const response = await route.fetch();
      const answer = (await response.json()) as { ok: boolean; data: string };
      const mesh = JSON.parse(answer.data) as { shapes: { name: string; parts: { name: string }[] } };
      const first = mesh.shapes.parts[0];
      if (!first) throw new Error("the cable box has parts");
      first.name = name;
      answer.data = JSON.stringify(mesh);
      await route.fulfill({ response, json: answer });
    });
    const query = new URLSearchParams({ bridge: bridge.url, token: bridge.token });
    await page.goto(`/?${query.toString()}#/hotend/p/${projectId}/design`);

    // The stage says that it has nothing to show; the rest of the project is there.
    await expect(page.getByTestId("stage").getByText("This version has no 3D view.")).toBeVisible({ timeout: 60_000 });
    await expect(page.getByTestId("inspector")).toBeVisible();
    await expect(page.locator("[id^=injected-]")).toHaveCount(0);
    await expect(page.locator(".tv-node-label")).toHaveCount(0);
    await expect(page.getByText("A screen could not be drawn.")).toHaveCount(0);
  });
}
