// Takes the pictures of docs/ux/screenshots/: the journey of journey.ts with the scripted
// conversation and the cable box, then first run and Settings. Every screen at 1280 x 800
// in both themes, the main ones at 1920 x 1080 too, all at two device pixels per pixel.
//
// It rewrites files that are kept in the repository, so it runs only when asked:
//   pnpm build:bridge && pnpm screenshots
import { mkdirSync } from "node:fs";
import { homedir, tmpdir, userInfo } from "node:os";
import { fileURLToPath } from "node:url";

import type { Page } from "@playwright/test";

import { expect, invoke, test, type Bridge } from "./bridge";
import { journey, type Snap } from "./journey";

const OUT = fileURLToPath(new URL("../../../docs/ux/screenshots/", import.meta.url));
const SMALL = { width: 1280, height: 800 };
const WIDE = { width: 1920, height: 1080 };
type Scheme = "dark" | "light";

/**
 * The pictures are public and the machine they are taken on is not: where a screen writes a
 * path of this machine (the stand-in slicer, an exported folder, a gauge), the picture shows
 * a neutral one. Only the text of the page under test is changed, right before the picture;
 * the product shows real paths. A path that is still on the page fails the run.
 */
async function neutralPaths(page: Page, bridge: Bridge) {
  const real = (path: string) => path.replace(/[\\/]+$/, "");
  const neutral: [string, string][] = [
    [real(bridge.dataDir), "/home/user/hotend-data"],
    [real(fileURLToPath(new URL("../../../", import.meta.url))), "/home/user/hotend"],
    [real(tmpdir()), "/home/user/exports"],
    [real(homedir()), "/home/user"],
  ];
  // The longest first: the checkout is inside the home folder.
  neutral.sort((a, b) => b[0].length - a[0].length);
  // The name alone, for a path written in another form. A name that is also an ordinary
  // word of the interface ("user", "root") says nothing about the machine.
  const name = userInfo().username;
  const forbidden = [...neutral.map(([path]) => path), ...(name.length > 4 ? [name] : [])].filter(
    (text) => !neutral.some(([, stand]) => stand.startsWith(text)),
  );
  const left = await page.evaluate(
    ({ neutral, forbidden }) => {
      const changed = (text: string) => neutral.reduce((sum, [path, stand]) => sum.split(path).join(stand), text);
      const textsOf = (root: Node) => {
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
        const found: Text[] = [];
        while (walker.nextNode()) found.push(walker.currentNode as Text);
        return found;
      };
      // A path is drawn in one piece per folder (bits.tsx, PathText): the pieces keep their places.
      for (const path of document.querySelectorAll("[data-testid=path]")) {
        const texts = textsOf(path);
        const whole = path.textContent ?? "";
        if (changed(whole) === whole) continue;
        const pieces = changed(whole).split(/(?<=[\\/])/);
        texts.forEach((text, index) => {
          text.nodeValue = index === texts.length - 1 ? pieces.slice(index).join("") : (pieces[index] ?? "");
        });
      }
      for (const text of textsOf(document.body)) {
        const value = text.nodeValue ?? "";
        if (changed(value) !== value) text.nodeValue = changed(value);
      }
      for (const element of document.querySelectorAll("[title]")) {
        element.setAttribute("title", changed(element.getAttribute("title") ?? ""));
      }
      const fields = [...document.querySelectorAll("input, textarea")].map((field) => (field as HTMLInputElement).value);
      const shown = [document.body.innerText, ...fields].join("\n");
      return forbidden.filter((text) => shown.includes(text));
    },
    { neutral, forbidden },
  );
  expect(left, "no path of this machine is in the picture").toEqual([]);
}

test.skip(process.env.HOTEND_SCREENSHOTS !== "1", "set HOTEND_SCREENSHOTS=1 to retake the pictures");

// A slower simulated printer than the journey test's: a running print of about an hour
// lasts ten minutes, long enough for its pictures (four of the print, two of the printer
// popover) where one wide picture takes a minute. On a machine that is busy with other
// work a wide picture can take five: HOTEND_PRINT_TIME_SCALE=3 makes the print last
// twenty minutes there.
const TIME_SCALE = process.env.HOTEND_PRINT_TIME_SCALE ?? "6";
test.use({
  deviceScaleFactor: 2,
  viewport: SMALL,
  colorScheme: "dark",
  bridgeArgs: ["--script", "builtin:cable_box_demo", "--answer-dialogs-for-tests", "--time-scale", TIME_SCALE],
});

test("the pictures of every screen", async ({ page, bridge, violations }) => {
  void violations;
  test.setTimeout(60 * 60_000);
  mkdirSync(OUT, { recursive: true });

  // The theme follows the system, so the test can switch it without leaving the screen.
  await page.addInitScript(() => {
    window.localStorage.setItem("app.theme", "system");
  });
  const query = new URLSearchParams({ bridge: bridge.url, token: bridge.token });
  await page.goto(`/?${query.toString()}`);

  const settle = async () => {
    // A new theme draws the 3D view again; a new size lays the columns out again.
    await page.waitForTimeout(350);
    if ((await page.getByTestId("viewport").count()) > 0) {
      await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", /\d+/, { timeout: 30_000 });
    }
    await page.waitForTimeout(650);
  };
  const snap: Snap = async (name, options = {}) => {
    if (name === "print-running") {
      // A print that has got somewhere: the plate is drawn up to the printed height.
      await expect
        .poll(async () => Number.parseInt((await page.getByTestId("print-percent").textContent()) ?? "0", 10), {
          timeout: 10 * 60_000,
        })
        .toBeGreaterThanOrEqual(20);
    }
    const variants: [Scheme, typeof SMALL][] = options.passing
      ? [["dark", SMALL]]
      : [["dark", SMALL], ["light", SMALL], ...(options.wide ? ([["light", WIDE], ["dark", WIDE]] as [Scheme, typeof SMALL][]) : [])];
    for (const [scheme, size] of variants) {
      await page.emulateMedia({ colorScheme: scheme });
      await page.setViewportSize(size);
      if (!options.passing) await settle();
      await neutralPaths(page, bridge);
      await page.screenshot({ path: `${OUT}${name}-${String(size.width)}x${String(size.height)}-${scheme}.png` });
    }
    if (!options.passing) {
      await page.emulateMedia({ colorScheme: "dark" });
      await page.setViewportSize(SMALL);
      await settle();
    }
  };
  const go = async (route: string) => {
    await page.evaluate((hash) => {
      window.location.hash = hash;
    }, route);
  };

  await journey(page, snap);

  // A project with nothing built: the stage says so on a card of its own.
  const empty = await invoke<{ id: string }>(bridge, "project_create", { name: "A wall hook" });
  await go(`/hotend/p/${empty.id}/design`);
  await expect(page.getByTestId("stage-empty")).toBeVisible();
  await snap("design-empty");

  // Settings, section by section.
  await go("/settings/ai");
  await expect(page.getByTestId("settings-ai")).toBeVisible();
  await snap("settings-ai", { wide: true });
  await go("/settings/printer");
  await expect(page.getByTestId("settings-printer")).toBeVisible();
  await snap("settings-printer");
  await go("/settings/slicer");
  await expect(page.getByTestId("settings-slicer").getByRole("listitem").first()).toBeVisible();
  await snap("settings-slicer");
  await go("/settings/appearance");
  await expect(page.getByTestId("settings-appearance")).toBeVisible();
  await snap("settings-appearance");
  await go("/settings/computer");
  await expect(page.getByTestId("health-rows")).toBeVisible({ timeout: 120_000 });
  // The rows stand while the checks still run; the picture shows their results.
  await expect(page.getByRole("button", { name: "Check again" })).toBeEnabled({ timeout: 120_000 });
  await snap("settings-computer");
  await go("/settings/shortcuts");
  await expect(page.getByTestId("settings-shortcuts")).toBeVisible();
  await snap("settings-shortcuts");
  await go("/settings/about");
  await expect(page.getByTestId("licences")).toBeVisible();
  await snap("settings-about");

  // First run: the three cards, then the key field of a provider that needs a key.
  await go("/setup");
  await expect(page.getByTestId("setup")).toBeVisible();
  await expect(page.getByTestId("health-rows")).toBeVisible({ timeout: 120_000 });
  await snap("setup", { wide: true });
  await page.getByTestId("ai-section").getByText("Anthropic", { exact: true }).click();
  await expect(page.getByTestId("ai-section").getByTestId("model-field")).not.toHaveValue("scripted");
  await page.waitForTimeout(500);
  await snap("setup-key-entry");
});
