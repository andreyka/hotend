// What was once broken, as tests in headless Chromium against the bridge: the focus
// ring on every tab stop, the columns at the smallest window, the part drawn as a shaded
// solid on a quiet bed, a message that could not be sent, and a Select under the
// window's content policy. The cable box is built through the core, without an AI.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { Page } from "@playwright/test";

import { expect, invoke, test, type Bridge } from "./bridge";

const MODEL = fileURLToPath(new URL("../../../python/tests/models/cable_box.py", import.meta.url));

type Box = { min: [number, number, number]; max: [number, number, number] };

// The outsides of the cable box's two end walls, from its default sizes: half the inside
// width (120 mm) plus the wall (2 mm). The box's bounding box is wider than this on one end,
// where the screw post stands outside the end wall (to 68 mm), so its edge is not a wall.
const END_WALLS = [-(120 / 2 + 2), 120 / 2 + 2];

async function buildCableBox(bridge: Bridge): Promise<{ projectId: string; boxes: Record<string, Box> }> {
  const project = await invoke<{ id: string }>(bridge, "project_create", { name: "Cable box" });
  const report = await invoke<{ version: { number: number } | null }>(bridge, "model_build", {
    projectId: project.id,
    code: readFileSync(MODEL, "utf8"),
    params: {},
    note: "polish test",
  });
  expect(report.version?.number).toBe(1);
  const detail = await invoke<{ result: { parts: { name: string; bbox: Box }[] } }>(bridge, "version_get", {
    projectId: project.id,
    number: 1,
  });
  return { projectId: project.id, boxes: Object.fromEntries(detail.result.parts.map((part) => [part.name, part.bbox])) };
}

async function open(page: Page, bridge: Bridge, route: string) {
  const query = new URLSearchParams({ bridge: bridge.url, token: bridge.token });
  await page.goto(`/?${query.toString()}#${route}`);
}

async function openDesign(page: Page, bridge: Bridge, projectId: string) {
  await open(page, bridge, `/hotend/p/${projectId}/design`);
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", "2", { timeout: 60_000 });
}

type Stop = { name: string; ring: boolean; inView: boolean; where: string };

/** Presses Tab until the focus comes round again, and says what each stop looks like. */
async function tabStops(page: Page, limit = 120): Promise<Stop[]> {
  const stops: Stop[] = [];
  const seen = new Set<string>();
  for (let step = 0; step < limit; step += 1) {
    await page.keyboard.press("Tab");
    const stop = await page.evaluate(() => {
      const element = document.activeElement;
      if (!(element instanceof HTMLElement) || element === document.body) return null;
      // The ring is on the element, or on the frame of a field that draws it for its input.
      const framed = element.closest<HTMLElement>(".app-ring-group");
      const drawn = [element, framed].some((candidate) => {
        if (!candidate) return false;
        const style = getComputedStyle(candidate);
        return style.outlineStyle === "solid" && Number.parseFloat(style.outlineWidth) >= 2;
      });
      const box = element.getBoundingClientRect();
      const text = element.getAttribute("aria-label") ?? element.getAttribute("data-testid") ?? element.textContent ?? "";
      const stage = element.closest("[data-testid=stage]") ? "stage" : element.closest("header") ? "titlebar" : "other";
      return {
        name: `${element.tagName.toLowerCase()}:${text.trim().slice(0, 40)}`,
        ring: drawn,
        inView: element.closest("[data-testid=viewport]") !== null,
        where: stage,
        key: `${element.tagName}:${text}:${String(Math.round(box.x))}:${String(Math.round(box.y))}`,
      };
    });
    if (stop === null) continue;
    if (seen.has(stop.key)) break;
    seen.add(stop.key);
    stops.push(stop);
  }
  return stops;
}

test("every tab stop shows the focus ring, and the 3D view is one stop after the verdict bar", async ({ page, bridge, violations }) => {
  void violations;
  const { projectId } = await buildCableBox(bridge);
  await openDesign(page, bridge, projectId);
  await page.getByTestId("home-button").focus();
  const design = await tabStops(page);
  expect(design.length).toBeGreaterThan(15);
  expect(design.filter((stop) => !stop.ring).map((stop) => stop.name), "tab stops without a ring in Design").toEqual([]);
  // The view is one stop, not two, and it comes after the stage's own controls (6.2).
  const inView = design.filter((stop) => stop.inView);
  expect(inView).toHaveLength(1);
  const view = design.findIndex((stop) => stop.inView);
  const tools = design.findIndex((stop) => stop.name === "button:Fit");
  expect(tools).toBeGreaterThan(-1);
  expect(tools).toBeLessThan(view);
  expect(await page.getByTestId("viewport-host").getAttribute("aria-hidden")).toBe("true");

  // Home and Settings: the same ring on the prompt, the cards, the links and the fields.
  for (const route of ["/hotend", "/settings/ai", "/settings/printer"]) {
    await open(page, bridge, route);
    await expect(page.getByTestId("home-button")).toBeVisible();
    await page.waitForTimeout(500);
    await page.getByTestId("home-button").focus();
    const stops = await tabStops(page);
    expect(stops.length).toBeGreaterThan(4);
    expect(stops.filter((stop) => !stop.ring).map((stop) => stop.name), `tab stops without a ring on ${route}`).toEqual([]);
  }
});

test("at 1100 × 700 no column is cut off, and the verdict is never cut", async ({ page, bridge, violations }) => {
  void violations;
  await page.setViewportSize({ width: 1100, height: 700 });
  const { projectId } = await buildCableBox(bridge);
  await openDesign(page, bridge, projectId);

  const fits = async () => {
    await page.waitForTimeout(600);
    const measured = await page.evaluate(() => {
      const box = (id: string) => {
        const element = document.querySelector(`[data-testid=${id}]`);
        if (!element) return null;
        const rect = element.getBoundingClientRect();
        return { left: rect.left, right: rect.right, width: rect.width };
      };
      const sentence = document.querySelector("[data-testid=verdict-sentence]");
      return {
        page: document.documentElement.scrollWidth,
        window: window.innerWidth,
        conversation: box("conversation-column"),
        stage: box("stage-column"),
        inspector: box("inspector"),
        cut: sentence ? sentence.scrollWidth > sentence.clientWidth + 1 : false,
      };
    });
    expect(measured.page).toBeLessThanOrEqual(measured.window);
    for (const column of [measured.conversation, measured.stage, measured.inspector]) {
      if (!column) continue;
      expect(column.left).toBeGreaterThanOrEqual(0);
      expect(column.right).toBeLessThanOrEqual(measured.window + 0.5);
    }
    expect(measured.stage?.width ?? 0).toBeGreaterThanOrEqual(520);
    expect(measured.cut, "the verdict sentence is shown whole").toBe(false);
  };

  const conversation = page.getByTestId("conversation-column");
  const inspector = page.getByTestId("inspector");
  // As it opens: the numbers are in view, whole, and the conversation has given way.
  await expect(inspector).toHaveCount(1);
  await expect(conversation).toHaveCount(0);
  await fits();
  await expect(page.getByTestId("proof-count")).toBeInViewport({ ratio: 1 });
  // Asking for the conversation shows it; the window then has room for one side column.
  await page.getByTestId("toggle-conversation").click();
  await expect(conversation).toHaveCount(1);
  await expect(inspector).toHaveCount(0);
  await fits();
  await page.getByTestId("toggle-inspector").click();
  await expect(inspector).toHaveCount(1);
  await expect(conversation).toHaveCount(0);
  await fits();
  await expect(page.getByTestId("proof-count")).toBeInViewport({ ratio: 1 });

  // Made wider, both are back at once; made narrow again, nothing spills.
  await page.setViewportSize({ width: 1400, height: 800 });
  await expect(conversation).toHaveCount(1);
  await expect(inspector).toHaveCount(1);
  await fits();
  await page.setViewportSize({ width: 1100, height: 700 });
  await expect(conversation).toHaveCount(0);
  await fits();
});

/** The colour of one point of the page, from a picture of it. */
async function colourAt(page: Page, x: number, y: number): Promise<[number, number, number]> {
  const png = (await page.screenshot()).toString("base64");
  return page.evaluate(
    async ([data, px, py]) => {
      const image = new Image();
      image.src = `data:image/png;base64,${data}`;
      await image.decode();
      const canvas = document.createElement("canvas");
      canvas.width = image.width;
      canvas.height = image.height;
      const context = canvas.getContext("2d");
      if (!context) throw new Error("no 2D canvas");
      context.drawImage(image, 0, 0);
      const scale = image.width / window.innerWidth;
      const pixel = context.getImageData(Math.round(px * scale), Math.round(py * scale), 1, 1).data;
      return [pixel[0] ?? 0, pixel[1] ?? 0, pixel[2] ?? 0] as [number, number, number];
    },
    [png, x, y] as const,
  );
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

const token = (page: Page, name: string) =>
  page.evaluate((property) => {
    const hex = getComputedStyle(document.documentElement).getPropertyValue(property).trim().replace("#", "");
    return [0, 2, 4].map((at) => Number.parseInt(hex.slice(at, at + 2), 16)) as [number, number, number];
  }, name);

const light = ([r, g, b]: readonly number[]) => 0.2126 * (r ?? 0) + 0.7152 * (g ?? 0) + 0.0722 * (b ?? 0);

for (const scheme of ["dark", "light"] as const) {
  test(`the part is a shaded solid in its own colour on a quiet bed (${scheme})`, async ({ page, bridge, violations }) => {
    void violations;
    await page.emulateMedia({ colorScheme: scheme });
    const { projectId, boxes } = await buildCableBox(bridge);
    const box = boxes.box;
    if (!box) throw new Error("the cable box has a box");
    await openDesign(page, bridge, projectId);
    await page.waitForTimeout(600);
    const canvas = page.getByTestId("viewport").locator("canvas").first();
    const area = await canvas.boundingBox();
    if (!area) throw new Error("the canvas has a size");

    // No red and green axis lines through the stage, with or without a part.
    expect(await axisPixels(page)).toBe(0);

    // Find one point on the wall that faces the maker and one on the wall beside it, by
    // asking the view what is under a grid of points.
    const host = page.getByTestId("viewport-host");
    const hits: { x: number; y: number; face: "front" | "side" }[] = [];
    for (let row = 0; row < 10 && hits.length < 2; row += 1) {
      for (let column = 0; column < 21 && hits.length < 2; column += 1) {
        const x = area.x + area.width * (0.2 + (0.6 * column) / 20);
        const y = area.y + area.height * (0.5 + (0.3 * row) / 9);
        const before = Number((await host.getAttribute("data-clicks")) ?? 0);
        await page.mouse.click(x, y);
        await expect(host).toHaveAttribute("data-clicks", String(before + 1));
        const chip = page.getByTestId("picked");
        if ((await chip.count()) === 0) continue;
        if ((await chip.getAttribute("data-part")) !== "box" || (await chip.getAttribute("data-kind")) !== "face") continue;
        const point = ((await chip.getAttribute("data-point")) ?? "").split(",").map(Number);
        const near = (value: number | undefined, to: number) => Math.abs((value ?? 1e9) - to) < 0.05;
        // Well inside the wall, away from its edges and the cable slot.
        const high = (point[2] ?? 0) > box.min[2] + 14 && (point[2] ?? 0) < box.max[2] - 8;
        if (!high) continue;
        if (near(point[1], box.min[1]) && Math.abs(point[0] ?? 0) < (box.max[0] - box.min[0]) * 0.3 && !hits.some((hit) => hit.face === "front")) {
          hits.push({ x, y, face: "front" });
        } else if (END_WALLS.some((wall) => near(point[0], wall)) && Math.abs(point[1] ?? 0) > (box.max[1] - box.min[1]) * 0.25 && !hits.some((hit) => hit.face === "side")) {
          hits.push({ x, y, face: "side" });
        }
      }
    }
    // Nothing picked, and the pointer off the part: the faces are in their own colours.
    if ((await page.getByTestId("picked").count()) > 0) await page.getByTestId("picked").getByRole("button", { name: "Clear" }).click();
    await page.mouse.move(area.x + area.width / 2, area.y + area.height - 3);
    await page.waitForTimeout(200);

    const front = hits.find((hit) => hit.face === "front");
    const side = hits.find((hit) => hit.face === "side");
    expect(front, "a point on the wall that faces the maker").toBeDefined();
    expect(side, "a point on the wall beside it").toBeDefined();
    if (!front || !side) return;
    const tint = await token(page, "--hotend-part-1");
    const frontColour = await colourAt(page, front.x, front.y);
    const sideColour = await colourAt(page, side.x, side.y);
    // The wall that faces the maker is the part's own colour, within 6 % a channel: not
    // washed out to white, not sunk to grey.
    frontColour.forEach((value, channel) => {
      expect(Math.abs(value - (tint[channel] ?? 0)), `channel ${String(channel)} of ${frontColour.join(",")} against ${tint.join(",")}`).toBeLessThanOrEqual(0.06 * 255);
    });
    // And the part has form: the wall beside it differs by a tenth or more.
    expect(Math.abs(light(sideColour) - light(frontColour)) / light(frontColour)).toBeGreaterThanOrEqual(0.1);
  });
}

test("an empty project says so on a card of its own, over a bed without axis lines", async ({ page, bridge, violations }) => {
  void violations;
  // A provider is chosen, so that the stage shows its message and not the request for a key.
  const settings = await invoke<Record<string, unknown>>(bridge, "settings_get");
  await invoke(bridge, "settings_set", { settings: { ...settings, provider: { kind: "ollama", model: "scripted" } } });
  const project = await invoke<{ id: string }>(bridge, "project_create", { name: "Nothing yet" });
  await open(page, bridge, `/hotend/p/${project.id}/design`);
  await expect(page.getByTestId("viewport")).toBeVisible();
  await page.waitForTimeout(800);
  expect(await axisPixels(page)).toBe(0);
  const card = page.getByTestId("stage-empty");
  await expect(card).toContainText("Nothing here yet.");
  // Opaque: the grid does not run through the words.
  const background = await card.evaluate((element) => getComputedStyle(element).backgroundColor);
  expect(background).toMatch(/^rgb\(/);
});

test("a message that cannot be sent comes back to the composer, with Send again", async ({ page, bridge, violations }) => {
  void violations;
  const { projectId } = await buildCableBox(bridge);
  let asked = 0;
  await page.route("**/invoke/message_send", async (route) => {
    asked += 1;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        ok: false,
        error: { kind: "busy", message: "Hotend is already working on this project.", next_step: "Wait for it to finish, or stop it." },
      }),
    });
  });
  await openDesign(page, bridge, projectId);
  const composer = page.getByTestId("conversation").getByRole("textbox");
  if (!(await composer.isVisible())) await page.getByTestId("toggle-conversation").click();
  await composer.fill("Make the cable slot 14 mm wide");
  await composer.press("Enter");
  // The words are not lost: they are back in the field, and the card offers to send them.
  await expect(page.getByTestId("send-again")).toBeVisible();
  await expect(composer).toHaveValue("Make the cable slot 14 mm wide");
  expect(asked).toBe(1);
  await page.getByTestId("send-again").click();
  await expect.poll(() => asked).toBe(2);
});

test("a Select opens under the window's content policy", async ({ page, bridge, violations }) => {
  void violations;
  await open(page, bridge, "/settings/printer");
  await expect(page.getByTestId("settings-printer")).toBeVisible();
  const select = page.getByTestId("settings-printer").getByRole("combobox").first();
  await select.click();
  await expect(page.getByRole("option").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("option")).toHaveCount(0);
  // Settings is one block in the middle of the window: the section list and, 48 px to its
  // right, the section. On a wide window the content does not drift away from its list.
  for (const width of [1280, 1920, 2560]) {
    await page.setViewportSize({ width, height: 1080 });
    const [block, nav, content] = await Promise.all(
      ["settings-block", "settings-nav", "settings-content"].map((id) => page.getByTestId(id).boundingBox()),
    );
    if (!block || !nav || !content) throw new Error("the settings block has a size");
    expect(Math.abs(block.x - (width - block.width) / 2), `the block is centred at ${String(width)}`).toBeLessThan(12);
    expect(content.x - (nav.x + nav.width)).toBe(48);
    expect(content.width).toBeLessThanOrEqual(640);
  }
  // Every section sits on the raised surface: no bare text on the window's background.
  const surfaces = await page.evaluate(() => {
    const raised = getComputedStyle(document.documentElement).getPropertyValue("--app-raised").trim().toLowerCase();
    const hex = (colour: string) =>
      `#${(colour.match(/\d+/g) ?? []).slice(0, 3).map((part) => Number(part).toString(16).padStart(2, "0")).join("")}`;
    return [...document.querySelectorAll("[data-testid=settings-printer] .rounded-card")].map(
      (card) => hex(getComputedStyle(card).backgroundColor) === raised,
    );
  });
  expect(surfaces.length).toBeGreaterThan(2);
  expect(surfaces.every(Boolean)).toBe(true);
});

test("a long project name stops before the stage switch, and a missing project is said in plain words", async ({ page, bridge, violations }) => {
  void violations;
  const name = `${"A cable box for the long shelf behind the desk ".repeat(3).trim().slice(0, 117)}x`;
  const project = await invoke<{ id: string }>(bridge, "project_create", { name });
  for (const width of [1100, 1280]) {
    await page.setViewportSize({ width, height: 800 });
    await open(page, bridge, `/hotend/p/${project.id}/design`);
    const shown = page.getByTestId("project-name");
    await expect(shown).toHaveAttribute("title", name);
    const label = await shown.boundingBox();
    const stages = await page.getByTestId("stage-switch").boundingBox();
    if (!label || !stages) throw new Error("the title bar has sizes");
    expect(label.x + label.width).toBeLessThanOrEqual(stages.x);
  }
  await open(page, bridge, "/hotend/p/00000000-0000-4000-8000-000000000000/design");
  const loading = page.getByTestId("studio-loading");
  await expect(loading).toContainText("This project is not here any more.");
  await expect(loading).not.toContainText("00000000-0000");
});

test("targets are at least 28 px, and with text at 125 % nothing in the title bar, the verdict or a number field is cut", async ({ page, bridge, violations }) => {
  void violations;
  await page.setViewportSize({ width: 1280, height: 800 });
  const { projectId } = await buildCableBox(bridge);
  await openDesign(page, bridge, projectId);
  // No target is under 28 px high: the verdict sentence, the rows of the Proof list and
  // the links inside an open row.
  // The example meets every requirement, so no problem is open by itself: the
  // row of its inside size is opened, which says what it found and links the guessed sizes.
  const must = page.getByTestId("proof-group-must");
  await expect(must).toBeVisible();
  const inside = must.locator('[data-testid=proof-line][data-key="contract:holds_power_strip"]');
  if ((await inside.count()) === 0) await must.locator(":scope > button").click();
  const row = inside.locator(":scope > button");
  if ((await row.getAttribute("aria-expanded")) !== "true") await row.click();
  await expect(inside.getByTestId("proof-summary")).toContainText("fits inside the box");
  const targets = await page.evaluate(() => {
    const proof = document.querySelector("[data-testid=proof-count]")?.closest("section");
    const found = [document.querySelector("button[data-testid=verdict-sentence]"), ...(proof?.querySelectorAll("button, a") ?? [])];
    return found.map((element) => ({
      name: element?.textContent?.trim().slice(0, 30) ?? "missing",
      height: element?.getBoundingClientRect().height ?? 0,
    }));
  });
  expect(targets.length).toBeGreaterThan(5);
  expect(targets.filter((target) => target.height < 28)).toEqual([]);
  // The slider's thumb is 16 px; a transparent frame around it makes the target 28 px.
  const thumb = await page.evaluate(() => {
    const element = document.querySelector("[data-testid=inspector] [data-slot=slider-thumb]");
    if (!element) return null;
    const around = getComputedStyle(element, "::before");
    return element.getBoundingClientRect().height - 2 * Number.parseFloat(around.top);
  });
  expect(thumb).toBeGreaterThanOrEqual(28);
  // What a system text-size setting does: the type roles and the rows are in rem.
  await page.evaluate(() => {
    document.documentElement.style.fontSize = "125%";
  });
  await page.waitForTimeout(600);
  const measured = await page.evaluate(() => {
    const cut = (element: Element | null) =>
      element !== null && (element.scrollWidth > element.clientWidth + 1 || element.scrollHeight > element.clientHeight + 1);
    const header = document.querySelector("header");
    const sentence = document.querySelector("[data-testid=verdict-sentence]");
    return {
      type: Number.parseFloat(getComputedStyle(document.body).fontSize),
      // Buttons and fields are in rem too: 13 px at 100 %, a quarter more at 125 %.
      button: Number.parseFloat(getComputedStyle(document.querySelector("[data-testid=verdict-primary]") ?? document.body).fontSize),
      field: Number.parseFloat(getComputedStyle(document.querySelector("[data-testid=inspector] input[inputmode]") ?? document.body).fontSize),
      header: cut(header),
      sentence: sentence ? sentence.scrollWidth > sentence.clientWidth + 1 : true,
      fields: [...document.querySelectorAll<HTMLInputElement>("[data-testid=inspector] input[inputmode]")].map(
        (input) => input.scrollWidth > input.clientWidth + 1,
      ),
      page: document.documentElement.scrollWidth > window.innerWidth,
      // The view tools grow with the text: the verdict bar still ends before them.
      beforeTools:
        (document.querySelector("[data-testid=view-tools]")?.getBoundingClientRect().left ?? 0) -
        (document.querySelector("[data-testid=verdict]")?.getBoundingClientRect().right ?? Infinity),
    };
  });
  expect(measured.beforeTools, "the verdict bar ends before the view tools").toBeGreaterThanOrEqual(8);
  expect(measured.type).toBeGreaterThan(16);
  expect(measured.button).toBeCloseTo(13 * 1.25, 1);
  expect(measured.field).toBeCloseTo(13 * 1.25, 1);
  expect(measured.header, "the title bar holds its text").toBe(false);
  expect(measured.sentence, "the verdict sentence is shown whole").toBe(false);
  expect(measured.fields.length).toBeGreaterThan(0);
  expect(measured.fields.every((field) => !field)).toBe(true);
  expect(measured.page).toBe(false);

  // Twice the text size is more than DESIGN_SPEC promises, and the window still
  // holds together: what is in a column wraps inside it, and nothing lies over the tools.
  await page.evaluate(() => {
    document.documentElement.style.fontSize = "200%";
  });
  await page.waitForTimeout(600);
  const outside = (inside: string) =>
    page.evaluate((selector) => {
      const column = document.querySelector("[data-testid=inspector]")?.getBoundingClientRect();
      if (!column) return ["no inspector"];
      return [...document.querySelectorAll(selector)]
        .filter((element) => {
          const box = element.getBoundingClientRect();
          // (A slider's own input is one pixel wide and not drawn.)
          return box.width > 1 && (box.right > column.right + 1 || box.left < column.left - 1);
        })
        .map((element) => `${element.tagName} ${element.textContent?.trim().slice(0, 24) ?? ""}`);
    }, inside);
  expect(await outside("[data-testid=sizes] button, [data-testid=sizes] input, [data-testid=sizes] span"), "sizes at 200 %").toEqual([]);
  const large = await page.evaluate(() => {
    const composer = document.querySelector<HTMLTextAreaElement>("#hotend-composer");
    const label = document.querySelector("[data-testid^=measure-][data-open] span[title]");
    return {
      beforeTools:
        (document.querySelector("[data-testid=view-tools]")?.getBoundingClientRect().left ?? 0) -
        (document.querySelector("[data-testid=verdict]")?.getBoundingClientRect().right ?? Infinity),
      // The name of the size that is asked for keeps a width it can be read in.
      label: label ? label.getBoundingClientRect().width / Number.parseFloat(getComputedStyle(label).fontSize) : 0,
      // The empty composer is as high as its placeholder.
      composer: composer ? composer.scrollHeight - composer.clientHeight : 99,
      // The stage switch and the printer chip keep a gap.
      chip:
        (document.querySelector("[data-testid=printer-chip]")?.getBoundingClientRect().left ?? 0) -
        (document.querySelector("[data-testid=stage-switch]")?.getBoundingClientRect().right ?? Infinity),
    };
  });
  expect(large.beforeTools).toBeGreaterThanOrEqual(8);
  expect(large.label).toBeGreaterThan(3.5);
  expect(large.composer).toBeLessThanOrEqual(1);
  expect(large.chip).toBeGreaterThanOrEqual(10);
  await page.getByTestId("stage-prepare").click();
  await expect(page.getByTestId("prepare-panel")).toHaveAttribute("data-pieces", "2", { timeout: 120_000 });
  expect(await outside("[data-testid=piece-controls], [data-testid=piece-controls] *"), "the piece rows at 200 %").toEqual([]);
});
