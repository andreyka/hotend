// Fails the window's build when anything of the development bridge is in it. The bridge
// transport (src/lib/core/bridge.ts) is reachable only behind `__HOTEND_BRIDGE__`, which
// is false in this build, so the bundler removes it; this proves that it did.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const dist = fileURLToPath(new URL("../dist", import.meta.url));
// Strings that only the bridge transport and Tauri's mock module contain.
const marks = ["/invoke/", "hotend.bridge", "hotend-bridge", "Bearer ", "mockIPC"];

const found = [];
for (const entry of readdirSync(dist, { recursive: true, withFileTypes: true })) {
  if (!entry.isFile() || !/\.(js|html|css)$/.test(entry.name)) continue;
  const file = join(entry.parentPath, entry.name);
  const text = readFileSync(file, "utf8");
  for (const mark of marks) {
    if (text.includes(mark)) found.push(`${file}: ${JSON.stringify(mark)}`);
  }
}
if (found.length > 0) {
  console.error("The window's build contains the development bridge:\n" + found.join("\n"));
  process.exit(1);
}
console.log("dist/ holds nothing of the development bridge.");
