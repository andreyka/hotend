# Dependencies

One line per direct dependency: name, version, licence, why it is here. Add yours in the same change.
A transitive dependency has a line when its licence needs a note. Hotend is Apache-2.0.

| Name | Version | Licence | Why |
| --- | --- | --- | --- |
| build123d | 0.13.0 | Apache-2.0 | CAD modelling API the agent writes against |
| cadquery-ocp-novtk | 8.0.1.1.0 | Apache-2.0 (OCCT: LGPL-2.1 with exception) | OpenCascade kernel bindings, no VTK |
| ocp-tessellate | 3.6.3 | Apache-2.0 | Tessellation in the format three-cad-viewer loads |
| bd-warehouse | 0.4.0 | Apache-2.0 | Standard parts: fasteners, threads, bearings |
| manifold3d | 3.5.5 | Apache-2.0 | The closed-manifold check on each part's tessellation, the ray casts of the wall-thickness check, and the free space of a `cavity_min` contract requirement (bounding box minus the part, eroded by the required cuboid with `minkowski_difference`) (`hotend_engine`) |
| lib3mf | 2.5.0 | BSD-3-Clause | Writes `export/model.3mf` and the plate files `plates/<n>/plate.3mf` of the `prepare` and `gauge` ops; called through build123d's `Mesher`, not imported by Hotend code. The engine tests import it to read the plate files back |
| pillow | 13.0.0 | MIT-CMU (HPND) | PNG encoding and caption text of the engine's rendered views |
| pydantic | 2.x | MIT | Schemas for contracts, requests and results |
| pydantic-ai-slim | 2.x | MIT | Agent loop and provider adapters |
| httpx2 | 2.16.2 | BSD-3-Clause | HTTP client of pydantic-ai-slim 2.x and the provider SDKs (already a transitive dependency); `hotend_agent` subclasses its `AsyncBaseTransport` to send every model request through the core |
| certifi (transitive) | 2026.10.23 | MPL-2.0 (file-level copyleft; shipped unmodified) | Mozilla's root certificate bundle, unmodified in the Python environment. Pulled in (`python/uv.lock`) by `httpx` 0.28.1 and `httpcore` 1.0.9, which come only through `google-genai`, and by `requests` 2.34.2, which comes through `build123d`, `google-auth`, `google-genai`, `threejs-materials` and `tiktoken`. Hotend's own Python never opens a network connection with it: the engine worker and the agent run in the sandbox with no network (the engine has no socket of any family, the agent none except an anonymous pair to itself; crates/hotend-sandbox), and the agent's model requests leave through `hotend_agent.transport.CoreTransport` over its pipe to the core, which makes the TLS connection in Rust. The agent's HTTP stack (`httpx2` under `pydantic-ai-slim`, `anthropic`, `openai`) does not depend on certifi (it uses `truststore`). So certifi is present but unused at run time. MPL-2.0 lets it be shipped unmodified with an Apache-2.0 program, its source available and its licence text in the notices |
| numpy | 2.5.5 | BSD-3-Clause | Mesh arrays of the tessellation, the mesh-based checks and the view renderer (`hotend_engine`) |
| pytest | 9.2.0 (dev) | MIT | Runs the Python test suite (`python/tests`) |
| pytest-timeout | 2.4.0 (dev) | MIT | Per-test time limits for the agent tests, which start real processes |
| ruff | 0.17.5 (dev) | MIT | Lint for `python/` (`uv run ruff check .`) |
| uv_build | 0.11.x (build backend, not installed at runtime) | MIT OR Apache-2.0 | Installs the `python/` packages editable so `python -I -m hotend_engine.worker` can import them |
| landlock (Rust, hotend-sandbox) | 0.4.7 | MIT OR Apache-2.0 | Landlock rulesets: the sandbox's filesystem confinement, with per-ABI compatibility handling |
| seccompiler (Rust, hotend-sandbox) | 0.5.0 | Apache-2.0 OR BSD-3-Clause | Compiles the sandbox's seccomp deny-list to BPF and installs it |
| libc (Rust, hotend-sandbox) | 0.2.191 | MIT OR Apache-2.0 | System calls the launcher makes directly: clone, prctl, setrlimit, capset, pidfd, kill |
| serde, serde_json (Rust, hotend-sandbox) | 1.0.229, 1.0.154 | MIT OR Apache-2.0 | The `probe` report as JSON, written by the launcher and parsed by the library |
| thiserror (Rust, hotend-sandbox) | 2.0.21 | MIT OR Apache-2.0 | Typed `SandboxError` |
| tempfile (Rust, hotend-sandbox, dev) | 3.27.0 | MIT OR Apache-2.0 | Throwaway job folders in the sandbox tests |
| tokio (Rust, hotend-core) | 1.54.1 | MIT | Async runtime: child processes (engine jobs, the agent), timers, channels, the event broadcast |
| serde, serde_json (Rust, hotend-core) | 1.0.229, 1.0.154 | MIT OR Apache-2.0 | Typed, strict parsing of engine results, tool arguments, JSON-RPC messages, settings |
| thiserror (Rust, hotend-core) | 2.0.21 | MIT OR Apache-2.0 | Typed errors of every module |
| rusqlite (Rust, hotend-core) | 0.43.0, feature bundled | MIT (bundled SQLite: public domain) | The store: projects, versions, messages, audit log, settings. Bundled so no system SQLite is needed |
| uuid (Rust, hotend-core) | 1.29.3, feature v7 | Apache-2.0 OR MIT | UUIDv7 ids of projects, jobs and runs |
| sha2 (Rust, hotend-core) | 0.11.1 | MIT OR Apache-2.0 | SHA-256 of tool arguments (audit), exported and sliced files |
| base64 (Rust, hotend-core) | 0.23.1 | MIT OR Apache-2.0 | Bodies of `llm.request` / `llm.response_chunk` and inlined images in tool results |
| directories (Rust, hotend-core) | 6.0.0 | MIT OR Apache-2.0 | The per-OS data directory |
| getrandom (Rust, hotend-core) | 0.4.3 | MIT OR Apache-2.0 | OS randomness for the per-run egress token |
| jsonschema (Rust, hotend-core) | 0.61.2, no default features | MIT | Validates tool arguments against the registry's JSON Schemas (draft 2020-12); default features off so it never fetches a remote schema |
| reqwest (Rust, hotend-core) | 0.13.5, features rustls, stream, no default features | MIT OR Apache-2.0 (TLS: rustls, aws-lc-rs) | The egress proxy's HTTPS client, the only code that talks to a model provider; also the search adapters' client (`hotend_core::search`), which talks only to the chosen search service's API host (`api.keenable.ai`, `api.tavily.com`). No new dependency was added for the search services |
| keyring (Rust, hotend-core) | 4.2.0 | MIT OR Apache-2.0 | Provider keys in the OS keychain (Secret Service, macOS Keychain, Windows Credential Manager) |
| futures-util (Rust, hotend-core) | 0.3.34 | MIT OR Apache-2.0 | `BoxFuture` in the `SliceBackend` trait and `Stream` for the run events |
| zeroize (Rust, hotend-core) | 1.9.1 | Apache-2.0 OR MIT | Wipes a provider key from memory when its holder is dropped |
| libc (Rust, hotend-core, unix) | 0.2.191 | MIT OR Apache-2.0 | SIGTERM to the sandbox launcher (its documented stop signal) and `O_NOFOLLOW` when importing artefacts |
| jiff (Rust, hotend-core) | 1.0.0, feature std only, no default features | Unlicense OR MIT | The current time as an RFC 3339 UTC string: the date the core writes into a calibration measurement (`calibration_record`), so that no caller supplies it |
| axum (Rust, hotend-core, dev) | 0.8.9 | MIT | The in-process loopback stand-in for a model provider in the egress and supervisor tests, and for a search service in the search adapter tests |
| tempfile (Rust, hotend-core) | 3.27.0 | MIT OR Apache-2.0 | The throwaway data directories of the doctor report's engine and agent self-tests, removed when the test ends; data directories in tests |
| zip (Rust, hotend-slicer) | 9.6.1, feature deflate-flate2-zlib-rs only | MIT | Reads the Bambu `.gcode.3mf` archive (entry metadata and capped entry reads), writes the fake slicer's output, and rewrites an exported project with every entry copied raw except the one settings file whose list of changed settings is filled |
| png (Rust, hotend-slicer) | 0.18.1 | MIT OR Apache-2.0 | Reads Hotend's own plate renders and writes the Hotend-made plate previews (512 and 128 pixel RGBA) put into a sliced file when the setting `previews` is `hotend_made` and the slicer drew none (`hotend_slicer::previews`) |
| roxmltree (Rust, hotend-slicer) | 0.21.2 | MIT OR Apache-2.0 | Read-only XML tree for `Metadata/slice_info.config`, with DTDs refused and a node limit; also the fake slicer's 3MF model reader |
| toml (Rust, hotend-slicer) | 1.2.1, features parse, serde | MIT OR Apache-2.0 | Parses the printer facts and the slicer version policy, both kept as TOML data files |
| serde, serde_json (Rust, hotend-slicer) | 1.0.229, 1.0.154 | MIT OR Apache-2.0 | `SliceResult` as JSON, the data files, Bambu-style settings profiles |
| thiserror (Rust, hotend-slicer) | 2.0.21 | MIT OR Apache-2.0 | Typed `SlicerError` |
| sha2 (Rust, hotend-slicer) | 0.11.1 | MIT OR Apache-2.0 | SHA-256 of the sliced file and of an exported project, held in memory between its runs |
| tempfile (Rust, hotend-slicer) | 3.27.0 | MIT OR Apache-2.0 | The empty job folder of a version probe (`--help` in the sandbox); throwaway folders in tests |
| tokio (Rust, hotend-slicer) | 1.54.1, feature rt (dev: macros) | MIT | `spawn_blocking` for the slicer run behind the async `SliceBackend` |
| futures-util (Rust, hotend-slicer) | 0.3.34 | MIT OR Apache-2.0 | `BoxFuture` in the `SliceBackend` implementation |
| libc (Rust, hotend-slicer, unix) | 0.2.191 | MIT OR Apache-2.0 | `O_NOFOLLOW` when opening the sliced file |
| clap (Rust, hotend-cli) | 4.6.8, feature derive | MIT OR Apache-2.0 | Argument parsing, help and usage errors of the `hotend` command |
| rpassword (Rust, hotend-cli) | 7.5.5 | Apache-2.0 | The hidden prompt of `hotend key set`, so a key is never an argument and never echoed |
| libc (Rust, hotend-cli, unix) | 0.2.191 | MIT OR Apache-2.0 | `tcflush` on the terminal before the print confirmation question, so that only an answer typed after the question counts |
| windows-sys (Rust, hotend-cli, Windows) | 0.61.2, feature Win32_System_Console | MIT OR Apache-2.0 | `FlushConsoleInputBuffer` for the same purpose on Windows (type-checked, not run on Windows) |
| tokio (Rust, hotend-cli) | 1.54.1, features rt-multi-thread, macros, signal, time | MIT | Runtime for the async `Core`, Ctrl-C during `hotend chat`, the time-out while following a print job's events |
| serde, serde_json (Rust, hotend-cli) | 1.0.229, 1.0.154 | MIT OR Apache-2.0 | `doctor --json`, the demo's `slice-result.json` and `audit-log.json`, reading agent events |
| thiserror (Rust, hotend-cli) | 2.0.21 | MIT OR Apache-2.0 | Typed `CliError` with its exit code |
| tempfile (Rust, hotend-cli, dev) | 3.27.0 | MIT OR Apache-2.0 | Throwaway data directories in the end-to-end tests |
| base64 (Rust, hotend-cli, dev) | 0.23.1 | MIT OR Apache-2.0 | Decodes the `mesh.json` buffers in the end-to-end test |
| zip (Rust, hotend-printer) | 9.6.1, feature deflate-flate2-zlib-rs only | MIT | Reads the `.gcode.3mf` as a hostile archive in `hotend_printer::archive` (entry metadata and capped entry reads); writes fixtures in tests |
| zip (Rust, hotend-core, tests only) | 9.6.1, feature deflate-flate2-zlib-rs only | MIT | Writes small sliced-file fixtures for the tests of the editable-project hand-over |
| roxmltree (Rust, hotend-printer) | 0.21.2 | MIT OR Apache-2.0 | Read-only XML tree for `Metadata/slice_info.config` (DTDs refused, node limit): time, filaments and filament map of the confirmation summary |
| toml (Rust, hotend-printer) | 1.2.1, features parse, serde | MIT OR Apache-2.0 | Parses the printer and protocol facts in `profiles/bambu.toml` |
| serde, serde_json (Rust, hotend-printer) | 1.0.229, 1.0.154 | MIT OR Apache-2.0 | The Bambu LAN report and request messages, the profile file, summaries and events for the interface |
| thiserror (Rust, hotend-printer) | 2.0.21 | MIT OR Apache-2.0 | Typed errors (`JobError`, `TokenError`, `StartError`, `PrinterError`, `DeliveryError`, `LanError`) |
| sha2 (Rust, hotend-printer) | 0.11.1 | MIT OR Apache-2.0 | SHA-256 of the sliced file (start token, uploads, placed copies) and of the pinned certificate |
| percent-encoding (Rust, hotend-printer) | 2.3.2 | MIT OR Apache-2.0 | Encodes path and name in the `bambu-connect://import-file` link |
| zeroize (Rust, hotend-printer) | 1.9.1 | Apache-2.0 OR MIT | Wipes the printer access code from memory when its holder is dropped |
| libc (Rust, hotend-printer, unix) | 0.2.191 | MIT OR Apache-2.0 | `O_NOFOLLOW` when opening the sliced file |
| rumqttc (Rust, hotend-printer, features lan-status and lan) | 0.25.1, feature use-rustls, no default features | Apache-2.0 | MQTT client for the printer's report and request topics (`LanPrinter`); the read-only status listener uses only its packet codec (`mqttbytes`), never its client, so it has no publish path; the codec also builds the stand-in broker of the tests. Brings tokio (current-thread runtime inside its blocking client). `hotend-core` turns on `lan-status` for the status listener, never `lan` |
| suppaftp (Rust, hotend-printer, feature lan only) | 13.0.0, features rustls-aws-lc-rs, deprecated (implicit FTPS), no default features | MIT OR Apache-2.0 | Uploads the sliced file to the printer over implicit FTPS |
| rustls (Rust, hotend-printer, features lan-status and lan) | 0.23.46 | Apache-2.0 OR ISC OR MIT (crypto: aws-lc-rs) | The TLS 1.2-only client configuration with the pinned-certificate verifier shared by MQTT and FTPS; the TLS side of the stand-in servers in tests |
| x509-parser (Rust, hotend-printer, features lan-status and lan) | 0.18.1 | MIT OR Apache-2.0 | Reads the common name (the serial number) from the printer's certificate |
| tempfile (Rust, hotend-printer, dev) | 3.27.0 | MIT OR Apache-2.0 | Throwaway folders for sliced-file fixtures |
| rcgen (Rust, hotend-printer, dev) | 0.14.10 | MIT OR Apache-2.0 | The stand-in certificate authority and the certificates it signs (and one self-signed certificate, which pairing must refuse) for the loopback stand-in printer of the `lan` tests |
| Bambu Lab printer certificate authorities (data, hotend-printer, features lan-status and lan) | Not shipped. The user supplies them as a PEM file (`<config>/bambu-ca.pem` for the status listener; lan::pair(endpoint, serial, authorities) for the loopback transport) | — | Pairing needs the authorities a printer's certificate must chain to. The certificates are Bambu Lab's, published with no licence, and the collection that publishes them (OpenBambuAPI) is under the GFDL; neither was shown to be compatible with this project's licence (Apache-2.0), so the file is not part of it. Without a usable file pairing refuses (`LanError::NoIssuers`) and nothing is sent |
| bytes (Rust, hotend-printer, features lan-status and lan; and dev) | 1.12.1 | MIT | The buffer type of rumqttc's packet codec, in the status listener and in the stand-in broker. Already in the lock through rumqttc; no new crate |
| cssparser, cssparser-macros, selectors, dtoa-short, option-ext (Rust, transitive) | 0.37.0 and 0.38.0, 0.7.1, 0.38.0 and 0.41.0, 0.3.5, 0.2.0 | MPL-2.0 | File-level copyleft, used unmodified. `cssparser`, `cssparser-macros`, `selectors` and `dtoa-short` come through `tauri` (desktop application only); `option-ext` comes through `directories` (`hotend-core`, so also the `hotend` command). MPL-2.0 allows linking them into an Apache-2.0 program; a binary distribution carries the licence text and says that the unmodified sources are on crates.io |
| tauri (Rust, hotend-desktop) | 2.12.6, default features (no devtools) | Apache-2.0 OR MIT | The desktop shell: the window, the IPC with its capability checks, channels for the event stream |
| tauri-build (Rust, hotend-desktop, build) | 2.7.5 | Apache-2.0 OR MIT | Generates one permission per command from `build/command_names.rs` and checks the capability and configuration at build time |
| tauri-plugin-dialog (Rust, hotend-desktop) | 2.8.4 | Apache-2.0 OR MIT | The folder and file pickers, called from Rust only; none of its commands is granted to the window. The print confirmation is not drawn with it (its message dialog cannot make Cancel the default button and passes the text to GTK as a format). It brings `tauri-plugin-fs` as a library; that plugin is not registered |
| dbus, libdbus-sys (Rust, hotend-desktop, transitive, Linux and the BSDs) | 0.9.12, 0.2.7 | Apache-2.0 OR MIT | Come through `tao` (Tauri's windowing). `libdbus-sys` links the system's libdbus-1 (AFL-2.1 OR GPL-2.0-or-later) dynamically; that library is not shipped |
| gtk (Rust, hotend-desktop, Linux and the BSDs) | 0.18.2 | MIT | The print confirmation dialog (`src/dialog.rs`): a `MessageDialog` with Cancel as the default and focused button, a confirming button that is dead for its first second, and the text as plain text in a scrolling label. The same crate and version tauri already links |
| rfd (Rust, hotend-desktop) | 0.17.2, feature common-controls-v6, no default features | MIT | The print confirmation dialog on Windows and macOS (`src/dialog.rs`), with Cancel as its first, default button. The same crate and version tauri-plugin-dialog already uses. Type-checked for both, not run on either |
| open (Rust, hotend-desktop) | 5.4.5 | MIT | The list of the system's own openers (`open::commands`) for an allowlisted link; Hotend starts them itself, without the variables that may hold a secret (`src/app.rs`) |
| tauri-specta (Rust, hotend-desktop) | =2.0.0-rc.29, features derive, typescript | MIT | Collects the Tauri commands and writes `src/lib/core/bindings.ts` |
| specta, specta-typescript (Rust, hotend-desktop) | =2.0.0-rc.28 (features derive, serde_json), =0.0.15 | MIT | The type descriptions tauri-specta exports (the mirrors in `wire.rs`) and the TypeScript writer; pinned exactly because they are pre-releases that tauri-specta rc.29 is built against |
| url (Rust, hotend-desktop) | 2.5.8 | MIT OR Apache-2.0 | Parses the addresses of the link allowlist and of the window's navigation check |
| tokio (Rust, hotend-desktop) | 1.54.1 | MIT | The review lock, `spawn_blocking` for native dialogs, the event forwarder; with `dev-bridge` also the listener and signals |
| serde, serde_json, thiserror (Rust, hotend-desktop) | 1.0.229, 1.0.154, 2.0.21 | MIT OR Apache-2.0 | Command arguments and answers, the dispatcher's errors |
| axum (Rust, hotend-desktop, feature dev-bridge only) | 0.8.9, features http1, json, tokio, no default features | MIT | The HTTP server of the development bridge (`hotend-bridge`): routes, server-sent events. Never in a release build |
| getrandom, futures-util (Rust, hotend-desktop, feature dev-bridge only) | 0.4.3, 0.3.34 | MIT OR Apache-2.0 | The bridge's random token; the stream type of its server-sent events |
| tempfile (Rust, hotend-desktop, feature dev-bridge and dev) | 3.27.0 | MIT OR Apache-2.0 | The bridge's temporary data directory; throwaway folders in the tests |
| libwebkit2gtk-4.1-dev, libgtk-3-dev, libsoup-3.0-dev, libjavascriptcoregtk-4.1-dev, librsvg2-dev (system, Linux) | Ubuntu 24.04 packages | LGPL-2.1+ (dynamically linked system libraries) | What Tauri's Linux web view (wry on WebKitGTK) links against; needed to build `hotend-desktop` |
| xvfb, xdotool (system, Linux, tests only) | Ubuntu 24.04 packages | MIT (X.Org), BSD-3-Clause | The virtual screen and the key presses and clicks of `apps/desktop/src-tauri/tests/confirm_dialog.sh`, which holds the native print confirmation to its rules. Not shipped; without them `scripts/test-all.sh` reports that step as skipped |
| xdg-utils (system, Linux) | Ubuntu 24.04 package | MIT | `xdg-open`, the first program of the `open` crate's list (after PowerShell on WSL) when `open_link` opens an allowlisted address in the person's browser. Not shipped; `scripts/setup-wsl.sh --apt` installs it |
| Node.js (development and tests of apps/desktop) | 22 or newer (engines in apps/desktop/package.json); scripts/setup-wsl.sh installs 22.23.5 from nodejs.org, checked against a SHA-256 in the script, when the machine has none | MIT | Runs Vite, vitest, eslint, the TypeScript compiler and Playwright. Ubuntu 24.04's own package is version 18. Not shipped |
| pnpm (development and tests of apps/desktop) | 10.28.0 (packageManager in apps/desktop/package.json), through corepack | MIT | Installs the interface's packages from `pnpm-lock.yaml` and runs its scripts. Not shipped |
| Chromium of Playwright (tests of apps/desktop) | the build @playwright/test 1.63.0 downloads (playwright install chromium), or the one HOTEND_CHROMIUM names | BSD-3-Clause and others | The browser the interface's end-to-end tests run in. Not shipped; the application's window uses the system's web view |
| react, react-dom (npm, apps/desktop) | 19.3.1 | MIT | The interface |
| react-router (npm) | 8.5.1 | MIT | The routes of DESIGN_SPEC 3.1 (hash router) |
| zustand (npm) | 5.1.0 | MIT | The Hotend store and the theme store |
| @tauri-apps/api (npm) | 2.12.5 | Apache-2.0 OR MIT | `invoke` and `Channel` for the window's transport; `mockIPC` routes the same generated commands to the bridge in the bridge build |
| three-cad-viewer (npm) | 5.1.3 (pins three 0.184.0) | MIT (three.js: MIT) | The 3D view: renders `mesh.json` unchanged (PROTOCOL 3.4), orbit, views, picking |
| @base-ui/react (npm) | 1.9.0 | MIT | The unstyled primitives under the vendored shadcn/ui components |
| class-variance-authority, clsx, tailwind-merge (npm) | 0.7.1, 2.1.1, 3.7.0 | Apache-2.0, MIT, MIT | The variant and class helpers the vendored shadcn/ui components use |
| shadcn/ui components (vendored source, src/components/ui/) | registry bases/base as of 2026-11 | MIT | Button, Input, Textarea on Base UI; copied, not installed, as shadcn/ui is meant to be used |
| motion (npm) | 14.14.0 | MIT | Entering list items (DESIGN_SPEC 9), with the system's reduced-motion setting |
| lucide-react (npm) | 1.74.0 | ISC | Icons |
| @fontsource-variable/archivo, @fontsource-variable/instrument-sans, @fontsource-variable/jetbrains-mono (npm) | 5.3.0, 5.3.0, 5.3.0 | OFL-1.1 | Archivo, Instrument Sans and JetBrains Mono, shipped in the built interface so that the window loads no font from the network |
| tailwindcss, @tailwindcss/vite (npm, dev) | 4.3.5 | MIT | Styles from the design tokens |
| vite, @vitejs/plugin-react (npm, dev) | 8.3.8, 6.3.0 | MIT | Builds the interface (`dist/` for the window, `dist-bridge/` for the tests) |
| typescript (npm, dev) | 6.0.3 | Apache-2.0 | Strict type check; 6.0 because typescript-eslint 8.73 does not accept 7 yet |
| eslint, @eslint/js, typescript-eslint, eslint-plugin-react-hooks, globals (npm, dev) | 10.15.0, 10.0.1, 8.73.0, 7.1.1, 17.14.0 | MIT | Lint, including the rule that only `src/lib/core` imports `@tauri-apps` |
| vitest, jsdom, @testing-library/react, @testing-library/dom (npm, dev) | 5.1.0, 31.0.1, 16.3.3, 10.4.2 | MIT | Unit tests of the interface code |
| @playwright/test (npm, dev) | 1.63.0 | Apache-2.0 | End-to-end tests in headless Chromium against the development bridge |
| @tauri-apps/cli (npm, dev) | 2.12.5 | Apache-2.0 OR MIT | `pnpm tauri dev` and `pnpm tauri build` |
| @types/node, @types/react, @types/react-dom (npm, dev) | 22.20.5, 19.3.0, 19.3.0 | MIT | Type declarations |
| pnpm (tool) | 10.28.0 | MIT | Installs `apps/desktop` from the committed `pnpm-lock.yaml` |

## Native libraries inside the Python wheels

An installer would ship the Python environment, and some wheels carry shared libraries of other
projects. None of them is linked into Hotend's own process: each is a separate shared
library file that only the sandboxed Python child loads, and the user can replace the file.
The copyleft ones are listed one per row, with the option Hotend takes where the project
offers a choice. The list comes from the wheels' own records
(`*.dist-info/sboms/auditwheel.cdx.json`), the files in `<wheel>.libs/` and, for a wheel
without such a record, its own licence file.

**Only the Linux x86_64 wheels were inspected.** The Windows and macOS wheels of the same
packages are built differently and may carry other libraries; they must be inventoried the
same way before an installer for those systems is made.

Where the last cell says "confirmed in the wheel", the licence was read from a licence file
inside the wheel. Everywhere else it is the licence the project publishes, the wheel ships no
text for it, and both must be settled before a release: confirm the licence, and put its text
in the installer's notices.

| Name | Version | Licence | Why |
| --- | --- | --- | --- |
| FreeImage (shared library inside a wheel) | 3.17.0 | FreeImage Public License 1.0 (the option Hotend takes; the project also offers GPL-2.0 and GPL-3.0) | Shared library inside the cadquery-ocp-novtk wheel, loaded only by the sandboxed Python process: OCCT's `libTKService` (image files of its own viewer) links it. Hotend calls nothing in it |
| LibRaw (shared library inside a wheel) | 0.19.5 | LGPL-2.1 (the option Hotend takes; the project also offers CDDL-1.0) | Shared library inside the cadquery-ocp-novtk wheel, loaded only by the sandboxed Python process, as a dependency of FreeImage. Hotend calls nothing in it |
| libgomp (shared library inside two wheels) | 8.5.0 | GPL-3.0-or-later WITH GCC-exception-3.1 | GCC's OpenMP runtime: a shared library inside the cadquery-ocp-novtk wheel and another inside the scikit-learn wheel, loaded only by the sandboxed Python process. The runtime library exception allows it beside code under any licence. Confirmed in the wheel for scikit-learn (`licenses/COPYING`); the cadquery-ocp-novtk wheel has no text |
| libgfortran (shared library inside a wheel) | 8.5.0 | GPL-3.0-or-later WITH GCC-exception-3.1 | GCC's Fortran runtime: a shared library inside the scipy wheel (scipy comes with build123d), loaded only by the sandboxed Python process. Confirmed in the wheel (`LICENSE.txt`) |
| libgfortran (statically linked into numpy's OpenBLAS) | as built by numpy 2.5.5 | GPL-3.0-or-later WITH GCC-exception-3.1 | `numpy.libs/libscipy_openblas64_*.so` contains GCC's Fortran runtime linked in statically (numpy's own `LICENSE.txt` says so; numpy ships no auditwheel record, so the other rows missed it). Loaded only by the sandboxed Python process. The GCC runtime library exception lets it be distributed with a program under any licence; the notice must go in the installer. Confirmed in the wheel (`numpy-2.5.5.dist-info/licenses/LICENSE.txt`) |
| libquadmath (shared library inside a wheel) | 8.5.0 | LGPL-2.1-or-later | GCC's quad-precision maths under libgfortran: a shared library inside the scipy wheel, loaded only by the sandboxed Python process. Confirmed in the wheel (`LICENSE.txt`) |
| FriBiDi (system library; not inside any wheel and not shipped) | the host's | LGPL-2.1-or-later | pillow loads the host's `libfribidi` at run time when there is one (text layout of the view captions) and works without it |
| OpenEXR, IlmBase, JasPer, jxrlib, Little CMS, libjpeg-turbo, libpng, libtiff, libwebp, OpenJPEG, FreeType, Fontconfig, bzip2, liblzma, libuuid, fmt (shared libraries inside a wheel) | 2.2.0, 2.2.0, 2.0.14, 1.1, 2.9, 1.5.3, 1.6.34, 4.0.9, 1.0.0, 2.4.0, 2.9.1, 2.13.1, 1.0.6, 5.2.4, 2.32.1, 6.2.1 | BSD-3-Clause, BSD-3-Clause, JasPer-2.0, BSD-2-Clause, MIT, IJG and BSD-3-Clause, Libpng, libtiff, BSD-3-Clause, BSD-2-Clause, FTL (the option Hotend takes; the project also offers GPL-2.0), MIT, bzip2-1.0.6, public domain, BSD-3-Clause, MIT | Shared libraries inside the cadquery-ocp-novtk wheel: the image and font libraries FreeImage and OCCT's `libTKService` link, loaded only by the sandboxed Python process. They are the copies of the wheel's build image (EL8) and old: FreeType 2.9.1, libwebp 1.0.0, libpng 1.6.34, OpenEXR 2.2.0 and JasPer 2.0.14 have known fixes since. Hotend gives them no file to read (the engine reads and writes images with pillow), which is what keeps that acceptable |

pillow's own record (`pillow-13.0.0.cdx.json`) also names libimagequant (GPL-3.0-or-later).
It is not in the wheel: `PIL.features.check("libimagequant")` is false and `pillow.libs/`
holds no such file. If a later pillow wheel carries it, that wheel cannot be shipped.

## Platform pins

cryptography comes in through google-auth (the google provider of pydantic-ai-slim) and is
Apache-2.0 OR BSD-3-Clause. From 49.0.0 on it publishes no wheel for Intel macOS, so the
lock holds two versions: 50.0.2 everywhere, and 48.0.1, the last release with a universal2
wheel, on Intel macOS only (`constraint-dependencies` in `python/pyproject.toml`). That keeps
`uv sync --frozen` free of a compiler on every desktop platform
(`python/tests/test_lock_platforms.py`). 48.0.1 gets no further fixes: if Intel Macs are not
to be supported, remove the constraint and say so in README.md instead.
Hotend's Python never holds a key and reaches a provider only through the core, so it does
not sign or encrypt anything with this library.
