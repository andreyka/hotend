// More of what was once broken, as tests in headless Chromium against the bridge: the bed
// stays quiet through every resize, a print keeps its own plate when the design moves on,
// a run survives a reload, a decimal comma is a decimal mark, the keyboard focus stays
// with the maker, and Home and Settings are laid out on one axis.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { Page } from "@playwright/test";

import { expect, invoke, test, type Bridge } from "./bridge";

const MODEL = fileURLToPath(new URL("../../../python/tests/models/cable_box.py", import.meta.url));
const MINUTE = 60_000;

test.use({ bridgeArgs: ["--script", "builtin:cable_box_demo", "--answer-dialogs-for-tests", "--time-scale", "10"] });

async function buildCableBox(bridge: Bridge): Promise<string> {
  const project = await invoke<{ id: string }>(bridge, "project_create", { name: "Cable box" });
  const report = await invoke<{ version: { number: number } | null }>(bridge, "model_build", {
    projectId: project.id,
    code: readFileSync(MODEL, "utf8"),
    params: {},
    note: "polish 2 test",
  });
  expect(report.version?.number).toBe(1);
  return project.id;
}

async function open(page: Page, bridge: Bridge, route: string) {
  const query = new URLSearchParams({ bridge: bridge.url, token: bridge.token });
  await page.goto(`/?${query.toString()}#${route}`);
}

async function openDesign(page: Page, bridge: Bridge, projectId: string) {
  await open(page, bridge, `/hotend/p/${projectId}/design`);
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", "2", { timeout: MINUTE });
}

/** How many pixels of the 3D view are a saturated red or green: the viewer's axis lines. */
async function axisPixels(page: Page): Promise<number> {
  const png = (await page.getByTestId("viewport").locator("canvas").first().screenshot()).toString("base64");
  return page.evaluate(async (data) => {
    const image = new Image();
    image.src = `data:image/png;base64,${data}`;
    await image.decode();
    const canvas = document.createElement("canvas");
    canvas.width = image.width;
    canvas.height = image.height;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("no 2D canvas");
    context.drawImage(image, 0, 0);
    const pixels = context.getImageData(0, 0, image.width, image.height).data;
    let count = 0;
    for (let index = 0; index < pixels.length; index += 4) {
      const [r = 0, g = 0, b = 0] = [pixels[index], pixels[index + 1], pixels[index + 2]];
      if ((r > 170 && g < 90 && b < 90) || (g > 140 && r < 90 && b < 90)) count += 1;
    }
    return count;
  }, png);
}

test("the bed has no tick numbers and no axis lines after a resize, a column toggle or Fit, in Design and in Prepare", async ({
  page,
  bridge,
  violations,
}) => {
  void violations;
  test.setTimeout(5 * MINUTE);
  await page.setViewportSize({ width: 1280, height: 800 });
  const projectId = await buildCableBox(bridge);
  await openDesign(page, bridge, projectId);
  const host = page.getByTestId("viewport-host");

  // The stage as it is drawn now: framed for its own size, no numbers, no solid axes.
  const quiet = async (after: string) => {
    await expect
      .poll(() => host.evaluate((element) => element.dataset.framed === `${String(element.clientWidth)}x${String(element.clientHeight)}`), {
        message: `framed for the stage after ${after}`,
      })
      .toBe(true);
    // The grid the viewer makes anew arrives a moment after the size: wait for it, then
    // have a frame drawn.
    await page.waitForTimeout(500);
    const area = await host.boundingBox();
    if (!area) throw new Error("the stage has a size");
    await page.mouse.move(area.x + area.width / 2, area.y + area.height - 4);
    await page.mouse.move(area.x + area.width / 2 + 3, area.y + area.height - 6);
    await page.waitForTimeout(200);
    await expect(host, `tick numbers after ${after}`).toHaveAttribute("data-ticks", "0");
    expect(await axisPixels(page), `axis lines after ${after}`).toBe(0);
  };

  await quiet("opening");
  for (const width of [1440, 1100, 1280]) {
    await page.setViewportSize({ width, height: width === 1100 ? 700 : 800 });
    await quiet(`a resize to ${String(width)}`);
  }
  await page.getByTestId("toggle-conversation").click();
  await quiet("closing the conversation");
  await page.getByTestId("toggle-conversation").click();
  await quiet("opening the conversation");
  await page.getByTestId("viewport").focus();
  await page.keyboard.press("f");
  await quiet("Fit");

  await page.getByTestId("stage-prepare").click();
  await expect(page.getByTestId("prepare-panel")).toHaveAttribute("data-pieces", "2", { timeout: 2 * MINUTE });
  await expect(host).toHaveAttribute("data-camera", "plate");
  await quiet("laying out the plate");
  // The view starts where the stage starts: what Hotend draws over and under it is in place.
  expect(
    await host.evaluate((element) => {
      const [stage, canvas] = [element.getBoundingClientRect(), element.querySelector("canvas")?.getBoundingClientRect()];
      return canvas ? [canvas.x - stage.x, canvas.y - stage.y] : null;
    }),
  ).toEqual([0, 0]);
  // The plate has a surface, and the whole of it is in view: its four corners are inside the stage.
  const whole = async (after: string) => {
    const points = ((await page.getByTestId("plate-surface").getAttribute("points")) ?? "").split(" ").map((pair) => pair.split(",").map(Number));
    const area = await host.boundingBox();
    if (!area) throw new Error("the stage has a size");
    expect(points, `the plate's corners after ${after}`).toHaveLength(4);
    for (const [x = -1, y = -1] of points) {
      expect(x).toBeGreaterThanOrEqual(8);
      expect(x).toBeLessThanOrEqual(area.width - 8);
      expect(y).toBeGreaterThanOrEqual(8);
      expect(y).toBeLessThanOrEqual(area.height - 8);
    }
  };
  await whole("laying out the plate");
  await page.setViewportSize({ width: 1440, height: 800 });
  await quiet("a resize on the plate");
  await whole("a resize to 1440");
  await page.getByTestId("toggle-inspector").click();
  await quiet("closing the inspector on the plate");
  await whole("closing the inspector");
});

test("a print keeps its own plate and version when the design changes, through a reload, and Home says what is going on", async ({
  page,
  bridge,
  violations,
}) => {
  void violations;
  test.setTimeout(10 * MINUTE);
  await page.setViewportSize({ width: 1280, height: 800 });
  const projectId = await buildCableBox(bridge);
  await open(page, bridge, `/hotend/p/${projectId}/prepare`);
  await expect(page.getByTestId("prepare-panel")).toHaveAttribute("data-pieces", "2", { timeout: 2 * MINUTE });
  // What the plate warns about is answered where the slice is set up.
  await expect(page.getByTestId("supports-row")).toContainText(/Needed under about \d+/);
  // One layout for every row: the name on its own line, the controls under it at the right.
  const rows = await page.evaluate(() =>
    [...document.querySelectorAll("[data-testid^=piece-]")].flatMap((row) => {
      const name = row.querySelector("[data-testid=piece-name]");
      const controls = row.querySelector("[data-testid=piece-controls]");
      if (!name || !controls) return [];
      return [{ below: controls.getBoundingClientRect().top >= name.getBoundingClientRect().bottom - 1, right: Math.round(controls.getBoundingClientRect().right) }];
    }),
  );
  expect(rows).toHaveLength(2);
  expect(rows.every((row) => row.below)).toBe(true);
  expect(new Set(rows.map((row) => row.right)).size).toBe(1);
  // Pointing at a piece's row outlines that piece on the plate.
  await page.getByTestId("piece-lid").hover();
  await expect(page.locator(".hotend-mark-box")).toHaveCount(1);
  await page.getByTestId("verdict").hover();
  await expect(page.locator(".hotend-mark-box")).toHaveCount(0);

  await page.getByTestId("verdict-primary").click();
  await expect(page.getByTestId("slice-time")).toBeVisible({ timeout: 2 * MINUTE });
  // The stand-in is said once, with the way to a real file.
  await expect(page.getByTestId("use-studio")).toBeVisible();
  await page.getByTestId("verdict-primary").click();
  await expect(page.getByTestId("start-print")).toBeEnabled({ timeout: MINUTE });
  // A requirement that is not met is said above the guesses and the button. The
  // example meets its inside size, so the review names no unmet requirement.
  await expect(page.getByTestId("review-unmet")).toHaveCount(0);
  await expect(page.getByTestId("review-facts")).not.toContainText("not stated");
  await page.getByTestId("start-print").click();
  await expect(page.getByTestId("running")).toBeVisible({ timeout: MINUTE });
  await expect(page.getByTestId("print-percent")).toBeVisible({ timeout: 2 * MINUTE });
  await expect(page.getByTestId("spool-in-use")).toHaveCount(1);

  // The design moves on while the print runs: a size changes, with a decimal comma.
  await page.getByTestId("stage-design").click();
  const height = page.getByTestId("size-height").locator("input").first();
  await expect(height).toHaveValue("40");
  await height.fill("46,5");
  await height.press("Enter");
  await expect(page.getByTestId("dot-2")).toBeVisible({ timeout: 2 * MINUTE });
  await expect(height).toHaveValue("46.5");
  await expect(page.getByTestId("overall-size")).toContainText("48.9");

  // Print still shows what is being printed: v1's plate, named as v1.
  await page.getByTestId("stage-print").click();
  const host = page.getByTestId("viewport-host");
  const printing = async () => {
    await expect(page.getByTestId("running")).toBeVisible({ timeout: MINUTE });
    await expect(host).toHaveAttribute("data-parts", "2", { timeout: MINUTE });
    await expect(page.getByTestId("job-version-note")).toContainText("Printing v1. Your design is now v2.");
    await expect(page.getByTestId("rail-label")).toHaveAttribute("data-job-version", "1");
    // v1's material, as the stand-in slicer estimates it: box 44 553 mm³ and lid 21 451 mm³,
    // 35 % of that extruded, PLA at 1.24 g/cm³: 28.6 g.
    await expect(page.getByTestId("inspector")).toContainText("28.6 g");
  };
  await printing();
  await page.reload();
  await printing();

  // Home says that a print is running, as simulated, and leads back to it.
  await page.getByTestId("home-button").click();
  const status = page.getByTestId("project-card").getByTestId("card-status");
  await expect(status).toHaveAttribute("data-state", "printing");
  await expect(status).toContainText(/Simulated print · \d+\s%/);
  await expect(status.locator("[data-status=sim]")).toHaveCount(1);
  await page.getByTestId("project-card").getByRole("link").click();
  await expect(page).toHaveURL(/\/print$/);
  await printing();

  // Stopped: one sentence of the window's own, one "Review again", an icon on the verdict.
  await page.getByTestId("stop-print").click();
  await page.getByTestId("stop-print-sure").click();
  await expect(page.getByTestId("outcome")).toHaveAttribute("data-status", "cancelled", { timeout: MINUTE });
  await expect(page.getByTestId("stopped-at")).toContainText("You stopped the simulated print");
  await expect(page.getByRole("button", { name: "Review again" })).toHaveCount(1);
  await expect(page.getByTestId("verdict")).toHaveAttribute("data-kind", "sim");
  // After the print, the plate of the design as it is now is not shown as the print's.
  await expect(page.getByTestId("job-version-note")).toContainText("This print is of v1.");
});

test("a run in progress is still there after a reload and after Home and back, and Stop gives the focus to the composer", async ({
  page,
  bridge,
  violations,
}) => {
  void violations;
  test.setTimeout(10 * MINUTE);
  await page.setViewportSize({ width: 1280, height: 800 });
  await open(page, bridge, "/hotend");
  const prompt = page.getByTestId("home-prompt");
  await prompt.fill("A box that hides a power strip, with a snap-on lid");
  await prompt.press("Enter");
  await expect(page).toHaveURL(/#\/hotend\/p\/[0-9a-f-]+\/design/);
  await expect(page.getByTestId("stop-run")).toBeVisible({ timeout: MINUTE });

  // A reload in the middle of the run: Hotend is still working, and says so.
  await page.reload();
  await expect(page.getByTestId("stop-run")).toBeVisible({ timeout: MINUTE });
  await expect(page.getByTestId("live-run")).toHaveAttribute("data-status", "running");

  // Home and back: the card says that Hotend is working, and the run is still shown.
  await page.getByTestId("home-button").click();
  const status = page.getByTestId("project-card").getByTestId("card-status");
  await expect(status).toHaveAttribute("data-state", "working");
  await expect(status).toContainText("Hotend is working");
  await page.getByTestId("project-card").getByRole("link").click();
  await expect(page.getByTestId("stop-run")).toBeVisible({ timeout: MINUTE });

  await page.getByTestId("stop-run").click();
  await expect(page.getByTestId("stop-run")).toHaveCount(0, { timeout: MINUTE });
  await expect(page.locator("#hotend-composer")).toBeFocused();
});

test("the keyboard focus stays with the maker: Look at an older version, and the Export popover", async ({ page, bridge, violations }) => {
  void violations;
  test.setTimeout(5 * MINUTE);
  await page.setViewportSize({ width: 1280, height: 800 });
  const projectId = await buildCableBox(bridge);
  await invoke(bridge, "parameter_set", { projectId, name: "height", value: 46 });
  await openDesign(page, bridge, projectId);
  await expect(page.getByTestId("dot-2")).toBeVisible();

  // The rail: to the older dot with the arrow keys, into its card, Enter on "Look at v1".
  await page.getByTestId("dot-2").focus();
  await page.keyboard.press("ArrowLeft");
  await expect(page.getByTestId("dot-1")).toBeFocused();
  await expect(page.getByTestId("version-card")).toHaveAttribute("data-version", "1");
  await page.getByTestId("look-at").focus();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/[?&]v=1/);
  await expect(page.getByTestId("dot-1")).toBeFocused();
  // The focus ring of a dot is drawn inside the strip, where nothing cuts it off.
  expect(await page.getByTestId("dot-1").evaluate((dot) => Number.parseFloat(getComputedStyle(dot).outlineOffset))).toBeLessThan(0);
  await page.getByTestId("verdict-primary").click();
  await expect(page).not.toHaveURL(/[?&]v=1/);

  // Export: Tab stays inside the open popover, and Esc gives the focus back to the name.
  const name = page.getByTestId("project-name");
  await name.focus();
  await page.keyboard.press("Enter");
  const popover = page.getByTestId("export");
  await expect(popover).toBeVisible();
  const inside = () => page.evaluate(() => document.activeElement?.closest("[data-testid=export]") != null);
  await expect.poll(inside, { message: "the popover opens with the focus inside" }).toBe(true);
  for (let step = 0; step < 12; step += 1) {
    await page.keyboard.press("Tab");
    // At its end the popover hands the focus back to its start, a moment later.
    await expect.poll(inside, { message: `the focus is in the popover after ${String(step + 1)} Tabs` }).toBe(true);
    await expect(popover).toBeVisible();
  }
  await page.getByTestId("export-go").click();
  await expect(page.getByTestId("exported")).toBeVisible({ timeout: MINUTE });
  // The files are named, not counted.
  await expect(page.getByTestId("exported-files")).toContainText(".step");
  await page.keyboard.press("Escape");
  await expect(popover).toHaveCount(0);
  await expect(name).toBeFocused();
});

test("Home is one column: the prompt, the suggestions and the cards share their edges, and the example comes last", async ({
  page,
  bridge,
  violations,
}) => {
  void violations;
  await buildCableBox(bridge);
  const second = await invoke<{ id: string }>(bridge, "project_create", { name: "A wall hook for a 32 mm pipe, held by two screws into a stud" });
  void second;
  for (const width of [1280, 1920]) {
    await page.setViewportSize({ width, height: 900 });
    await open(page, bridge, "/hotend");
    await expect(page.getByTestId("project-card")).toHaveCount(2);
    const measured = await page.evaluate(() => {
      const box = (element: Element | null) => {
        const rect = element?.getBoundingClientRect();
        return rect ? { left: Math.round(rect.left), right: Math.round(rect.right), height: Math.round(rect.height) } : null;
      };
      const cards = [...document.querySelectorAll("[data-testid=cards] > li")];
      return {
        prompt: box(document.querySelector("[data-testid=home-prompt]")),
        suggestions: box(document.querySelector("[data-testid=suggestions]")),
        grid: box(document.querySelector("[data-testid=cards]")),
        cards: cards.map(box),
        order: cards.map((card) => card.getAttribute("data-testid")),
        // The window's own thin scroll bar, without arrow buttons.
        bar: getComputedStyle(document.querySelector("[data-testid=home]") ?? document.body, "::-webkit-scrollbar").width,
      };
    });
    expect(measured.suggestions?.left).toBe(measured.grid?.left);
    expect(measured.suggestions?.right).toBe(measured.grid?.right);
    expect(Math.abs((measured.prompt?.left ?? 0) - (measured.grid?.left ?? 0))).toBeLessThanOrEqual(1);
    expect(Math.abs((measured.prompt?.right ?? 0) - (measured.grid?.right ?? 0))).toBeLessThanOrEqual(1);
    expect(new Set(measured.cards.map((card) => card?.height)).size, "every card is the same height").toBe(1);
    expect(measured.order).toEqual(["project-card", "project-card", "example-card"]);
    expect(measured.bar).toBe("8px");
  }
  // The mark and the name are in the title bar outside a project.
  // The mark carries the product's name; the wordmark beside it is lowercase.
  await expect(page.getByTestId("brand")).toHaveText("hotend");
  await expect(page.getByTestId("brand").getByRole("img", { name: "Hotend" })).toBeVisible();
  await expect(page.getByTestId("brand").getByTestId("brand-mark")).toBeVisible();
});

test("a theme change is drawn at once, and a spool near the surface's colour keeps its outline", async ({ page, bridge, violations }) => {
  void violations;
  await page.addInitScript(() => {
    window.localStorage.setItem("app.theme", "system");
  });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.setViewportSize({ width: 1280, height: 800 });
  await open(page, bridge, "/settings/printer");
  await expect(page.getByTestId("settings-printer").getByTestId("swatch").first()).toBeVisible();
  const colours = () =>
    page.evaluate(() =>
      [...document.querySelectorAll("button, a, [role=combobox], input")].map((element) => {
        const style = getComputedStyle(element);
        return `${style.color}|${style.backgroundColor}|${style.borderTopColor}`;
      }),
    );
  const near = () =>
    page.evaluate(() => [...document.querySelectorAll("[data-testid=swatch]")].map((swatch) => swatch.hasAttribute("data-near")));
  // Dark: the black spool is the one that needs its outline. No swatch is a circle.
  expect(await near()).toEqual([false, true, false, false, false]);
  expect(
    await page.getByTestId("swatch").first().evaluate((swatch) => Number.parseFloat(getComputedStyle(swatch).borderRadius) < swatch.getBoundingClientRect().width / 2),
  ).toBe(true);
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  // Two frames on: every control is in the new theme's colours, as it will be half a second later.
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve(null)))));
  const early = await colours();
  await page.waitForTimeout(600);
  expect(early).toEqual(await colours());
  expect(await near()).toEqual([true, false, false, false, false]);
});

test("Calibration offers only projects that have a fit to gauge, and says once what every row of the gauge says", async ({ page, bridge, violations }) => {
  void violations;
  test.setTimeout(5 * MINUTE);
  await page.setViewportSize({ width: 1280, height: 800 });
  // A part without a snap, a slide or a screw hole: there is no gauge to make of it.
  const plain = await invoke<{ id: string }>(bridge, "project_create", { name: "Thin cup" });
  const cup = fileURLToPath(new URL("../../../python/tests/models/broken_thin_wall.py", import.meta.url));
  await invoke(bridge, "model_build", { projectId: plain.id, code: readFileSync(cup, "utf8"), params: {}, note: "polish 2 test" });
  await open(page, bridge, "/settings/calibration");
  await expect(page.getByTestId("gauge-no-fits")).toContainText("nothing to gauge yet");
  await expect(page.getByTestId("gauge-export")).toHaveCount(0);
  await expect(page.getByRole("alert")).toHaveCount(0);

  // With a project that has fits, that one is offered, though the cup is the newer of the two.
  await buildCableBox(bridge);
  await invoke(bridge, "parameter_set", { projectId: plain.id, name: "wall", value: 0.4 });
  await page.reload();
  const section = page.getByTestId("settings-calibration");
  await expect(section.getByRole("combobox")).toContainText("Cable box", { timeout: MINUTE });
  await section.getByRole("combobox").click();
  await expect(page.getByRole("option")).toHaveText(["Cable box"]);
  await page.keyboard.press("Escape");
  await page.getByTestId("gauge-export").click();
  const features = page.getByTestId("gauge-features");
  await expect(features).toBeVisible({ timeout: 2 * MINUTE });
  await expect(page.getByTestId("gauge-nothing")).toHaveCount(0);
  // Where on a feature to measure is the same for all four: it is said once, above the rows.
  await expect(page.getByTestId("gauge-every")).toContainText("On every one: measure above the first millimetre from the bed.");
  await expect(features).not.toContainText("first millimetre");
  // A caliper reading with a decimal comma is read as it was meant.
  const first = features.locator("tbody tr").first().locator("input").first();
  await first.fill("15,9");
  await first.press("Tab");
  await expect(first).toHaveValue("15.9");
});
