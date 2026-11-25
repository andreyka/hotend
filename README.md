<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/hotend-logo-dark.svg">
    <img src="docs/assets/hotend-logo-light.svg" alt="Hotend" width="230">
  </picture>
</p>

<p align="center"><strong>Idea in. Part out.</strong><br>
A desktop AI agent that takes makers from an idea to a CAD model to a successful 3D print.</p>

<p align="center">
  <a href="https://hotend.dev">hotend.dev</a> ·
  <a href="ARCHITECTURE.md">Architecture</a> ·
  <a href="#security-model">Security model</a> ·
  <a href="#status">Status</a>
</p>

---

> **The source code is landing.** This repository opened with its documents first. The code
> follows as soon as its final test run and review are done; the commands and the paths
> into `docs/`, `crates/`, `python/` and `apps/` below become live with it.

You describe the part you want. An AI agent, running on your own API key, writes it as
parametric [build123d](https://github.com/gumyr/build123d) code. That code runs in a sandbox.
Checks on the exact geometry decide whether the result is a valid solid that fits your
printer, with fits taken from your printer's calibration. A vendor slicer turns it into a
print job. You, never the AI, confirm the print.

Hotend is built for people who want a part that fits and prints the first time, without
becoming CAD or slicer experts.

## What makes it different

- **CAD, not mesh.** Models are exact solids (STEP and BREP), written as readable parametric
  code. Every size is a parameter you can change with a slider.
- **Proven Fit.** Clearances for lids, pegs, holes and screw bosses come from a calibration
  table, and a check measures every declared fit on the finished geometry.
- **Checked before it is offered.** Valid solid, watertight, inside the build volume and no
  colliding parts are decided on the exact geometry. Wall thickness is sampled. Overhangs,
  bed contact and small features are reported as warnings. A check that cannot decide says
  so; it never passes silently.
- **The agent cannot print.** It can only propose a job. You review a summary computed from
  the sliced file itself and confirm in a native dialog the agent cannot reach.
- **Your key, your machine.** Bring your own model provider key. Only the Rust core reads
  keys. AI-written code and the agent run in a sandbox with no network of their own.
- **It looks things up.** When a request leaves sizes open, the agent can research them on
  the web, within limits the core enforces, and records its sources.

## Status

Early, and honest about it. Hotend runs from a checkout on Linux and on Windows through
WSL2. There is no installer yet.

**Works today**

- The sandbox for every child process that runs model-influenced code (the CAD engine, the
  agent, the slicer): no network, writes only to its job folder, resource caps, whole-tree
  kill. Landlock and seccomp, Linux only.
- The CAD engine: runs a model script, checks it, and writes STEP, 3MF, STL, a mesh for the
  3D view and PNG views.
- Proven Fit: calibration table, paired feature cards (snap lid, peg and hole, screw boss),
  gauge plates, and the check that measures every declared fit.
- The core: project store with immutable versions, tool gateway with an audit log, key
  storage, and the egress filter that is the agent's only way to a model provider.
- The agent. It has been run for real with Anthropic. OpenAI, Google, OpenRouter and Ollama
  are implemented from their public API documentation and not yet run against the real
  services.
- Web research through a search service (Keenable, run for real; Tavily, not yet) or through
  the model provider's own search (Anthropic only, not yet run for real). It is off until
  you agree once per source.
- Slicing with an installed Bambu Studio, run headless inside the sandbox, and a G-code
  check that every X2D file must pass before it can be reviewed or delivered.
- Print preparation: orientation, copies and plate layout.
- The way to a print: propose, review, confirm, then deliver to the simulated printer, as a
  file for Bambu Connect, or into a folder. A real print has gone from the application
  through Bambu Connect to an X2D.
- The desktop application: projects, conversation, 3D view, sizes with sliders, checks,
  plate, slice summary, print review, snapshots and settings.

**Simulated or unproven**

- `hotend demo` replays a written conversation instead of asking a model, slices with a
  stand-in slicer whose output is not printable, and uses a simulated printer. It says so in
  its output.
- The interface is tested in headless Chromium against a development bridge. No automated
  test opens the desktop application's own window. On Windows and macOS the application was
  type-checked, never built or run.
- Read-only printer status (subscribe only; Hotend sends the printer no command) is tested
  against a stand-in broker, not yet proven on a real printer.
- X2D facts are verified in real slices, none yet on a printer. A fitted 0.4 mm nozzle is
  assumed, and the print review says so.
- Default fit numbers are general FDM practice until you print a gauge and record what you
  measured.
- Only a joint made with a feature card is verified. A hand-made gap in model code is
  flagged as a warning, not proven.
- The macOS sandbox is written and untested, so it stays off. The Windows sandbox is not
  written; on Windows, Hotend runs inside WSL2.

**Not built**

- An installer. The application runs from a checkout only.
- Control of a real printer. Prints are handed to Bambu Connect (Windows and macOS) or
  written to a folder, and you start them yourself.
- Delivery for printers other than the Bambu Lab X2D. A Prusa profile can be sliced; its
  files are not checked or delivered yet.
- Colour planning, organic shapes and mesh generation.

## Quick start (Linux or WSL2, Ubuntu 24.04)

```bash
git clone https://github.com/andreyka/hotend && cd hotend
bash scripts/setup-wsl.sh --apt      # checks the machine, installs what is missing, builds,
                                     # then runs `hotend doctor`
target/debug/hotend demo             # the whole loop offline, about 40 seconds
target/debug/hotend demo --simulate-print   # the same, then asks you and runs the job on
                                     # the simulated printer (nothing is printed)
bash scripts/test-all.sh             # the full test run, then the demo; about half an hour
```

The sandbox needs Linux 6.2 or newer. On WSL2, run `wsl --update` in PowerShell if
`hotend doctor` says the kernel is too old. These commands make a debug build.

A first real conversation, with your own key:

```bash
target/debug/hotend key set anthropic    # typed at a hidden prompt, never an argument
                                         # (no keychain, as on WSL2? add --secrets file to
                                         # keep keys in a file of your own)
target/debug/hotend chat "Cable box" "A box that hides a power strip, with a snap-on lid" \
    --provider anthropic --model <model id>
target/debug/hotend project show "Cable box"
```

`hotend --help` and `hotend <command> --help` describe every command and option.
`apps/desktop/README.md` says how to start and work on the desktop application.

Real slicing needs your own slicer. Hotend detects an installed Bambu Studio or PrusaSlicer,
checks its version, and runs it inside the sandbox. It bundles no slicer.

## Security model

1. The model can be wrong or manipulated, so safety comes from what the agent cannot do.
2. AI-written code runs only inside the sandbox: no network, writes only to its job folder,
   a read-only view of the Python runtime, time and memory caps.
3. If the sandbox cannot be applied, nothing runs. Debug builds have two switches for
   development (run unconfined, with a warning; use another launcher program), and on macOS
   an opt-in variable enables the untested backend. Release builds have neither debug
   switch.
4. The agent and the slicer run in the same kind of sandbox. The agent has no network
   socket and can touch no file outside its scratch folder and the read-only runtime.
5. API keys live in the OS keychain or, if you choose, in the environment or in a file of
   your own (weaker than a keychain: any program running as you can read it). Only the Rust
   core reads keys. The window can set a key and never read one; Python never sees a key;
   no command prints one.
6. The agent reaches a model only through the core, which allows the chosen provider's host
   and paths, adds the key itself, and caps the requests and bytes of a run and the output
   tokens of each request.
7. Web research is capped by the core per message and per project. A page is untrusted
   text: it cannot add a tool, raise a cap or confirm a print.
8. Tool calls are schema-checked, policy-checked and written to the audit log with a hash
   of their arguments.
9. What a child process writes is read as hostile input: size caps, no symlinks, no path
   outside the job folder, strict parsing of the fields the core acts on.
10. The agent has no tool that starts a print and cannot set G-code. A job is delivered only
    on a confirmation the tool gateway cannot produce: a native dialog in the application,
    or `yes` typed at a terminal in the command line.
11. Every X2D file passes a G-code check before it can be reviewed or delivered, and the
    summary you confirm is computed from that file.

[ARCHITECTURE.md](ARCHITECTURE.md) shows where each rule is enforced and where its edges are.

Found a security problem? Please report it privately to the maintainer through GitHub
instead of opening a public issue.

## Where things are

| Path | What |
| --- | --- |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | The components, the trust boundaries and the path from a sentence to a print |
| `docs/DESIGN.md` | What the product is and why |
| `docs/PROTOCOL.md` | The contract between the components |
| `docs/WEB_RESEARCH.md` | How the agent researches, and what limits it |
| `docs/PRINT_SETTINGS.md` | The print settings model and its rules |
| `docs/DEPENDENCIES.md` | Every dependency, its licence and why it is here |
| `crates/hotend-core` | Store, sandboxed jobs, tool gateway, agent supervisor, egress, secrets, print jobs |
| `crates/hotend-sandbox` | Sandbox launcher and library |
| `crates/hotend-slicer` | Slicer detection, sandboxed runs, result parsing |
| `crates/hotend-printer` | G-code check, confirmation summary, deliveries, simulated printer, status listener |
| `crates/hotend-settings` | Print settings model and rule engine |
| `crates/hotend-cli` | The `hotend` command |
| `apps/desktop` | The desktop application: Tauri shell and React interface |
| `python/hotend_engine`, `hotend_fit`, `hotend_agent` | CAD worker, Proven Fit, agent loop |
| `scripts/` | Setup and the full test run |

## Licence

Hotend is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE).

Slicers are separate programs under their own licences. Hotend runs them through their
command lines and file formats and ships none of their code.

Bambu Lab, Bambu Studio, Bambu Connect, Prusa, PrusaSlicer and other product names are
trademarks of their owners. Hotend is an independent project and is not affiliated with or
endorsed by any of them.
