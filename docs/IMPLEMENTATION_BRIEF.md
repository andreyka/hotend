# Hotend: implementation brief

This is the original build brief, kept for the record; milestones 1 and 2 of section 5 are done.
`README.md` says what works today. The layout, the rules of section 4 and the embedding rules of
section 9 still hold.

This file describes how the repository is organised and which rules are not negotiable.
`docs/DESIGN.md` says what the product is and why; `docs/PROTOCOL.md` is the contract between
components.

If this brief and the design disagree, the design wins on product intent and this brief wins on
repository mechanics.

## 1. What Hotend is, in five lines

A desktop app for people who make 3D-printed products but are not CAD or printer experts. The user
describes a product; an AI agent (the user's own API key) writes build123d code; that code runs in a
sandboxed worker; exact geometric checks prove the part is a valid, printable solid with fits taken
from the printer's calibration; the app shows it in a 3D view with sliders; a vendor slicer turns it
into a print job; the user, never the AI, confirms the print. Primary printer: Bambu Lab X2D
(dual nozzle). Secondary: Prusa. Platforms: Windows, macOS, Linux. First test bed: Linux.

## 2. Repository layout

```
Cargo.toml                 Rust workspace
crates/
  hotend-sandbox/         Sandbox trait, per-OS backends, `hotend-sandbox` launcher binary
  hotend-core/            Project store, worker supervisor, tool gateway, secrets, LLM egress (over the agent's stdio channel)
  hotend-slicer/          Slicer detection, CLI runner (inside the sandbox), result parsing, X2D rules
  hotend-printer/         Printer trait, Bambu LAN message model, command allowlist, start token,
                           G-code linter, simulated printer, hand-off and USB export paths
  hotend-settings/        Print settings: the closed field model, rule data and rule engine
                           (docs/PRINT_SETTINGS.md); pure, no dependency on another crate
  hotend-cli/             `hotend` binary: headless driver for everything the app can do
apps/desktop/              Tauri 2 shell + React 19 UI (src-tauri depends on hotend-core)
python/
  pyproject.toml, uv.lock  One hash-locked environment, Python 3.13
  hotend_engine/          CAD worker: run model code, checks, tessellation, export, measure, views
  hotend_fit/             Proven Fit runtime: contract schema, fit resolver, paired feature cards,
                           gauges, range sampler
  hotend_agent/           Agent loop on Pydantic AI; tools are proxies to the Rust tool gateway
  tests/
docs/                      DESIGN.md, this brief, PROTOCOL.md, DEPENDENCIES.md, PRINT_SETTINGS.md,
                           WEB_RESEARCH.md, ux/ (design specification, pictures of every screen),
                           notes/
scripts/                   setup-wsl.sh, test-all.sh, tests/ (the scripts run against stand-in
                           tools; what the documents promise, shown on real builds)
```

Record each new dependency in `docs/DEPENDENCIES.md` (one line: name, version, licence, why). When
the contract must change, change `docs/PROTOCOL.md` with it.

## 3. Process model (who runs what)

```
UI (webview, no secrets, no network)
   |  typed Tauri commands (tauri-specta)
Rust core  ---- owns: SQLite store, files, OS keychain, sockets, printers, child processes
   |-- engine worker   python -m hotend_engine.worker   fresh process per job, sandboxed, no network
   |-- agent process   python -m hotend_agent           sandboxed; reaches the LLM only through the
   |                                                      core's egress proxy; every tool call is a
   |                                                      request to the core's tool gateway
   |-- slicer          vendor CLI, sandboxed, no network
   `-- printer         simulated; a read-only status listener for one printer the user names
```

The Rust core is the only component that holds secrets, opens network connections (to the chosen AI
provider, the chosen search service and a printer on the user's own network) or starts processes.
Python never sees an API key or a printer access code.

## 4. Non-negotiable rules

Security
1. AI-written code runs only inside the sandbox (no network, writes only to its job folder, read-only
   view of the Python runtime and of a list of system paths, time and memory caps). If the sandbox
   cannot be set up, the job fails closed. Debug builds have two development switches:
   `HOTEND_UNSAFE_NO_SANDBOX=1` (run unconfined, with a warning) and `HOTEND_SANDBOX_BIN` (use
   another launcher program). Release builds ignore both. On macOS `HOTEND_SANDBOX_MACOS_UNTESTED=1`
   enables the untested backend.
2. Treat every file the worker, the slicer or the agent produces as hostile input when the core
   reads it: size caps, strict parsing, no path from a child is trusted (resolve and check it stays
   inside the job folder; reject symlinks).
3. No secrets in the repository, in tests, in logs or in the SQLite store. Keys live in the OS
   keychain (`keyring` crate) or, if the user chooses, in the environment or in a file of their own.
   Redact access codes, API keys and bearer tokens in all logging.
4. The agent has no start-print tool. A print can start only with a one-time token minted after a
   human click in a confirmation the core draws, bound to the SHA-256 of the sliced file, the
   printer serial and an expiry. The token is a type that cannot be constructed any other way.
5. Printer commands go through an allowlist of typed commands. There is no raw G-code command.
6. Nothing in this repository sends a command or a print to a real printer, or connects to Bambu's
   cloud. The one connection to a real printer is the read-only status listener
   (`docs/PROTOCOL.md`, "Printer status (read-only)"). The test suite makes no network calls
   beyond the loopback address: it uses the simulated printer, stand-ins, and fixtures that are
   hand-written or recorded from Hotend's own runs.
7. The webview gets a strict CSP, no remote content, and only the Tauri commands it needs.

Licences
8. Hotend is Apache-2.0. Bambu Studio, OrcaSlicer, PrusaSlicer, Bambuddy and Blender are
   AGPL or GPL. Do not copy, port or paraphrase their source; none of their code is copied or
   linked. Use them only as separate programs through documented command lines and file formats.
   Protocol facts may come from the OpenBambuAPI notes (facts only: its text and files are under the
   GFDL and stay out) and ha-bambulab (MIT), with a comment naming the source.
9. Add a dependency only if it is maintained and its licence is MIT, Apache-2.0, BSD, ISC, MPL-2.0,
   Zlib or similar. LGPL only as a dynamically linked or separate component and only with a note.
   No GPL or AGPL in-process. Record each one in `docs/DEPENDENCIES.md`.

Engineering
10. Do not reinvent: use the libraries named in section 6 before writing geometry, mesh, protocol or
    UI primitives yourself.
11. Versions are pinned and lock files are committed (`uv.lock`, `Cargo.lock`, `pnpm-lock.yaml`).
12. Every component ships tests that run headless on Linux with no GPU, no display and no network.
    `scripts/test-all.sh` must stay green.
13. Cross-platform from the first line: no hard-coded `/tmp` or `/` separators, use `std::path`,
    `pathlib`, `directories` for data dirs. Code that is OS-specific sits behind a trait with a
    backend per OS; a backend that is not written yet returns a clear "not supported" error.
14. Errors are typed (`thiserror` in libraries), carry what the user can do next, and never include
    secrets. No `unwrap()` or `expect()` on anything that depends on input.
15. Write for the next reader: short module docs that say what the module guarantees, comments only
    where the reason is not obvious.

## 5. Milestones

M1. Headless core loop
- `hotend-sandbox`: trait, Linux backend (Landlock + seccomp + no network), launcher binary, denial
  tests. macOS Seatbelt and Windows backends: where one is missing, an explicit "not supported".
- `hotend_engine`: run a model script, checks, tessellation for the viewer, STEP/3MF/STL export,
  measure, views for the AI.
- `hotend_fit`: contract schema with provenance, calibration table and fit resolver, three paired
  feature cards with gauges, range sampler.
- `hotend-core`: store, worker supervisor, tool gateway, secrets, LLM egress proxy.
- `hotend_agent`: Pydantic AI loop with gateway-proxied tools, tested with a scripted model.
- `hotend-slicer`: detection, sandboxed CLI run, result parsing, X2D nozzle rules, fake slicer.
- `hotend-cli`: `hotend demo` runs describe -> code -> checks -> mesh -> slice end to end with a
  scripted model and the fake slicer, and prints where the artefacts are.
Done when `scripts/test-all.sh` passes on a clean Ubuntu 24.04 after `scripts/setup-wsl.sh`.

M2. Desktop app
- Tauri 2 + React UI: projects, chat, 3D view (three-cad-viewer), sliders, checks, slice summary,
  settings (provider and key), all against `hotend-core`.
- `hotend-printer`: typed Bambu LAN model, allowlist, start token, G-code linter, simulated printer,
  Bambu Connect hand-off, USB export, native confirmation.
- Runs with a real provider and with the real Bambu Studio command line, outside the default suite.

M3 and later (see the roadmap in DESIGN.md): calibration wizard and outcome ledger, colour and
recolouring, control of a real X2D over the LAN, Prusa, mesh and organic shapes, hand-editing
connectors.

## 6. Pinned technology

| Area | Use | Notes |
| --- | --- | --- |
| Python | 3.13, uv, one locked env in `python/` | `uv sync --frozen`; never install at runtime |
| CAD | build123d 0.13.0 on OCCT 8.0.1 (`cadquery-ocp-novtk`) | No VTK. bd_warehouse for standard parts |
| Viewer data | ocp-tessellate | Output must load in three-cad-viewer unchanged |
| Mesh | manifold3d, lib3mf | trimesh, libigl and Blender are added when mesh import arrives |
| Validation, sampling | pydantic 2, a seeded range sampler in hotend_fit | `uv.lock` and docs/DEPENDENCIES.md are the truth |
| Agent | pydantic-ai-slim 2.x | `FunctionModel` / `TestModel` for tests; no real provider calls in tests |
| Rust | stable, edition 2024 | tokio, serde, thiserror, rusqlite (bundled), keyring, reqwest (rustls), hyper |
| Sandbox | landlock, seccompiler crates on Linux | `docs/notes/sandbox.md` |
| Printer | rumqttc, suppaftp, rustls | M2 |
| Desktop | Tauri 2.12.x, React 19, TypeScript, Vite, Tailwind 4, shadcn/ui on Base UI, motion, Zustand, tauri-specta | M2 |
| Slicers | Bambu Studio CLI (X2D), PrusaSlicer CLI >= 2.9.7 | Never bundled and never downloaded; an installed one is detected |

## 7. X2D facts the code must encode as data, not scattered constants

- Main nozzle (direct drive): reach 256 x 256 x 260 mm. Auxiliary nozzle (Bowden): 235.5 x 256 x 256 mm,
  X from 20.5 mm, 200 mm/s limit. TPU, PLA Aero and PPA-CF only on the main nozzle.
- Slicer extruder 1 = main nozzle = device extruder id 1; slicer extruder 2 = auxiliary = device id 0.
  Always map by id.
- The Bambu Studio CLI's automatic filament grouping is not trusted: always write
  `filament_map_mode` = manual and explicit `filament_maps`.
- These are unverified on a real printer. Keep them in one versioned printer-profile file with a
  `verified: false` flag per fact.

## 8. How to work

- `docs/PROTOCOL.md` is the contract of every interface. A change that shows the contract to be
  wrong or incomplete also corrects the document.
- A change is small, complete and tested. What it leaves out is stated; there are no stubs that
  pretend to work.
- A claim about behaviour names the command that shows it.
- Python: `cd python && uv run pytest -q`, `uv run ruff check .`. Rust: `cargo test -p <crate>`,
  `cargo clippy -p <crate> -- -D warnings`, `cargo fmt`.

## 9. Embedding rules

Hotend is a 3D modelling and printing product. A short list of seams, cheap today and expensive
to retrofit, lets another program drive it or hold it:

1. Everything Hotend can do is reachable headless, with JSON in and JSON out, through the Core
   facade and the `hotend` command. No capability may exist only behind the window: a script or
   another program must be able to drive Hotend without the UI.
2. A project is a product, not "a CAD file". Nothing in the store, the facade or the interface may
   assume a project holds only mechanical versions; mechanical data lives under its own namespace
   (`mechanical` / `hotend`), so other data can live beside it.
3. Ids are global (UUIDv7), files are addressed relative to the project, and every artefact Hotend
   produces says what it is (kind, units, coordinate frame, producer and version) so another tool
   can consume it without reading Hotend's code.
4. Bought parts are first-class. A motor, a circuit board or a battery is a component with an
   envelope, mounting pattern and connector positions; a printed part mates with it through the
   same paired-feature and fit machinery as two printed parts. Do not hard-code the idea that both
   halves of a joint are printed.
5. Exchange is through open formats: STEP for geometry in both directions, plus mass, centre of
   mass and inertia per part and per assembly as data.
6. The window is a host with Hotend as one module: its routes, state and styles are namespaced,
   and nothing global is named after CAD or printing. Build no placeholder for other modules (no
   empty tabs, no "coming soon").
7. The agent is callable as a specialist: a run takes a typed task and returns a typed result.

## 10. Example scenarios

Two products that exercise the whole loop, used as example prompts, demo content and evaluation
cases wherever an example is needed.

1. **A soft figure in a steamer basket.** The user has a soft figure and wants a holder for it
   in the shape of a steamer basket. The size of the figure must come from the real object: a
   model file imported as a reference, or a measurement the user gives. It must not be guessed
   silently. The result is an original design, not a copy of an existing model.
2. **A case for a phone.** Hotend finds the phone's real dimensions (with a source) and proposes a
   case whose geometry and size are correct, and that prints. Every number that came from a lookup
   carries its source; anything uncertain becomes a question or a small test print.

What these scenarios require of the product: reference objects (a mesh or a bought device) that a
printed part is fitted to, flexible materials such as TPU with their own fit classes and nozzle
rules, and a trustworthy way to get the dimensions of real-world things.
