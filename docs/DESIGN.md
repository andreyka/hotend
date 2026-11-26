# Hotend: design

This document says what Hotend is meant to be and why. `README.md` says what is built, simulated or
missing today; `docs/IMPLEMENTATION_BRIEF.md` says how the repository is organised; `docs/PROTOCOL.md`
is the contract between the components. Facts about other products are as of November 2026. Hotend is
Apache-2.0.

## Summary

Hotend is a local-first desktop app where a maker describes a product, an AI agent writes it as
parametric CAD code, a validation engine checks that the part is geometrically correct and printable,
and the app slices it and hands it to the printer: a Bambu Lab X2D first, Prusa second. The user
brings their own AI API key.

Two ideas carry the design:

- **The source of truth is code-CAD** (build123d on the OpenCascade kernel), not a GUI app. Language
  models write Python CAD reliably, and code runs headless, diffs cleanly and can be tested. Other CAD
  apps are connectors around that core for people who want to edit by hand.
- **Every design carries executable checks, like unit tests.** A version is "ready" only when no check
  of severity `error` fails: kernel validity, a manifold mesh, wall thickness (sampled), the build
  volume, colliding parts, declared fits and the dimensions the user asked for. Overhangs, bed contact
  and small features are warnings. A check that cannot decide says so; it never passes silently.

The stack: a Tauri 2 shell with a Rust core (secrets, printer layer, process sandboxing), a React and
TypeScript interface with a WebGL viewport, and Python workers for geometry and the agent loop.

## Users and the journey

The user makes printed products. They know what they want to make and own a good printer, but do not
model in CAD and do not know why a print warps or a snap-fit breaks.

1. **Describe.** Text, a sketch, a reference, or a measured object ("a wall bracket for this 32 mm
   pipe"). The agent asks only what it must: key dimensions, material, how the part is used.
2. **Spec.** The agent writes a short design spec the user can read and edit: dimensions, fits, load
   cases, material, print constraints. The spec becomes the checks.
3. **Model.** The agent writes build123d code. The viewport updates; exposed parameters appear as
   sliders.
4. **Validate.** Checks run on every change. Failures go back to the agent to fix; a version with a
   failed `error` check is marked as such and is not offered for printing.
5. **Refine.** The user drags a slider, types a dimension, or points at a face and says what to change.
6. **Prepare.** Hotend picks orientation, supports and the nozzle, slices with tuned profiles, and
   shows time, grams and cost.
7. **Print.** The user confirms; Hotend delivers the job and shows progress where it can.
8. **Learn.** After the print, the user rates fit and finish, or measures one dimension with calipers.
   Hotend updates the printer and material profile so the next print is closer.

## Architecture

Four processes with hard trust boundaries. The Rust core is the only process that holds secrets, talks
to printers, or reaches the internet. Everything that runs AI-written code is sandboxed and offline.

| Process | Language | Owns | Can reach |
| --- | --- | --- | --- |
| Interface (Tauri webview) | TypeScript, React | Chat, 3D viewport, spec and checks panel, print preparation, printer view | Rust core over typed IPC only |
| Core | Rust | Secrets in the OS keychain, the egress filter that adds API keys, the printer layer, the slicer runner, the project store (SQLite and files), the sandbox supervisor | Internet (allowlisted AI providers and the chosen search service), LAN printers |
| Agent worker | Python | Agent loop, tool calls, design spec, memory of the project | Core only, through its stdio channel; never sees an API key |
| Geometry sandbox | Python | Runs AI-written build123d code, validation, rendering, mesh operations | No network; writes only its job folder; reads that, the Python runtime and a list of system paths; CPU, memory and time limits |

The agent's tools are narrow and typed, defined once in a tool gateway inside the core that executes or
refuses every call. There is no tool that starts a print. `propose_print` creates a pending job, and
only a confirmation drawn by the core can start it.

## Modelling strategy

build123d is the engine. Driving a GUI app as the main engine would make every design depend on a
running, licensed, signed-in desktop app and on an API built for add-in authors, not agents.

| Engine | Role in Hotend | Why | Integration |
| --- | --- | --- | --- |
| [build123d](https://github.com/gumyr/build123d) on OpenCascade | Default for every functional part | Exact B-rep geometry, STEP export, parametric, headless, Apache-2.0 | A fresh sandboxed worker per build |
| In-app direct editing | First way to edit by hand: drag handles and typed dimensions on picked faces rewrite parameters in the code | The code stays the single source of truth, so nothing is lost | The viewport's face picking plus the parameters |
| STEP hand-off to any CAD app | For Fusion, FreeCAD, Onshape and others | Works with everything. It is a one-way freeze: the edited solid returns as a new base body with no link to the original code | STEP export; re-import parsed in the sandbox |
| Live connectors (FreeCAD, Fusion, Onshape) | Optional, for people who use them | Hand editing in a familiar program | Fixed operations only; check each vendor's terms for commercial use |
| Mesh import | Sculpted shapes that cannot be built from operations: figurines, animals, faces | Models reach 3D through code, and a figurine built that way comes out blocky | The user imports a mesh made anywhere; Hotend rebuilds it locally into a printable solid that CAD features can be added to |
| [Manifold](https://github.com/elalish/manifold) and other mesh libraries | Mesh booleans and checks | Guaranteed manifold output; decorative meshes are clipped away from functional faces so they cannot alter a fit | Python bindings in the sandbox |
| Blender, as a separate program | Surface textures, reliefs, organic forms, mesh repair | Covers decoration. It is GPL and large, so it stays a separate program, and its output is never trusted as printable | Sandboxed in its own process; every result is rebuilt into a solid locally and checked |

In-app editing and STEP export come first; live connectors follow.

Validation treats "watertight" and "printable solid" as different things: it also checks enclosed
volume against the bounding shape, the number of separate pieces and minimum wall thickness.

### How correctness is proven

Each design has a spec, and the spec compiles into checks that run on every change. A version is
"ready" only when no check of severity `error` fails; warnings are shown beside it.

- **Kernel validity:** the B-rep is valid and closed (OpenCascade's shape checker).
- **Mesh health:** the exported mesh is watertight and manifold, with no self-intersections.
- **Dimensions:** every number in the spec is measured on the model, such as "bore 32.0 mm, clearance
  fit".
- **Fits and tolerances:** mating parts carry clearances tuned to this printer and material, learned
  from a fit-test calibration print.
- **Printability:** minimum wall versus nozzle width, smallest feature, overhang map for the chosen
  orientation, bridge spans, and fit inside the reach of the nozzle that prints it: 256 × 256 × 260 mm
  for the X2D's main nozzle, 235.5 × 256 × 256 mm for the auxiliary one.
- **Visual review:** the sandbox renders views and sections; the agent's vision model checks them
  against the request, and the user sees the same images.

Failed checks return to the agent as structured errors with the face or edge they refer to, so it fixes
the geometry rather than guessing. The same checks apply to imported models.

## Modelling method: Proven Fit

The AI does not model fits; it composes them. Everything functional (how parts join, move, fasten or
fit a real object) comes from a library of paired features whose two halves are generated from one
definition, with clearances resolved from this printer and filament. The AI writes free build123d code
only for the body around those fixed connection points.

A real print is the only thing that can mark a feature as proven, and each result feeds back into the
clearances used for the next design. Every ingredient is published or in use somewhere; Hotend combines
them.

### What is reused and what is new

| Step | Reused | New in Hotend |
| --- | --- | --- |
| Contract before geometry | Spec-first generation is published ([ReliCAD](https://arxiv.org/abs/2609.22325), [CADTests](https://arxiv.org/html/2605.07807v1)) | Every dimension carries a tag (measured, standard, calibrated, guessed, style) that decides what is locked, what becomes a slider and what the user is asked to measure |
| Paired features | [bd\_warehouse](https://github.com/gumyr/bd_warehouse) and [b3dkit](https://pypi.org/project/b3dkit/) for threads, inserts, dovetails, snap fits | Each feature emits both halves, its connection points and its own test gauge, so the two halves cannot disagree |
| Fit from printer | Calibration prints | No clearance is ever typed. A fit class (press, snug, slide, free) resolves to a gap by feature type, axis and nozzle. A filament change rebuilds both halves with no AI call |
| Check and repair | build123d exact measurement | Number-only failures can be fixed without the AI; it is called again only for structural faults |
| Range check | Precedent in [Fab Forms](https://cfg.mit.edu/publications/fab-forms-customizable-objects-fabrication-validity-and-geometry-caching) (2015) | Checked at the tightest and loosest the printer can produce; sliders are clamped to the range that passed, shown as "every size in this range was checked" |
| Test, then print | The slicer's command line for time and grams | A minutes-long fit gauge prints first when it is cheaper than risking the full print. One answer per feature after each print (works, tight, loose, broke) goes into a ledger per printer and filament |
| Steering | Click-a-face context | Hover a slider to see which faces move; pin a measurement and it becomes a permanent check the AI cannot break later |

### What we cut, and why

- **Generating several candidates and racing them by default.** It multiplies spend on the user's key
  for a small published gain ([consensus selection](https://arxiv.org/abs/2608.09706)).
- **Image-to-3D as the base of functional parts.** Kept only for decorative skins that cannot alter a
  fit.
- **Strength simulation.** The open solvers are GPL, and a ranking is easily read as a load rating.
- **Formal proofs shown to the user.** The checks run internally; the user sees the checked slider
  range and a "printed and worked on your machine" badge.

### First test

Ten two-part products (snap-lid box, jar with lid, hinge with pin, tube clip, drawer divider and the
like), built both ways with the same AI model: plain AI-written CAD, and four to six paired features
with calibrated fits. The method is in doubt if first-print fit is less than 10 points better, if
attaching features to AI-written bodies fails more than about a third of the time, or if it costs more
than twice the turns. Measured: about 0.3 s per rebuild on a two-part lidded box, so 200 range checks
take about a minute on one core.

### Risks specific to the method

- **Attaching library features to an arbitrary AI-written body** is unproven. The first test measures
  it.
- **A wrong contract gives an empty guarantee.** If the contract misses what the user meant, everything
  passes and the part is still wrong.
- **Sampling is not proof.** Wording must say "checked", never "guaranteed".
- **Sparse evidence.** One user prints few distinct features a week, and wet filament can look like a
  feature failure, so the proven badge needs conservative thresholds and demotion.

## Print pipeline

Hotend drives the vendors' own slicers from the command line and owns the profiles and calibration on
top. Writing a slicer is out of scope; tuning one is the product.

### Slicing

- **Bambu, including the X2D:** the
  [Bambu Studio command line](https://raw.githubusercontent.com/wiki/bambulab/BambuStudio/Command-Line-Usage.md),
  run as a separate program. Hotend pins one exact public release and upgrades only after a regression
  run passes (same inputs, compare layers, filament and time). The pinned version is recorded in the
  slicer version data.
- **Not OrcaSlicer for the X2D.** As of November 2026 its X2D dual-nozzle support exists only in an
  alpha release.
- **Prusa:** the PrusaSlicer command line, 2.9.7 or later (that release carries security fixes).
- **Slicers are not bundled or linked.** All three are AGPL; none of their code is copied or linked.
  Hotend finds the user's installed copy and talks to it only through the command line and files.
- **The slicer runs inside the same no-network sandbox as AI-written code,** with a fresh temp folder,
  a time cap and an app-owned settings folder. It is a large parser fed files Hotend did not write.
  Settings come from Hotend's vetted profiles and rules; the agent picks from a list and never writes
  start or end G-code.
- **Slicing and sending must agree.** The filament-to-nozzle map is written into the job, and a job
  whose map does not match the printer is refused.

### Profiles and calibration

Profiles are layered and versioned: vendor system profile, then Hotend's tuned overlay per printer,
nozzle and filament, then per-product overrides. Every change records why it was made.

Calibration prints small coupons and asks one thing per coupon: pick the best strip, or type one
caliper reading. It covers flow, pressure advance, temperature, retraction, a fit-test coupon for hole
and peg clearances, and shrinkage for dimensional parts. The X2D already runs automatic leveling and
flow calibration on board, so Hotend focuses on what the printer cannot measure: fit, shrinkage and
surface finish per filament.

After real prints, the user's rating and optional measurements feed the same profile history.

### Printer control

Full control of an X2D over the local network needs LAN Only mode plus Developer Mode
([third-party integration](https://wiki.bambulab.com/en/software/third-party-integration),
[Developer Mode](https://wiki.bambulab.com/en/knowledge-sharing/enable-developer-mode)). Outside
Developer Mode a printer still reports status but rejects print, motion and temperature commands from
anything but Bambu's apps
([announcement](https://blog.bambulab.com/firmware-update-introducing-new-authorization-control-system-2)).
Today Hotend reads a real printer's status only (`docs/PROTOCOL.md`, "Printer status (read-only)").

The printer layer is one interface with three ways to deliver a job:

| Path | What Hotend can do | Cost to the user |
| --- | --- | --- |
| Developer Mode over the LAN (not offered by the core) | Upload the sliced job, start it after confirmation, pause, resume, stop, live status, AMS contents, camera | The printer leaves Bambu Cloud and the Handy app, and Bambu gives no support in this mode |
| Bambu Connect hand-off | Hotend opens the sliced file in Bambu Connect through its `bambu-connect://import-file` link; the user sends it from there. Hotend can still watch status read-only | Nothing is given up. Hotend gets no job result back |
| Export to a folder or USB drive | Writes the sliced `.gcode.3mf` | Manual, but depends on nothing |

### How Developer Mode control works

| Channel | Details |
| --- | --- |
| Discovery | The printer announces itself on the LAN (SSDP, UDP 2021). The user confirms the serial number shown on the printer's screen |
| Status and commands | MQTT over TLS on port 8883, user `bblp`, password = the printer's 8-character access code. Hotend subscribes to the printer's report topic and publishes typed commands to its request topic ([protocol notes](https://github.com/Doridian/OpenBambuAPI/blob/main/mqtt.md)) |
| File upload | FTPS on port 990 with the same credentials |
| Camera | RTSPS on port 322. The core receives the stream and passes frames to the interface, so the access code never reaches the UI |
| Starting a print | Upload, confirm the upload, then one `project_file` command naming the plate, the calibration options and which AMS slot feeds which filament. A watchdog then checks that the print really started, because the printer can accept the command and stay idle, for example while the AMS is drying |

The X2D belongs to a newer protocol generation than the X1, so Hotend keeps a per-model profile (camera
type, storage, nozzle count, field types) as data it can update.

### Safety rules for printer control

- **Developer Mode switches off the printer's own authorization.** Anyone on the network with the access
  code gets full control, files and the camera. A separate network for the printer is recommended, the
  code lives only in the secret store, and it is redacted from logs.
- **A short allowlist of commands.** Start, pause, resume, stop, speed, light and status. There is no
  raw G-code command.
- **The agent cannot start a print.** It has no start tool; it can only produce a pending job. The core
  starts a job only with a one-time token created by a human click in a confirmation the core draws
  itself (a native dialog, not the chat window), bound to the hash of the exact sliced file, the printer
  serial and an expiry. The dialog shows values read from the file: temperatures, filament, time.
- **G-code is checked before upload.** Moves inside the reachable area of each nozzle, temperatures
  under the limits for the filament, no firmware or configuration commands.
- **Printer identity.** Hotend pins the printer's certificate at pairing and checks that it names the
  serial the user confirmed. Hotend offers only TLS 1.2.
- **The printer does its own job.** First-layer inspection, flow calibration and failure detection are
  the X2D's on-board features. Hotend requests them and shows the printer's own error codes; it does not
  rebuild them.

### What Hotend will not do

No use of Bambu's cloud API, no loading of Bambu's network plugin or its open replacements, and no
extraction of signing certificates to get control without Developer Mode.

Hotend writes its own small Rust client. The references are
[OpenBambuAPI](https://github.com/Doridian/OpenBambuAPI) as the written protocol and ha-bambulab's
parser (MIT) for the message model. Bambuddy and Bambu Studio are AGPL; none of their code is copied or
linked.

### Unverified on a real X2D

The profile data in `crates/hotend-printer/profiles/` carries a `verified` flag per fact. Not verified
on a printer:

- the exact `project_file` payload the X2D accepts, and whether FTPS writes to internal storage or only
  to USB;
- the certificates the printer presents for file upload and camera;
- which nozzle index is main and which is auxiliary, and how each AMS reports the nozzle it feeds;
- the reach and material rules of each nozzle;
- the camera address and that it streams in LAN mode.

### Prusa

PrusaLink's local HTTP API is the first Prusa path, on firmware 6.4 or later. It uses HTTP Digest
sign-in over plain HTTP, so the link is unencrypted and the user is told so at pairing. Jobs are
uploaded without auto-start and started by a separate request after the click.

## Language and libraries

Three languages, each where it is strongest: Python because the open CAD and mesh ecosystem lives there
and language models write it well; Rust for the code that touches secrets, sockets and heaters;
TypeScript because the best 3D and UI tooling is on the web. `docs/DEPENDENCIES.md` is the list of
versions and licences.

| Layer | Choice | Why |
| --- | --- | --- |
| Desktop shell | [Tauri](https://tauri.app) 2.12.x, written so the runtime can be swapped | 2.12.0 is the minimum: it fixes a flaw that let one window read data streamed to another ([advisory](https://github.com/tauri-apps/tauri/security/advisories/GHSA-7gmj-67g7-phm9)) |
| Interface | React 19, TypeScript, Tailwind 4, shadcn/ui on Base UI, `motion`, tauri-specta for typed calls to the core | Typed end to end |
| 3D viewport | three.js on WebGL2, with [three-cad-viewer](https://github.com/bernhard-42/three-cad-viewer) and ocp-tessellate for face, edge and vertex picking | Reuses the build123d viewer stack. WebGPU is not a baseline: it is absent from Linux's WebKitGTK |
| Core | Rust: tokio, rumqttc (MQTT), suppaftp (FTPS), reqwest, rusqlite, keyring | One process owns secrets and sockets |
| CAD kernel | build123d on OpenCascade, with [bd\_warehouse](https://github.com/gumyr/bd_warehouse) parts | Runs headless and offline, so it fits the sandbox |
| Mesh | manifold3d, lib3mf; libigl and trimesh when mesh import arrives | libigl's winding-number rebuild gave a real solid where a voxel remesh gave a zero-volume skin that passed every watertight check |
| Slicing | Bambu Studio and PrusaSlicer command lines; the user's own install | Not bundled (AGPL), sandboxed, version-pinned |
| Agent loop | Python on Pydantic AI 2.x, with every tool call executed and checked in one Rust tool gateway | Maintained adapters for Anthropic, OpenAI, Google, OpenRouter and local models |
| Sign-in to AI | The user's own API key only | Subscription sign-in is not supported |
| Sandbox | One interface with a backend per OS: Landlock and seccomp on Linux, a Seatbelt profile on macOS, an AppContainer plan for Windows | `docs/notes/sandbox.md` |
| Packaging | uv with a hash-locked environment; no package installs at run time | Reproducible |

The embedded Python runtime would add about 250 to 300 MB to an installer and about 1 GB once installed
(measured on the locked environment, November 2026).

Linux is the weak platform for the interface, not for the engine. Tauri uses the system's WebKitGTK
there, where the 3D view can silently fall back to software rendering. The core, CAD engine, checks and
slicer are developed and tested on Linux, including WSL.

Hotend is Apache-2.0. AGPL and GPL components (slicers, Blender) are used only as separate programs,
through their command lines and file formats; none of their code is copied or linked.

## Security model

Hotend runs AI-written code on the user's machine and works with a device that has a 300 °C nozzle and a
65 °C heated chamber, so the core assumption is that the model can be wrong or manipulated. Safety comes
from what the agent is unable to do, not from what the prompt asks it not to do.

### Threats and controls

| Threat | Control |
| --- | --- |
| API key theft from disk, logs or a compromised webview | Keys live in the OS keychain (or, when the user chooses, the environment or one file of their own). Only the Rust core reads them and adds them to outbound requests. The agent reaches a provider only over its private pipe to the core, which allows an exact list of hosts and paths per provider and caps requests, bytes and output tokens. The webview can set or delete a key but never read one |
| Prompt injection from imported files, CAD documents, meshes, web pages or printer messages | Untrusted content is tagged as data. Once it is in context, the model cannot trigger anything with outside effects. The agent has no shell, no start-print tool and no way to set custom G-code; it may change only an allowlisted, range-checked set of settings |
| A forged "human click" | The chat window also renders AI output, so a click there is not proof of a human. A print start and a hand-off to Bambu Connect are confirmed in a native dialog raised by the core (or a typed `yes` at a terminal), bound to the hash of the exact file, with a one-time token |
| Malicious or buggy AI-written code | A fresh, killable worker per build: no network, read-write only to the job folder, CPU, memory and time limits. A kernel crash is a normal, reported outcome. Linux uses seccomp with Landlock; the macOS profile is written and untested; Windows has no backend yet |
| Files Hotend did not write (STEP, 3MF, meshes, slicer profiles) | Parsed only inside the sandbox, never in the core. The slicer runs sandboxed too. What a child process writes is read as hostile input |
| AI-chosen text reaching the interface | Part names, labels and error text are validated and length-limited in the core; the page has a strict script policy and renders AI output as sanitised text. Object names are filtered before display because the viewer renders them as HTML |
| Unsafe printer commands | A command allowlist and a G-code check in the core, with per-nozzle reach and per-filament temperature limits. Prints never auto-start |
| Printer impersonation on the LAN | The certificate is checked against the printer's authorities or pinned at the first confirmation, and matched to the serial the user confirmed; a changed certificate is refused. Access codes are stored like API keys |
| Connectors acting outside the sandbox | A CAD app's script-running tool is treated as shell access and is never exposed to the agent. Connectors offer fixed operations only |
| Data leaving the machine | Projects stay on disk. Outbound traffic goes only to the AI provider the user chose and, when the user has agreed to web research, to that provider or the chosen search service (`docs/WEB_RESEARCH.md`); on the LAN, to the printer the user set up. There is no telemetry |
| Supply chain | Hash-locked Python and npm installs, committed lock files |

There are no permission modes to learn. One rule covers it: the agent designs freely, and anything
physical, public or costly needs the user's confirmation. The sandbox's known limits are in
`docs/notes/sandbox.md`, section 5.

## Product and UX

One window, the part always centre stage, the agent beside it rather than in front of it. Dark-first
with a light theme, the fonts of `docs/ux/DESIGN_SPEC.md`, soft materials, 150 to 250 ms motion, and no modal walls.
`docs/ux/DESIGN_SPEC.md` is the specification.

| Screen | What the user sees |
| --- | --- |
| Studio | Conversation on the left, the live 3D part in the centre, the list of promises and checks on the right (green, amber, red). Parameter sliders limited to the checked range. A version history with one-click revert |
| Point and say | Click a face, edge or hole; it becomes a chip in the chat ("this face: 3 mm thicker"). Hover a slider to see which faces move. Pin a measurement so it can never silently change |
| Colour | The filaments loaded in the printer, the model's named regions, and three or four ready-sliced colour options with time, waste and cost |
| Prepare | Plate view with orientation, supports and which nozzle prints what, plus time, grams and cost |
| Printer | Progress, temperatures, AMS slots; a camera and pause and stop where the delivery allows it. After the print, one answer per feature: works, tight, loose, broke |
| First run | Enter an API key, set up the printer, print one short fit gauge |

Interactivity rule: slider changes should redraw a simple part in under 300 ms (a target to measure, not
a promise for complex parts), with heavier checks streaming in after.

## Colour: recolour with what is loaded

"Recolour this model with what is in my printer and show me the best options" is one pipeline that works
for parts the agent designed and for imported models. Hotend owns everything up to a coloured project
file; the slicer does everything after it. It is not built yet.

1. **Palette from the printer.** Read each AMS slot from the printer's local status report: material,
   colour, profile, how much is left, and which nozzle that AMS feeds.
2. **One colourable model.** Agent-designed parts keep their bodies and faces. STEP colours are read
   with OpenCascade. Textured and vertex-coloured meshes are baked to a colour per face. STL is geometry
   only.
3. **Regions.** Split the model deterministically (bodies, sharp edges, flat and round surfaces, height
   bands), then the vision model looks at marked-up renders and groups the pieces into 2 to 8 named
   regions with roles: base, secondary, accent, text. The user can correct with a brush.
4. **Candidate schemes.** Assign loaded filaments to regions; for models that already have colour, match
   it to the loaded palette at 2, 3 and 4 colours; and always include colour-by-height, which needs the
   fewest changes. Each scheme passes printability rules: region width against nozzle size, colour depth,
   and an opaque backing under light colours.
5. **Fast scoring.** Candidates are scored for looks (contrast between neighbours, legible text, harmony)
   and for cost (purge, changes per layer, spool remaining).
6. **Real numbers for the finalists.** The best few are sliced for true time, grams and purge waste, and
   rendered. The vision model ranks them.
7. **Three or four options.** Closest to original, best looking, lowest waste, fastest, each already
   sliced.

For parts the agent designs, colour is modelled as separate solid bodies (inlays of set depth) so width,
depth and backing are design parameters. Paint per triangle is for imported meshes only.

Reused: slicing, purge tower, flush volumes and colour mixing by the slicer's command line; its project
file format; the
[Open Filament Database](https://raw.githubusercontent.com/OpenFilamentCollective/open-filament-database/main/README.md)
(MIT) for true filament colours; CIEDE2000 colour distance. New in Hotend: the region split and naming,
ranking several schemes by looks, waste, time and cost, limiting choices to the spools loaded and to
which nozzle each can reach, and printability rules for colour. Bambu Studio has its own import wizard
for textured models; Hotend adds the items just listed.

### X2D rules the planner must follow

- **The two nozzles are not equal.** The main nozzle is direct drive. The auxiliary nozzle is fed by a
  rear Bowden extruder, reaches 235.5 mm in X instead of 256, and Bambu limits it to 200 mm/s. TPU, PLA
  Aero and PPA-CF print on the main nozzle only.
- **Splitting two colours across the two nozzles removes most purging** but puts a visible colour on the
  slower nozzle, so options are ranked on time and quality as well as waste.
- **Nozzle numbering is inverted between slicer and printer.** Hotend maps by number, never by the words
  "main" or "left".
- **The real routing is read live.** Hotend writes an explicit filament-to-nozzle map instead of relying
  on automatic grouping.
- **Colour-by-height cannot be combined with painted or multi-part colour on one plate** in the slicer.
  For mixed cases the body is split by height into parts instead.

### Limits and checks

- Regions come from geometry plus the vision model: off-the-shelf 3D part-segmentation models have
  licences that do not fit an Apache-2.0 project.
- Mixed colours (virtual colours from alternating layers) stay opt-in.
- PrusaLink does not expose loaded filaments per slot, so Prusa starts with a palette the user maintains.
- The file-format details are unverified against a real slice. Before relying on them: a golden-file test
  per colour route against the pinned slicer, and a status report captured from the printer to confirm
  nozzle ids and AMS routing.

## Designed to be embedded

Everything works without the window, artefacts describe themselves, bought parts are first-class, and
exchange is through open formats. The rules are section 9 of `docs/IMPLEMENTATION_BRIEF.md`.

## Roadmap

Not built yet, in rough order: the calibration wizard and the print ledger; colour; control of a real
printer over the LAN; in-app hand editing and STEP import; mesh import and the Blender worker;
hand-editing connectors; PrusaLink; installers for Windows, macOS and Linux.

## Risks

- **Developer Mode is at the printer vendor's discretion.** Mitigation: three delivery paths behind one
  interface, with export depending on nothing.
- **Printer facts are partly unverified.** Protocol details come from documentation and open projects;
  the `verified` flags say which ones a real slice or printer has confirmed.
- **AI CAD quality on complex parts.** Mitigation: the Proven Fit method, a regression set of test parts
  run on every change, and hand editing as the escape hatch.
- **Three operating systems, three sandboxes.** The Windows and macOS sandboxes are the hard ones; the
  macOS backend is untested.
- **Licences.** Slicers and Blender are copyleft; Hotend runs them only as separate programs through
  their command lines and file formats and ships none of their code.

## Sources

- Bambu: [third-party integration](https://wiki.bambulab.com/en/software/third-party-integration), [Developer Mode](https://wiki.bambulab.com/en/knowledge-sharing/enable-developer-mode), [authorization control announcement](https://blog.bambulab.com/firmware-update-introducing-new-authorization-control-system-2), [X2D launch post](https://blog.bambulab.com/xcellence-made-simple-bambu-lab-presents-the-x2d/), [Bambu Studio command line](https://raw.githubusercontent.com/wiki/bambulab/BambuStudio/Command-Line-Usage.md)
- Printer protocol: [OpenBambuAPI](https://github.com/Doridian/OpenBambuAPI/blob/main/mqtt.md), [X2D in LAN-only mode, hands-on](https://robig.net/blog/posts/connect-to-bambulab-x2d-on-lan-only-mode/)
- X2D reviews: [3D Printing Industry](https://3dprintingindustry.com/news/bambu-lab-launches-x2d-dual-nozzle-3d-printer-targeting-reduced-post-processing-and-material-waste-251005/), [Tom's Hardware](https://tomshardware.com/3d-printing/bambu-lab-x2d-review), [Notebookcheck](https://www.notebookcheck.net/Bambu-Lab-X2D-Combo-review-Dual-nozzle-3D-printer-tested.1311491.0.html)
- CAD and viewer: [build123d](https://github.com/gumyr/build123d), [bd\_warehouse](https://github.com/gumyr/bd_warehouse), [three-cad-viewer](https://github.com/bernhard-42/three-cad-viewer), [CAD-bench](https://raw.githubusercontent.com/CAD-bench/cad-bench/main/README.md), [CADGenBench](https://raw.githubusercontent.com/huggingface/cadgenbench/main/README.md)
- Sandbox and security: [Tauri advisory](https://github.com/tauri-apps/tauri/security/advisories/GHSA-7gmj-67g7-phm9), [MCP local server security](https://modelcontextprotocol.io/docs/2026-07-28/tutorials/security/local-server-security.md)
- Agent: [Pydantic AI](https://pypi.org/project/pydantic-ai/)
- Prusa: [PrusaLink API](https://github.com/prusa3d/Prusa-Link-Web/blob/master/spec/openapi.yaml)
