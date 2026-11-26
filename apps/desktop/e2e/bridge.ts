// Starts `hotend-bridge` for a test file and opens the interface against it.
import { test as base, expect, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

export type Bridge = { url: string; token: string; dataDir: string };

// Cargo names the program `hotend-bridge.exe` on Windows.
const PROGRAM = process.platform === "win32" ? "hotend-bridge.exe" : "hotend-bridge";
const BINARY =
  process.env.HOTEND_BRIDGE_BIN ?? fileURLToPath(new URL(`../../../target/debug/${PROGRAM}`, import.meta.url));

async function start(extra: string[]): Promise<{ bridge: Bridge; process: ChildProcess }> {
  if (!existsSync(BINARY)) {
    throw new Error(
      `${BINARY} is not built. Run: cargo build --locked -p hotend-desktop --features hotend-desktop/dev-bridge ` +
        "-p hotend-sandbox -p hotend-slicer",
    );
  }
  const child = spawn(BINARY, extra, { stdio: ["ignore", "pipe", "inherit"] });
  const lines = createInterface({ input: child.stdout });
  const first = await new Promise<string>((resolve, reject) => {
    lines.once("line", resolve);
    child.once("exit", (code) => {
      reject(new Error(`hotend-bridge stopped with code ${String(code)} before it was ready`));
    });
    child.once("error", reject);
  });
  const ready = JSON.parse(first) as { url: string; token: string; data_dir: string };
  return { bridge: { url: ready.url, token: ready.token, dataDir: ready.data_dir }, process: child };
}

async function stop(child: ChildProcess) {
  if (child.exitCode !== null) return;
  const gone = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  await gone;
}

type Options = { bridgeArgs: string[] };
type Fixtures = { bridge: Bridge; app: Page; violations: string[] };

/**
 * `bridge` is one hotend-bridge per test, with its own temporary data. `app` is the
 * interface opened against it. `violations` collects what the content policy refused and
 * every uncaught error; each test ends by checking that it is empty.
 */
export const test = base.extend<Fixtures & Options>({
  bridgeArgs: [[], { option: true }],

  bridge: async ({ bridgeArgs }, use) => {
    const { bridge, process } = await start(bridgeArgs);
    await use(bridge);
    await stop(process);
  },

  violations: async ({ page }, use) => {
    const seen: string[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") seen.push(`console: ${message.text()}`);
    });
    page.on("pageerror", (error) => seen.push(`error: ${error.message}\n${error.stack ?? ""}`));
    await page.addInitScript(() => {
      document.addEventListener("securitypolicyviolation", (event) => {
        console.error(
          `content policy refused ${event.violatedDirective} for ${event.blockedURI} ` +
            `(${event.sourceFile ?? ""}:${String(event.lineNumber)})`,
        );
      });
    });
    await use(seen);
    expect(seen, "no content policy violation and no uncaught error").toEqual([]);
  },

  app: async ({ page, bridge, violations }, use) => {
    void violations;
    const query = new URLSearchParams({ bridge: bridge.url, token: bridge.token });
    await page.goto(`/?${query.toString()}`);
    await use(page);
  },
});

export { expect };

/** Calls a command of the bridge directly, as the interface's transport does. */
export async function invoke<T>(bridge: Bridge, command: string, args: object = {}): Promise<T> {
  const response = await fetch(`${bridge.url}/invoke/${command}`, {
    method: "POST",
    headers: { authorization: `Bearer ${bridge.token}`, "content-type": "application/json" },
    body: JSON.stringify(args),
  });
  const answer = (await response.json()) as { ok: boolean; data: T; error?: { message: string } };
  if (!answer.ok) throw new Error(`${command}: ${answer.error?.message ?? "refused"}`);
  return answer.data;
}
