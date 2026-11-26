// End-to-end tests: the bridge build of the interface in headless Chromium, talking to
// `hotend-bridge` (scripted agent, stand-in slicer, simulated printer, temporary data).
// Nothing here reaches the network: both servers listen on 127.0.0.1.
import { defineConfig } from "@playwright/test";

// HOTEND_CHROMIUM is the Chromium to use (scripts/setup-wsl.sh says where it is). Without
// it Playwright uses the browser of its own `playwright install chromium`.
const chromium = process.env.HOTEND_CHROMIUM;

export default defineConfig({
  testDir: "e2e",
  outputDir: "test-results",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: true,
  timeout: 180_000,
  expect: { timeout: 20_000 },
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:4173",
    viewport: { width: 1280, height: 800 },
    launchOptions: {
      ...(chromium ? { executablePath: chromium } : {}),
      // WebGL without a GPU: ANGLE on SwiftShader.
      // No network: Chromium's own background requests are off, and no name but the
      // loopback address resolves.
      args: [
        "--use-angle=swiftshader",
        "--enable-unsafe-swiftshader",
        "--ignore-gpu-blocklist",
        "--disable-background-networking",
        // A proxy named in the environment would resolve names for the browser: none is used.
        "--no-proxy-server",
        "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
      ],
    },
  },
  webServer: {
    // Serves dist-bridge/ under the window's content policy (vite.config.ts).
    command: "pnpm preview:bridge",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
