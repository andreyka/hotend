// The window's content security policy has one source: src-tauri/tauri.conf.json. This
// module reads it for the places outside Tauri that need it: the development server a
// debug window loads the interface from, the preview server the Playwright tests load it
// from, and the tests that check the policy itself.
import { readFileSync } from "node:fs";
import { join } from "node:path";

const CONFIG = join(import.meta.dirname, "src-tauri", "tauri.conf.json");

export type Policy = Record<string, string>;

/** The policy of the shipped window, directive by directive. */
export function windowPolicy(): Policy {
  const config = JSON.parse(readFileSync(CONFIG, "utf8")) as {
    app: { security: { csp: Policy } };
  };
  return config.app.security.csp;
}

/**
 * The policy for the interface in a browser, talking to the development bridge. It is the
 * window's policy with one change: `connect-src` names the loopback bridge instead of
 * Tauri's IPC, because that is the only thing that differs between the two transports.
 */
export function bridgePolicy(): Policy {
  return { ...windowPolicy(), "connect-src": "http://127.0.0.1:*" };
}

/** Where the Vite development server listens (vite.config.ts; `devUrl` of tauri.conf.json). */
export const DEV_SERVER = "localhost:1420";

/**
 * The policy of the Vite development server (`pnpm dev`, `pnpm dev:bridge`). Tauri puts
 * the window's policy only on files it serves itself; a debug window loads the interface
 * from this server, so without this header it would run under no policy at all and a
 * change that breaks the policy would first show in a release.
 *
 * It is `base` (the window's or the bridge build's policy) plus what Vite's development
 * client needs and nothing else: its script and style tags carry `nonce`, and it talks to
 * its own server for hot reloading. No `unsafe-inline` and no `eval` are added.
 */
export function devPolicy(base: Policy, nonce: string): Policy {
  const also = (directive: string, sources: string) => `${base[directive] ?? ""} ${sources}`.trim();
  return {
    ...base,
    "script-src": also("script-src", `'nonce-${nonce}'`),
    "style-src": also("style-src", `'nonce-${nonce}'`),
    "connect-src": also("connect-src", `'self' ws://${DEV_SERVER}`),
  };
}

export function policyHeader(policy: Policy): string {
  return Object.entries(policy)
    .map(([directive, sources]) => `${directive} ${sources}`)
    .join("; ");
}
