import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

import { bridgePolicy, devPolicy, policyHeader, windowPolicy } from "./csp.ts";

/**
 * The licence list of About, read from docs/DEPENDENCIES.md when the interface is built,
 * so that the window and the document cannot disagree: [name, version, licence] per row.
 */
function licences(): [string, string, string][] {
  const text = readFileSync(fileURLToPath(new URL("../../docs/DEPENDENCIES.md", import.meta.url)), "utf8");
  const rows = new Map<string, [string, string, string]>();
  for (const line of text.split("\n")) {
    const cells = line.split("|").map((cell) => cell.trim());
    const [, name, version, licence] = cells;
    if (!name || !version || !licence || name === "Name" || name.startsWith("---")) continue;
    if (/\((dev|build)/.test(version) || /\bdev\)/.test(name)) continue;
    // A row may name several packages ("a, b" with "1.0, 2.0"), and a version cell may
    // go on about features: the About page lists one package per line, with its number.
    const names = name.replace(/\s*\(.*\)\s*$/, "").replaceAll("`", "").split(/,\s*/);
    const versions = version
      .replace(/\s*\(.*\)\s*$/, "")
      .replace(/,\s*(features?|no default|default)\b.*$/i, "")
      .split(/,\s*/);
    const kinds = licence.split(/,\s*/);
    names.forEach((plain, index) => {
      if (rows.has(plain)) return;
      rows.set(plain, [
        plain,
        (versions.length === names.length ? versions[index] : versions.join(", ")) ?? "",
        (kinds.length === names.length ? kinds[index] : licence) ?? licence,
      ]);
    });
  }
  return [...rows.values()].sort((a, b) => a[0].localeCompare(b[0]));
}

// Two builds of the same interface:
//   `vite build`               the window's files (src-tauri embeds dist/). No bridge code.
//   `vite build --mode bridge` the same interface for a normal browser, talking to the
//                              development bridge (dist-bridge/). Never shipped.
export default defineConfig(({ mode, command, isPreview }) => {
  const bridge = mode === "bridge";
  // The development server sends the window's policy too (csp.ts, `devPolicy`). Vite
  // marks the tags it writes with a nonce, new each time the server starts.
  const nonce = command === "serve" && !isPreview ? randomBytes(18).toString("base64url") : null;
  return {
    plugins: [react(), tailwindcss()],
    resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
    define: { __HOTEND_BRIDGE__: JSON.stringify(bridge), __HOTEND_LICENCES__: JSON.stringify(licences()) },
    clearScreen: false,
    ...(nonce === null ? {} : { html: { cspNonce: nonce } }),
    server: {
      port: 1420,
      strictPort: true,
      host: "localhost",
      ...(nonce === null
        ? {}
        : { headers: { "Content-Security-Policy": policyHeader(devPolicy(bridge ? bridgePolicy() : windowPolicy(), nonce)) } }),
    },
    preview: {
      port: 4173,
      strictPort: true,
      host: "127.0.0.1",
      // The tests load the interface under the window's own content policy.
      headers: { "Content-Security-Policy": policyHeader(bridgePolicy()) },
    },
    build: {
      target: "es2022",
      // A data: URL would need its own source in the content policy; every asset is a file.
      assetsInlineLimit: 0,
      // The viewer bundles three.js; it is loaded only with the studio.
      chunkSizeWarningLimit: 4000,
    },
    test: {
      environment: "jsdom",
      include: ["tests/**/*.test.{ts,tsx}"],
      restoreMocks: true,
    },
  };
});
