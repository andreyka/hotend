// The main journey through the interface, against the real core behind the bridge: the
// scripted conversation builds the cable box, a size changes, a question is answered, the
// plate is laid out and sliced by the stand-in slicer, the print is confirmed through the
// bridge's test-mode dialog, and the simulated printer finishes it. Calibration follows,
// because it needs a project with a part.
//
// `journey.spec.ts` runs it as a test. `screens.spec.ts` runs the same steps and takes
// the pictures of docs/ux/screenshots/ at each `snap`.
import type { Page } from "@playwright/test";

import { expect } from "./bridge";

export type SnapOptions = {
  /** Also at 1920 x 1080. */
  wide?: boolean;
  /** The state does not last: one picture only. */
  passing?: boolean;
};
export type Snap = (name: string, options?: SnapOptions) => Promise<void>;

export const FIRST_MESSAGE = "A box that hides a power strip, with a snap-on lid";

const MINUTE = 60_000;

export async function journey(page: Page, snap: Snap): Promise<void> {
  // Home, with nothing made yet.
  await expect(page.getByTestId("home-empty")).toBeVisible();
  // Before anything is made, the example shows what Hotend makes: a card with a picture.
  await expect(page.getByTestId("example-card").locator("img")).toBeVisible();
  await expect(page.locator("[data-transport]")).toHaveAttribute("data-transport", "bridge");
  // The address no longer holds the bridge's token.
  expect(page.url()).not.toContain("token=");
  await snap("home-empty", { wide: true });

  // Describing the product creates the project and sends the first message.
  const prompt = page.getByTestId("home-prompt");
  await prompt.fill(FIRST_MESSAGE);
  await prompt.press("Enter");
  await expect(page).toHaveURL(/#\/hotend\/p\/[0-9a-f-]+\/design/);
  const conversation = page.getByTestId("conversation");
  await expect(conversation).toContainText(FIRST_MESSAGE);
  // The keyboard goes with the maker: the focus is in the composer, not on the page.
  await expect(page.locator("#hotend-composer")).toBeFocused();

  // The run streams: words, then a quiet line that says what Hotend is doing. No tool
  // names, no JSON.
  await expect(page.getByTestId("live-run")).toBeVisible({ timeout: MINUTE });
  await expect(page.getByTestId("work-line")).toBeVisible({ timeout: MINUTE });
  await expect(page.getByTestId("dot-1")).toBeVisible({ timeout: 3 * MINUTE });
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", "2", { timeout: 2 * MINUTE });
  await snap("design-streaming", { passing: true });
  await expect(page.getByTestId("run-caption")).toBeVisible({ timeout: 4 * MINUTE });
  await expect(page.getByTestId("stop-run")).toHaveCount(0);
  await expect(conversation).not.toContainText(/write_model_code|render_views|\{"/);

  // The scripted conversation fixes its first version: two versions, the second current.
  await expect(page.getByTestId("dot-2")).toBeVisible();
  // The project is named after the first clause of the message, not its first six words.
  await expect(page.getByTestId("project-name")).toHaveText("A box that hides a power strip");
  await expect(page.getByTestId("proof-count")).toBeVisible();
  await expect(page.getByTestId("overall-size")).toContainText("mm");
  // The first build is framed for the stage it is shown on, not for the one it arrived
  // in while the columns were still opening, and the bed carries no tick numbers.
  const host = page.getByTestId("viewport-host");
  await expect
    .poll(() => host.evaluate((element) => element.dataset.framed === `${String(element.clientWidth)}x${String(element.clientHeight)}`))
    .toBe(true);
  await expect(host).toHaveAttribute("data-ticks", "0");
  await snap("design-ready", { wide: true });
  await expect(host).toHaveAttribute("data-ticks", "0");

  // A size changes: the part rebuilds and the rail gets a third version.
  const height = page.getByTestId("size-height");
  await expect(height.locator("input").first()).toHaveValue("40");
  await height.locator("input").first().fill("46");
  await height.locator("input").first().press("Enter");
  await expect(page.getByTestId("progress-line")).toBeVisible();
  await snap("design-rebuilding", { passing: true });
  await expect(page.getByTestId("dot-3")).toBeVisible({ timeout: 2 * MINUTE });
  await expect(page.getByTestId("progress-line")).toHaveCount(0);
  await expect(height.locator("input").first()).toHaveValue("46");
  await expect(page.getByTestId("rail-label")).toContainText("v3");

  // The rail says what changed, and an older version can be looked at and left again.
  await page.getByTestId("dot-2").hover();
  await expect(page.getByTestId("version-card")).toHaveAttribute("data-version", "2");
  await page.getByTestId("dot-3").hover();
  await expect(page.getByTestId("version-card")).toContainText("46");
  await snap("design-version-card");
  await page.getByTestId("dot-2").click();
  await expect(page).toHaveURL(/[?&]v=2/);
  await expect(page.getByTestId("verdict")).toContainText("v2");
  await snap("design-looking-at-older");
  await page.getByTestId("verdict-primary").click();
  await expect(page).not.toHaveURL(/[?&]v=2/);

  // Cut open, and a click on the part that goes into the message box as words.
  const view = page.getByTestId("viewport");
  await view.focus();
  await page.keyboard.press("c");
  await expect(page.getByRole("slider", { name: "Where the cut is" })).toBeVisible();
  await snap("design-cut-open");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("slider", { name: "Where the cut is" })).toHaveCount(0);
  const canvas = view.locator("canvas").first();
  const area = await canvas.boundingBox();
  if (!area) throw new Error("the canvas has a size");
  await page.mouse.click(area.x + area.width / 2, area.y + area.height / 2);
  await expect(page.getByTestId("picked")).toBeVisible();
  await snap("design-face-picked");
  await page.getByRole("button", { name: "Mention in message" }).click();
  const composer = conversation.getByRole("textbox");
  // In words, with the place on the part named; the coordinates follow in brackets.
  await expect(page.getByTestId("picked-text")).toHaveText(/^(box|lid) · (the|a|an) [a-z ]+$/);
  await expect(composer).toHaveValue(/^On the (box|lid), at (the|a|an) [a-z ]+ \(near x /);
  await composer.fill("");
  await page.getByTestId("picked").getByRole("button", { name: "Clear" }).click();

  // Export: STEP, 3MF and STL into a folder (the bridge answers the folder picker).
  await page.getByTestId("project-name").click();
  await expect(page.getByTestId("export")).toBeVisible();
  await page.getByTestId("export").getByRole("checkbox").nth(2).click();
  await page.getByTestId("export-go").click();
  await expect(page.getByTestId("exported")).toBeVisible({ timeout: MINUTE });
  await snap("design-export");
  await page.keyboard.press("Escape");

  // A question is answered where it is asked: the measurement goes to Hotend as a
  // message. The written conversation has no further turn, and says so.
  await page.getByTestId("measure-cable").click();
  const measure = page.getByTestId("measure-cable");
  await expect(measure).toHaveAttribute("data-open", /.*/);
  await measure.locator("input").first().fill("14");
  await snap("design-question");
  await page.getByTestId("use-measurement-cable").click();
  await expect(conversation).toContainText("I measured Cable slot: 14 mm. Use this value and mark it as measured.");
  await expect(page.getByTestId("stop-run")).toHaveCount(0, { timeout: 2 * MINUTE });
  await expect(conversation).toContainText("The scripted demo has no more turns.", { timeout: MINUTE });
  // The run made no new version: the row says that the measurement was not applied.
  await expect(page.getByTestId("not-applied")).toContainText("Not applied: the part is still 12 mm.");

  // Prepare: the plate is laid out, each part with the way it lies and why.
  await page.getByTestId("stage-prepare").click();
  await expect(page.getByTestId("prepare-panel")).toHaveAttribute("data-pieces", "2", { timeout: 2 * MINUTE });
  await expect(page.getByTestId("piece-box")).toBeVisible();
  await expect(page.getByTestId("piece-lid")).toBeVisible();
  await expect(page.getByTestId("verdict-primary")).toHaveText("Slice");
  await snap("prepare-laid-out");
  await page.getByTestId("turn-lid").click();
  await expect(page.getByTestId("turn-popover")).toBeVisible();
  await expect(page.getByTestId("turn-popover")).toContainText("Recommended");
  await snap("prepare-turn");
  await page.keyboard.press("Escape");
  // A Select opens under the window's content policy (the fixture fails on a refusal).
  await page.getByRole("combobox", { name: "Quality" }).click();
  await expect(page.getByRole("option", { name: "Fine · 0.12 mm" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("option", { name: "Fine · 0.12 mm" })).toHaveCount(0);

  // Slice with the stand-in slicer: time, grams, and the honest tag.
  await page.getByTestId("verdict-primary").click();
  await expect(page.getByTestId("slice-time")).toBeVisible({ timeout: 2 * MINUTE });
  await expect(page.getByTestId("slice-section")).toHaveAttribute("data-stand-in", /.*/);
  await expect(page.getByTestId("slice-section")).toContainText("Stand-in slicer");
  await expect(page.getByTestId("slice-section")).toContainText(/\d+ g/);
  await expect(page.getByTestId("verdict-primary")).toHaveText("Review print");
  // Stand-in numbers are marked as rough where they are shown: the flask, not the tick.
  await expect(page.getByTestId("verdict")).toHaveAttribute("data-kind", "sim");
  await expect(page.getByTestId("verdict")).toContainText("Sliced (rough): about");
  await snap("prepare-sliced", { wide: true });

  // Print: the review, and the one button that asks the person. The bridge's test mode
  // answers the confirmation dialog; without it nothing would start.
  await page.getByTestId("verdict-primary").click();
  await expect(page).toHaveURL(/\/print/);
  await expect(page.getByTestId("review-facts")).toBeVisible({ timeout: MINUTE });
  await expect(page.getByTestId("send-to")).toContainText("Simulated");
  await expect(page.getByTestId("start-print")).toBeEnabled({ timeout: MINUTE });
  await expect(page.getByTestId("start-print")).toHaveText("Start simulated print…");
  await expect(page.getByTestId("review-stand-in")).toContainText("Stand-in slicer");
  await expect(page.getByTestId("review-facts")).not.toContainText("contains no moves");
  await snap("print-review", { wide: true });
  await page.getByTestId("start-print").click();

  // The simulated printer prints, labelled as simulated, and finishes.
  await expect(page.getByTestId("running")).toBeVisible({ timeout: MINUTE });
  await expect(page.getByTestId("sim-band")).toContainText("Simulated printer");
  await expect(page.getByTestId("print-percent")).toBeVisible({ timeout: 2 * MINUTE });
  await snap("print-running", { wide: true });
  await page.getByTestId("printer-chip").click();
  await expect(page.getByRole("button", { name: "Go to the print" })).toBeVisible();
  await snap("print-printer-chip");
  await expect(page.getByTestId("chip-sim-note")).toBeVisible();
  await page.keyboard.press("Escape");
  // Stop asks first, and the question stays until it is answered: after five seconds it
  // is still there, and Esc gives the focus back to Stop.
  await page.getByTestId("stop-print").focus();
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("stop-print-sure")).toBeFocused();
  await page.waitForTimeout(5000);
  await expect(page.getByTestId("stop-asking")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("stop-print")).toBeFocused();
  // The pictures' printer is slow on purpose (screens.spec.ts): up to twenty minutes.
  await expect(page.getByTestId("outcome")).toHaveAttribute("data-status", "finished", { timeout: 20 * MINUTE });
  await expect(page.getByTestId("sim-band")).toHaveCount(0);
  // A simulated print made nothing: no green tick, no "Print again".
  await expect(page.getByTestId("verdict")).toHaveAttribute("data-kind", "sim");
  await expect(page.getByTestId("verdict")).toContainText("Simulated print finished. Nothing was made.");
  await expect(page.getByTestId("verdict-primary")).toHaveText("Back to Design");
  await expect(page.getByRole("button", { name: "Print again" })).toHaveCount(0);
  await snap("print-finished");

  // Home lists the project with the picture of its current version.
  await page.getByTestId("stage-design").click();
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", "2", { timeout: MINUTE });
  // The picture is taken once the version has been on the stage for a second.
  await page.waitForTimeout(2500);
  await page.getByTestId("home-button").click();
  const card = page.getByTestId("project-card");
  await expect(card).toHaveCount(1);
  await expect(card).toContainText("A box that hides a power strip");
  await expect(card.getByTestId("card-status")).toContainText("v3");
  await expect(card.getByTestId("card-picture")).toBeVisible();
  await snap("home-projects", { wide: true });

  // Calibration: the gauge of this project is exported, a measurement goes in, and the
  // printer's table changes.
  await page.keyboard.press("Control+,");
  await expect(page.getByTestId("settings")).toBeVisible();
  await page.getByRole("link", { name: "Calibration" }).click();
  await expect(page.getByTestId("settings-calibration")).toBeVisible();
  await page.getByTestId("gauge-export").click();
  await expect(page.getByTestId("gauge-exported")).toBeVisible({ timeout: 2 * MINUTE });
  const features = page.getByTestId("gauge-features");
  await expect(features).toBeVisible();
  // A hole that came out a little small, as holes do: 0.1 mm under what was drawn.
  const first = features.locator("tbody tr").first();
  const drawn = Number.parseFloat((await first.locator("td").nth(1).textContent()) ?? "");
  expect(drawn).toBeGreaterThan(0);
  // A reading far from the drawing is a slip: it is said at the field and nothing is sent.
  await first.locator("input").first().fill("99");
  await first.locator("input").first().press("Tab");
  await expect(page.getByTestId("gauge-off")).toContainText("Check the unit and the feature.");
  await expect(page.getByTestId("calibration-record")).toBeDisabled();
  await first.locator("input").first().fill((drawn - 0.1).toFixed(2));
  await first.locator("input").first().press("Tab");
  await page.getByTestId("calibration-record").click();
  await expect(page.getByTestId("calibration-result")).toBeVisible({ timeout: MINUTE });
  // In a maker's words, and without the green tick: one gauge print does not prove a fit.
  await expect(page.getByTestId("calibration-result")).toContainText(/Holes on your printer come out 0\.\d+\smm smaller than drawn\. Hotend now draws them larger\./);
  const state = page.getByTestId("calibration-state");
  await expect(state).toContainText("Calibrated from 1 gauge print (1 feature).");
  await expect(state).toContainText("Fits will still show as not proven");
  await expect(state.locator("[data-status=ok]")).toHaveCount(0);
  // The files are named, and the one to print is pointed at.
  await expect(page.getByTestId("gauge-files")).toContainText(".3mf");
  await expect(page.getByTestId("gauge-print")).toHaveText("Print this");
  await snap("settings-calibration");
  // The loop closes: the project is built again with what was measured, and its Proof
  // says the same thing Settings says.
  await page.getByTestId("calibration-rebuild").click();
  await expect(page.getByTestId("calibration-rebuilt")).toContainText("built with your printer's values", { timeout: 2 * MINUTE });
  await page.getByTestId("calibration-rebuilt").getByRole("link").click();
  await expect(page.getByTestId("viewport-host")).toHaveAttribute("data-parts", "2", { timeout: MINUTE });
  await expect(page.getByTestId("rail-label")).toContainText("Built again with your printer's values");
  // One hole was measured: the Proof names that and nothing more.
  const fits = page.getByTestId("inspector").getByRole("button", { name: /Fits partly measured on this printer, not yet proven/ });
  await fits.click();
  await expect(page.getByTestId("fits-state")).toHaveAttribute("data-fits", "measured");
  await expect(page.getByTestId("fits-state")).toContainText("This version uses your printer's measured hole sizes.");
  await expect(page.getByTestId("fits-state")).toContainText(
    "Hotend still uses general values for peg sizes, shrinkage across the plate, shrinkage in height and the smallest gap that still comes apart.",
  );
}
