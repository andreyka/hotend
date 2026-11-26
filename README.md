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
- The desktop application (`apps/desktop`): projects, conversation, 3D view, sizes with
  sliders, checks, plate, slice summary, print review, snapshots and settings.

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
  assumed until you set it, and the print review says so.
- Default fit numbers are general FDM practice until you print a gauge and record what you
  measured.
- Only a joint made with a feature card is verified. A hand-made gap in model code is
  flagged as a warning, not proven.
- The macOS sandbox is written and untested, so it stays off. The Windows sandbox is not
  written; on Windows, Hotend runs inside WSL2.
- A few tests measure local reference files: other people's designs, which are not in the
  repository. They run only when `HOTEND_REFS_DIR` names the folder that holds the files and
  their list (`reference-models.json`); `scripts/test-all.sh` says when they were left out.

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

`setup-wsl.sh` installs for your user, when they are missing: rustup, uv, Node 22 (Ubuntu's
own is too old; the official archive, checked against a SHA-256 in the script), pnpm, the
interface's packages and the Chromium the end-to-end tests run in. System packages (a C
compiler, the WebKitGTK and GTK headers the desktop application is built against, a
virtual screen for the dialog test) are installed only with `--apt`; without it the script
prints the exact `apt` command and stops. If you already have a Chromium, name it and nothing
is downloaded: `HOTEND_CHROMIUM=<program>`, for both scripts.

`test-all.sh` runs a few steps only where they can run, and its summary says which were
skipped and why:

- the tests that need a real PrusaSlicer, when `prusa-slicer` is on PATH (it also prints
  how many Rust tests cargo ignored and which);
- the tests that measure the local reference models, when `HOTEND_REFS_DIR` names the
  folder that holds them and their list (`reference-models.json`). They are not in the
  repository, so on any other machine this step is skipped;
- the native print confirmation, when `xvfb-run` and `xdotool` are installed
  (`setup-wsl.sh --apt` installs both).

A first real conversation, with your own key:

```bash
# Without a keychain, as on WSL2, keep keys in a file of your own: add --secrets file to both
# commands, `key set` and `chat`, or set it once for the shell: export HOTEND_SECRETS=file
target/debug/hotend key set anthropic    # typed at a hidden prompt, never an argument
target/debug/hotend chat "Cable box" "A box that hides a power strip, with a snap-on lid" \
    --provider anthropic --model <model id>
target/debug/hotend project show "Cable box"
```

Real slicing needs your own slicer. Hotend detects an installed Bambu Studio or PrusaSlicer,
checks its version, and runs it inside the sandbox. It bundles no slicer.

With Bambu Studio the settings are the system profiles of its installation plus Hotend's own
print settings, and you choose the build plate (`hotend settings set --plate-type ...`). For
PrusaSlicer put your settings under
`<data-dir>/slicer-profiles/prusa-slicer/{machine,process,filament}/`
(`docs/PROTOCOL.md`, section 6); without them a generic demo profile is used, and the result
says so.

The desktop application, from the checkout (there is no installer):

```bash
cd apps/desktop
# The window, on a scratch data folder; the first start compiles for a while. A window
# started this way loads its interface from the development server on port 1420
# (apps/desktop/README.md says what follows from that).
HOTEND_DATA_DIR=~/hotend-scratch pnpm tauri dev

# The interface in a browser instead, against a scripted core (no model, no key,
# simulated printer); apps/desktop/README.md has the details:
cargo build -p hotend-desktop --features hotend-desktop/dev-bridge -p hotend-sandbox -p hotend-slicer
../../target/debug/hotend-bridge  # prints {"url": ..., "token": ...}
pnpm dev:bridge                    # then open http://localhost:1420/?bridge=<url>&token=<token>
```

If the checkout is on a Windows drive (`/mnt/d/...`) builds are much slower there. Keep the
build outputs on the Linux filesystem:

```bash
export HOTEND_CACHE_DIR=~/.cache/hotend      # put this in ~/.bashrc
bash scripts/setup-wsl.sh
```

That moves the cargo target directory and the Python environment to
`~/.cache/hotend/target` and `~/.cache/hotend/venv`, and the binary to
`~/.cache/hotend/target/debug/hotend`. To run that binary from a shell without the
scripts, also set `HOTEND_PYTHON=~/.cache/hotend/venv/bin/python` and
`HOTEND_PYTHON_SRC=<checkout>/python`. If you work as root, pick a folder other users can
enter (`/var/cache/hotend`): one sandbox test repeats its checks as the user `nobody`.

## Commands

`hotend --help` and `hotend <command> --help` describe every option. Projects live in the
data directory: `--data-dir`, else `HOTEND_DATA_DIR`, else the platform's data folder
(`~/.local/share/hotend` on Linux).

| Command | What it does |
| --- | --- |
| `hotend doctor [--json]` | Reports the sandbox, the Python environment, an engine self-test and an agent self-test (both run for real in the sandbox; the agent replays a written conversation, no model is asked), the slicers with the version policy's verdict and the printers each serves, the printer, and the key stores. Exits 1 if a hard requirement is missing and names the fix |
| `hotend demo [--out <dir>] [--simulate-print]` | The offline end-to-end run on a cable box: describe, contract, code (the first version fails a check), fix, checks, views, mesh, prepare and slice, pending print job, G-code check with the confirmation summary. With `--simulate-print`, and only after you type `yes` at the terminal, the job runs on the simulated printer to its end. Uses its own data directory inside `<dir>` |
| `hotend project new <name>` / `list` / `show [<project>]` | Projects, their versions, print jobs and conversation |
| `hotend build <model.py> [--param k=v] [--project <p>]` | Builds a model script in the sandbox and stores it as the next version of the project (named after the file by default) |
| `hotend param set <name> <value>` | Rebuilds the current version with one parameter changed, as a new version |
| `hotend versions` / `hotend revert <n>` | Lists the versions; makes an earlier one current again (nothing is removed) |
| `hotend snapshot create\|list\|export\|restore\|delete` | Names a version, writes a named version's files into a folder, goes back to one |
| `hotend checks [--only a,b]` | Runs the checks on the current version |
| `hotend measure --query '<json>'` | Exact measurements on the current version |
| `hotend questions` | What the version's contract still asks you, because it rests on a guess |
| `hotend export --format step\|3mf\|stl [--out <dir>]` | Writes the current version (or `--version n`) as files |
| `hotend views [--view iso] [--section XZ@5] [--out <dir>]` | Renders PNG views, optionally cut open |
| `hotend prepare [--copies part=n] [--up part=-z]` | Lays the version out for printing: orientation of each part, copies, plates |
| `hotend plan show` / `explain` | Hotend's print settings for a plate, what is assumed or still to answer, and the reason for each value |
| `hotend slice [--profile <id>] [--fake]` | Prepares the current version if needed and slices the plate with an installed slicer; `--fake` uses the stand-in |
| `hotend gauge [--card '<json>'] --out <dir>` | Writes a gauge plate (3MF and views) for the version's joints or for the given cards |
| `hotend calibrate [--reading hole:<drawn>=<measured>]` | Records what you measured on a printed gauge into the printer's calibration table, dated by Hotend; without a reading, shows the table |
| `hotend baseline set` / `show` / `remove` | The sticking settings of a print you reported as good: the floor for later plans on the same printer, plate and material |
| `hotend printers` | The printers a job can run on (one, simulated) and the ways a confirmed job can be delivered |
| `hotend printer status` / `access-code` / `forget-certificate` | Reads a real printer's status over your network (read-only); stores its LAN access code (never shown). The address and serial are set with `hotend settings set` |
| `hotend print propose` / `status [<job>]` / `review <job>` / `cancel <job>` | Proposes the sliced file as a print job; shows jobs; checks a job's G-code and shows what would be printed, read from the file |
| `hotend print confirm <job> [--to simulated\|bambu-connect\|folder] [--out <dir>]` | Shows the review, asks you at the terminal and delivers the job after `yes`. Refuses without a terminal; no option or environment variable confirms a print |
| `hotend print project [--out <dir>]` | Makes the editable Bambu Studio project of a plate with Hotend's print settings; with `--out`, saves a copy after a typed `yes`. Opens nothing |
| `hotend settings show` / `set [...]` / `slicers` | Printer, material, build plate, nozzle, provider, model, turns per message, hand-off folder; the slicers and which is in use (`--slicer prusa-slicer` selects PrusaSlicer and its printer) |
| `hotend audit [--all] [--limit n]` | The audit log: every tool call, sandboxed job, version, preparation and print step |
| `hotend chat <project> "<text>" --provider <kind> --model <id> [--secrets keyring\|env\|file]` | Sends a message to the real agent with your key and streams its text and tool calls. Ctrl-C stops it |
| `hotend research status` / `on` / `off` / `source` / ... | Web research: turns it on after a typed `yes`, chooses where it comes from (the AI provider, Keenable, Tavily or off), shows what was found and confirms or rejects its numbers |
| `hotend key set <provider>` / `hotend key status` | Stores a key (of an AI provider, Keenable or Tavily) in the system keychain (typed at a hidden prompt or piped in, never an argument); shows which keys are set, never the keys |

A project is named by id, by the start of an id, or by name; most commands default to the
most recently changed project; most take `--json`. Exit codes: 0 done, 1 the outcome is a
failure (a model that does not build, a failed check, a job that was not confirmed), 2 the
command could not run.

## What leaves your computer

- What the agent works with goes to the AI provider you chose, with your key: your
  messages, the model's code, what Hotend's tools answer (checks, measurements, print
  settings, slices, what the code prints) and pictures of the part. In full: your messages
  and the conversation so far; the model's code, its parameters and the part sizes; the
  results of its builds, the error text and traceback when one fails, and up to 2,000
  bytes of what the code prints per result; check results, contract results and open
  questions; measurements; pictures of the part (Hotend adds some after each build that
  passes, the agent asks for others); the print plan and a slice's summary (time,
  filament, plates, warnings); the printer, material, build plate and nozzle size of the
  settings and the slicer profiles Hotend knows; the results of web research and the
  stored research notes; and Hotend's own instructions, tool list, documentation and
  component data, including component files you put beside the sample. Error text can name
  folders on this computer (for example the data folder, a job folder, the Python runtime,
  the slicer, the folder Hotend runs from), and a path can contain your account name; when
  slicing fails, the end of the slicer's own output is sent too. The code is the model's
  own, so what it prints or raises can include anything the sandbox lets it read or ask the
  system (the paths are in the security model below, item 2). Among it: the job folder's
  path, the computer's name, the system, processor and memory description, the time zone,
  your numeric user id, the names and contents of files under the system paths and in the
  Python runtime, the list of mounted drives and network shares with their addresses (from
  its own `/proc` entry), where the kernel does not let the sandbox use namespaces also this
  computer's network addresses and routes, and, for a path whose name it guesses, whether it
  exists, its size, dates and permissions and where a link points. Sliced files, exported
  files and other projects are not sent. With Ollama all of this goes to its server on this
  machine (127.0.0.1) instead.
- Requests to the AI provider and to a search service go through the proxy named in
  `HTTPS_PROXY` or `ALL_PROXY` if you set one (an HTTP proxy; SOCKS proxies are not
  supported, and a request sent to one fails). `HTTP_PROXY` alone changes nothing, because
  all these requests are HTTPS. Requests to Ollama never use a proxy.
- Web research is off until you agree to it. When it is on, search queries and the addresses
  of the pages to read go to the AI provider, which runs them, or to the search service you
  chose (Keenable or Tavily). Queries can contain words from your messages. Keenable can be
  used without a key; such requests are identified by your internet address and are logged
  by the service (`docs/WEB_RESEARCH.md`, section 16).
- The printer status listener talks to one printer on your own network, at the private
  address you set.
- Everything else stays on disk: your projects, exported files and sliced files.

## Security model

1. The model can be wrong or manipulated, so safety comes from what the agent cannot do.
2. AI-written code runs only inside the sandbox, with no network and with time and memory
   caps. It can write only in its job folder (its home and temporary folder are inside it). It
   can read that folder, read and run the Python runtime (the environment, its interpreter
   and, in a checkout, the folder `python/`) and a fixed list of system paths: `/usr`, `/lib`,
   `/lib32`, `/lib64`, `/bin`, `/sbin`, a few files and folders under `/etc`, the processor
   list under `/sys`, five files of `/proc` and its own `/proc` entry, and four device files
   (`/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/random`). It cannot open or list anything
   else, your own files included. For a path whose name it knows it can still see that the
   path exists, its size, dates and permissions, and where a link points
   (`docs/notes/sandbox.md`).
3. If the sandbox cannot be applied, nothing runs. Debug builds have two switches for
   development (run unconfined, with a warning; use another launcher program), and on macOS
   an opt-in variable enables the untested backend. Release builds have neither debug
   switch.
4. The agent and the slicer run in the same kind of sandbox, with the same system paths. The
   agent has a scratch folder of its own, which is removed after the run, and the Python
   runtime; it has no network socket. The slicer has the job folder of its slice and the
   folder it is installed in, to read and run.
5. API keys live in the OS keychain or, if you choose, in the environment or in a file of
   your own (weaker than a keychain: any program running as you can read it). Only the Rust
   core reads keys. The window can set a key and never read one; Python never sees a key;
   no command prints one. A program Hotend starts for a link, a file or your browser does not
   inherit the variables that may hold a key; Hotend's own environment keeps them, and the
   web view's helper processes inherit it.
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
    summary you confirm is computed from that file. An editable Bambu Studio project leaves
    this check: Bambu Studio slices it again, and Hotend does not check what it then makes.

[ARCHITECTURE.md](ARCHITECTURE.md) shows where each rule is enforced and where its edges are.

Found a security problem? Please report it privately to the maintainer through GitHub
instead of opening a public issue ([SECURITY.md](SECURITY.md)).

## Where things are

| Path | What |
| --- | --- |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | The components, the trust boundaries and the path from a sentence to a print |
| `docs/DESIGN.md` | What the product is and why |
| `docs/IMPLEMENTATION_BRIEF.md` | Repository layout and rules |
| `docs/PROTOCOL.md` | The contract between the components |
| `docs/WEB_RESEARCH.md` | Web research: what it does, what it sends and to whom, and what limits it |
| `docs/PRINT_SETTINGS.md` | The print settings model and its rules |
| `docs/DEPENDENCIES.md` | Every dependency, its licence and why it is here |
| `docs/notes/` | The sandbox in detail with its known limits; an open defect |
| `docs/ux/` | The interface's design specification and pictures of every screen |
| `crates/hotend-core` | Store, sandboxed jobs, tool gateway, agent supervisor, egress, secrets, print jobs |
| `crates/hotend-sandbox` | Sandbox launcher and library |
| `crates/hotend-slicer` | Slicer detection, sandboxed runs, result parsing |
| `crates/hotend-printer` | G-code check, confirmation summary, deliveries, simulated printer, status listener |
| `crates/hotend-settings` | Print settings model and rule engine |
| `crates/hotend-cli` | The `hotend` command |
| `apps/desktop` | The desktop application: Tauri shell and React interface |
| `python/hotend_engine`, `hotend_fit`, `hotend_agent` | CAD worker, Proven Fit, agent loop |
| `scripts/` | Setup, the full test run, and the tests of both and of what the documents promise |

## Licence

Hotend is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE).
`docs/DEPENDENCIES.md` lists every dependency with its licence.

Slicers are separate programs under their own licences. Hotend runs them through their
command lines and file formats and ships none of their code; none of their code is copied or
linked.

Bambu Lab, Bambu Studio, Bambu Connect, Prusa, PrusaSlicer, Apple, Keenable, Tavily, Anthropic,
OpenAI, Google and other product names are trademarks of their owners. Hotend is an
independent project and is not affiliated with or endorsed by any of them.
