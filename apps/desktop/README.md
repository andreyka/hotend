# Hotend desktop

The window: a Tauri 2 shell (`src-tauri/`) around a React interface (`src/`). The shell is a
thin layer over the core's facade (`hotend_core::Core`); the contract is section 8.3 of
`docs/PROTOCOL.md`, the design is `docs/ux/DESIGN_SPEC.md`.

| Where | What |
| --- | --- |
| `src-tauri/src/ipc.rs` | The one table of commands: Tauri commands, the TypeScript binding and the bridge's dispatcher are generated from it |
| `src-tauri/src/confirm.rs` | `print_confirm`: the native dialog, the only place a print is confirmed |
| `src-tauri/capabilities/main.json`, `tauri.conf.json` | What the window may call, and its content policy |
| `src/lib/core/` | The typed client: one interface, two transports (Tauri, development bridge). `bindings.ts` is generated |
| `src/host/`, `src/hotend/` | The frame, theme and routes; Hotend's screens, store and 3D view |
| `tests/`, `e2e/` | Unit tests (vitest) and end-to-end tests (Playwright against the bridge) |

```sh
pnpm install --frozen-lockfile
pnpm typecheck && pnpm lint && pnpm test
pnpm build                      # dist/, what the window embeds; fails if bridge code is in it
pnpm tauri dev                  # the window, with the interface from the Vite server

# The interface in a browser, against a scripted core (no model, no key, simulated printer):
cargo build -p hotend-desktop --features hotend-desktop/dev-bridge -p hotend-sandbox -p hotend-slicer
../../target/debug/hotend-bridge          # prints {"url": ..., "token": ...}
pnpm dev:bridge                            # then open http://localhost:1420/?bridge=<url>&token=<token>

# End to end, headless (HOTEND_CHROMIUM names the Chromium to use):
pnpm build:bridge && HOTEND_CHROMIUM=<path to chromium> pnpm e2e

# Retake the pictures in docs/ux/screenshots/ (the journey of e2e/journey.ts, both themes):
pnpm build:bridge && HOTEND_CHROMIUM=<path to chromium> pnpm screenshots
# (on a slow machine add HOTEND_PRINT_TIME_SCALE=3: the simulated print
#  then lasts twenty minutes, long enough for its pictures)

# After changing a command or a type in src-tauri/src/ipc.rs or wire.rs:
pnpm bindings
```

The bridge and `hotend-confirm-dialog` need the cargo feature `dev-bridge`, which the build
script refuses in a release profile.

**A debug window trusts port 1420.** A debug build (`pnpm tauri dev`, or
`target/debug/hotend-desktop` run by hand) embeds no interface: it loads whatever answers
on `http://localhost:1420` and gives that page every command of the window. Tauri puts the
window's content policy only on files it serves itself, so in a debug window the policy is
the header Hotend's own development server sends (`csp.ts`, `devPolicy`: the window's
policy plus a nonce for Vite's tags and its hot-reload connection). Start a debug window
only through `pnpm tauri dev`, which starts that server first, and give it a scratch data
folder (`HOTEND_DATA_DIR=...`, `HOTEND_SECRETS=env`), never your real projects and keys.
A release build embeds its interface and is not affected.

`.npmrc` turns on pnpm's shell emulator, so that the scripts which set a variable
(`pnpm bindings`, `pnpm screenshots`) run on Windows as they do elsewhere.
