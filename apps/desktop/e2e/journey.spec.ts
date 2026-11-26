// The main journey, as a test (see journey.ts).
import { test } from "./bridge";
import { journey } from "./journey";

// The simulated printer runs 120 times faster than a real one: the print of about an hour
// takes half a minute.
test.use({ bridgeArgs: ["--script", "builtin:cable_box_demo", "--answer-dialogs-for-tests", "--time-scale", "120"] });

test("from a sentence to a finished simulated print", async ({ app }) => {
  test.setTimeout(15 * 60_000);
  await journey(app, async () => {});
});
