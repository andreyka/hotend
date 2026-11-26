// The window's content policy (src-tauri/tauri.conf.json) and the one the tests load the
// interface under.
import { describe, expect, it } from "vitest";

import { join } from "node:path";
import { loadConfigFromFile } from "vite";

import { bridgePolicy, devPolicy, policyHeader, windowPolicy } from "../csp";

describe("the window's content policy", () => {
  const policy = windowPolicy();

  it("starts from nothing", () => {
    expect(policy["default-src"]).toBe("'none'");
  });

  it("runs only the window's own script files: no inline script, no eval", () => {
    expect(policy["script-src"]).toBe("'self'");
    for (const sources of Object.values(policy)) {
      expect(sources).not.toContain("unsafe-eval");
      expect(sources).not.toContain("wasm-unsafe-eval");
    }
    expect(policy["script-src-elem"]).toBeUndefined();
    expect(policy["script-src-attr"]).toBeUndefined();
  });

  it("loads nothing from another computer", () => {
    for (const [directive, sources] of Object.entries(policy)) {
      for (const source of sources.split(/\s+/)) {
        const local =
          ["'none'", "'self'", "data:", "ipc:", "http://ipc.localhost"].includes(source) ||
          (directive === "style-src-attr" && source === "'unsafe-inline'");
        expect(local, `${directive} ${source}`).toBe(true);
      }
    }
    expect(policy["connect-src"]).toBe("ipc: http://ipc.localhost");
  });

  it("allows inline styles only as attributes, never as style sheets", () => {
    expect(policy["style-src"]).toBe("'self'");
    expect(policy["style-src-attr"]).toBe("'unsafe-inline'");
  });

  it("allows data: only for pictures", () => {
    const withData = Object.entries(policy)
      .filter(([, sources]) => sources.includes("data:"))
      .map(([directive]) => directive);
    expect(withData).toEqual(["img-src"]);
  });

  it("closes what the window never uses", () => {
    for (const directive of ["base-uri", "form-action", "frame-src", "object-src", "worker-src"]) {
      expect(policy[directive]).toBe("'none'");
    }
  });
});

describe("the policy of the bridge build", () => {
  it("is the window's policy with the loopback bridge in place of Tauri's IPC", () => {
    const { "connect-src": windowConnect, ...windowRest } = windowPolicy();
    const { "connect-src": bridgeConnect, ...bridgeRest } = bridgePolicy();
    expect(bridgeRest).toEqual(windowRest);
    expect(windowConnect).not.toBe(bridgeConnect);
    expect(bridgeConnect).toBe("http://127.0.0.1:*");
  });

  it("is written as one header", () => {
    expect(policyHeader({ "default-src": "'none'", "img-src": "'self' data:" })).toBe(
      "default-src 'none'; img-src 'self' data:",
    );
  });
});

// Tauri puts the window's policy only on files it serves itself. A debug window loads the
// interface from the Vite development server, which therefore sends the policy too.
describe("the policy of the development server", () => {
  const NONCE = "bm9uY2Ugb2YgdGhlIHRlc3Q";

  it("is the window's policy plus a nonce for Vite's own tags and its hot-reload connection", () => {
    const base = windowPolicy();
    const dev = devPolicy(base, NONCE);
    expect(Object.keys(dev)).toEqual(Object.keys(base));
    const changed = Object.keys(base).filter((directive) => dev[directive] !== base[directive]);
    expect(changed.sort()).toEqual(["connect-src", "script-src", "style-src"]);
    expect(dev["script-src"]).toBe(`'self' 'nonce-${NONCE}'`);
    expect(dev["style-src"]).toBe(`'self' 'nonce-${NONCE}'`);
    expect(dev["connect-src"]).toBe("ipc: http://ipc.localhost 'self' ws://localhost:1420");
    expect(devPolicy(bridgePolicy(), NONCE)["connect-src"]).toBe("http://127.0.0.1:* 'self' ws://localhost:1420");
  });

  it("adds no inline script without the nonce, no eval and no other computer", () => {
    const base = windowPolicy();
    const dev = devPolicy(base, NONCE);
    for (const [directive, sources] of Object.entries(dev)) {
      const added = sources.split(/\s+/).filter((source) => !(base[directive] ?? "").split(/\s+/).includes(source));
      for (const source of added) {
        expect([`'nonce-${NONCE}'`, "'self'", "ws://localhost:1420"], `${directive} ${source}`).toContain(source);
      }
    }
    expect(dev["default-src"]).toBe("'none'");
    expect(dev["script-src"]).not.toMatch(/unsafe|eval/);
  });

  const config = async (command: "serve" | "build", mode: string, isPreview = false) => {
    const loaded = await loadConfigFromFile({ command, mode, isPreview }, join(import.meta.dirname, "..", "vite.config.ts"));
    if (!loaded) throw new Error("vite.config.ts was not read");
    return loaded.config;
  };
  const sent = (headers: unknown): string => String((headers as Record<string, unknown> | undefined)?.["Content-Security-Policy"] ?? "");

  it("is what the development server sends, with a nonce that is new at each start", async () => {
    const [first, second, bridge] = await Promise.all([config("serve", "development"), config("serve", "development"), config("serve", "bridge")]);
    const nonce = first.html?.cspNonce ?? "";
    expect(nonce).toMatch(/^[A-Za-z0-9_-]{24}$/);
    expect(sent(first.server?.headers)).toBe(policyHeader(devPolicy(windowPolicy(), nonce)));
    expect(second.html?.cspNonce).not.toBe(nonce);
    expect(sent(bridge.server?.headers)).toBe(policyHeader(devPolicy(bridgePolicy(), bridge.html?.cspNonce ?? "")));
  });

  it("leaves the window's own files and the preview of the tests without a nonce", async () => {
    const [build, preview] = await Promise.all([config("build", "production"), config("serve", "bridge", true)]);
    expect(build.html?.cspNonce).toBeUndefined();
    expect(sent(build.server?.headers)).toBe("");
    expect(preview.html?.cspNonce).toBeUndefined();
    expect(sent(preview.preview?.headers)).toBe(policyHeader(bridgePolicy()));
  });
});
