# Hotend: component contracts

Version 0 (draft). This is the contract between the Rust core and its child processes, and between
the UI and the core. Change it in the same change as the code, and bump the version when a field
changes meaning.

All JSON is UTF-8. All lengths are millimetres, angles degrees, mass grams, time seconds.
Every file path a child reports is relative to its job folder; the core rejects anything else.

## 1. Data on disk

```
<data-dir>/                         from the `directories` crate, overridable with HOTEND_DATA_DIR
  hotend.db                         SQLite: projects, versions, messages, audit log, settings,
                                    slices, pending print jobs, snapshots
  projects/<project-id>/
    snapshots/<snapshot-id>/        the exact sliced file of the print of a snapshot's version
                                    (section 8, "Snapshots"); written once, never changed
    versions/.staging-<job-id>/     a version being imported; renamed to <n> or removed
    versions/<n>/                   immutable once written (files are read-only)
      model.py                      the model script (section 2)
      params.json                   parameter values used for this build
      result.json                   engine result (section 3.3)
      model.step  model.brep        exact geometry
      model.parts.json              names and colours of the parts in model.brep, in order
      built_for.json                {printer_id, material_id, calibration_sha256}: the settings
                                    the version was built and checked with
      mesh.json                     tessellation for the viewer
      views/<name>.png              renders for the AI and for thumbnails
      prepare/                      the print preparation (section 3.5): prepare.json, the
                                    engine's prepare result and plates/<i>/plate.3mf with its
                                    views; replaced as a whole when the version is laid out again
        plan/<plate>.json           the print plan of a plate (section 3.5, "Print
                                    settings"): a record, never read back as settings
      slice/<profile-hash>/         slicer inputs and outputs
      slicer-project/<plate>-<plan>/  editable Bambu Studio project and its test slice (8.2)
  jobs/<job-id>/                    scratch folder of one sandboxed job; deleted after import
  handoff/                          sliced files placed for Bambu Connect (section 9)
  hotend.lock                       empty; every open core holds a shared lock on it
  calibration/<printer-id>.json     calibration table (section 4.2)
  proven-baselines.json             proven baselines, one per printer, plate and material
                                    (section 8, "Proven baselines"); absent until one is
                                    recorded
```

`prepare/`, `slice/` and `slicer-project/` are the only folders added to a version after it
was written.

The print plan of a plate, `prepare/plan/<plate>.json`, is `PlanRecord {schema: 1, project_id,
version, plate, sha256, basis, plan, created_at_ms}`. `basis` is everything the plan was
computed from: the plate facts as the engine stated them (`facts`) and the number of pieces
they must name (`pieces`), `material_id`, `plate_type`, the spool (`spool_slot`,
`spool_colour`, `spool_colour_set`, `spool_silk`), `nozzle_diameter_mm` and `nozzle_size_set`,
`printer_id`, `max_layer_height_mm`, `bed_limit_c` and `feeds` (`{slot, nozzle_id, main}` per
slot the printer data know). `plan` is the plan as `hotend-settings` writes it (values,
decisions with their sentences, checklist, data revisions). The record is never read back as
settings: the core computes the plan again from the current settings and the stored facts,
and writes a new record whenever the hash or the basis differs (a changed setting, other rule
data, an altered file). `modeling::verify_record` says whether a record's own basis still
gives its hash.

Ids are UUIDv7 strings. A version is created only from a build that finished; failed builds are
kept in the audit log, not as versions.

How the core (`hotend-core`) keeps this:
- A version is created from a build with `status` `ok` or `check_error`. The core copies the
  artefacts out of the job folder under the fixed names above (it never uses a name the engine
  chose), into a staging folder, and renames that folder to `<n>` inside the database transaction
  that inserts the row. Version rows cannot be updated or deleted (SQLite triggers). Each version
  records its `parent` (the version that was current) and its `origin` (`write_model_code` or
  `set_parameter`). A version build always asks for STEP, BREP, the mesh and the `iso` view.
- Importing a job is hostile-input handling: `result.json` is size-capped (4 MB) and parsed with
  unknown fields rejected; every artefact path must be the fixed path of section 3 for that
  artefact, relative, without `..`, not a symbolic link at any component, not a hard link, inside
  the job folder after resolving, a regular file and under its size cap; a view must start with
  the PNG signature. Anything else is the typed error `malformed_result` or `unsafe_artifact`, an
  audit entry, and no version.
- Audit entries have a `kind`: `job` (every sandboxed engine job, with its outcome), `version`
  (created, or refused at import), `tool` (every gateway call, with the SHA-256 of its arguments
  as canonical JSON: keys sorted, no whitespace), `egress` (every model request: provider, route,
  status, byte counts; never a header value or a body), `agent` (an agent run and how it ended),
  `secret` (a provider key was set or deleted; never the key), `prepare` (a version was laid
  out on plates), `gauge` (a gauge plate was exported), `calibration` (measurements were
  recorded into a printer's table; named after the printer, without a project), `slice` (one
  entry named `slice` per slice, by the agent's tool or a person's command, outcome `ok` or
  `failed`; `args_sha256` is the sliced file's SHA-256; `detail {project_id, version, plate,
  slice_id, profile_hash, plan_sha256, slicer, slicer_version, previews_added_by_hotend,
  sha256}`, plus `error` (the code) on failure, with `null` for what was not known yet:
  `slice_id` is the slice's folder name (`slice/<profile-hash>`), so it equals
  `profile_hash`; never a path, the file's contents or the slicer's text) and `print` (one
  entry per state a print job reaches after its proposal, named after the state, with the
  SHA-256 of the sliced file and `detail {job, delivery, simulated, message}`; the
  `handed_off` entry adds `opener` (`LinkOpener::describe`), `opener_started` (true only
  when a real opener started a program) and, when the opener failed, `opener_error`, a short
  fixed text; never the link or the file's path, which can carry the project's name; the
  `project_handed_over` entry of an editable project placed for Bambu Studio adds the same
  three fields). A revert is a `version` entry named `revert`.
- A `job`, `tool` or `egress` entry is written however the operation ends. Besides the outcomes
  of sections 3.1, 5 and 5.1 there are `cancelled` (the run it belonged to ended or was
  cancelled while it was in flight; the job was stopped) and `abandoned` (the operation was
  dropped before it could finish, for example because the application closed). Every entry has
  `detail.duration_ms`.
- The `detail.sandbox` of a `job` entry and of an `agent` entry says how the child was confined:
  `{"mode": "on", "launcher": "<path>", "launcher_overridden": false}`, `{"mode": "off"}` (a debug
  build with `HOTEND_UNSAFE_NO_SANDBOX=1`) or `{"mode": "unavailable"}`.
- A job folder is removed when its job ends, also when the job is cancelled or abandoned (its
  launcher is stopped first). What a core that was killed left behind is removed by the next core
  that opens the data directory while no other core has it open: every folder in `jobs/` and
  every `versions/.staging-*`, recorded as one `job` entry named `sweep` with the counts. "No
  other core" is decided by `hotend.lock`: the sweep runs only while an exclusive lock can be
  taken; afterwards, and otherwise, the core holds a shared lock until it closes.
  The same core ends every print job that was left `starting` or `printing` as `failed`, with
  the reason and a `print` audit entry: the simulated printer lives in the core's process, so
  such a job can never report an end.
- A data folder that has entries in `projects/` but no `hotend.db` is refused when a core opens
  it, and nothing is created or changed (also no lock file): a new, empty database would show
  none of the projects.
- The `messages` table holds, per project, the provider-native history the agent returned (opaque
  to the core) and a plain transcript (user text, assistant text) for the UI. A transcript line
  has `remembered`: false for the lines of a run whose conversation did not reach the stored
  history (the run failed, ended with `stop_reason: "error"`, or was stopped before the agent
  answered), so the next message continues without them. The lines stay in the transcript: they
  are what the user saw.
- `<profile-hash>` is the first 16 hex digits of the SHA-256 of `{"plate", "profile"}` as canonical
  JSON.
- Printer facts have one source. For a printer the printer layer knows (the X2D) it is
  `crates/hotend-printer/profiles/bambu.toml` (section 9): `hotend_core::profiles` reads the
  nozzle ids, reach and speed limit from the profile compiled into `hotend-printer`. A printer
  Hotend only slices for (`prusa-mk4s`) is in `crates/hotend-core/profiles/printers.json`,
  which may not name a printer the printer layer has (a test fails if it does). The nozzle
  blocks of `hotend-slicer`'s `printers.toml` are generated from `hotend_core::profiles`
  (section 6). Every fact carries `verified: false` until it has been measured on a real
  printer.
- A version is built and checked for one printer and one material (`built_for.json`). A
  preparation for other settings still runs, and says so in `Preparation.warnings` ("Version
  N was built and checked for the material ... build it again before you print it"); the
  slice and the print review carry that warning with them. `prepare` refuses copies or
  orientations for a part the version does not have, and a nozzle the printer does not have
  (`invalid_input`). A layout in which nothing fits is stored like any other: `plates` is
  empty, `unplaced` says why, `status` is `check_error`.
- A print preparation is stored as `versions/<n>/prepare/prepare.json`: `Preparation
  {project_id, version, printer_id, material_id, options: {copies, orientations}, status,
  orientations, plates, unplaced, checks, warnings, created_at_ms}` (the lists are the engine's,
  section 3.5). It is written to a staging folder and renamed into place. A slice records the
  `created_at_ms` of the preparation it read; a slice of an earlier layout no longer counts as
  the version's slice (`no_slice`).
- Migration 3 of the store adds `slices.job_json` (`{plate, printer_id, material, pieces,
  prepared_at_ms}`) and, to the print jobs, `delivery`, `detail_json` and `updated_at_ms`. A
  print job's `status` is one of `pending`, `rejected`, `exported`, `handed_off`, `starting`,
  `not_started`, `printing`, `finished`, `failed`, `cancelled`; a change is written only from
  the states it may follow, so two confirmations of one job cannot both win.

## 2. Model script

A model is one Python file. The agent writes it; the engine imports it inside the sandbox.

```python
from build123d import *
from hotend_fit import fit, cards            # optional

PARAMS = {
    "width":  {"default": 80.0, "min": 40.0, "max": 200.0, "unit": "mm",
               "label": "Width", "provenance": "style"},
    "wall":   {"default": 2.0,  "min": 1.2,  "max": 4.0,   "unit": "mm",
               "label": "Wall thickness", "provenance": "guessed"},
}

def build(p, ctx):
    """p: parameter values by name. ctx: BuildContext (printer, material, fit resolver).
    Returns one Part, or a dict of named Parts. Each named Part is one printed piece."""
    ...
    return {"box": box, "lid": lid}
```

Rules the engine enforces:
- `PARAMS` is required (it may be `{}`) and is a dict of literal specs: the engine reads it from
  the source with `ast.literal_eval` and validates it before any of the script runs. Parameter
  names match `[a-z][a-z0-9_]{0,31}`, at most 64 of them. A spec has `default` (required, a
  number) and `provenance` (required, one of `measured`, `standard`, `calibrated`, `guessed`,
  `style`), and optionally `min`, `max` (numbers, `min <= default <= max`), `unit` (`mm`, `deg`,
  `count` or `ratio`; default `mm`; a `count` is a whole number and reaches `build` as an `int`)
  and `label` (1 to 80 characters; default the name). Unknown keys are rejected.
- Request `params` are merged over the defaults and nothing is clamped: an unknown name, a value
  that is not a number or a value outside `[min, max]` is a `model_error` of type
  `ParameterError` that names the parameter.
- `p` is a dict that also allows `p.width`. `ctx` has `printer`, `material` (the request's
  context objects, attribute access) and `fit` (section 4.2).
- `ctx.references` are the real things the request lists (`context.references`, section 3.2),
  by name: `ctx.references["part"]` is a `hotend_fit.reference.Reference` with exact
  measures (`size`, `bbox`, `volume`, `hull_volume`), `hull()`, `section(height)`,
  `silhouette()` and `pocket(fit, ctx=ctx, shape=...)`, the room for it sized by the fit
  resolver (section 4.6). `ctx.component("sample_phone")` is a bought device from
  `hotend_fit.components`: its body as a reference, its buttons, ports and camera as
  features with keep-out volumes, every number with provenance (section 4.7). A model never
  opens a reference file itself and never types a clearance around a reference. A reference
  is not a part: a model does not return it, and it is never exported.
- `build` must be deterministic and must not read or write files, the network, the clock or
  the environment. (The sandbox enforces the first two; the rest is checked by building twice when
  `verify_determinism` is set.)
- Parts are build123d `Part`/`Solid`/`Compound` objects (a `BuildPart` builder is accepted and
  its `.part` used). Part names match `[a-z][a-z0-9_]{0,31}`, at most 64 parts; a single returned
  Part is named `part`. A value that holds no solid (a sketch, a face, an empty Part) is a
  `model_error` of type `NotASolid`. The engine flattens every part to its solids in world
  coordinates.
- Optional declarations, each a literal dict assigned once at the top level of the script. Like
  `PARAMS` they are read from the source with `ast.literal_eval` and validated (pydantic models
  in `hotend_fit.contract`, unknown keys rejected) before any of the script runs. A declaration
  that does not validate is a `model_error` whose message names the bad field
  (`CONTRACT is not valid: requirements.0.cavity_min.size.1: Input should be a valid number`)
  and whose `line` is the assignment; the type is `ContractError`, `AssemblyError` or
  `PrintError`. A declaration that names a part `build` did not return is the same error,
  raised after the build.
  - `CONTRACT`: what the product must do (section 4.1).
  - `ASSEMBLY = {"lid": {"mate": "box.rim", "note": "snaps on"}}`: per part, the part it mates
    with, written `<part>` or `<part>.<feature>` (the feature is a free label). It is validated
    and otherwise not used by the engine yet.
  - `PRINT = {"lid": {"up": "-z"}}`: per part, the direction of the part as modelled that should
    point up on the bed: `+x`, `-x`, `+y`, `-y`, `+z`, `-z` or a vector `[x, y, z]` (not zero).
    Print preparation uses it unless it cannot print that way (section 3.5).

## 3. Engine worker

### 3.1 Invocation

```
hotend-sandbox run --profile engine --job <job-dir> -- \
    <python> -I -m hotend_engine.worker <job-dir>
```

One process handles one request and exits. The job folder is the only writable location. The worker
reads `<job-dir>/request.json`, writes artefacts into the job folder and writes
`<job-dir>/result.json` last (write to a temp name, then rename). The core treats a missing or
malformed `result.json` as a crash. Stdout and stderr are logs only; the worker captures the model
script's own prints into `result.json` (`stdout`, truncated to 16 kB; `stdout_truncated` says so).

The worker points HOME, the temp directory and the XDG cache/config directories at
`<job-dir>/.engine-scratch` while it runs (CAD libraries cache font lists under HOME) and removes
that folder before it writes `result.json`. `hotend_engine` must be importable in isolated mode,
so the read-only runtime view includes the environment's site-packages and, for an editable
install, the `python/` source folder.

**Who writes the verdict.** The worker never executes model code. For a `build` it parses the
script (syntax, `PARAMS`, parameters), reads the calibration, and then starts a second process
inside the same sandbox, `<python> -I -m hotend_engine.model_run <job-dir>` (the model
process), which is the only place the script runs. The model process hands over three files and
nothing else: `model.brep` and `model.parts.json` (the parts `build` returned) and
`model.build.json` (the record: `{"protocol": 0, "status": "ok"|"model_error", "error",
"stdout", "stdout_truncated", "fits", "timings"}`, parsed strictly, at most 4 MB; `fits` is the
declared feature cards as data, section 4.4). Everything in them is a claim. When the model
process has ended the worker
1. kills and reaps every process it left behind (on Linux the worker is a child subreaper and
   walks `/proc/self/task/*/children` until none is left; if that cannot be done the job fails),
2. deletes everything the model process left in the job folder except those three files, which
   must be plain files, and both scratch folders,
3. loads the B-rep and computes the part summary, the checks, the mesh, the views and the
   exports from it, and writes `model.brep` and `model.parts.json` again itself.
So `status`, `parts`, `checks` and every artefact of a result come from a process that the script
could not reach: patching the engine's modules, writing a `result.json`, an `atexit` hook or a
process left running change nothing the core reads. The model process waits for one byte on its
stdin before it runs the script; the worker sends it after loading its own libraries. On systems
other than Linux a process the script detached is not accounted for (the sandbox launcher's own
process-group kill is the only bound there).

Exit codes: 0 = `result.json` written (the build itself may still have failed, see `status`);
2 = bad request (the reason is on stderr and, when the job folder is writable, in `result.json`
with `status: "internal_error"` and `error.type: "bad_request"`); anything else = crash. The supervisor kills the worker at `limits.wall_seconds`
and reports `status: "timeout"` itself. When the model process ends without a usable record (the
script called `os._exit`, the kernel crashed, the record or the B-rep is malformed) the worker
writes no `result.json`, removes any that is there, and exits with the model process's exit code
(128 + n for signal n), or 70 when that code is 0, 2, 123 to 127 or otherwise reserved: a crash
for the core, never a result.

The supervisor is `hotend_core::jobs`. It finds the interpreter in `HOTEND_PYTHON` and the
folder of an editable install in `HOTEND_PYTHON_SRC` (one folder). A debug build without them
uses the checkout it was compiled in (`python/.venv/bin/python` and `python/`); a release build
requires `HOTEND_PYTHON` and fails with a typed error that names it (the path of the build
machine means nothing on another machine). On Unix the interpreter is used only if the file and
its folder belong to the user or to root and are not writable by everyone: the core runs it
once outside the sandbox to ask for its prefixes. The read-only view is the interpreter's
`sys.prefix`, `sys.base_prefix` and those folders. The sandbox enforces `limits.wall_seconds`
(launcher exit code 124); the core adds a grace period and then stops the launcher itself. Job
outcomes are typed, never a result: `timeout`, `crashed` (with the exit code and the last 32 kB
of stderr), `limit_reached` (launcher exit code 123: the job was killed at its process, memory or
disk limit; the limit is named, the job is not restarted and nothing is kept, and the person is
told in a `[Hotend: ...]` line), `bad_request` (exit code 2), `engine_internal_error` (`status: "internal_error"`),
`malformed_result`, `unsafe_artifact`, `sandbox_unavailable`, `spawn_failed`, `io_error`,
`cancelled` (the run the job belonged to ended; the launcher got SIGTERM and the job folder was
removed). A crash is retried once: the same inputs go into a new job folder together with
`restart.json` (`{"attempt": 2, "previous_job_id", "previous_exit"}`), and only a second crash
is the job's `crashed`. Each crash is kept in `crashes/<job-id>/` (the newest 10), a folder the
core creates new; the crashed job folder itself is removed and nothing the worker wrote is kept,
opened or followed (a planted link or FIFO is never touched). It holds the core's own inputs of
the job (`request.json`, `restart.json`, input files up to 1 MB each and 4 MB together; larger
ones are named in `left_out`) and `crash.json`: the op, the attempt, the exit status, the signal (of the launcher, or of the worker
as an exit code of 128 + n), whether a core was dumped, the elapsed time, the peak resident
memory of the job's process tree and of its largest process (sampled from `/proc` every 200 ms,
Linux only), the memory and time limits and the end of stderr; the job's audit entry carries the
same object as `crash`, and the restart's entry names it in `restart_of`. When the job belongs
to an agent run, the person is told in the run's text (a `[Hotend: ...]` line, kept in the
transcript) that the engine was restarted, or that it crashed again. A `result.json` whose `op` is not
the op that was asked for, or whose `protocol` is not 0, is malformed. With
`checks.verify_determinism` the model runs twice, so `stdout` holds its prints twice.

### 3.2 Request

```json
{
  "protocol": 0,
  "op": "build",
  "model_path": "model.py",
  "params": {"width": 90.0},
  "context": {
    "printer": {"id": "bambu-x2d", "nozzles": [
        {"id": 1, "role": "main", "diameter": 0.4, "reach": {"x": [0, 256], "y": [0, 256], "z": [0, 260]}},
        {"id": 0, "role": "auxiliary", "diameter": 0.4, "reach": {"x": [20.5, 256], "y": [0, 256], "z": [0, 256]}}]},
    "material": {"id": "pla-basic", "family": "PLA"},
    "calibration_path": "calibration.json"
  },
  "outputs": {"step": true, "brep": true, "mesh": true, "views": ["iso", "front", "top", "right"],
              "stl": false, "threemf": false},
  "checks": {"enabled": true, "min_wall": 0.8, "verify_determinism": false},
  "limits": {"wall_seconds": 120, "max_triangles": 2000000}
}
```

Every field except `op` has a default. Unknown fields are rejected (exit code 2), except inside
`context` and `limits`. `model_path` and `calibration_path` are relative to the job folder and may
not leave it. Optional fields beyond the example: `outputs.view_size` (`[w, h]`, 64 to 2048,
default `[768, 768]`), `checks.only` (list of check ids to run), `checks.sample_ranges`,
`material.density_g_cm3`, `material.max_overhang_deg` (default 45 when absent).
View names: `iso`, `front`, `back`, `left`, `right`, `top`, `bottom`.

Reference objects. `context.references` lists real things the model is built around, each a
file in the job folder (at most 8):

```json
"references": [{"name": "part", "path": "refs/part.3mf", "format": "3mf",
                "units": "mm", "scale": 1.0, "kind": "soft", "plate": 1, "object": "body"}]
```

`name` (`[a-z][a-z0-9_]{0,31}`) is what the model asks for; `path` is relative to the job
folder and may not leave it (a symbolic link is not followed); `format` is `stl`, `3mf` or
`step` (default: by extension); `units` is `mm`, `cm`, `m` or `in` (default: what the file
states, else mm); `scale` is applied on top of the unit and of every placement inside the
file; `kind` is `rigid` (default) or `soft`. `plate` (the plate's number) and `object` (the
object's name or 1-based index) name one object of a file that holds several; they are never
merged, and a request that does not single one out is a bad request (exit code 2) whose
message lists what the file holds. The file is hostile input (`hotend_engine.reference_io`):
at most 256 MB (STEP 64 MB), 4096 archive entries, `limits.max_reference_triangles` triangles
(default 2,000,000) over all its objects, XML without document types or entities, part names
inside a 3MF that stay inside the package, coordinates under 100 m. Nothing is repaired
silently. What the model gets is the object as printed: every component and build-item
transform applied, modifier volumes ignored, negative parts cut away, parts of one object
united, standing on z = 0 with the centre of its footprint on the Z axis. Both the model
process and the worker read the file before any script code runs; the worker verifies with its
own copy. `outputs.ghosts` (default true) shows placed references in mesh.json and the views
(section 3.4).

Three more options of `checks`, all off by default so that a request that does not name them
gets the results it always got:
- `orientation`: `as_modelled` (default) or `prepared`. With `prepared`, `overhangs` and
  `bed_contact` judge every part in the orientation print preparation chooses for it (section
  3.5, with the model's `PRINT` and the main nozzle's reach), and `data.orientation` holds
  `source`, `up`, `turned` and `explanation`. For a caller that prints the prepared plate: a lid
  modelled rails-down then no longer warns about an overhang that preparation removes.
  `fits_build_volume` then judges the same plate (section 3.3): per nozzle, the part turned
  the way preparation turns it for that nozzle, against the reach less the 2 mm margin
  preparation keeps by default.
- `contract`: measure the model's `CONTRACT` on the solids (check id `contract`, section 4.1)
  and add `questions` to the result. `checks.only` may name `contract` instead.
- `undeclared_joints`: look for joints made without a feature card (`fit_pairs` results with
  `data.kind` = `undeclared`, section 4.4).

`model.parts.json` is `{"protocol": 0, "parts": [{"name", "color"}], "print": {"orientations":
{part: [x, y, z]}, "fits": [...]}}`. `print` is what print preparation needs besides the solids:
the `up` vectors of the model's `PRINT`, and the feature cards of the build as declarations
(section 4.4), written by the worker from the cards it rebuilt itself. When the file would pass
56 kB, `fits` is null and `fits_left_out` true (the build result has a warning), so the file
stays under the 64 kB the core copies. A reader ignores a `print` object it cannot use.
A build with pockets (section 4.6) also writes `print.pockets` (their declarations) and
`print.components` (the ids of the components it used), for the `gauge` op.

Other ops, each on a fresh process that loads `model.brep` and `model.parts.json` (both written
by a build with `outputs.brep`) from the job folder; the core copies the two files in:
- `measure`: `{"op": "measure", "queries": [{"kind": "bbox", "part": "lid"}, {"kind": "distance",
  "a": {...}, "b": {...}}, {"kind": "volume"}, {"kind": "section", "plane": "XZ", "offset": 0}]}`.
  `part` is optional (default: the whole model). A distance selector is `{"part": name}`,
  `{"part": name, "face": i}`, `{"part": name, "edge": i}` (indices as in mesh.json) or
  `{"point": [x, y, z]}`. Result: `results`, one object per query in order, each with `kind`,
  `ok` and either its values (`bbox`: `min`, `max`, `size`; `volume`: `volume`, `area`,
  `center_of_mass`; `distance`: `distance`, `point_a`, `point_b`; `section`: `area` and per part
  `area`, `regions`, `bbox`) or `error` when `ok` is false.
  An edge index is mesh.json's (section 3.4), which also numbers degenerate edges (the pole of a
  sphere, the tip of a cone, the corner patches of a filleted box); selecting one of those is
  answered with `ok: false` and an error that says it is a point, not a curve. A `distance`
  is measured between boundaries, so when a selector is a whole part the other side is first
  classified against that part's material: a point, face, edge or part lying inside it gives
  `distance: 0` and `"inside": true` (otherwise `"inside": false`), and two whole parts that
  share more than 0.001 mm3 give `distance: 0`, `"inside": true` and `"overlap_volume"` (mm3).
  `point_a` and `point_b` are still the closest boundary points.
- `export`: `{"op": "export", "formats": ["3mf", "stl", "step"], "parts": ["lid"]}` (`parts`
  optional, default all). Result `artifacts`: `step` = `export/model.step` (one assembly, parts
  named), `threemf` = `export/model.3mf` (one object per part), `stl` = `{part: "export/<part>.stl"}`.
- `views`: `{"op": "views", "views": [...], "size": [768, 768]}`, optionally with
  `"section": {"plane": "XY"|"XZ"|"YZ", "offset": 0}` to cut the model open first. Result
  `artifacts.views` as for a build.
- `docs`: `{"op": "docs", "query": "fillet edges", "limit": 5}` -> excerpts (signature + docstring)
  from the installed build123d, bd_warehouse and hotend_fit, so they always match the pinned
  versions. Needs no model. Result: `versions` and `docs`, a list of `{name, kind, module,
  signature, doc, score}`, best first.
- `prepare`, `gauge`, `calibrate`: section 3.5. `prepare` works on the two files of a build;
  `gauge` on them or on cards named in the request; `calibrate` needs no model.
- `references`: `{"op": "references", "context": {"references": [...], "material": {...}},
  "components": ["sample_phone"]}` reads the reference files and says what each holds, without
  building anything. Result `references`, one entry per listed reference: `name`, `file`,
  `status` (`ok`, `needs_selection`: the file holds several objects and the request names
  none or not exactly one, `unreadable`), `message`, and when the file could be read `format`,
  `bytes`, `sha256`, `unit`, `producer`, `notes` and `objects`: per object `index`, `name`,
  `plate`, `plate_name`, `size`, `triangles` and its `parts` (`name`, `role`, `geometry`,
  `triangles`, `size`). With status `ok`, `reference` holds the measures as in a build result.
  `components`: the full report of each named component with `envelope` and `rules` (every
  number with value, tolerance, provenance, confidence, source URL); `available_components`:
  the ids of this installation; `material`: the facts of `context.material` (section 4.8).
- `sweep`: `{"op": "sweep", "model_path": "model.py", "params": {...the current version's
  values}, "settings": [{"wall": 1.2}, {"wall": 1.6}], "context": {...}, "min_wall": 0.8,
  "budget_seconds": 180}` builds the script at each setting (1 to 16, each naming 1 to 8
  parameters over `params`) and stores nothing: no geometry, mesh or view is written. The
  worker parses `PARAMS` and refuses (`model_error`, `ParameterError`) a setting that names an
  unknown parameter or a value outside its declared range before any script code runs; then
  the model process runs the range sampler's `sweep` (section 4.5) and writes its report into
  its record (`model.build.json` `sweep`). No setting is started after `budget_seconds`, and a
  setting that would not finish before the job's own time limit is not started or is stopped
  at a per-setting deadline just under it (`time_limit`); those left are `not_run` and the
  report says `truncated`, so a slow sweep still returns a partial report. Result: `params`, `param_specs` and
  `sweep` = `{protocol, builds, truncated, checks, samples: [{params, status: ok | check_error
  | model_error | not_run, failed: [{check, message}], requirements: [{id, status, met,
  not_checkable, message}], parts: {name: [x, y, z]}}]}`, at most 16 failures and 32
  requirements per setting, messages at most 300 characters. The worker passes the report on
  only in that shape and the core bounds it again (`EngineResult::sanitize`). It is what the
  process that ran the script claims, so it is advice for the agent (`sweep_parameters`,
  section 5.1), never stored and never the check outcome of a version: a version is always
  built and judged by `build`.

### 3.3 Result

```json
{
  "protocol": 0,
  "status": "ok",
  "error": null,
  "stdout": "",
  "params": {"width": 90.0, "wall": 2.0},
  "param_specs": {"width": {"default": 80.0, "min": 40.0, "max": 200.0, "unit": "mm",
                             "label": "Width", "provenance": "style"}},
  "parts": [{
    "name": "box", "volume": 41250.3, "area": 30211.9, "mass_estimate_g": 51.2,
    "bbox": {"min": [0, 0, 0], "max": [90, 60, 40]},
    "solids": 1, "faces": 26, "is_valid": true, "is_manifold": true
  }],
  "checks": [{
    "id": "fits_build_volume", "status": "pass", "severity": "error",
    "message": "Fits the main nozzle's reach with 83 mm to spare in X.",
    "part": "box", "data": {}
  }],
  "artifacts": {"step": "model.step", "brep": "model.brep", "parts_index": "model.parts.json",
                "mesh": "mesh.json", "views": {"iso": "views/iso.png"}},
  "warnings": [],
  "timings": {"import_s": 2.0, "build_s": 0.41, "checks_s": 0.22, "tessellate_s": 0.10,
              "export_s": 0.02, "views_s": 1.1, "total_s": 4.2}
}
```

Every result also carries `op`. A build adds `stdout_truncated`, `parts[].color` (the colour used
in mesh.json and the views) and, when asked for, `artifacts.threemf` and `artifacts.stl` as in the
export op. `mass_estimate_g` is the solid (100 % infill) mass from the material's density, or null
when the material is unknown; the slicer gives the real figure. `warnings` lists artefacts that
could not be produced (for example a mesh over `limits.max_triangles`).

`status` is `ok`, `model_error` (the script raised or broke a rule; `error` has `type`, `message`,
`line`, and a traceback trimmed to the model script), `check_error` (built, but an `error`-severity
check failed; all artefacts are still written), `timeout`, or `internal_error`. Check `status` is
`pass`, `warn`, `fail` or `skipped`. Per-part checks give one entry per part; `data` holds the
numbers behind the message, with face indices as in mesh.json where a check points at faces.
`skipped` means the check had nothing to judge or lacked an input, and says which. A check
that broke while running (it raised) is not skipped: what it tests is unproven, so it is a
`fail` when its severity is `error` (the build is a `check_error`) and a `warn` otherwise,
with `data.could_not_run` = true and `data.exception` (type and text, for a bug report). The
message is a plain sentence that never holds the exception. The other checks still run.
Severity `error`: `valid_solid`, `manifold`, `single_solid_per_part`, `fits_build_volume`,
`min_wall`, `parts_do_not_intersect`, `fit_pairs`; severity `warning`: `overhangs`, `bed_contact`,
`small_features`, `parameter_ranges`. With `checks.verify_determinism` the model is built twice;
different volumes or bounding boxes are a `model_error` of type `NonDeterministicBuild`.
`timings` of a build also has `model_process_s` (the model process from its go signal to its
end); `build_s` is the model process's own figure and is informative only.

Only when the request sets `checks.contract`, a build result also has `questions`: the questions
for the user that the contract leaves open, a list of `{requirement, text, guessed_params}`
(empty when the model declares no contract or nothing in it is a guess; section 4.1). The check
id `contract` (severity `error`) appears only with `checks.contract` or when `checks.only`
names it. The results of the `prepare`, `gauge` and `calibrate` ops are in section 3.5.

A build whose request lists references also has `references`, one object per listed reference:
`name`, `label` (the object's name in the file), `kind`, `provenance` (`measured`), `source`
(`file`, `format`, `bytes`, `sha256`, `producer`, `unit`, `scale`, `object`, `object_index`,
`plate`, `plate_name`, `parts`, `file_bbox`, `objects_in_file`), `bbox`, `size`, `volume`,
`area`, `closed`, `hull_volume`, `vertices`, `triangles`, `silhouette` (`direction`, `area`,
`size`, `radius`, `center`), `notes` (what was left out, united or could not be closed),
`used` and `pockets` (per pocket of the model: `pocket`, `fit_class`, `shape`, `gap` = the
printed gap aimed for, `drawn_offset`, `size`, `placed`). A build that used a component has
`components`: per component its report (`id`, `name`, `kind`, `frame` = where its origin and
axes are, in words, `size`, `provenance`, `sources`, `numbers` = how many its data holds,
`features`, `used_features`, `proven`, `unverified` = the numbers the model relied on that
are not verified, `questions`) and `body` (the body's measures). The questions of the
components are also in `questions` (same three keys; `requirement` is `<component>_<feature>`,
or the bare `<component>` for the size of its body, cut to 32 characters; `guessed_params` is
empty), whether or not `checks.contract` is set.
None of the three keys is present in a build without references and components.

A build that made a pocket for a reference file asks about it by itself, contract or not
(`hotend_engine.checks.reference_fit.reference_questions`; same three keys, `guessed_params`
empty). Per such reference: `reference_<name>`, which says what was read (file, object, plate,
scale, size, how many other objects the file holds) and asks whether that is the thing at the
size the user has, to be confirmed with a caliper: a file says what was drawn, not what was
printed. And, when its pockets take a clearance that is a guess (every `cradle` and `stretch`
fit until its gauge has been printed, any class on an uncalibrated printer), one question
named after the first such pocket that gives the gap aimed for and asks for the ring gauge.
The second is left out when the contract has a `reference_fit` requirement for that
reference (section 4.1), which asks the same.

The check id `reference_fit` (severity `error`, section 4.6) reports only for a model that
made a pocket, used a component or took a reference fit class by hand; `checks.only` may name
it. A model is fully proven only when every `reference_fit` result is `pass`: an unverified
number of a component is a `warn`.

How a result is validated. The core reads `result.json` as hostile input: the types of
`hotend_core::engine` refuse an unknown field in every object they describe and a missing
one that is not optional (the list below says which are), and `EngineResult::sanitize` then
holds names, enumerations, paths and the counts of lists and maps to fixed bounds and refuses
the whole result (`malformed_result`) when one is broken. Free text in fields of their own
(messages, stdout, labels, question texts) is cut to a fixed length instead. JSON the core keeps
without a type (`checks[].data` except those of `keepout`, `results`, `candidates`, `measures`,
a gauge's `args`, a device's nested lists) is neither read strictly nor cut inside: it is held
to its field's size where one is named (`components`, `references`, `table`, the print facts)
and otherwise to the 4 MiB of the whole file alone, past which the result is refused.
`components` is kept with the version as a description of what it was designed
around and decides nothing (`proven` is a claim, the `reference_fit` check is the verdict):
at most 32 of them, ids used once, at most 64 features each, `sources`, `features`,
`unverified` and `body` as JSON that is only held to be objects, and the list to 512 KiB of
JSON text in all. `references` is at most 8 JSON objects in 512 KiB. A question's `requirement`
(the id of a contract requirement, `<component>_<feature>`, the bare `<component>` when the
size of its body is not verified, `reference_<name>` for a file, or the name of a pocket) is
`[a-z][a-z0-9_]{0,63}`; a list has at most 512 questions.
So that a field the worker starts to write and the core refuses is noticed in the tests, both
are held to one list of every field of every object and of the largest counts and sizes,
`crates/hotend-core/tests/fixtures/engine_result_fields.json`: the Rust test
`engine_result_contract` proves that the core accepts exactly the listed fields and exactly
the listed counts and sizes, and the Python tests check the results the worker writes in them
(`python/tests/result_contract.py`; not those of the `references` op, nor `hostile_no_file`, a
run whose names the core would refuse) and read the engine's
source for the keys it writes (`tests/engine/test_engine_result_contract.py`, which fails on a
write whose keys it cannot read). This is a net of the tests, not a guarantee: a field or a
count that no test produces and no scanned function writes is refused at run time. A new field
is added to the list, to the Rust type (with its bound), to the mirror in
`apps/desktop/src-tauri/src/wire.rs` and here in one change. The `references` op is outside the
list: the core never requests it.

Check ids (each is one function; a check that has nothing to judge or lacks an input reports
`skipped` with the reason): `valid_solid` (the kernel's validity test and a positive volume; a
`warn` when the kernel computes a flat face's area differently from the area inside that face's
own edges, by more than 0.5 % and 0.5 mm2. The triangles drawn for the face pick the candidates;
a candidate counts when the edges, followed as curves in space, disagree with the kernel as
well: `data.inconsistent_faces` (`face`, `kernel_area`, `drawn_area`: the area inside the
edges), `data.inconsistent_face_count` and `data.mesh_volume`, the volume the exported mesh
encloses. It happens after a chamfer or fillet over many short edges; the part's `volume`,
`mass_estimate_g` and the bed contact are then off by about that much), `manifold`,
`single_solid_per_part`, `fits_build_volume` (per nozzle reach; below), `min_wall` (sampled
thickness; the ray starts outside the surface, so a skin of any thickness, down to 0, is
measured), `overhangs` (area steeper than the material's limit without support), `bed_contact`
(flat area on the bed for the proposed orientation), `small_features` (below nozzle resolution),
`parts_do_not_intersect` (`data.overlaps[]` and `data.intended[]` hold `parts`, `volume` and
`intended`: the interference a verified joint between the two parts means to have, section 4.4),
`fit_pairs` (every declared mating pair has the clearance the fit resolver asked for, measured
on the solids; the joint can be assembled; the fit keeps its class across the printer's spread:
section 4.4), `parameter_ranges` (from the range sampler, when asked).

`fits_build_volume` and the plating share one rule for "fits the reach"
(`hotend_engine.prepare.reach_spare`): the piece's footprint, as it lies or a quarter turn
about Z further, is inside the X and Y reach less a margin on each side, and its height is
inside the Z reach. With `checks.orientation` = `as_modelled` the check holds the part's
bounding box as drawn against the full reach (margin 0): all that can be said to a caller
that does not run print preparation. With `prepared` it chooses, for every nozzle, the
orientation the `prepare` op chooses for a part given to that nozzle (section 3.5), turns the
part and holds it against the reach less `prepare`'s default `margin` of 2 mm. A nozzle then
`fits` exactly when a `prepare` with the default margin places the part on an empty plate: a
part too long as drawn that fits standing another way up passes and the message says how it
is printed; one that fits the bare reach but not the margin fails. A brim (`clearance`) is
not known to a build; a piece that has no room for it is reported by `prepare` with cause
`clearance`. `pass`: every nozzle fits; `warn`: some do; `fail`: none. `data`: `size` (as
drawn), `margin`, `nozzles` (per nozzle `nozzle_id`, `role`, `name`, `reach`, `fits`,
`needs_quarter_turn`, `spare` = [x, y, z] left over, negative where it is too large; with
`prepared` also `source`, `up`, `turned` and `size` as printed; when no way up fits, `spare`
and `turned` are those of the part as modelled) and, with `prepared`, `orientation` as for
`overhangs` (the main nozzle's).

`export/model.3mf` and `export/<part>.stl` are meshed so that no curve of a part deviates from
the exact surface by more than 0.01 mm, whatever its radius (a round hole comes out at most
0.005 mm tight per side at the middle of a facet).

### 3.4 mesh.json

The tessellation that three-cad-viewer loads without conversion: the object produced by
ocp-tessellate for the viewer (`shapes` tree with `vertices`, `triangles`, `normals`, `edges`,
per-face and per-edge ids, plus `bb`). The engine adds a top-level `"hotend": {"protocol": 0,
"parts": {"box": "<path in the shapes tree>"}}` so the UI can map a picked face to a part.

The form written is ocp-tessellate 3.6.3's *instanced* form, which three-cad-viewer 5.1.3 detects
and decodes itself in `Viewer.render(data, ...)` (`isInstancedFormat` / `decodeInstancedFormat`):

```json
{
  "instances": [{"vertices": {"shape": [834], "dtype": "float32", "buffer": "<base64>", "codec": "b64"},
                 "triangles": {...}, "normals": {...}, "edges": {...}, "obj_vertices": {...},
                 "face_types": {...}, "edge_types": {...},
                 "triangles_per_face": {...}, "segments_per_edge": {...}}],
  "shapes": {"version": 3, "id": "/model", "name": "model", "loc": null,
             "bb": {"xmin": -45, "xmax": 45, "ymin": -30, "ymax": 30, "zmin": 0, "zmax": 40},
             "parts": [{"id": "/model/box", "name": "box", "type": "shapes", "subtype": "solid",
                        "shape": {"ref": 0}, "state": [1, 1], "color": "#4c78a8", "alpha": 1.0,
                        "loc": [[0, 0, 0], [0, 0, 0, 1]], "renderback": false}]},
  "hotend": {"protocol": 0, "format": "ocp-tessellate-instanced-b64",
              "viewer": {"package": "three-cad-viewer", "version": "5.1.3"},
              "deviation": 0.1, "parts": {"box": "/model/box"}}
}
```

Read as hostile input. three-cad-viewer writes a leaf's `name` and `color` into the page as
HTML and builds selectors from the names, also for the tree it does not show. A reader must
therefore hold the `shapes` tree to exactly this form before the viewer gets it, and refuse
the file otherwise (the window does: `apps/desktop/src/hotend/viewport/mesh.ts`, `parseMesh`
and `checkScene`; a refused mesh shows as "This version has no 3D view."):

- the root is `name` `model`, `id` `/model`, without a `color`;
- every entry of `parts` is a leaf (no `parts` of its own) whose `name` is a part name
  (`^[a-z][a-z0-9_]{0,31}$`) or a ghost name (`^ghost\.[a-z][a-z0-9_]{0,31}(\.[0-9]{1,4})?$`),
  each used once, and whose `id` is `/model/<name>`;
- a `color` is `#RRGGBB`, or a list of them;
- every other text in the tree (`type`, `subtype`) is one word of at most 32 letters,
  digits, `_` and `-`, at any depth (so a `texture` or `material` that names a file or an
  address is refused);
- every key of `hotend.parts` is a part name and its value is `/model/<that name>`.

Buffers are base64 of little-endian `float32` (`vertices`, `normals`, `edges`, `obj_vertices`) and
`int32` (the rest), flat, not nested lists. Each part is one leaf `/model/<name>`; a leaf's
vertices are in its own frame and `loc` (position, quaternion xyzw) places them. Faces are in
the order of the part's B-rep faces (build123d's `shape.faces()`). Edges are in the
tessellator's order: every edge of the part in the kernel's map order, including degenerate
edges, which build123d's `shape.edges()` leaves out (`hotend_engine.tessellate.mesh_edges`
returns the B-rep edges in this numbering). `triangles_per_face` and `segments_per_edge` give
the per-face and per-edge ids that `measure` selectors use. When the counts of a part do not
agree with those orders the build result carries a warning. Equal geometry gives equal bytes.

Ghosts. Every reference a model placed with a pocket (section 4.6) is one more leaf of the
shape tree, `/model/ghost.<reference>` (`ghost.<reference>.<n>` for a second placement), with
`alpha` 0.35, `state` `[1, 0]`, no edges and a single face that holds all its triangles, in
world coordinates. `hotend.references` lists them: `{name, label, path, pocket, kind,
provenance, mesh, triangles, color, alpha}`; `mesh` is `exact`, `simplified to within x mm`
(a reference of more than 40,000 triangles is simplified for display only; the checks use the
exact mesh) or `convex hull`. `hotend.parts` still names the parts only, so a client can keep
ghosts out of picking and measuring. A ghost is display only: it is not in `model.brep`,
`model.step`, the STL or 3MF exports, `model.parts.json` or any plate. The rendered views
draw it in a pale colour, named `<label> (reference)` in the caption. `outputs.ghosts: false`
leaves ghosts out of both.

### 3.5 Print preparation, gauges and calibration

Three ops of the worker. None of them runs model code: they read the B-rep of a build, feature
cards as data, or numbers.

**`prepare`** turns a built version into plates.

```json
{"protocol": 0, "op": "prepare",
 "context": {"printer": {...}, "material": {...}, "calibration_path": "calibration.json"},
 "parts": ["box", "lid"], "copies": {"lid": 2}, "nozzles": {"lid": 0},
 "orientations": {"lid": {"up": "-z"}},
 "gap": 5.0, "margin": 2.0, "threemf": true, "views": ["iso", "top"], "view_size": [768, 768],
 "limits": {"max_triangles": 2000000}}
```

Only `op` is required, but `context.printer` must list the nozzles with their `reach` (the
reach is never assumed; a request without it is a bad request). `parts` (default all), `copies`
(per part, 1 to 64, default 1; at most 256 pieces in all), `nozzles` (per part, the `id` of the
nozzle that prints it; default the main nozzle), `orientations` (per part, `{"up": ...}` as in
`PRINT`: the user's choice, used before the model's), `gap` between pieces (mm, default 5),
`margin` kept to the edge of a nozzle's reach (mm, default 2), `threemf` (default true), `views`
(default `iso` and `top`; `[]` for none), `view_size`. The job folder holds `model.brep` and
`model.parts.json` of the build and, when the build had one, the same calibration file: the
cards are rebuilt with it to find the mating faces.

Orientation, per part. Candidates: the part as modelled, every direction in which a planar face
of at least 20 mm2 on the outside of the part can lie on the bed, and the orientation asked for.
Each is measured on the B-rep (planar faces exactly) and the tessellation (curved faces):
`contact` (planar area on the bed, mm2), `overhang` (area facing down more steeply than
`material.max_overhang_deg`, mm2), `height` (mm), `footprint` ([x, y] mm), `fit_overhang` (the
part of `overhang` on mating faces of declared fits), `weak_snaps` (feature cards whose bending
part, a snap rail, would stand upright so that the layers lie across it). The score, lower is
better, is `overhang + 3 * fit_overhang + 150 * (number of weak snaps) + tipping + height`,
with `tipping` = 100 for each unit by which `height / sqrt(contact)` exceeds 2, or 1000 when
under 20 mm2 lies on the bed. The lowest score wins, but the part stays as modelled unless
another orientation is better by more than 25. An orientation the request or the model's
`PRINT` asks for is used instead, unless nothing flat would lie on the bed or the part would
not fit the nozzle's reach that way; then the best-scoring one is used and the explanation says
so. Only candidates that fit the nozzle's reach less `margin` and `clearance` are scored
against each other (all of them when none fits): a part that does not fit as modelled is
printed the best way up that does, and the explanation says that as modelled it does not fit.
The weights are general FDM practice and not measurements; the terms of every candidate are
in the result so that the choice can be checked.

Plating: a deterministic shelf layout, not a nesting optimiser. The turned pieces, deepest
footprint first, are put in rows from the front left, `gap` apart, each inside the reach of the
nozzle that prints it less `margin` (so a piece of an auxiliary nozzle whose reach starts at
X = 20.5 never lies left of 20.5 + margin). A piece is turned a quarter turn about Z when it
only fits that way. When a row does not fit, a new plate is started; a piece that fits no empty
plate is listed in `unplaced` with the reason.

Result (besides `protocol`, `op`, `status`, `error`, `timings`):
- `orientations`: one per part: `part`, `source` (`as_modelled`, `face` or `preferred`), `up`
  (the direction of the part as modelled that points up), `turned` (words: `as modelled`,
  `upside down`, `onto its -X side`, ...), `score`, `terms` (`support`, `fit_support`,
  `weak_snaps`, `tipping`, `height`), `measures` and `as_modelled` (the measures above, chosen
  and as modelled), `preferred_by` (`the request`, `the model` or null), `overridden` (why an
  asked orientation was not used, or null), `candidates` (the eight best: `up`, `turned`,
  `source`, `score`, `fits_reach`, `contact`, `overhang`, `height`, `fit_overhang`,
  `weak_snaps`), `explanation` (one plain sentence), `copies`, `nozzle_id`.
- `plates`: one per plate: `index` (from 1), `size` ([x, y] of the bed area the nozzles reach),
  `items` (one per piece: `name` = the part's name, or `<part>.<n>` when it has copies; `part`,
  `copy`, `nozzle_id`, `quarter_turn`, `transform` = 12 numbers, a 3 x 4 matrix row by row
  with plate = R * model + t, and `bbox` `min`/`max` on the plate), and the artefacts, at fixed
  paths: `threemf` = `plates/<index>/plate.3mf` (one object per piece, named as the item, in
  millimetres, already in plate coordinates, meshed as the export op's 3MF) and `views` =
  `{view: "plates/<index>/views/<view>.png"}` (the pieces on the bed sheet).
- `unplaced`: `{name, part, copy, reason}` per piece that fits no plate.
- `checks`: the checks of the plate, as in a build result. Per part: `overhangs` and
  `bed_contact` judged as the part is printed; `fit_orientation` (severity `warning`, only for
  parts with declared fits: `warn` when mating faces would print on support or a snap is
  printed across its layers); `plate_fit` (severity `error`: every copy lies inside its
  nozzle's reach; `fail` when a piece could not be placed; `data`: `nozzle_id`, `placed`,
  `unplaced`, `plates`, `reach`, `margin`).
- `warnings`: among others that the pieces were split over several plates.
`status` is `ok`, or `check_error` when a piece could not be placed.

When the request's material is flexible (section 4.8), `prepare` adds the check
`material_nozzle` (severity `error`) per part: `pass` on the main nozzle, `fail` on any other,
because flexible filament is fed only by the direct-drive main nozzle of the X2D.

**Print settings.** `prepare` and `gauge` take four more fields, `stage`, `clearance`,
`anchor` and `keepouts`, and a request that names one of them gets the facts and the layout
of each plate back; docs/PRINT_SETTINGS.md, "Engine: print facts and plate files", is the
full text. How the core uses them:

- The core always prepares in two passes. Pass one: `"stage": "orient"`,
  `"clearance": 0`, `"anchor": "centre"`, `"margin": 5`, the printer's keep-out zones, no
  plate file and no views. The core reads the top-level `facts` (every piece, copies
  included) and works out the room each piece needs for its brim: the widest brim the print
  rules give, plus 1 mm, 0 without a brim. While the plan still waits for an answer (no
  plate chosen), the brim rules, which read only geometry and material, are run with each
  plate type in turn; refused facts give 0. Pass two: `"stage": "plate"` with every part's
  `up` from pass one pinned (a vector), that `clearance`, `"anchor": "centre"` (parts
  mid-bed), `"margin": 5` and the keep-out zones. The stored preparation keeps the
  orientations of pass one (with their reasons) and everything else of pass two.
- The keep-out zones are printer data: `[[printer.keepout]]` rows of
  `crates/hotend-printer/profiles/bambu.toml` (`id`, `x`, `y`, `reason`, `verified`,
  `source`), read by `PrinterProfile::keepouts`. The X2D has one, `start_load_line`, X 55 to
  145 and Y 0 to 8 mm around the load line the start routine draws (`verified = false`).
- The result is hostile input like the rest: `stage` must be `all`, `orient` or `plate`;
  every facts object must be an object, and all facts of one result together at most 8 MiB
  of text; `plates[].layout` is `{clearance (0 to 20), margin (0 to 1000), anchor
  (front_left or centre), offset ([x, y] within 1000)}` and nothing else;
  `unplaced[].cause` is `clearance`, `keepout`, `too_large` or `no_reach`; the `keepout`
  check has severity `error`, no part and `data` of exactly `{clearance, zones: [{id, min,
  max}], collisions: [{piece, plate, zone}]}` with ids and piece names of the fixed patterns.
  Anything else makes the whole result invalid. The facts themselves are read fact by fact
  only where a rule reads them (`PlateFacts::from_value`): facts that are refused, or that
  name another number of pieces than the plate holds, give the plan `facts_invalid`.
- `Preparation` gains `clearance_mm`, `margin_mm`, `keepouts` and `plans` (per plate `{plate,
  sha256, blocked: {code, sentence} or null, settings_source}`, the plans when the version was
  prepared). A preparation without `clearance_mm` (made before) or for other keep-out zones
  is laid out again before a plan or a slice is made from it, as is one for another printer or
  material. A plan whose brim needs more room than the layout kept lays the version out again
  once; if it still does not fit, the slice fails with `brim_does_not_fit`.
- Not built: the two passes for `gauge` (the core's gauge export still asks in the old way),
  and asking again with a narrower brim when a piece is unplaced for `clearance`.

**`gauge`** exports the test gauges of feature cards as a plate of their own.

```json
{"protocol": 0, "op": "gauge", "context": {...}, "nozzle_id": 1,
 "cards": [{"card": "peg_hole", "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}}],
 "gap": 5.0, "margin": 2.0, "threemf": true, "views": ["iso", "top"]}
```

With `cards` (1 to 32 of `{card, args, name?}`: a card kind and its public arguments) the
gauges of those cards; without it the gauges of every card the built model in the job folder
uses (`model.parts.json`), equal cards once. The gauges are drawn with the clearances of the
request's calibration, like the product. Result: `gauges` (per card: `card`, `name`, `args`,
`parts` = the names of its two objects on the plate, `<name>.<half>`; `size`; `dimensions` =
what to measure with a caliper: `id`, `half`, `object`, `feature` (`hole` = an inside size,
`peg` = an outside size), `drawn` (mm) and `what` (words); `clearances`: `pair`, `fit_class`,
`gap`, `provenance`), `plates` and `unplaced` as for `prepare`, `warnings`.

A reference fit has a gauge too. `{"card": "pocket", "name": "ring", "args": {"reference":
"part", "fit": "cradle", "shape": "cylinder", "direction": "+z"}}` (the reference is a
name of `context.references` or a component id) gives one piece, `<name>.ring`: a ring 5 mm
tall with a 2 mm wall whose inside is the pocket's outline. For a clearance fit the thing
must drop through it; for a `stretch` fit it is a thin band of the case's rim that must
stretch over the thing and stay on. The entry has the keys of a card's gauge: `card` =
`pocket`, `name`, `args`, `parts` (one name), `size`, `dimensions` (the two inside widths;
`what` says what to try, in words) and `clearances`. Without `cards`, the pockets of the
built model in the job folder get their gauges as well.

**`calibrate`** turns readings of a printed gauge into the updated calibration table.

```json
{"protocol": 0, "op": "calibrate", "context": {...}, "nozzle_id": 1,
 "measured_at": "2026-11-17T09:00:00Z",
 "readings": [{"feature": "hole", "drawn": 6.49, "measured": 6.31},
              {"feature": "peg", "drawn": 5.958, "measured": 6.0}]}
```

`readings` (1 to 64) pair a gauge dimension's `feature` and `drawn` with what the caliper
`measured`. `measured_at` is required: the engine reads no clock, the caller says when the
gauge was measured. `context.printer` and `context.material` say whose entry it is
(`nozzle_id` default the main nozzle), `context.calibration_path` the table to update (absent:
a new table). Result: `table` (the whole new table, section 4.2; the caller stores it, the
engine writes no file), `changes` (per changed quantity: `quantity`, `before`, `after`,
`sigma`, `readings`) and `entry_provenance`. Readings that give an implausible offset (beyond
1 mm) are a bad request.

## 4. Proven Fit

### 4.1 Contract

`CONTRACT` in a model script (or produced by the agent before the script) is the product's
checkable promise. It is a pydantic model in `hotend_fit.contract` (`parse_contract(raw)`):

```json
{
  "protocol": 0,
  "name": "Desk cable box",
  "parts": ["box", "lid"],
  "requirements": [
    {"id": "holds_power_strip", "kind": "cavity_min", "part": "box",
     "size": [250, 55, 40], "provenance": "measured"},
    {"id": "lid_snaps", "kind": "fit", "card": "snap_lid", "between": ["box", "lid"],
     "fit_class": "snap", "provenance": "calibrated"}
  ]
}
```

Every number has a provenance (`measured`, `standard`, `calibrated`, `guessed`, `style`). A
requirement whose numbers are `guessed` is shown to the user as a question before the first print.

Requirement kinds (`kind` selects the fields; unknown keys are rejected): `cavity_min` and
`outer_max` (`part`, `size` [x, y, z]), `wall_min` (`part`, `value`), `dimension` (`part`,
`name`, `value`, `tolerance` default 0.1), `fit` (`card`, `between` two different parts,
`fit_class`), `reference_fit` (`reference`: the name of a reference of the request or the id
of a component; optional `fit_class`: `cradle`, `stretch`, `loose`, `slide` or `snug`),
`standoff` (`part`, `reference`, `side`: `+x` ... `-z` in the reference's own frame, `value`:
the least stand-off in mm; optional `at_most`, `feature`: a proud feature of a component,
`region`: two corners `[[a0, b0], [a1, b1]]` across the side, the lower first, at least 1 mm
apart, given as the two coordinates other than the side's axis, in the reference's frame).
Every requirement has `id` (unique, `[a-z][a-z0-9_]{0,31}`), `provenance`, and
optionally `params` (names of the model's PARAMS its numbers come from) and `note`. Parts named
by a requirement must be listed in `parts`.

`open_questions(contract, param_specs)` returns what to ask the user: every requirement tagged
`guessed`, and every requirement that names a parameter whose PARAMS provenance is `guessed` (or
that the model does not define). Each question has `requirement`, `text` and `guessed_params`.

The engine reads `CONTRACT` from the script and validates it before the script runs (section
2). With `checks.contract` every requirement becomes one result of the check `contract`
(severity `error`, `data.kind` = `requirement`, `part` = the part when it names exactly one):
- `cavity_min`: a cuboid of the size, axis-aligned as the part is modelled, fits in the free
  space inside the part's bounding box without touching the part (decided on the tessellation:
  the free space eroded by the cuboid, tried 0.01 mm smaller than asked, is not empty). A boss
  in a corner therefore counts. Space under a part that stands on feet counts too: the
  requirement is meant for containers. When it does not fit, `data.fits_at_most` says how much
  does along each axis with the other two sizes kept.
- `outer_max`: the bounding box is no larger than the size, axis by axis (`data.actual`).
- `wall_min`: the `min_wall` measurement with the requirement's value.
- `fit`: a card of the named kind joins the two parts, one of its pairs has the named fit class,
  and that card's pairs and joint passed `fit_pairs` (`data.cards`).
- `reference_fit`: the model made at least one pocket for the named reference (with the
  named fit class, when one is given) and every such pocket passed the `reference_fit` check
  (section 4.6): room all round and a clear way in and out (`data.pockets`: `pocket`,
  `fit_class`, `status`, `target`, `clearance_provenance`). No pocket is not met.
- `standoff`: how far the part stands beyond the real thing on the named side, measured on
  the meshes in the frame of the pocket the model placed for the reference. The thing is not
  taken where it was drawn: it is first pushed towards that side as far as the parts let it
  go (`data.travel`: rays from every corner of the thing to the parts and from the parts'
  corners back to the thing, started 0.001 mm inside the thing's outline so that a surface
  it only lies on does not stop it; exact where a corner meets a face, within the size of
  the facets where two edges meet). `data.standoff` = `part_face` - `thing_face` - `travel`:
  the part's outermost point on that side, the thing's face there (the body's, or the face
  of `feature` where it stands proud), and the travel. A phone case's rim over the glass is
  `side: "+z"`; the phone slides onto a rim that leans over its rounded edge, and only what
  is left counts. With `region`, rays come in from outside on a 1 mm grid over the rectangle
  and the part's first surface under each is its face there: `data.standoff` is the
  thinnest place, `data.standoff_max` the thickest, `data.open` the number of places with no
  material (not met when any is open), and `at_most` bounds the thickest (a case's back of
  uniform thickness). Not met when the model placed no pocket for the reference, when
  nothing holds the thing on that side, or outside `value` ... `at_most`.
- `dimension`: not measured (a dimension named in words has no location on the solid): always
  not checkable.
`data` also has `requirement` (the id), `requirement_kind`, `provenance`, `params`, `parts`,
`met` (true, false, or null when not measured) and `question`. Status: `pass` when met, `fail`
when not. A requirement the check cannot evaluate (a `dimension`, a kind it has no measurement
for, a part without a usable mesh, too many triangles for the free space) is never left out
and never passed: it is reported as not checkable, status `skipped` with `data.not_checkable`
true, the reason in `data.reason`, and a message that starts with "Not checkable:" and gives
the reason (resting on a guess, a `warn` with the same flag, reason and prefix). The gateway
counts and lists these apart from other skipped checks (`N not checkable`, `- NOT CHECKABLE`
lines, section 5.1). A requirement that rests on a guess (one of
the `open_questions`) is never more than a `warn`, met or not: a guess proves nothing either
way, so the result carries the question in `data.question`, the message says whether the
geometry meets the guess, and the same questions are the result's top-level `questions`.
A `fit` requirement tagged `calibrated` and any `reference_fit` requirement also rest on a
guess when the clearances behind them are guessed: on a printer that was never calibrated
the resolver hands out guessed clearances, so a press fit tagged `calibrated` is a `warn`
with the question "this printer has not been calibrated ... print the fit gauge" and never a
plain `pass`. The tag says where the number should come from, not that this printer has
been measured. (A `fit` tagged `standard`, a bought screw or insert, keeps its own verdict.)
A model without a `CONTRACT` gives one `skipped` result.

### 4.2 Calibration table and fit resolver

`calibration/<printer-id>.json`: per material and nozzle, measured offsets from the fit coupon:
`hole_offset`, `peg_offset`, `xy_shrink`, `z_shrink`, `min_clearance`, each with `value`, `sigma`,
`provenance` and `measured_at`. A missing table falls back to conservative defaults with
provenance `guessed`.

```json
{"protocol": 0, "printer_id": "bambu-x2d", "entries": [
  {"material": "pla-basic", "nozzle_id": 1, "nozzle_diameter": 0.4,
   "hole_offset":   {"value": 0.06,  "sigma": 0.01,   "provenance": "calibrated", "measured_at": "2026-11-02T10:00:00Z"},
   "peg_offset":    {"value": 0.02,  "sigma": 0.01,   "provenance": "calibrated", "measured_at": "2026-11-02T10:00:00Z"},
   "xy_shrink":     {"value": 0.002, "sigma": 0.0005, "provenance": "calibrated", "measured_at": "2026-11-02T10:00:00Z"},
   "z_shrink":      {"value": 0.001, "sigma": 0.0005, "provenance": "calibrated", "measured_at": "2026-11-02T10:00:00Z"},
   "min_clearance": {"value": 0.08,  "sigma": 0.01,   "provenance": "calibrated", "measured_at": "2026-11-02T10:00:00Z"}}]}
```

Lengths are per side (on a radius or on one face, never on a diameter). `hole_offset`: how far a
printed inside surface sits inwards of where it was drawn, (drawn diameter - measured) / 2.
`peg_offset`: how far a printed outside surface sits outwards, (measured diameter - drawn) / 2.
`xy_shrink`, `z_shrink`: ratios, measured = drawn * (1 - shrink). `min_clearance`: the smallest
printed gap between separately printed surfaces that still assembles. `material` is the request's
`material.id` and `nozzle_id` the id of the printer's main nozzle. Loading is strict: unknown keys,
numbers written as strings, non-finite or implausible values (offsets beyond 1 mm, shrinkage
beyond 5 %, a negative `min_clearance`), a `measured` or `calibrated` value without `measured_at`,
duplicate entries, a file over 256 kB or a `printer_id` other than the request's printer make the
whole table unusable (`CalibrationError`; the engine reports it as `internal_error`). A missing
file, a table without an entry for this material and nozzle, or an entry whose `nozzle_diameter`
differs from the fitted main nozzle's by more than 1e-6 mm (offsets measured with one nozzle
size say nothing about another), gives the defaults (for a 0.4 mm
nozzle: `hole_offset` 0.08 +/- 0.04, `peg_offset` 0.03 +/- 0.04, `xy_shrink` and `z_shrink`
0.003 +/- 0.002, `min_clearance` 0.10 +/- 0.03; offsets scale with the nozzle diameter), all
`guessed`.

`fit.clearance(fit_class, nominal, ctx) -> Clearance` is the only source of clearances in a model.
The engine builds `ctx.fit` with `hotend_fit.fit.resolver_for(context)`, where `context` is the
request's `context` object as a dict with `calibration_path` made absolute (or null). The resolver
has `clearance(fit_class, nominal)` and a list `declared_fits` that the feature cards append to;
the `fit_pairs` check reads it after the build and reports `skipped: no declared fits` when it is
empty. Without hotend_fit installed, any use of `ctx.fit` is a `model_error`.
Fit classes: `press`, `snug`, `slide`, `loose`, `snap`, `thread_tap`, `thread_free`.

What is enforced. `fit_pairs` verifies the mating faces a feature card made, and nothing else.
A card face that does not have the clearance this build's resolver issued for it is a `fail`
(a card takes no clearance argument, and a card face the script moved, widened or cut afterwards
fails its pair), and a clearance taken from the resolver without a card is one `warn`
(section 4.4). A joint the script makes by hand with a typed gap is never verified, in a model
with or without cards. Unless the request asks, nothing looks for mating surfaces outside the
declared pairs, so such a build is `ok` without a word (`parts_do_not_intersect` reports it only
when the two parts overlap). With `checks.undeclared_joints` it is reported as a `warn`
(`data.kind` = `undeclared`, section 4.4): the joint is pointed out, its gap is still not
checked against anything. "No clearance is ever typed" is therefore a rule of the
agent's prompt for everything a card did not make, not a check.

Calibration from a gauge. `hotend_fit.calibration.calibrate_from_gauge(path, readings, *,
measured_at, printer_id, material, nozzle_id, nozzle_diameter) -> (table, changes)` (the
`calibrate` op, section 3.5): a `hole` reading gives `hole_offset` = (drawn - measured) / 2, a
`peg` reading `peg_offset` = (measured - drawn) / 2; each quantity that was read becomes the
mean of its readings (rounded to 1e-6 mm) with provenance `measured` and the caller's
`measured_at`. Its `sigma` is the standard deviation of the readings, at least 0.02 mm, or 0.04
mm for a single reading (one print says where this print landed, not how much the printer
varies). The entry for this material and nozzle is replaced or added (an entry measured with
another nozzle diameter is started again from the defaults); quantities that were not read and
all other entries are kept. A gauge measures the two offsets only: `xy_shrink`, `z_shrink` and
`min_clearance` stay as they were, so an entry that started from the defaults still has guessed
quantities and `offsets_provenance` stays `guessed` until they are measured too. The function
writes no file and reads no clock. The next resolver that loads the table draws every hole
`after - before` wider per side, and every peg thinner by its change.

The drawn gap is `target + hole_offset + peg_offset`, per side, where `target` is the printed gap
the class aims for (`base + per_mm * nominal`; the table and where each number comes from are in
`hotend_fit/fit.py`; clearance classes are never aimed below `min_clearance`). `nominal` is the
nominal size of the mating feature in mm. Optional keywords: `axis="z"` (a gap along the build
direction: no hole or peg offset), `printed_hole=False` / `printed_peg=False` (that side is a
bought part; then shrinkage is compensated instead), `datasheet_gap` (a maker's own per-side gap
replaces the class target; used by cards that read it from a standards table). A `Clearance` has
`fit_class`, `nominal`, `axis`, `gap`, `hole_delta` (added to the hole's radius), `peg_delta`
(taken off the peg's radius; `gap = hole_delta + peg_delta`), `target`, `target_source` (`class`
or `datasheet`), `printed_hole`, `printed_peg`, `offsets_provenance` (`calibrated` only when
every quantity of the calibration entry was measured, else `guessed`), `target_provenance`
(`guessed` for the target of a fit class, until a gauge result exists; `standard` for a maker's
datasheet gap), `provenance` (the weaker of the two: `calibrated` only when the offsets are
calibrated and the target is not a guess), `extreme` and `index`. Every clearance handed out is
kept in `resolver.issued`; cards register in `resolver.cards`.

**Material extremes.** A build draws the model for the printer as calibrated. What that drawing
becomes on another printer of the same spread is computed, never redrawn:
`resolver.printed_gap(clearance, extreme)` is the gap per side the drawn pair prints with on the
`tight` printer (every calibration value at + k sigma) or the `loose` one (at - k sigma), `k` =
`context.fit_k`, default 2: `gap - (hole_offset + shrink)` for a printed hole and
`- (peg_offset - shrink)` for a printed peg, with the extreme's values (along Z only shrinkage).
`resolver.robustness(clearance)` compares both with the window of the fit class (printed gap per
side, mm): `press` -0.15 to -0.01, `snug` 0 or more, `slide` 0.05 or more, `loose` 0.15 or more,
`snap` -0.45 to -0.15, `thread_tap` -0.15 x nominal to -0.05 x nominal, `thread_free` 0.05 or
more, a datasheet gap +/- 0.10 around the maker's value (the table is in `hotend_fit/fit.py`).
The windows are guesses like the class targets. The `fit_pairs` check reports the outcome
(section 4.4). `context.fit_extreme` (`nominal` by default, `tight`, `loose`) still exists for
experiments: it draws the model compensated for that extreme printer; no check and no sampler
uses it.

### 4.3 Feature cards

A card builds both mating halves from one definition, exposes named ports (location + direction
+ keep-out volume), and can emit its own small test gauge:

```python
card = cards.snap_lid(opening=(80, 60), wall=2.0, ctx=ctx)
box = card.apply_a(box_part, at=port)    # cuts/adds on the first part, returns the new part
lid = card.apply_b(lid_part, at=port)    # the mating half
card.gauge()                             # -> Part: the smallest print that tests this fit
card.checks()                            # -> list of fit_pairs checks the engine runs
```

Cards: `snap_lid`, `peg_hole` (press or slide), `screw_boss` (self-tapping screw or heat-set
insert, standard sizes from bd_warehouse).

Rules: a card takes no clearance argument; `at` is where the card's frame sits in the model (a
build123d `Location` or `Plane`, or an `(x, y, z)` position; default the origin) and may differ
between the two halves (parts shown apart): the fit is measured with the halves brought together.
A card is one joint: each half is applied once, after the body of its part is finished, and the
part is not moved or reshaped afterwards. Several cards between the same two parts must place
them alike: `at` of half A times the inverse of `at` of half B is the same for all of them
(within 1e-6 mm and 1e-9 rad). A card that cannot be built raises `CardError` (a
`model_error`) naming the dimension to change; `snap_lid` raises it when pushing the lid on
would bend the rail by more than the material family takes (PLA 2 %, PETG 4 %, the lowest for
an unknown family; the message names the depth that is enough), and its rail is 0.8 x wall but
never more than 2 mm thick. `card.ports` maps the two half names to
`Port(name, location, direction, keep_out, note)` in the card's frame; `keep_out` is a solid in
the port's own frame (`at * port.location * keep_out` in the model): room the host of that half
must leave free of material. `card.gauge()` is one
compound of both halves laid flat on the bed; `card.gauge_set()` gives them as named parts
(`gauge_<half>`) with the fit pairs that verify them and `dimensions`, what to measure on the
print with a caliper (`card.gauge_dimensions()`: `id`, `half`, `feature` = `hole` or `peg`,
`drawn`, `what`). A gauge is built with the product's own clearance records. `card.flex_axes()`
names, per half, the direction along which a part that must bend is long (the rails of
`snap_lid`), and `card.feature_boxes()` the bounding boxes of the solids the card added or cut;
print preparation and the undeclared-joint search use them. `cards.list_cards()` lists name, call signature, half names and documentation.

| Card | Half A / half B | Arguments | Fit pairs (class) |
| --- | --- | --- | --- |
| `snap_lid(opening, wall, *, ctx, depth=8.0)` | `box` (grooves in the long inner walls) / `lid` (four rails under the lid plate, the long ones with a snap bead) | inner size of the opening at the rim, wall thickness | on each side `wall` (slide), `snap` (snap, negative), `seat` (snug), `end` (loose), `locate` (slide): 10 |
| `peg_hole(diameter, length, fit="slide", *, ctx, through=False)` | `hole` / `peg` | `fit` is `press`, `snug`, `slide` or `loose`. The peg's tip is chamfered and the hole's mouth is bevelled at 45 degrees (0.08 x diameter, at most 0.4 mm): a lead-in that also keeps the mouth open when it is the first layer on the bed. The fit is the straight part below it | `radial` (`fit`), `relief` at the bottom of a blind hole (loose, z) |
| `screw_boss(screw="M3", kind="self_tap", *, height=8.0, plate=2.4, ctx, insert=None, maker="CNCKitchen")` | `boss` / `plate` (the screw's through hole) | `screw`: `M2`..`M10` (bd_warehouse) or `ST2.2`..`ST4.8` (ISO 1478 / ISO 7049 table); `kind`: `self_tap` or `heat_set` (bd_warehouse HeatSetNut, makers CNCKitchen and ruthex) | `thread` (thread_tap) or `insert` (the maker's hole) and `insert_depth` (loose, z); `screw` (thread_free). Measured against the bought part's standard surface |

### 4.4 The `fit_pairs` check

Each entry of `declared_fits` is a `hotend_fit.verify.FitPair`: the card and instance name, the
pair's name, its `Clearance`, and for each side either a probe (a point on the mating face and the
outward normal there, in the card's frame) or, for a bought part, the surface itself.

The cards are applied in the model process; the worker verifies them without having run the
script. The model process hands over `fits` in its record (section 3.1): `{"cards": [{"card":
kind, "name", "args": the card's own public arguments, "frames": {"a", "b"}}], "loose":
[{"fit_class", "nominal", "axis"}]}`, a frame being the 12 numbers of a rigid 3 x 4 placement
or null for a half that was not applied, `loose` the clearances taken without a card. The
worker rebuilds every card from its kind and arguments on its own resolver (its own calibration,
clearances, probes and reference solids) and places it with the declared frames. A declaration
carries no clearance, probe or tolerance. One that cannot be rebuilt (unknown kind, arguments
that are not the card's, a frame that is not a rigid placement, more than 256 cards) is a `fail`
with `data.kind: "declaration"`.

All results have the id `fit_pairs`, `part` null, and are told apart by `data.kind`:

- `pair`, one per mating pair. The faces are looked up on the finished parts and the pair is
  measured with the kernel's exact distance between the two faces; the measured clearance is the
  component of the vector between their closest points along the printed face's outward normal
  (negative = interference). `data`: `card`, `instance`, `pair`, `fit_class`, `nominal`,
  `expected`, `measured`, `difference`, `distance`, `tolerance` (0.001 mm), `parts` (the part
  found for each side, null for a bought part), `points`, `provenance`, `offsets_provenance`,
  `target_provenance`, `extreme`, `reason`. `fail`: the clearance differs by more than the
  tolerance; a mating face is not where the card put it (within 1 mm it is still measured and
  reported, beyond that it counts as removed); the mating face is not whole (its area on the
  part is smaller than on the card's own reference half, or a round hole or pin is not a
  complete cylinder: the feature runs off its host, is blocked or was cut away); a half was
  never applied; or the pair's clearance was not issued by this build's resolver.
- `joint`, one per card: can the two parts be put together as the card says. `data`: `card`,
  `instance`, `parts` (the host of each half), `problems`, `keep_out` (mm3 of material in each
  half's keep-out), `in_place`, `placement_shift`, `assembled_overlap`, `intended_overlap`,
  `disagrees_with`. `fail`: a half was never applied or both halves are on one part; a host has
  material in the keep-out of its half; cards between the same two parts place them differently;
  the parts are drawn touching while the halves of the card are not in the same place; or, with
  the second part moved to where the card puts it on the first, the two share more volume than
  the card's own interference (`intended_overlap`, above 0 only for a card drawn with a negative
  gap) plus 0.001 mm3.
- `robustness`: what the drawing becomes at the material extremes (section 4.2). All clearances
  inside their class window: one `pass`. A clearance outside it: `fail`, one per clearance, when
  the calibration entry is measured; one `warn` for all of them when it is guessed (on a guess
  nothing is proven either way; the message says to print the gauge or calibrate, and in
  particular that a press, snap or tapped fit may not hold). `data`: `k`, and per clearance
  `fit_class`, `nominal`, `drawn`, `target`, `printed` (`tight`, `loose`), `window`, `pairs`,
  `offsets_provenance`, `target_provenance`, `reason`.
- `unverified`, one `warn`: clearances were taken from the resolver without a card, so nothing
  verifies them.
- `declaration`: see above.
- `undeclared`, only with `checks.undeclared_joints`: one `warn` per pair of parts that have a
  joint no feature card made. Found on the tessellation: points are spread (0.5 mm apart) over
  the faces of one part that lie near the other, a ray is cast from each along the face's
  outward normal, and a point counts when the ray meets a face of the other part that looks
  back at it (normals opposed within about 45 degrees) at most 1 mm away. A pair of parts is
  reported when such points stand for at least 10 mm2 and lie outside the solids that declared
  cards added or cut (`Card.feature_boxes`, widened by 1 mm). `data`: `parts`, `area` (mm2),
  `gap_min`, `gap_max`, `gap` (the mean over the face pairs, weighted by area), `reach` (1.0), `faces` (up to 12 of
  `{a, b, area, gap, contact}`, face indices as in mesh.json). Not reported: planar faces that
  only touch in one direction (a lid resting on a rim; faces in contact are reported, as
  `touching`, when the two parts hold each other from two opposite directions), parts drawn
  apart from each other, and anything inside a declared card's own solids. It is a detector,
  not a measurement: gaps of planar faces are exact, of curved faces within the tessellation's
  deviation (about 0.1 mm), and the gap is not compared with anything. When a part has no
  usable mesh the search is reported as one `skipped` result with this kind.

`skipped` (no `data.kind`): nothing declared, or the declared entries are not hotend_fit pairs.
A message of a `pair` says "offsets calibrated, target guessed" when only the offsets were
measured; no pair of a fit class is called calibrated before its gauge has been printed.
An interference fit whose joint passed may be drawn assembled: `parts_do_not_intersect` allows
the volume the joint intends between its two parts, and nothing beyond it.

### 4.5 Range sampler

`hotend_fit.sampler.sample_ranges(model_path, context, *, seed=0, max_corners=16,
random_samples=8, sweep=9, k=2.0, min_wall=0.8, checks=<the error-severity checks>, ranges=None,
max_builds=300, refine_steps=3) -> dict` builds the model in-process, once per setting, as the
drawing that would be printed (never redrawn for an extreme printer; the `fit_pairs` check
evaluates every fit of that drawing at both material extremes, `k` sigmas out). Settings, in
order: the defaults; for each parameter a sweep with the others at their defaults (every value of
a `count` parameter with up to 16 values, `sweep` evenly spaced values otherwise); the corners
of the declared ranges (all up to `max_corners`, otherwise a seeded choice that always holds the
all-low and all-high corners); a seeded Latin hypercube inside the ranges (at least
`random_samples` points and four per sampled parameter; none when `random_samples` is 0). A
setting passes when it builds and no error-severity check fails.

Repeatability, stated with its limits. Which settings are built depends only on the model's
`PARAMS`, the arguments and `random.Random(seed)`. Before every build the sampler empties the
caches the libraries keep in a process, as the tessellator does before every tessellation
(`hotend_engine.tessellate.clear_library_caches`: ocp-tessellate's bounding boxes, which are
keyed on a shape's address and would otherwise be handed to a later shape at the same address,
and for the sampler also its mesh cache), so a build does not depend on the builds before it.
The same model, context and seed then give the same report, byte for byte as JSON, when the
script is itself deterministic and the run is on the same machine with the pinned libraries:
the tests check that twice in one process and across two processes. It is not promised across
platforms or library versions (a mesh-based check near its threshold can fall the other way),
and it does not cover a script that keeps state in a module global or changes a library: the
script shares the sampler's process, so such state carries from one setting to the next.

Clamping: each parameter's range is cut to the run of passing sweep values around its default,
the bound refined by `refine_steps` bisections towards the first failing value (so a failing
band inside a range, or one bad count, ends the range there). A failing setting still inside the
box is attributed before anything is cut: its parameters are put back to their defaults one at a
time (then cumulatively), and the cut is made on a parameter whose change makes it pass. Then
the corners of the clamped box and a fresh stratified set inside it are built; this repeats until
they all pass.

Report: `status` (`pass`: every sample passed and every declared fit is proven at the extremes;
`warn`: some samples failed, see `passed_ranges`, or nothing failed but the calibration is
guessed so the fits are unproven, or the build budget ran out; `fail`: the defaults fail),
`message`, `model`, `seed`, `k`, `extremes`, `min_wall`, `checks`, `skipped_checks` (checks
that had nothing to check or could not run), `calibration` (`source`, `provenance`), `fits`
(`declared`, `proven`, `unproven_settings`, `example`), `parameters` (per name: `default`,
`unit`, `sampled`, and when sampled `declared`, `passed`, `clamped`, `swept` = values in its
sweep, `values_checked` = distinct values of it that were built inside the reported box),
`samples` (`settings`, `builds`, `passed`, `failed`, `by_kind` with the kinds `default`,
`sweep`, `corner`, `inner`, `refine`, `attribute`, `confirm_corner`, `confirm_inner`,
`declared_corners_checked`, `declared_corners_total`), `failures` (per failing check, or
`build:<ErrorType>` when the script raised: `count`, `box` = the range of each parameter over
the failing settings, `separated_by` = parameters whose value alone splits failing from passing
settings, `example`), `failed_settings` (`params`, `kind`, `failed` = the failing check ids),
`passed_ranges` (the suggested clamped slider ranges: a box that contains the defaults, holds no
failing sample, and whose corners and inner points were built and passed; null when the defaults
fail), `clamped`, `verified`, `truncated` (the build budget ran out; `verified` is then false),
`rounds`. The wording is "checked", never "guaranteed": a fault narrower than the spacing of the
samples can be missed.

The sampler runs the script in its own process, so its report is advice for choosing ranges,
not a verdict (a script could influence it); the engine's `parameter_ranges` check and
`checks.sample_ranges` do not call `sample_ranges`. A caller runs it itself, inside the sandbox.

`hotend_fit.sampler.sweep(model_path, context, *, base, settings, min_wall=0.8,
deadline=None, hard_deadline=None, clock=time.monotonic) -> dict` is the same runner pointed at settings a caller
chose: every entry of `settings` (1 to 16) overrides some values of `base`, and each setting
is built once, in order, the caches emptied before each build, and judged by the
error-severity checks and the model's `CONTRACT` (as a build of the product judges it). No range
is clamped. Per setting: `params` (the overrides), `status` (`ok`, `check_error`,
`model_error`, or `not_run` when `deadline` had passed before it started, or when it would
not finish before `hard_deadline` or was stopped at it, with failure `time_limit`), `failed` (check id
or `build:<ErrorType>`, with the message), `requirements` (`id`, `status`, `met`,
`not_checkable`, `message`) and `parts` (the size of every part). The engine's `sweep` op
(section 3.2) runs it in the model process; the agent reaches it through `sweep_parameters`.
The same caveat holds: it is advice, not a verdict.

### 4.6 References and pockets

A reference is a real thing a printed part is fitted to: a reference object (a file the
request lists, `ctx.references[name]`, provenance `measured`) or the body of a bought
component (`ctx.component(id).reference`, section 4.7). `hotend_fit.reference.Reference`:

- measures in mm, in the reference's own frame: `bbox`, `size`, `center`, `volume`, `area`,
  `closed`, `hull_volume`; `kind` (`rigid`, `soft`), `provenance`, `source`, `notes`;
- `hull()`: the convex hull as a Solid (described by planes; it contains the exact hull);
- `section(height, direction="+z")`: the cross-section at a height as a Sketch, and
  `section_points` as numbers;
- `silhouette(direction="+z")`: the largest outline seen along a direction, holes filled, at
  most 160 corners and never inside the exact outline; `silhouette_points`, `silhouette_size`;
- `envelope(fit, ctx, shape=..., direction=...)`: the room the thing needs as a Solid;
- `pocket(fit, ctx=ctx, shape=..., direction=..., removable=..., name=...)`: a `Pocket`.

A pocket is the joint between a printed part and a real thing. It takes no clearance: `fit`
is `cradle`, `stretch` (section 4.8), `loose`, `slide` or `snug`, and the gap comes from the
fit resolver for the thing's largest extent with only the printed side compensated
(`printed_peg=False`). Shrinkage is applied as a scale of the whole room (`1 + xy_shrink`),
the offsets as a uniform gap; `pocket.gap` is the drawn gap at the widest point and
`pocket.clearance.target` the printed gap aimed for. `shape` is `silhouette` (the outline
along the direction, extruded), `hull`, `cylinder` or `box`; every shape contains the thing
(`pocket.envelope_tolerance` says how far a simplified outline may lie outside the exact one).
`radius` and `center` are the smallest circle around the outline, computed exactly: a circle
that is only close to the smallest makes every round holder larger than the fit asks for.
`direction` is the way the thing is taken out, in the reference's frame; it rests on the
pocket's seat at the other end, where no gap is drawn. `pocket.cut(part, at)` cuts the room
(open along the direction by default, closed for `stretch`) and returns the new part;
`pocket.place(at)` only records where the thing sits. `pocket.room(offset=..., top=...)` is the
same outline with another offset, for walls and lips; `radius`, `center`, `size`, `depth`,
`gap` size the part around it. `pocket.taper(low, high, inset, offset=...)` is the outline
between two heights above the seat as a frustum whose upper end is moved in by `inset`
corner for corner (negative: out; inset 0: a prism over the same corners): a lip that leans
over a rounded edge at an angle that prints without support, or a bevel around an opening.
Every face of it is flat. The outline is first tidied (`tidy_polygon`: corners closer than
0.001 mm to the line through their neighbours are dropped), so tapers and prisms made with
`taper` share their corners exactly; `silhouette` pockets only. When the reference states the
section of its edge along the pocket's direction (a component with `envelope.edge_profile`,
section 4.7), `pocket.fitted` is true and `pocket.room(fitted=True)` is the room that follows
the rounded edges: the outline's rings, each moved in by the section's inset, stacked; below
the widest level they stay where the seat puts them, from the widest level up they are
raised by the gap at the far end. It lies inside the prism, and it is the room the check
proves free. A clearance taken from the resolver by hand for a reference is
reported like any clearance taken without a card.

The model process declares its pockets in `model.build.json` (`fits.pockets`: `pocket`,
`reference`, `fit`, `shape`, `direction`, `removable`, `frame`, `through`; `fits.components`:
`id`, `features`). The worker trusts none of it: it rebuilds each pocket for its own copy of
the reference with its own resolver and verifies on the solids. `reference_fit` gives one
result per pocket (`data.kind` = `pocket`; `data` has `reference`, `fit_class`, `shape`,
`direction`, `expected`, `target`, `offset`, `scale`, `envelope_tolerance`, `provenance`,
`robustness`, `room`, `intrusions`, `clearance_proven`, `blocked`, `path_clear`, for a thing
whose source states tolerances `thing_tolerance`, and for a stretch fit
`grip_coverage`, `lip_depth`, `stretch_strain`, `stretch_limit`, `material_flexible`):

- clearance all round: no part shares volume with the room (tested 0.005 mm smaller than
  drawn, so the pocket's own walls are contact). The room is the fitted one where the
  reference states its edge section (`data.room` = `fitted to the stated edge section`),
  otherwise the prism over the outline (`outline prism`): a lip may lean into the corner a
  rounded edge leaves, and material inside the fitted room is a `fail`;
- the thing's own tolerance: when the reference's source states tolerances for its sizes (a
  component's envelope), half the largest across the pocket's direction is added to both
  ends of the printed spread (`data.thing_tolerance`: `per_side`, `printed` [tightest,
  loosest], `window`, `ok`); a spread that then leaves the fit's window is a `warn` that
  asks to try the gauge on the real thing;
- insertion: the room swept along the direction until it leaves the model meets no part
  except those named `removable`; material in the way is a `fail` that says the thing is
  trapped (an undercut, a rim narrower than the thing);
- `stretch`: the material must be flexible (a rigid one is a `fail` that says the part would
  crack or never go on), walls must stand where the fit put them around at least half of the
  rim, and the lip the thing is pushed past must not be deeper than the material stretches
  (`2 * lip / narrowest width <= stretch_strain`); a shell with no lip is a `warn`;
- a pocket that was never placed, whose clearance is not the resolver's, or that could not be
  rebuilt is a `fail`; a fit that does not keep its class across the printer's spread, and a
  guessed stretch interference, are a `warn` that asks for the gauge.

The range sampler (section 4.5) reads the request's references from the model's folder and
runs `reference_fit` with its other error checks. The `docs` op finds short recipes for all of
this in `hotend_fit.capabilities` (`reference_objects`, `bought_components`,
`flexible_materials`, `reference_fit_classes`, `reference_fit_gauges`,
`leaning_lips_and_stand_off`).

### 4.7 Components

A bought device is data, one JSON file per component in `python/hotend_fit/components/`
(`<id>.json`, schema 1; the format is documented in the package's docstring and enforced by
its loader and by `tests/fit/test_fit_components.py`). Top level: `schema`, `id`, `name`,
`kind`, `units` (mm), `frame` (where the origin is, in words), `sources` (per key: `title`,
`url` (https), `kind`, `retrieved`, `note`), `defaults`, `envelope` (`width`, `height`,
`depth`, optional `corner_profile`, `edge_profile` and further numbers), `features` and
`rules`. `edge_profile` is the section of the body's edge as the maker draws it: points
[distance inwards from the outline, height from the middle of the thickness], from the
edge's upright band (distance 0) to where the flat face starts (height = half the depth).
The body is then the outline moved in by that distance at every height, the same on the
back and the front (three parabola arcs per stated segment, convex, through every stated
point); without it the body is the upright prism. `component.edge_section()` returns the
whole section from the back to the front as (distance inwards, height above the back).

Every number is a bare value that takes the feature's and the component's defaults, or
`{"value", "tolerance", "unit", "source", "provenance", "confidence", "note"}`. After the
defaults every number has a tolerance, a source listed in `sources`, a provenance (`measured`,
`standard`, `calibrated`, `guessed`, `style`) and a confidence (`verified`, `likely`,
`unverified`); a source of kind `estimate` cannot back a `standard` or `measured` number. A
number is proven when its provenance is `measured`, `standard` or `calibrated` and its
confidence is `verified`. A feature with any other number must carry a `question`.

A feature has `id`, `kind` (button, port, camera, microphone, speaker, magnet, sensor, or
area: a region the maker's drawing marks, such as where a case must be of uniform
thickness; it has a `body` patch and no keep-out), `label`, `face` (`+x` ... `-z`: the face it sits on), an optional `body` shape, a list of
`keep_out` shapes, named `data` numbers, `question` and `note`. Shapes sit on the face and
run outwards: `box` (`center`, `size`, optional corner `radius`, optional `flare`: an angle
in degrees from the face's normal, more than 0 and less than 90, by which a keep-out widens
on all four sides from the surface outwards; inside the body it stays straight), `cylinder` (`center`,
`diameter`, `length`), `bar` (`center`, `size`, `end_profile`), `cone` (`center`, `angle`,
optional `diameter`, `length`). Along the face's normal, 0 means a patch on the surface and
`null` means as far as it goes.

In a model: `component.size`, `component.reference` (the body: the outline through the
maker's corner points over the full depth, with the edges rounded as `edge_profile` states), `component.feature(id)` (records that the model
relies on it) with `center`, `size`, `solid` (the feature where it stands proud),
`keep_out(reach, inward)` (the maker's keep-out volume, flared where the maker asks for it), `opening(ctx, fit, reach, inward)`
(the feature's outline grown by a resolver clearance) and `value(name)`; `component.rule(name)`
is a design rule of the maker as a number. For a component placed by a pocket, `reference_fit`
adds: a `fail` when a part collides with a proud feature or leaves material in the keep-out
of a feature the model asked for, a `warn` when it covers a keep-out it never asked for
(`data.kind` = `component`), and a `warn` "not fully proven" when the model relies on a number
that is not verified (`data.kind` = `provenance`, with `unverified` and `questions`).

Shipped: `sample_phone` (Sample phone, invented). It is no real device: every number is
invented, its sources say so, and it exists for the examples and the tests. Some of its
numbers play sizes that were only scaled off a drawing (the camera plateau's outline) or are
not certain (the height of the USB-C opening); they are tagged as such and become questions.

A real device is a local file: put `<id>.json` into `python/hotend_fit/components/` of the
checkout. Git ignores every file there except the shipped sample; the loader validates it the
same way and `list_components` names it as a file of this installation that is not shipped.
Numbers taken from a maker's document belong in such a file only, unless its licence allows
publishing them.

### 4.8 Flexible materials and the reference fit classes

`hotend_fit.materials.material_facts(material)` says what is assumed of the request's
material family: `flexible`, `shore` (read from an id such as `tpu-95a`), `max_overhang_deg`,
`wall_mm` (thinnest and thickest useful wall), `stretch_strain` (how far a rim may be
stretched to put the part on, as a share of its width; null for a rigid material), `nozzle`
(`direct_drive` or `any`), `guidance`, `sources` and `provenance` (`guessed` for every
entry). TPU is the flexible family: walls 1.2 to 2.5 mm, 45 degrees, 8 % stretch, main
nozzle only on the X2D. `ctx.fit.material` gives the same object to a model.

Two fit classes are for a printed part around a real thing; they are in
`hotend_fit.fit.REFERENCE_FIT_CLASSES`, the feature cards do not offer them, and both are
`guessed` until the pocket's gauge has been printed (the sources are in comments beside the
numbers):

| class   | printed gap aimed for, per side | window          | meaning                                  |
| ------- | ------------------------------- | --------------- | ---------------------------------------- |
| stretch | -0.05 mm                        | -0.40 to +0.10  | a flexible shell that grips a rigid thing |
| cradle  | +1.50 mm + 1 % of the size      | +0.50 and up    | a soft thing dropped into a rigid holder |

The resolver hands a `stretch` clearance out for any material; the `reference_fit` check
fails it in a rigid one, so the result says what is wrong.

## 5. Core <-> agent

The agent is a child process. Transport: JSON-RPC 2.0, one message per line, over the child's
stdin and stdout. Stderr is logs.

The child is started as `hotend-sandbox run --profile agent --job <jobs/<run-id>> --
<python> -I -m hotend_agent`, one process per run (section 5.2 describes the process). The core sends `agent.run` with id 1 as the
first line. The child must not write anything but JSON-RPC lines to stdout.

Core -> agent requests
- `agent.run` `{"run_id", "project_id", "provider": {"kind": "anthropic|openai|google|openrouter|ollama",
  "model": "...", "base_url": "..."}, "egress_token": "<64 hex>", "history": [...provider-native
  messages...], "user_message": {"text": "...", "attachments": []}, "tools": [{"name",
  "description", "input_schema"}], "budget": {"max_turns": 24}}`
  -> result `{"history": [...], "stop_reason": "done|budget|cancelled|error", "usage": {...}}`,
  optionally with `"error": "<text for the user>"`. `history` must be a JSON array; the core
  stores it without reading it (unless `stop_reason` is `error`, which leaves the stored history
  as it was) and hands it back on the next run of the same project. A project whose history was
  written with one provider kind refuses messages while another kind is selected.
  `provider.base_url` is the base URL to configure in the provider SDK (for example
  `https://api.openai.com/v1`), so that the paths the SDK builds are the ones the egress table
  allows; no socket is ever opened to it from the agent. `tools[].input_schema` is JSON Schema
  draft 2020-12; give it to the model unchanged.
- `agent.cancel` `{"run_id"}` (id 2). The agent should stop and answer `agent.run` with
  `stop_reason: "cancelled"`. After a grace period (5 s) the core kills the process; the run then
  ends as `cancelled` and the stored history is unchanged. A cancel also stops the engine jobs of
  tool calls that are in flight; those calls answer `ok: false` with the code `cancelled`.

Agent -> core
- request `tool.call` `{"run_id", "name", "args"}` -> `{"ok": true, "content": [...]}` or
  `{"ok": false, "error": {"code", "message"}}`. Content items are `{"type": "text", "text"}` or
  `{"type": "image", "path", "media_type": "image/png", "data_b64"}` (the core has already
  inlined the image bytes for the model; `path` only names the image; the agent never reads
  files). A failure of the model's own work (a script that raises, a failed check) is `ok: true`
  with a text the model can act on; `ok: false` is for calls the gateway refused or could not
  run. `"auto": true` (only with `render_views`) marks the look review's own call after a build
  (`write_model_code` or `set_parameter`) whose status is `ok`: the core answers with its own
  standard views (iso, front, top at 512 x 512) of the current version, once per such build, and
  `not_offered` at any other time. Such a call counts against no cap of the model and is never a
  refusal; it is audited as `render_views` with `auto: true`.
- notification `agent.event` `{"run_id", "event": {"type": "text_delta|tool_start|tool_end|thinking|usage", ...}}`.
  `text_delta` and `thinking` carry `text`; the core builds the stored assistant transcript from
  the `text_delta` texts. Other fields are passed to the UI as they are. An event with another
  `type`, or over 64 kB, is dropped.

- request `llm.request` `{"run_id", "request_id", "token", "method", "path", "headers": {...}, "body_b64"}`.
  This is the agent's only way to reach a model. `token` is the run's `egress_token`. `path` is
  the URL path with its query string, without scheme or host. The core answers with the
  notifications `llm.response_start` `{"request_id", "status", "headers"}`, zero or more
  `llm.response_chunk` `{"request_id", "data_b64"}` (at most 16 kB of body each, in order), then
  the JSON-RPC result `{"ok": true}` (or an error). An upstream HTTP error status is a normal
  response (`status` 4xx/5xx and its body), not a JSON-RPC error. The agent side is an
  `httpx2.AsyncBaseTransport` (the HTTP library of Pydantic AI 2.x and the provider SDKs) handed
  to the Pydantic AI provider, so streaming works unchanged (section 5.2).

Rules of the channel
- Every `run_id` must be the run's own; unknown fields in `tool.call` and `llm.request` params are
  rejected. Requests may be in flight concurrently, at most 8 at a time (error -32001 beyond).
- JSON-RPC error codes: -32600 not a valid message, -32601 unknown method, -32602 bad params,
  -32000 the egress refused or failed (`data.code` is one of `not_authorised`, `cap_exceeded`,
  `denied`, `invalid_request`, `no_credential`, `secret_store_error`, `upstream_error`,
  `setup_error`), -32001 too many requests in flight.
- A line over 48 MB, eight protocol violations, the process exiting before it answered
  `agent.run`, the wall-clock limit (30 minutes) or 20 refusals end the run with a typed error
  (`RunEvent` `failed`), the child is killed and the stored history is unchanged. A refusal is a
  tool call answered with `unknown_tool`, `arguments_too_large`, `invalid_arguments` or
  `rate_limited`, or a model request answered with `not_authorised`, `cap_exceeded`, `denied` or
  `invalid_request`; the error code of the run is `too_many_refusals`. Until the run has ended,
  refusals past the twentieth are not audited one by one: the run leaves one `tool` entry named
  `(refused calls)` with the outcome `refused` and the counts by code.
- When a run ends, however it ends, requests that are still in flight are finished first: their
  engine jobs are stopped (the calls end as `cancelled`), their job folders are removed and their
  audit entries are written. A call that does not come back within 8 s is dropped and audited as
  `abandoned`. Nothing an agent started outlives its run.

The agent process writes only its scratch folder, reads that, the Python runtime and the
sandbox's system paths, and has no network: no socket except an anonymous pair to itself. The core
is the egress proxy. For each `llm.request` it checks the run's provider, allows only that
provider's API host, paths and methods, drops every header outside a short allowlist, adds the real
credential from the keychain, enforces the spend cap (requests, bytes and `max_tokens` ceiling per
run), and redacts the credential from anything it logs. The agent never sees a key, on any OS.

The provider table (`hotend_core::egress::providers`; a request that matches no row is refused
with `denied`):

| Provider | Origin | SDK base URL | Allowed requests (all POST) | Credential | Extra headers | Token field |
| --- | --- | --- | --- | --- | --- | --- |
| `anthropic` | `https://api.anthropic.com` | same | `/v1/messages`, `/v1/messages/count_tokens` (query `beta=true` allowed) | `x-api-key` | `anthropic-version`, `anthropic-beta` (filtered, see below) | `max_tokens` |
| `openai` | `https://api.openai.com` | `.../v1` | `/v1/chat/completions`, `/v1/responses` | `Authorization: Bearer` | | `max_output_tokens`, `max_completion_tokens` or `max_tokens` |
| `google` | `https://generativelanguage.googleapis.com` | same | `/v1beta/models/<model>:generateContent`, `:streamGenerateContent` (query `alt=sse` allowed) | `x-goog-api-key` | | `generationConfig.maxOutputTokens` |
| `openrouter` | `https://openrouter.ai` | `.../api/v1` | `/api/v1/chat/completions` | `Authorization: Bearer` | | `max_completion_tokens` or `max_tokens` |
| `ollama` | `http://127.0.0.1:11434` | `.../v1` | `/v1/chat/completions`, `/api/chat` | none | | optional |

- Request headers passed on: `accept`, `content-type` and the provider's extra headers. Every
  other header is dropped, including any `authorization`, `x-api-key` or `x-goog-api-key` the
  agent set. Response headers passed back: `content-type`, `retry-after`, `request-id`,
  `x-request-id`, `x-should-retry`.
- Every request body must be a JSON object (`invalid_request` otherwise). A generating request,
  except for `ollama`, must carry one of the provider's token fields with a whole number no
  higher than the ceiling (32 000 by default); otherwise `denied`. So the agent must always set
  an output-token limit.
- The body is checked against the provider's body rule (`BodyRule` in the provider table;
  `denied` otherwise), so that the user's key pays for one answer of the chosen model and for
  nothing the provider would run on its own servers:

  | Provider | Model | `tools` entries | Refused fields | Must be 1 if present |
  | --- | --- | --- | --- | --- |
  | `anthropic` | body `model` | no `type`, or `type: "custom"` | `mcp_servers`, `container` | |
  | `openai`, `ollama` | body `model` | `type: "function"` | `web_search_options`, `prompt` | `n` |
  | `openrouter` | body `model` | `type: "function"` | `web_search_options`, `prompt`, `plugins`, `models`, `route` | `n` |
  | `google` | the `<model>` of the path | an object with only `functionDeclarations` | `cachedContent`; any key with an underscore at the top level or in `generationConfig` | `generationConfig.candidateCount` |

  The model must equal `provider.model` of `agent.run`, also on `count_tokens`. A `null` value
  counts as absent. The body sent to the provider is the parsed JSON written out again, so a key
  that was given twice reaches the provider with the one value that was checked.
- `anthropic-beta` is a comma-separated list; only features whose name starts with
  `interleaved-thinking-`, `fine-grained-tool-streaming-`, `token-efficient-tools-`,
  `prompt-caching-`, `extended-cache-ttl-`, `context-management-`, `compact-` or
  `thinking-binding-controls-` are passed on,
  the others are dropped from the header (the header is dropped when none is left).
- Per run by default: 100 requests, 8 MB per request body, 128 MB of request bodies, 256 MB of
  response bodies (`cap_exceeded` beyond), 10 minutes per request. Refused requests count
  towards the 100. Once the request cap is used up, only the first refusal is audited; the rest
  are counted in the `agent` entry (`detail.refused_requests`). Redirects are not followed.
- The credential is read from the secret store for each request and is redacted from response
  headers and body chunks before they reach the agent, should a provider echo it.

### 5.1 Tools (executed by the gateway, never by the agent process)

| Tool | Args | Effect |
| --- | --- | --- |
| `write_model_code` | `code`, `note` | Stores the script, runs a build with checks, creates a version if it built. Returns status, errors with line numbers, check results, part summary |
| `set_parameter` | `name`, `value` | Rebuilds the current version with one parameter changed |
| `sweep_parameters` | `params` (1 to 3 of `{name, values}` or `{name, from, to, steps?}`), `combine?` | Read-only. Builds the current version at up to 16 settings (one parameter at a time, the others as the version has them, or every combination with `combine`) through the engine's `sweep` op (section 3.2: the range sampler of section 4.5 in the model process) and returns per setting whether it builds and passes the error checks, the failed checks, every contract requirement (met, NOT met, not checkable, resting on a guess) and the part sizes, then per parameter the values that passed and failed. Stores nothing and creates no version. Refused (`invalid_sweep`, not a refusal) for an unknown parameter, one without a range, a value outside the declared range, a count that is not whole, or more than 16 settings |
| `run_checks` | `checks?` | Runs checks on the current version. It does not run the range sampler (`sweep_parameters` does) |
| `render_views` | `views`, `section?` | Returns PNGs of the current version |
| `measure` | `queries` | Exact measurements on the current version |
| `read_docs` | `topic` or `query`, `limit?` | Returns excerpts of the bundled build123d and Hotend card docs for the pinned versions |
| `list_cards` | | Feature cards and their ports: the engine `docs` op filtered to `hotend_fit.cards`. Fails with `not_installed` while that package is absent |
| `list_components` | | Read-only. The bought devices shipped as components (section 4.7): per component its id for `ctx.component(id)`, name, kind, size, the provenance of its body, the number of its numbers and of those not verified (by feature) and its sources with their URLs. It joins the entries of `hotend_fit.components.catalogue` (an introduction, `shipped_components`, and one `shipped_component_<id>` per component, each a docstring the package writes from its shipped data), found through the engine `docs` op with the words `shipped component` like `list_cards`; one entry per component keeps each under the index's cut. Fails with `not_installed` while that package is absent; says so when no component is shipped |
| `list_print_profiles` | | Read-only. The printer, material, build plate and nozzle size of the settings, and the slicer backend's table for the printer (`SliceBackend::catalogue`): the process profiles `slice` accepts (with aliases), the materials and the build plates, each with whether its name was seen on a real installation. Says so when there is no slicer backend or no table for the printer |
| `print_plan` | `plate?`, `explain?` | Read-only. The print plan of one plate of the current version (`print_plan_get`, the text of `hotend plan show`, or with `explain` of `hotend plan explain`): every decision with its value and origin (with `explain` its rule, basis and sentence), the questions, warnings, notes, assumptions and checklist, or why it is blocked. It is computed from the version's stored layout and stores nothing: it writes no plan record and never lays the version out (either could change the plan of a pending job, `plan_changed`); before the version is laid out for the settings it fails with `not_prepared`, and a plan whose brim needs more room than the layout kept says that the next `slice` lays it out again. It changes no setting, baseline or print |
| `slice` | `profile`, `plate?` | Lays the current version out for printing if it is not yet (section 3.5 `prepare`, for the printer and material in the settings) and slices one plate; returns time, grams, per-nozzle filament, how each part was oriented, what did not fit, warnings. Refused (`failed_checks`) for a version with `status: "check_error"` |
| `propose_print` | `note` | Creates a pending print job for the user to review. It cannot start a print. Refused (`failed_checks`) for a version with `status: "check_error"` |
| `snapshot_model` | `name` (one plain line, 1 to 80 characters), `note?` (up to 500) | Names the current version as a snapshot (section 8, "Snapshots"), with the maker's name for it or one the agent made from the maker's words. It writes the snapshot's row (with the print recorded for the version, if any) and keeps that print's exact file with the snapshot, as `snapshot_create` does; nothing else: no version, not the current version; it cannot export (it has no path) and cannot go back or delete. Refused with `name_taken` for a name the project already uses (compared without regard to ASCII case) and `invalid_name` for one that is not one plain line. Audited as the tool call and as a `snapshot`/`create` entry with `by: "agent"` |

No tool confirms, hands off, starts or cancels a print, and none changes a proven baseline, a
consent, the research settings, the other settings, the calibration or a key: the test
`gateway::tools::tests::the_agent_has_no_tool_that_prints_or_changes_what_only_the_person_decides`
asserts the full list of tools an agent can be offered (the registry, `record_source`,
`web_search`, `web_fetch`) and which of them change anything (`write_model_code`,
`set_parameter`, `slice`, `propose_print`, `snapshot_model`, `record_source`).
`snapshot_model` writes a snapshot's row and, for a printed version, the kept copy of the
print's file only; `tests/read_only_tools.rs` holds a scripted conversation with it against
every file of the project, its versions and its current version, and
`hotend-slicer/tests/snapshots.rs` one that checks the kept copy.

A run with web research also gets `record_source`; a run whose research goes through a search
service (docs/WEB_RESEARCH.md section 16) gets `web_search` (`query`, 2 to 400 characters) and,
while it may read pages, `web_fetch` (`url`). The core makes their calls to the chosen
service; the result is the service's answer framed as data from the web. Their failure codes:
`url_not_allowed` (not a plain address of a named public site: an IP address, a login part, a
local or intranet name, a port other than the usual one; outside the project's site limits;
a model-sharing site (docs/WEB_RESEARCH.md section 17); not that very address, query string
included, as a search found it, the maker wrote it or a stored source has it; or the service read the page at another address that breaks one of
these rules, and its text was not used), `research_cap_reached` (the per-message cap, or the
project's allowance, checked before every call), `search_key_missing`, `search_key_rejected`, `search_rate_limited`,
`search_out_of_credits`, `page_unavailable`, `search_refused`, `search_unavailable`,
`search_timeout`, `search_response_too_large`, `search_redirect_refused`,
`search_bad_response`, `storage_error`. None of them counts as a refusal.

Every call is validated against a JSON Schema, checked against policy (size caps, rate caps, state),
written to the audit log with a hash of its arguments, and executed by the core.

The registry (`hotend_core::gateway::tool_specs()`) is the source of the names, descriptions and
schemas; all schemas reject unknown properties. Policy, in the order it is checked: the call is
counted and the run is under 200 calls, refused ones included (`rate_limited`); the tool exists
(`unknown_tool`); the arguments are at most 256 kB as JSON (`arguments_too_large`); they match
the schema (`invalid_arguments`, with the failing paths); the tool is under its own cap per run
(`rate_limited`; 40 for `write_model_code`, `set_parameter`, `measure`, `read_docs`; 20 for
`run_checks`, `render_views`; 10 for `list_cards`, `list_components`, `list_print_profiles`, `print_plan`,
`slice`;
8 for `sweep_parameters`; 3 for `propose_print` and `snapshot_model`); the project
has a current version where the tool needs one (`no_current_version`). The first four are
refusals and count towards the run's refusal limit (section 5). Other failure codes: `not_found`,
`not_configured` (no slicer backend), `failed_checks` (`slice` or `propose_print` for a version
that failed a check of severity `error`; the same holds for the `slice_run` command of section
8), `no_slice` (`propose_print` before `slice`, or after the version was laid out again),
`no_plate` (`slice` for a plate the layout does not have; when the layout has no plate at
all the message names the first piece that fits none and why, and the next step is to make
the part smaller, split it or choose a printer it fits), `hash_mismatch`, `unusable_output`,
`cancelled` and the other job codes of section 3.1. Every argument the schema of `read_docs` allows is
accepted: `topic` and `query` are joined and the search text is cut at 400 characters, the
longest the engine's `docs` op takes.

Results are short texts written for a model: a `status:` line, what happened to the version
("version 3 created and now current" or "no version was created; the current version is still
2"), the error with its line number and trimmed traceback, parameters with ranges, one line per
part, a check summary line and one line per check that did not pass, then the script's stdout.
The summary line is `checks: F fail, W warn, P pass, S skipped`, followed by `, N not
checkable` when contract requirements could not be evaluated (section 4.1); those are listed
as `- NOT CHECKABLE contract [part]: Not checkable: ...` with the reason, apart from the
`- skipped` lines of checks that had nothing to do.
`render_views` returns one text item and one image item per view. `measure` returns one numbered
line of JSON per query.

A build made for a version (`write_model_code`, `set_parameter`) runs the checks with the
product options of section 3.2: the `contract` check, overhangs judged in the orientation the
part will be printed in, and joints drawn without a feature card reported by `fit_pairs`.
`contract` is also a name `run_checks` accepts. When the engine result carries `questions`
(requirements of the contract that rest on a guess, what is not verified about a device), the
result text ends with `questions for the user (N; ask them, do not answer them yourself):`, N
being all of them, and one line `- [<requirement>] <text>` per question: first those a fit
depends on (of a bought device or a reference file, whether the contract's rows are in the
result or not; of `fit`, `reference_fit`, `cavity_min`, `outer_max`, `standoff` and `dimension`
requirements; and, when the rows are there, every question without one, a pocket's). Without
the rows (a partial check run, a crashed check) a requirement's kind is not known: then come
the questions without guessed parameters, a pocket's among them, and then those with one; all of
them count as ones a fit may depend on. Those whose row names another kind are last. At most
16 lines and 5,200 bytes are listed, fewer (at least 3,000 bytes of them) when the rest of the
result is long; when some are missing the last line is `- and N more, not listed here (a fit may
depend on M of them): the maker sees all of them in Hotend's list of open questions`, or
`(none of them about a fit)` when the rows say so (the agent has no tool for that list). A long result is cut before
the questions, never in them.

The `slice` result reads: `status: ok (version N, profile P, plate X of Y)`, the time, filament
and layers with one line per filament slot, `orientation on the plate:` with one line per part
(`- <part>[ xN]: <turned>. <explanation>`), then `every piece fits a plate` or `DID NOT FIT:`
with the pieces and reasons, one `plate check warn|FAIL <id>: <message>` line per check of the
preparation that did not pass, the slicer's warnings and the slicer's name. No tool confirms,
delivers or starts a print; a test enumerates the registry and calls every tool against a core
with a pending job to prove it (`no_tool_of_the_gateway_can_start_or_confirm_a_print`).

### 5.2 The agent process (`hotend_agent`)

`python -I -m hotend_agent [--script <script>]`. One process serves one `agent.run` and exits
with code 0 after it has written the result. It exits with 0 and without an answer when its
stdin closes, with 2 after refusing unusable `agent.run` params (JSON-RPC error -32602 on the
run's id), and with 1 after an internal failure (error -32603). A second `agent.run` is error
-32600, any other method -32601. The process takes over file descriptor 1 at start: only the
channel writes to it, and anything a library prints goes to stderr. Stderr carries one log line
per run, model request and tool call (ids, routes, sizes, outcomes), never a prompt, an
argument, a result or a model's text.

`agent.run`
- `budget.max_turns` is the number of model requests the run may make (1 to 100, default 24).
  The run ends with `stop_reason: "budget"` when the next request would exceed it; the returned
  history holds everything up to and including the last tool results, and the next message
  continues from there. Optional `budget.max_output_tokens` (default 16 000, capped at 32 000)
  is the output limit sent with every model request.
- `history` is Pydantic AI's message list as JSON (`ModelMessagesTypeAdapter`), with one
  change: an image in a tool result is replaced by the text `[image <path>: shown to the model
  when it was made]`, so the stored history holds no pixels. The system prompt is sent as
  instructions with each request. A history the agent cannot parse ends the run with
  `stop_reason: "error"`.
- `stop_reason` `error` always comes with `error` (a text for the user) and the `history` that
  was passed in. `cancelled` returns the conversation as far as it got, with the user's message.
- `usage` is `{requests, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
  tool_calls}` for the run.
- `user_message.attachments` are not read yet.

`agent.cancel` is answered at once with the result `{}`. The run then normally ends within
milliseconds; if it has not ended after 3 s the agent answers `agent.run` itself with
`stop_reason: "cancelled"` and the history that was passed in, and exits.

`agent.event` (each line stays under 16 kB)
- `text_delta` `{text}`: at most 8000 characters; the texts of separate model turns of one run
  are separated by a `text_delta` of two newlines, so the joined transcript reads as paragraphs.
  A run without web research whose answer has a line beginning `web research is off:` (the
  prompt's step 1: the answer would have looked a fact up) ends with one `text_delta` whose `text`
  is empty and which carries `research_needed: {"lookup": "<what it would look up, at most 200
  characters>", "state", "source"}`: the `state` and `source` of the run's `research` parameter
  (null when the core sent none). The transcript is unchanged by it. The window offers, next to
  that answer: for `needs_consent` (or null) "Turn on web research", which opens the shell's own
  consent dialog (`research_consent_ask`) for the chosen source, or Settings when no usable source
  is chosen or it is agreed already; for `off`, `needs_key`, `provider_refused` and
  `unsupported_provider` the reason and a button to Settings, AI model, Web research; for
  `budget_used` and `stopped` the reason alone. It says research is on only after the person
  agreed in that dialog. A run with web research never sends it.
- `thinking` `{text}`.
- `tool_start` `{name, tool_call_id, args}`, or `args_omitted: true` instead of `args` when the
  arguments are over 4000 characters as JSON (a model script), and `note` (the call's `note`
  argument, at most 500 characters) when there is one.
- `tool_end` `{name, tool_call_id, ok, summary, images}`: `ok` is the gateway's `ok`; `summary`
  is the first line of the result text (the `status:` line, or `error (<code>): <message>`), at
  most 300 characters; `images` is the number of images the model was shown.
- `usage`: the fields of the result's `usage`, cumulative for the run, after each model answer.

Tools. Each entry of `tools` becomes one model tool with the core's name, description and
schema unchanged; the agent validates nothing. Tool calls of one model answer run one at a
time, in the model's order. A gateway refusal (`ok: false`) reaches the model as the text
`error (<code>): <message>` and does not end the run; text items reach it as text and image
items as images (PNG, JPEG, WebP or GIF).

Model requests. All five providers use one HTTP client whose transport sends `llm.request`.
The SDKs are given a placeholder key; the transport never forwards `authorization`,
`x-api-key`, `x-goog-api-key`, `api-key`, `cookie`, `host`, `content-length`,
`accept-encoding` or `user-agent`, and forwards every other header for the core to filter.
What the pinned SDKs send (pydantic-ai-slim 2.69, anthropic 2.9, openai 3.52, google-genai
2.34), each streamed and each inside the table above:

| Provider | Request | Output limit field |
| --- | --- | --- |
| `anthropic` | `POST /v1/messages` with `anthropic-version` | `max_tokens` |
| `openai` | `POST /v1/responses` | `max_output_tokens` |
| `google` | `POST /v1beta/models/<model>:streamGenerateContent?alt=sse` | `generationConfig.maxOutputTokens` |
| `openrouter` | `POST /api/v1/chat/completions` | `max_tokens` |
| `ollama` | `POST /v1/chat/completions` | `max_tokens` |

A JSON-RPC error with `data.code` `denied`, `cap_exceeded`, `not_authorised` or
`invalid_request` is handed to the SDK as an HTTP 403 response (`no_credential` as 401) with
`x-should-retry: false`, so it is not retried, and ends the run with `stop_reason: "error"` and
the core's message. Any other error is a connection error to the SDK, which may retry it. An
upstream error status ends the run with the status and the provider's message.

Scripted mode (development and the offline demo only). With `--script <path>` or
`--script builtin:<name>` (a script shipped in `hotend_agent/scripts/`), or the same value in
`HOTEND_AGENT_SCRIPT`, the agent replays a written conversation instead of calling a model: no
`llm.request` is sent; tools, events, budget, cancel and history work as in a real run. The
core passes the argument through `AgentCommand.args` (the sandbox gives the child an empty
environment unless a variable is allowlisted). A script is `{"protocol": 0, "name",
"description", "user_messages": [text...], "turns": [{"expect": [text...], "say": text,
"calls": [{"tool", "args"}]}]}`.
Each model request plays the next turn, counted by the model answers already in the history,
so a script continues across runs of a project; a turn without calls ends a run. `expect`
lists texts that must all occur in what the model was just sent (the previous turn's tool
results, or the user's message); otherwise the run ends with `stop_reason: "error"`. After the
last turn the model says only that the script is over. `user_messages` is optional and is for
whoever drives the script: the user's side, one message for each turn that ends a run, in
order. The agent never sends them; a script whose count differs is refused.

Two scripts are shipped. `builtin:cable_box_demo` (one message) designs
`python/tests/models/cable_box.py`: it lists the cards, writes a version whose wall is too
thin, reads the failed `min_wall` check, writes the fixed version, renders an outside view and
a section, and ends with a summary whose last sentence is the first question for the user.
`builtin:cable_box_print_demo` (four messages) begins with the same five turns and goes on:
the user answers that question and the contract and parameters follow (`measured`), a second
question is asked and answered, the agent measures the cable slot, slices (`slice` with the
profile `demo-0.20mm`), turns the flat top of the slot into a roof because the plate summary
says it needs support, slices again, suggests printing the fit gauge first and, when the user
asks for it, calls `propose_print`. It needs a slicer for the two `slice` calls (the tests and
`python -m hotend_agent.evals --scripted` configure the fake slicer). What the gateway
answered to each of its calls is recorded in
`python/tests/agent/fixtures/cable_box_print_demo_results.json`.

## 6. Slicer

`hotend-slicer` finds a vendor slicer the user installed, runs its documented command line under
`hotend-sandbox run --profile slicer` and reads the result as hostile input. Slicers are never
bundled or downloaded. Entry points: `Slicer::new(SlicerConfig)`, `slicer.detect()`,
`slicer.slice(&SliceJob) -> Result<SliceResult, SlicerError>` and
`slicer.export_project(&SliceJob) -> Result<ProjectExport, SlicerError>`.

The Bambu Studio side follows a run of the real program: version 02.09.01.70, sliced by hand
on Windows for the X2D on 17 November 2026. "Measured" below means seen in that run or in the
files it wrote. `crates/hotend-slicer/README.md` lists what is proven against that recording,
what is not, and the steps for a machine with the real program.

**Detection.** Per slicer kind (`bambu-studio`, `prusa-slicer`), in this order: the explicit
path in `SlicerConfig::executables` (application settings); the environment variable
`HOTEND_BAMBU_STUDIO` / `HOTEND_PRUSA_SLICER`; the `PATH`; the usual install folders of the
operating system (the list is `hotend_slicer::detect::candidates`). An explicit path is final:
nothing else is searched, and a path that does not exist is an error. The version is read by
running `<executable> --help` inside the sandbox in an empty temporary job folder; no slicer code
runs unconfined. `detect()` returns `Installation {kind, path, found, version, fake, read_only,
verdict, note}`. A program that prints nothing has no version here (measured: the Windows
Bambu Studio is a launcher without a console and prints nothing, also with `--debug`). The
sliced G-code states the program that sliced it in its first line (`; BambuStudio <version>`).
After a run of a real Bambu Studio the version policy is always applied to that statement,
whatever `--help` gave, before the file is used: a program that was asked may not be the one
that sliced. A refused version is the error `slicer_version_refused`. The version in the result
is the file's, or `unknown` when it states none (judged by the policy's `unknown` verdict). When
`--help` gave a version and the file states another or none, the result also carries the warning
`slicer_version_mismatch`.

**Version policy.** `crates/hotend-slicer/profiles/slicer-versions.toml` is the pinned-version
table: per kind an ordered list of version ranges with a verdict `allow`, `warn` (runs; every
result carries the warning `slicer_version`) or `refuse` (error `slicer_version_refused`), and a
verdict for an unreadable version. PrusaSlicer below 2.9.7 and from 3.0 is refused. For Bambu
Studio the table names 02.09.01.70 as the measured release. It is still `warn`, because Hotend
has not yet passed its regression suite with it, and every other version is `warn` with the
reason that it is not the measured one.

**Printer facts.** `crates/hotend-slicer/profiles/printers.toml` (`schema`, `revision`) holds,
per printer: the slicer kind; per nozzle `ids = {device_extruder, slicer_extruder}`, `reach`
(`[low, high]` per axis) and `max_speed`; `material_rule` entries (families that only the listed
device extruders may print); and `filament_map` (how the manual filament map is passed to the
slicer). Every fact has its own `verified` flag: `true` only for a fact
measured on a real printer, so all are `false`. The nozzle blocks (ids, reach,
speed limit) are not written by hand: they stand between `# BEGIN GENERATED nozzles of <id>`
and `# END GENERATED nozzles of <id>` and are rendered from `hotend_core::profiles` (section
1). The test `the_nozzle_blocks_are_generated_from_the_core_profiles` fails when the file
differs from what the profiles render, or when a printer of the core has no block; run it with
`HOTEND_REGENERATE_PROFILES=1` to rewrite the blocks.

**SliceJob.** `{job_dir, inputs, printer, process_profile, filaments: [{slot, filament_profile,
family, nozzle_id}], plate, arrange, extents: [{name, slot, min, max, placed}], bed_type,
print_settings, nozzle_diameter_mm}`.
- `job_dir` is an existing folder and the only place the slicer can write. `hotend-slicer` owns
  `profiles/`, `output/`, `slicer-config/` and `.sandbox/` in it and empties them at the start of
  every slice. The slicer's `HOME`, temp folder and settings folder are inside the job folder.
- `inputs` are relative paths inside `job_dir`: 3MF or STL for Bambu Studio (its documented
  inputs), also STEP for PrusaSlicer. A 3MF is an archive, and both slicers keep project files
  in one: settings, per-object settings, G-code to insert at a layer. Whether the command line
  applies those next to the settings it is given has not been measured, so they never reach it:
  a 3MF input must pass the archive limits of the output reader (below) and hold nothing but
  the parts of a plain model (`[Content_Types].xml`, `_rels/.rels`, `3D/3dmodel.model`,
  `3D/_rels/3dmodel.model.rels`, `3D/Objects/<name>.model` and the folder entries for them;
  `hotend_slicer::input::model_part`). Anything else is the error `input_refused`, which names
  the entries, before any slicer runs. The content of the model parts is not inspected.
- `printer` is an id from `printers.toml`; it selects the nozzle rules, the slicer kind and the
  machine settings profile of the same id.
- Settings are named by id only (`^[A-Za-z0-9 ._@()+-]{1,120}$`, not starting with `.`, `-` or a
  space, without `..`). A job cannot carry setting values, so nothing the agent wrote becomes
  start or end G-code. For PrusaSlicer and for the stand-in slicer a profile is the file
  `<profile-dir>/<slicer kind>/<machine|process|filament>/<id>.<json|ini>` in the application's
  profile folder (`SlicerConfig::profile_dir`), or else a built-in demo profile. Demo profiles are
  generic and untuned: a result sliced with one carries the warning `demo_profile`. A real Bambu
  Studio never gets them; its settings are the system profiles of its installation (below).
- `bed_type` is the plate on the printer, an id of `profiles/system-profiles.toml`: `cool`,
  `textured-pei`, `high-temp` or `engineering`. `null` is `SlicerConfig::default_bed_type`.
  Which plate is on the printer is a fact only the person at it knows, and the program's own
  default heats the bed for the Cool Plate, so there is no default for a real slice: without a
  plate from the job or the settings a real Bambu Studio is not run (`bed_type_not_chosen`,
  which names the known ids and the setting `plate_type`). Only the stand-in, whose file cannot
  be printed, then slices for the table's `stand_in_bed_type` (`textured-pei`). Bambu Studio
  only; with a value, a PrusaSlicer job is `invalid_job`. An unknown id is `invalid_job` with
  the known ones. The ids and names of the plates a print can be checked for are the same in the
  printer layer's table (section 9); `tests/plate_names.rs` fails when they differ.
- `filaments` are slots 1, 2, ... without gaps; `nozzle_id` is the device extruder id.
- Before any slicer runs, the job is refused if a material family is assigned to a nozzle its
  rule excludes (`material_not_allowed`) or if an extent does not fit the reach of the nozzle of
  its slot (`out_of_reach`). A `placed` extent is compared with the reach box in bed coordinates;
  an unplaced one by size only, allowing a quarter turn about Z. Without extents the slice runs
  and carries the warning `reach_unchecked`.
- For a printer with a `filament_map` section the command line always carries the manual mode and
  an explicit map: one slicer extruder number per slot, looked up from the nozzle row with that
  device extruder id.
- `print_settings` is the plate's `hotend_settings::PrintSettings` (from its print plan) or
  `null`. With a real Bambu Studio the settings decide the process preset (`shell.process`) and
  the material (the plan's filament), are put on top of the flattened system profiles through
  the keymap (below), and the sliced file must pass the proof (below). The plate of the
  settings must be the job's `bed_type` when both are given (`invalid_job` otherwise); without a
  `bed_type` the settings' plate is used. A job whose process profile names another preset gets
  the warning `process_from_plan`, one whose filament names another material
  `material_from_plan`. Version 1 settings are for one filament. Without settings a
  real slice runs with the installed profiles as they are and carries the warning
  `print_settings_missing`. The stand-in slicer ignores the settings: its file is not
  printable and stays labelled.
- `nozzle_diameter_mm` is the nozzle fitted to the printer. For a Bambu Studio printer of the
  system table, a size other than the table's `nozzle` (only 0.4 today) is refused before
  anything runs (`nozzle_not_supported`, naming the fitted size and that Hotend has no profile
  for it yet), for the real program and the stand-in alike. `null` is not checked.

**System profiles (Bambu Studio).** The settings of a real slice are the profiles the installed
program ships, in `<profiles>/BBL/{machine,process,filament}/<name>.json`, where `<profiles>` is
`SlicerConfig::system_profiles` or else `resources/profiles` next to the program (measured on
Windows) or one folder above it. `crates/hotend-slicer/profiles/system-profiles.toml` (`schema`,
`revision`, `measured_on`) maps a printer to its machine profile, a process preset id (with
`aliases`; `demo-0.20mm` is an alias of `standard-0.20mm`) and a material id to profile names,
each with a `verified` flag, and lists the plate types (`id`, `slicer_value`, `temperature_key`,
`first_layer_temperature_key`, `verified`), the option that could carry the plate type
(`[bed_type_option]` with `option`, `verified` and `use`), `stand_in_bed_type`, and per printer
the nozzle inventory of both nozzles (`[printer.nozzle_inventory]`: `verified`, `measured_on`
and `values`, the six keys of `keymap::NOZZLE_INVENTORY_KEYS` with one value per nozzle). The
materials are `pla-basic`, `pla-generic` (Generic PLA, measured), `petg-basic` and `tpu-95a`. Of
the plate types, `cool` and `textured-pei` are `verified = true` (measured: the sliced file
states the plate and heats the bed to that plate's keys); `high-temp` and `engineering` are
`verified = false`, never measured. A job selects a process preset and a material by an id (or
alias) of this table and by nothing else. An id the table does not list is `invalid_job` with
the ids it knows, so the `profile` argument of the agent's `slice` tool cannot name a file of
the installation: not a profile for another printer, not a base profile, not the preset's own
file name. To slice with another installed profile, add it to the table. A real Bambu Studio
without a profile folder is not run: `profile_not_found`.
- The profiles are layered and the command line does not resolve the layers fully (measured:
  an unflattened filament profile gave a density of 0 and a weight of 0.00 g). Each profile is
  therefore flattened: its `inherits` chain from the root down, then the templates it names in
  `include` (without their `name`, `type`, `from`, `instantiation`, `inherits`, `include`,
  `setting_id`), then its own keys, each step overriding the one before. `inherits` and `include`
  are not in the result.
- The profiles are files Hotend did not write, so the core never parses them. The program
  `hotend-profile-flatten <request.json> <out-dir>` (built from this crate, found next to the
  running program) runs under the `slicer` sandbox profile. Its job folder, the only place it
  can write, is `<job>/profiles`, and it is given two read-only paths: the vendor folder and
  its own program file. It cannot read the model, the rest of the job folder or the folder its
  program lies in. (`SlicerConfig::profile_reader` names another reader program; tests use it.)
  The core writes `<job>/profiles/flatten-request.json` (`{root, machine, limits, items: [{role,
  name, output, items: [{key, op, text, absent}], inventory}]}`) and reads back only
  `<job>/profiles/flatten-report.json` (`{ok, items: [{role, name, output, chain, includes,
  keys, compatible: "yes" | "no" | "unstated", dropped: [key], applied: [{key, old: [text] |
  null, new: [text], changed}], unchanged: [key]}], error: {code, role, name, message, key} |
  null}`, at most 256 KiB, unknown fields rejected). The report must cover the request, item by
  item (role and output), and every setting asked for must be reported written or unchanged;
  `applied` may name only keys the request asked for (or the inventory keys of the machine), with
  at most 12 entries of 64 characters each.
  The helper prints nothing; exit code 0 done, 3 a profile could not be flattened, 2 called
  wrongly. It writes all files of a request or none.
- A profile is used only as what it is. The flattened profile must say `type` equal to the role
  it was asked for (`machine`, `process`, `filament`) and `instantiation` true: a base profile
  or a profile of another role in the wrong folder is `profile_unusable` (helper code
  `not_a_preset`). A process or filament profile that lists compatible printers without the
  machine (`compatible: "no"`) is `profile_unusable`; one that lists none gives the warning
  `profile_compatibility`.
- What the printer is told around and between the layers comes from the machine profile only.
  The slicer loads the process settings after the machine's and the filament settings after
  both, so in a process profile no key ending in `_gcode` is accepted, and in a filament
  profile none but `filament_start_gcode` and `filament_end_gcode`. `post_process` (a program
  the slicer would run) is accepted in no profile. Such a key with content is
  `profile_unusable` (helper code `forbidden_key`, naming the profile and the key); one without
  content (`null`, blank text, a list of those) is left out of the flattened file, listed in
  `dropped` and reported in the warning `profile_keys_dropped`.
- Limits of the helper: a profile is found by comparing its name with the file names present in
  the role's folder (three levels deep), never by building a path; names with `/`, `\`, `..`, a
  leading dot or a control character are refused; symbolic links are not followed at any level:
  a linked profile file, a linked role folder (`machine`, `process`, `filament`), a linked
  request root and a linked vendor folder are errors, and the vendor folder must lie directly
  in the profile folder; 1 MiB per file, nesting 16, 64 files and 16 templates per profile, 4096
  keys, 4 MiB per flattened file; a circle through `inherits` or `include` is an error. Values
  are carried as the bytes of the file: nothing is reformatted, interpreted or executed.
- On top, Hotend sets only items of the keymap (below): the plate type of the job
  (`curr_bed_type`, always), the `overrides` of the process preset in the table (keys of process
  rows of the keymap with a measured value; no preset sets one today), and with print settings
  the plan's values. The nozzle inventory goes into the machine profile for a project export
  only, with the values of the table compiled into the helper; no other machine key is ever
  written. Every value that was set is named in the warning `profile_overrides` of the result.
- A missing profile is `profile_not_found`, naming the profile, the folder and similar names
  found there; anything else is `profile_unusable`. A name with `verified = false` gives the
  warning `unverified_profile_names`.

**Print settings: the keymap.** `crates/hotend-slicer/profiles/keymap-bambu-studio.toml`
(`schema`, `revision`, `measured_on`, one `[[map]]` per key: `field`, `role` (`process` or
`filament`), `key`, `shape` (`scalar`, `per_filament`, `per_variant`), `form` (`integer`,
`decimal2`, `percent`, `flag01`, `enum` with `tokens`, `colour`, `plate`), `op` (`set` or
`cap`), `absent` (`refuse` or `add`), `min`/`max`, `verified`, optional `verified_texts`,
`zero_is_off`, `plate`, `enabled`) says which key carries each field of `hotend-settings` and how
a value is written (`hotend_slicer::keymap`). The loader refuses a row whose field is unknown or
carried by no key, a machine row, a key that is not lower-case ASCII letters, digits and `_` or
that is on the constant deny list `keymap::DENIED` (G-code, scripts, paths, files, hosts, tokens,
names, ids, identities, compatibility, machine limits), a range other than the field's hard range,
a form that does not fit the field, an enum token outside `^[A-Za-z0-9_()]{1,32}$` or a table
that does not give every field token one, `absent = "add"` for a key outside
`keymap::ADD_ALLOWED` (`curr_bed_type`, `brim_type`, `filament_colour`: measured absent and
working when added), and a table that leaves a mapped field (or a plate's two bed rows) without a
row. The rows: the plate (`curr_bed_type`), brim kind, width and gap, skirt, elephant foot, first
layer height, line widths, speeds and acceleration, the two bed temperatures of each plate, the
two nozzle temperatures, part fan delay, auxiliary fan on the first layers and its limit, walls,
infill density and pattern, seam, supports (on, kind, angle, where), the motion limits, the jerk
rows (reserved, `enabled = false`), the prime tower and the filament colour. Verified are only the
keys the measured real slices changed and echoed: `curr_bed_type`, `brim_type` (`outer_only` and
`no_brim`; `no_brim` measured on 18 November 2026 with the Linux command line 02.09.01.70: the
file's settings say `no_brim` and no layer has a brim feature), `brim_width`, `brim_object_gap`, `skirt_loops`, `initial_layer_speed`,
`initial_layer_infill_speed`, the two Textured PEI bed keys, `close_fan_the_first_x_layers`,
`first_x_layer_fan_speed`, `additional_cooling_fan_speed`, `wall_loops`, `enable_support` (`0`
only), `enable_prime_tower` (`0` only) and `filament_colour`.
- `Keymap::items(settings, bed)` turns every field that is not "as the installed profile has it"
  into items `{field, role, key, op, text, absent}`; `text` comes only from fixed formatters (a
  whole number, a decimal with at most two places, a percentage with its sign, `0` or `1`, a
  token of the row's table, `#RRGGBB`, the plate's `slicer_value`). The bed rows of other plates
  give nothing, and while supports are off the support kind, angle and where are not written.
  `Keymap::items_for(settings, bed, base)` resolves the items against installed values the caller
  holds; the helper does the same on the real profiles.
- The helper checks every item again against the keymap compiled into it: the key must be an
  enabled row of the item's role and pass the deny list, the operation and the absent policy must
  be the row's, and the text must have the row's form and lie in its range
  (`setting_key_denied`, `setting_out_of_range`). `set` writes the text into every entry and
  keeps the length; `cap` lowers each entry to at most the value and raises none, an entry that
  is not a plain number is `setting_not_numeric`, an entry of 0 is `setting_not_applied` unless
  the row says 0 is off. An installed value of another shape is `setting_shape_mismatch`, a
  missing key with `absent = "refuse"` is `setting_key_missing`.
- A row that is not verified, or a value outside its `verified_texts`, is used only when the
  asked value equals what the installed profile holds: then nothing is written and the key is
  reported `unchanged`. Any other value is `setting_unmeasured`, whose message names the field
  in plain words, the asked value, and that it has never been measured with this slicer. Nothing
  is run. (The recipe of the test slices, level 1 with a 5 mm brim, uses only verified rows or
  stock values; a plan that changes an unmeasured setting, such as the gauge's outer wall limit,
  is refused until the setting is measured. A plan without a brim is measured and runs.)

**The proof** (`hotend_slicer::proof`). After a real slice with print settings, the sliced file
is read once more as hostile input and judged against the settings; a file that does not pass is
an error with the proof's code and a sentence made of field names and parsed numbers, and
nothing of the run is a result. A pass adds the warning `print_settings_proven`. The marker
words, fan words and the closed feature list are printer-family facts kept in
`crates/hotend-slicer/profiles/bambu-gcode-facts.toml` (`layer_marker` `CHANGE_LAYER`,
`feature_prefix` `FEATURE:`, the settings block markers, part fans `M106` without `P` and `P1`,
auxiliary fans `P2` and `P10`, chamber fan `P3`, `tower_word`, `brim_feature`, and the
first-layer features `Brim`, `Outer wall`, `Inner wall` and `Gap infill` bound to the first
layer speed and `Bottom surface` bound to the infill speed; `Gap infill` was measured on 18
November 2026 filling first-layer gaps at the wall speed); they stand here and not in the
printer layer's table because this crate does not depend on that layer.
1. Completeness first (`proof_incomplete`): the whole file was read, the settings block ended in
   the bytes kept for it, every enabled keymap key is stated once and not dropped for length (the
   scanner records the key of a value it drops), three layer markers and an extruding move on
   layer 3 were seen, every extruding move of layer 1 carries a listed feature and a commanded
   feed rate, and the header lists the filaments.
2. The echo, per enabled row: every entry (one per extruder for a six-entry key) parses with the
   row's form and lies in the field's hard range (`profile_value_out_of_range`; a token Hotend
   does not know is `unreadable`); a field that was asked equals the asked value, a limit is at
   most the limit and one of the values the helper wrote (`setting_not_applied`); an unwritten
   sticking field states the STK-1 baseline of `hotend-settings` (elephant foot 0.15, first layer
   height 0.20, line width 0.50, acceleration at most 500; `setting_not_applied`).
3. The measured facts, from the commands with the state tracked from the first line: the plate
   in the settings block (`bed_type_not_applied`); exactly filament 1
   (`unexpected_filament_change`); the bed target at every extruding move of layer 1 and at the
   first of layer 3 within 1 C of the two bed fields (`bed_temperature_not_applied`); the part
   fan at 0 and the auxiliary fans at most the asked percentage at every move of layer 1, and the
   part fan first on after `cooling.part_fan_off_layers` layers (`fan_on_first_layer`); the feed
   rate of every layer 1 move at most the first layer speed (wall-bound features) or infill speed
   (fill-bound) plus 0.5 mm/s (`first_layer_too_fast`); brim moves on layer 1 exactly when a brim
   is asked (`brim_missing`, `brim_unexpected`); no prime tower feature (`unexpected_prime_tower`).
   The chamber fan during layer 1 is reported, never judged.
`ProofReport` is `{status: passed | failed | incomplete, code, missing, echo: [{field, key, asked,
echoed, ok, code}], facts: [{id, asked, measured, ok, code}]}`: parsed numbers, flags, Hotend's
tokens, ids of its tables and codes only; no text of the file is copied into it.
`proof::check_sliced_file(path, plate, &Expected)` judges a file Hotend did not make the same way.
On a test slice of a holder the proof of that recipe passes (bed 65 then 60 C, part fan from layer
4, brim and walls at 20 mm/s, bottom at 30 mm/s, chamber fan 127 of 255) and a stock plan fails.

**The editable project.** `Slicer::export_project(&SliceJob) -> Result<ProjectExport,
SlicerError>` (a real Bambu Studio and print settings only; the stand-in answers
`project_export_failed`):
1. The job is set up as for a slice, with the nozzle inventory added to the machine profile.
2. Run A writes `<job>/output/project.3mf` without slicing. A failed run or no project is
   `project_export_failed`.
3. `project::fill_difference_list` replaces only the text inside the three empty strings of
   `different_settings_to_system` in `Metadata/project_settings.config`: process, filament 1,
   printer, each the sorted, `;`-joined keymap keys the helper reported as changed.
   `filament_colour` is not listed and the program's mirrors of the two auxiliary fan keys
   (`optional_cooling_fan_speed`, `first_x_layer_optional_fan_speed`) are listed beside them
   (`project::NOT_LISTED`, `project::LIST_MIRRORS`), as a project saved from Bambu Studio's own
   window lists them; the printer entry stays empty. Every other entry must come out
   byte-identical uncompressed and in the same order, and the settings file equal as JSON but
   for that list; anything else (a list that is not three empty texts, a key twice) is
   `project_export_failed`.
4. The filled project is written to `<job>/proof/input/project.3mf`, its SHA-256 held in memory
   and checked against the file before run C. Run C slices it alone in `<job>/proof` (no
   profiles there, no settings option). A run that fails is `project_not_self_contained`; its
   sliced file goes through every check of a slice and the proof.
5. The project is returned only when the proof passed and the file still has the hash:
   `ProjectExport {project, sha256, keys: {process, filament, printer}, proof, result}`
   (`result` is run C's slice). `input.rs` still refuses every 3MF that is more than a plain
   model as a slicer input; the project is admitted to run C only by the hash this call computed.

**Command lines** (the wiki page "Command Line Usage" and the measured run for Bambu Studio, the
program's `--help` for PrusaSlicer):

```
bambu-studio --debug 3 --slice <0|n>
    --load-settings "<job>/profiles/machine.json;<job>/profiles/process.json"
    --load-filaments "<job>/profiles/filament_1.json;<job>/profiles/filament_2.json"
    --filament-map-mode=Manual --filament-map=2,1
    --arrange <0|1> --outputdir <job>/output --export-3mf plate_<n>.gcode.3mf
    <job>/input/model.3mf
bambu-studio --debug 3 --load-settings ".." --load-filaments ".." [map options]
    --arrange 0 --outputdir <job>/output --export-3mf project.3mf <job>/input/plate.3mf
bambu-studio --debug 3 --slice 0 --arrange 0 --outputdir <job>/proof/output
    --export-3mf sliced.gcode.3mf <job>/proof/input/project.3mf
prusa-slicer --export-gcode --datadir <job>/slicer-config --loglevel 2
    --load profiles/machine.ini --load profiles/process.ini --load profiles/filament_1.ini
    [--dont-arrange] --output <job>/output/plate_1.gcode input/model.step
```

(The input is whatever the job names; the core's backend passes `input/plate.3mf`.)

For Bambu Studio every path is absolute, as in the measured run; a job folder with `;` in its
path is `invalid_job`. Plate 1 is asked for as `--slice 0` (all plates of a file that holds
one), the measured form. `--arrange` is always stated, because without `--arrange 0` the program
moves the parts (measured). The plate type is always stated, because the program's own default
is the Cool Plate with a 35 C bed (measured): as `curr_bed_type` in the flattened process
settings. The option `--curr-bed-type=` is not passed (`[bed_type_option] use = false`):
the Linux build of 02.09.01.70 took the plate from the process setting alone (18 November
2026). Which plates count as measured is what the plate rows of `system-profiles.toml` say:
`cool` and `textured-pei` are `verified = true`; `high-temp` and `engineering` are
`verified = false`. Only the stand-in's stored demo profiles, which carry no plate,
still get the option. The second line is run A of a project export (no `--slice`), the third
run C (the project alone, no settings option). The working directory is the job folder, so a file
the program drops there stays inside the job (`--info` was seen to write its `result.json`
there). Nothing the program prints is used. PrusaSlicer jobs have one plate and one filament in
this version.

The measured command line was `--debug 3 --slice 0 --load-settings <machine;process>
--load-filaments <filament> --arrange 0 --outputdir <dir> --export-3mf plate_1.gcode.3mf
<input>`; without Hotend's additions the command line built here is that one, word for word
(unit test `the_measured_part_of_the_bambu_studio_command_line_is_unchanged`). Of the additions
the two filament map options (names and values from `printers.toml`) were accepted in real runs
with one filament; two filaments, `--arrange 1` and a plate number other than 0 are not
confirmed. Each has a `verified` flag in its data file or is listed by the code. The real
program says nothing when it fails, so when a real run fails, reports an error or leaves no
`result.json`, the error message lists the parts of that run no real run has confirmed (and the
profile names with `verified = false`) as the first suspects.

**result.json (Bambu Studio).** The program's only account of a run is `<job>/output/result.json`.
It is read before the sliced file, as hostile input (a regular file inside the job folder, at
most 1 MiB, every field type-checked, unknown fields ignored): `return_code`, `error_string`, and
per entry of `sliced_plates` the `id`, `total_predication` (seconds), `filaments[].{id,
filament_id, total_used_g}` and `warning_message`. `sliced_plates` is absent after `--info`,
`objects` is absent after `--arrange 0`; the `bbox` of an object is not the printed size and is
not read. No `result.json` after a run that exited with 0 is the error `result_missing`; a
`return_code` other than 0 is `slicer_failed` with the slicer's words, whatever the exit code; a
file that is not such a report is `hostile_output`. After a success the report is compared with
the sliced file: a total time or filament weight that differs by more than rounding gives the
warning `summary_disagrees` (the file's numbers are used), a plate without an entry
`result_incomplete`, a `warning_message` the warning `slicer_warning`.

**SliceResult.** `{ "status": "ok", "output": "output/plate_1.gcode.3mf", "sha256", "time_s",
"filament": [{"slot", "grams", "metres", "nozzle_id"}], "layers", "bed": {"id", "plate_type",
"temperature_c"} | null, "nozzle_temperature_c", "extent": {"x": [low, high], "y", "z", "source"}
| null, "warnings": [{"code", "message"}], "slicer": {"name", "version"} }`. `output` is relative
to the job folder. `time_s`, `layers`, `grams` and `metres` are `null` when the sliced file does
not state them, each with a warning (`time_unknown`, `layers_unknown`, `filament_use_unknown`,
`summary_missing`, `summary_ambiguous`, `slice_info_missing`, `slice_info_unreadable`); a value
is never estimated. `filament` has one entry per slot of the job; `nozzle_id` is the device
extruder id the job assigned.
- `bed` (Bambu Studio; `null` for PrusaSlicer) is the plate the file was sliced for: Hotend's
  `id`, the slicer's name for it and the highest bed temperature the file commands. The file
  must state the plate the job asked for: another plate, or no statement at all, is the error
  `bed_type_not_applied`. The bed temperature the file commands is compared with the one its
  own settings give for that plate (the plate's `temperature_key` and that key with
  `_initial_layer`): more than 1 C above the higher one, or a commanded temperature in a file
  whose settings do not state the plate's, is the error `bed_temperature_not_applied`; more
  than 1 C below the lower one is the warning `bed_temperature_low`. A file with neither a
  command nor a setting carries `bed_temperature_unknown`.
- `nozzle_temperature_c` is the highest nozzle temperature the file commands, or `null`.
- `extent` is the box the file prints in, in bed coordinates. `source` is `gcode` (the box
  around the extruding moves) or `plate_metadata` (the outline the slicer recorded, for a file
  without moves). For Bambu Studio the box is checked after every slice. It must lie inside the
  reach of the nozzle that prints (inside the box around the reaches when the job uses several
  nozzles), or the slice is the error `out_of_reach`: the reach was checked on the parts before
  slicing, and the file is what the printer runs. With `arrange` off and `placed` extents the
  box must also cover the box of the placed parts to within 3 mm and must not reach more than
  15 mm beyond it (room for a brim and a skirt), or the slice is the error
  `positions_not_kept`. With more than one filament a prime tower stands apart from the parts,
  so there the excess is the warning `printed_outside_parts` instead. A file that does not
  show where it prints cannot be compared: with `arrange` off and `placed` extents that is the
  error `positions_unknown`, and after a real program only the G-code counts (the recorded
  outline is the slicer's own claim, accepted from the stand-in only). Otherwise a result
  without an extent carries `extent_unknown`.
- A file that states a filament density of 0, or a weight of 0 g for a centimetre or more of
  filament, was sliced with incomplete settings and is the error `unusable_output`.

Other warning codes: `print_settings_missing`, `print_settings_proven`, `process_from_plan`,
`material_from_plan`,
`fake_slicer`, `slicer_version`, `slicer_version_mismatch`, `demo_profile`,
`profile_compatibility`, `profile_keys_dropped`, `profile_overrides`, `unverified_profile_names`,
`reach_unchecked`, `unverified_printer_facts`, `bed_temperature_low`, `printed_outside_parts`,
`filament_map_unconfirmed` (the sliced file does not report the map),
`unexpected_filament`, `summary_disagrees`, `result_incomplete`, `gcode_scan_incomplete`,
`slicer_warning` (a warning the slicer itself recorded). A failed slice is an error with a
`code`, not a result: `invalid_job`, `unknown_printer`, `out_of_reach`, `material_not_allowed`,
`slicer_not_installed`, `slicer_version_refused`, `profile_not_found`, `profile_unusable`,
`input_format`, `input_refused`, `bed_type_not_chosen`, `sandbox_unavailable`, `slicer_timeout`,
`slicer_failed`, `result_missing`, `bed_type_not_applied`, `bed_temperature_not_applied`,
`positions_not_kept`, `positions_unknown`, `unusable_output`, `incomplete_result`,
`hostile_output`, `io`, `internal_data`, `nozzle_not_supported`, the mapping codes
`setting_unmeasured`, `setting_out_of_range`, `setting_key_denied`, `setting_key_missing`,
`setting_not_numeric`, `setting_shape_mismatch`, `setting_not_applied`, the proof codes
`proof_incomplete`, `profile_value_out_of_range`, `setting_not_applied`, `bed_type_not_applied`,
`bed_temperature_not_applied`, `fan_on_first_layer`, `first_layer_too_fast`, `brim_missing`,
`brim_unexpected`, `unexpected_filament_change`, `unexpected_prime_tower`, and for a project
`project_export_failed`, `project_not_self_contained`.

**Reading the output.** The sliced file must be a regular file inside the job folder, not reached
through a symbolic link, with one hard link, non-empty and at most 4 GiB; it is hashed and parsed
through one open handle.
- Bambu Studio: the `.gcode.3mf` is a zip. Limits: at most 2048 entries and an 8 MiB central
  directory (checked before the archive is parsed), 4 GiB declared per entry, 8 GiB in total,
  200:1 declared compression for entries over 1 MiB; no entry name that is absolute or contains
  `..`, `\`, `:` or a control character; no duplicate names, overlapping entries, encrypted
  entries or symbolic links. Nothing is extracted to disk. `Metadata/plate_<n>.gcode` must exist.
  The G-code is believed first, because it is what the printer runs. It is read in one pass,
  lines over 16 KiB skipped. From the first 1 MiB: the header comments (`; BambuStudio <version>`,
  `; model printing time: ..; total estimated time: ..`, `; total layer number: N`,
  `; total filament length [mm] : ..`, `; total filament weight [g] : ..`, `; filament: <ids>`,
  `; filament_density: ..`) and the settings block of `; key = value` comments (`curr_bed_type`
  and the plate's temperature key, `nozzle_temperature`). From the commands: the highest `S` of
  `M140`/`M190` (bed) and `M104`/`M109` (nozzle), and the box around every `G0` to `G3` move
  that extrudes while moving in X or Y, arcs over their whole path, between
  `; MACHINE_START_GCODE_END` and `; MACHINE_END_GCODE_START` when the file has those markers.
  A file that commands no temperature has the one of its settings block. Then, from
  `Metadata/slice_info.config` (at most 4 MiB, XML without DTD), for the plate whose `index` is
  the sliced plate: the reported filament map (`filament_maps`) and `<warning>` entries, and
  `prediction` (seconds) and `<filament id used_g used_m>` for what the header lacks; a number
  both state differently gives `summary_disagrees`. From `Metadata/plate_<n>.json` only
  `bbox_all` (`[min x, min y, max x, max y]`), as the extent of a file without moves. A reported
  filament map that differs from the job's is the error `hostile_output`.
- PrusaSlicer: the G-code is streamed; the summary comments `; filament used [mm] = `,
  `; filament used [g] = ` and `; estimated printing time (normal mode) = ` are read, and
  `;LAYER_CHANGE` lines are counted. A summary key that occurs twice with different values is
  unknown (`summary_ambiguous`). Binary G-code is `unusable_output`.

**Fake slicer.** The binary `hotend-fake-slicer` (`hotend_slicer::fake::locate()` finds it next
to the running program) accepts both command-line styles above, reads STL and 3MF meshes and
writes a file of the same shape. Its G-code is comments only, and the file says of itself only
what it does: in the Bambu Studio style the header and `slice_info.config` state 0 s, no
filament and no layers (there is no `; total layer number` line), the archive carries
`Metadata/plate_<n>.gcode.md5`, and the settings block states the `printer_model` and
`nozzle_diameter` of the machine settings it was given next to the plate. So the G-code check
of section 9 passes the file as one that prints nothing (warning `no_moves`) without any rule
of its own for the stand-in. Time, filament, layers and height estimated from the mesh volume
and height stand apart, in a labelled comment block (`; HOTEND_ESTIMATE_BLOCK_START` to
`; HOTEND_ESTIMATE_BLOCK_END`: `estimated time [s]`, `estimated layer number`, `estimated
filament length [mm]`, `estimated filament weight [g]`, `estimated height [mm]`). The adapter
reads that block only when the installation is the stand-in (`installation.fake`) and then
reports the estimates as the slice's time, filament and layers; in a file of any other
program the block is ignored. `result.json` carries the estimates as before. It is used only
when configured as an explicit executable,
is reported as `Hotend fake slicer (<kind> command line)` and every result carries the warning
`fake_slicer`. With a machine profile that has several nozzles it refuses to run without the
manual filament map. In the Bambu Studio style it behaves as the real program was measured to,
so that a regression on Hotend's side fails a test: it prints nothing, also when it fails; it
writes `result.json` in the real shape into the output folder (into the working directory for
`--info` or without an output folder), with the `return_code` and `error_string` of a failure;
it moves the parts to the middle of the bed unless told `--arrange 0`, and lists `objects` only
then; it does not resolve `inherits` or `include`, so a filament file without a density of its
own gives 0.00 g and an empty filament id; and it slices for the Cool Plate unless
`curr_bed_type` (option or process setting) says otherwise, stating the plate and its
temperature in the file. It is given the built-in demo profiles, or flattened system profiles
when `SlicerConfig::system_profiles` is set.

The core does not depend on `hotend-slicer`. It defines the seam, `hotend_core::SliceBackend`:
`fn slice(&self, SliceRequest) -> BoxFuture<Result<SliceSummary, SliceError>>` and
`fn installations(&self, executables) -> Vec<SlicerInstallation>` (default: none).
`SliceRequest` is `{project_id, version, plate_file (the prepared plate's 3MF,
versions/<n>/prepare/plates/<i>/plate.3mf, read-only), pieces: [{name, nozzle_id, min, max}]
(what lies on that plate, in bed coordinates), output_dir (an existing folder,
versions/<n>/slice/<profile-hash>/), profile, plate (the plate's number in the preparation),
printer_id, material, plate_type (the build plate chosen in the settings, an id, or `null`),
slicer_executables (the programs chosen in the settings, by slicer kind),
print_settings (the plate's PrintSettings from its print plan, or null),
nozzle_diameter_mm (the fitted nozzle)}`. `SliceSummary` is
the `SliceResult` above with `output` relative to `output_dir` and without
`nozzle_temperature_c` and `extent`; its `bed` is `{id, plate_type, temperature_c} | null`
(a summary stored before the field existed reads as `null`); unknown fields are rejected. `SliceError` is `NotConfigured` or `Failed {code, message}`. The
application injects a backend with `Core::set_slice_backend` (`hotend_slicer::install(&core,
Programs::Installed | Programs::StandIn)` does it); until then the `slice` and `propose_print`
tools and `slice_run` answer `not_configured`. The core checks that `output` is a regular file
inside `output_dir` (same rules as section 1), hashes it itself and refuses a result whose
`sha256` differs (`hash_mismatch`). `propose_print` only writes a pending print job row; what
can happen to it afterwards is section 8.2, and none of it is reachable from a tool.

Slicing consumes the plate, never the model: `slice_current` lays the version out first unless
a stored preparation is for the printer and material in the settings (a stale one is redone
with its own options), then hands the backend the plate's 3MF.

`hotend_slicer::CoreSliceBackend` implements the seam. It uses `output_dir` as the job folder,
copies the plate to `output_dir/input/plate.3mf`, slices one filament slot (profile id
`material.id`, family `material.family`) on the printer's main nozzle with `profile` as the
process profile, tells the slicer not to arrange (`arrange: false`; the slicer-side plate is
always 1, because a plate file holds one plate) and runs the reach check on the pieces where
they lie (`placed` extents). A plate with a piece for another nozzle is refused (`invalid_job`)
instead of being printed with the wrong one. `CoreSliceBackend::from_config` looks for the
programs itself and makes a new `Slicer` when the settings' executables change;
`CoreSliceBackend::new(slicer)` always runs the given one. The backend slices for the plate of
the request (`plate_type`, the setting of the same name; passed on as the job's `bed_type` for
a Bambu Studio printer and left out for others). Until the settings name a plate, a slice with
a real Bambu Studio fails with `bed_type_not_chosen`, and the stand-in slices for its own
plate. The request's `print_settings` and `nozzle_diameter_mm` become the job's (the core fills
them from the plate's print plan and the printer setting). `CoreSliceBackend::export_project(request)
-> Result<ProjectExport, SlicerError>` makes the editable project of the same request into its
`output_dir`. Through the seam it is `SliceBackend::export_project(request) ->
BoxFuture<Result<ProjectExportSummary, SliceError>>` (default `NotConfigured`; the stand-in
answers `project_export_failed`): `ProjectExportSummary {project, sha256, keys: {process,
filament, printer}, slice, proof, stand_in}`, where `project` (the `.3mf`) and `slice.output`
(run C's sliced file, `proof/...`) are relative to `output_dir`, `keys` are the three lists of
`different_settings_to_system` as the project file holds them, `slice` is run C as a
`SliceSummary`, `proof` is `passed` and `stand_in` is true for a stand-in's answer. The core
never uses an answer that is a stand-in's, not proven, for another plate, or whose files are
not regular files inside `output_dir` with the reported hashes (section 8.2, "The editable
Bambu Studio project"). There is no environment variable for the
plate; `hotend_slicer::install` still takes
the profile folder from `HOTEND_BAMBU_PROFILES`. The plate the file was sliced for comes back
as `SliceSummary.bed`. For a real slicer the backend also says which plate and bed temperature the file is for in the
warning `bed_plate`.

**What the slice record keeps for the print.** The `job` of a `SliceRecord` holds, next to
the plate's number, the printer, the material and the pieces: `plate_type`, the id of the build plate, and
`nozzles`, `[{id, diameter_mm}]`. `plate_type` is set only when the settings named a plate
and the backend answered that the file was sliced for the same one (`bed.id`); a backend
that answers another plate fails the slice (`bed_type_not_applied`), and without a plate in
the settings the field is `null`, whatever the stand-in sliced for. `nozzles` are the nozzle
sizes of the core's printer profile (section 1) when the version was sliced. The review of a
print (section 8.2) declares the job from these two, never from what the file says.

`SlicerInstallation` is `{kind, name, path, found, version, stand_in, verdict: "allow" | "warn"
| "refuse" | "missing", reason, fix, usable, printers}`: one row per slicer kind the build
knows, also when the program is not installed (`missing`, with the fix), and the printers whose
profile names that slicer. It is what `slicers_list` and the doctor report return.

One limit of the seam as it stands: `SliceSummary` cannot carry `null`. A result with an unknown
time, layer count or filament use is returned to the core as the failure `incomplete_result`.

## 7. Sandbox launcher

```
hotend-sandbox run --profile <engine|agent|slicer> --job <dir>
    [--ro <path>]... [--env <NAME>]...
    [--wall-seconds N] [--cpu-seconds N] [--memory-mb N]
    [--max-processes N] [--max-file-mb N] [--max-disk-mb N] [--max-open-files N]
    [--no-namespaces] -- <program> <args>...
hotend-sandbox probe          prints JSON: which mechanisms are available on this machine
```

Rust callers do not build this command line by hand: `hotend_sandbox::command(&policy, program,
args)` returns a ready `std::process::Command` (wrap it with `tokio::process::Command::from`). The
launcher is looked up next to the current executable. In a debug build `HOTEND_SANDBOX_BIN`
overrides the path (the tests of the other components use it); a release build ignores the
variable and says so on stderr, because whatever program it named would decide how AI-written
code is confined. `hotend_sandbox::confinement()` reports which launcher a child would get, or
that it would run unconfined; the core records it with every job (section 1) and `hotend
doctor` shows an override.

**Paths.** `--job` and every `--ro` path are absolute and must exist. The program starts with
`--job` as its working directory. It can write only below `--job`, and read only `--job`, the
`--ro` paths and a fixed list of system runtime paths (`/usr`, `/lib*`, `/bin`, a few files under
`/etc`, `/proc` and `/dev`; the list is in `docs/notes/sandbox.md`). For Python pass the virtual
environment and the interpreter's real install prefix as `--ro`; with the editable install used in
this repository also pass the `python/` directory.

**Environment.** The child's environment is built from nothing: `PATH=/usr/local/bin:/usr/bin:/bin`,
`HOME=<job>/.sandbox/home`, `TMPDIR`, `TMP` and `TEMP` `=<job>/.sandbox/tmp`, `LANG=C.UTF-8`, plus
each `--env NAME` copied from the launcher's environment when it is set there. `--env` refuses the
names above and any name containing `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `CREDENTIAL` or
`ACCESS_CODE`.

**Limits.** A flag that is left out takes the profile's default.

| Profile | wall s | CPU s | memory MB | processes | file MB | disk MB | open files |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `engine` | 120 | 480 | 4096 | 512 | 1024 | 2048 | 1024 |
| `agent` | 3600 | 3600 | 2048 | 128 | 64 | 256 | 256 |
| `slicer` | 900 | 3600 | 8192 | 512 | 4096 | 8192 | 1024 |

`--memory-mb` limits both the address space of each process and the resident memory of the whole
tree. `--max-processes` counts tasks, so threads count; a tree that reaches it is killed (exit
123). `--cpu-seconds` is per process.
`--max-file-mb` is the size of one file; `--max-disk-mb` is everything in the job folder
together (each file counts for the larger of its length and the space it occupies), which the
launcher adds up while the job runs. The job folder may also hold at most
100 000 entries, nested at most 64 folders deep. The check is periodic (every 100 ms, or four
times as long as the last walk of the folder took, whichever is longer), so a job can overshoot
the limit by what it writes between two checks.

**The process limit is enforced by the kernel**, in every mode in which the launcher runs a job;
the launcher's own count, taken every 10 to 100 ms, is the second layer that kills a tree at
the limit. Which mechanism applies depends on who runs the launcher, and the probe names it
(`process_cap`):

| Launcher runs as | Mechanism | The kernel guarantees |
| --- | --- | --- |
| an ordinary user, with the namespace layer | `RLIMIT_NPROC` inside the job's user namespace | exact: never more than `--max-processes` tasks (one of them is the namespace's init process) |
| anyone, where the launcher can create a pids cgroup inside its own cgroup and the row above does not apply; the only way for root (uid 0 outside any container's user namespace) | `pids.max` of a cgroup per job: on a cgroup v1 pids hierarchy, or on cgroup v2 as a child of the root cgroup or as a threaded cgroup inside the launcher's own. Every job cgroup is tried with a `fork` that must fail before the job is put into it | exact: never more than `--max-processes` tasks. The two cgroup v2 layouts have not run on a machine yet (`docs/notes/sandbox.md`, section 2); where they do not work the row below, or the last one for root, applies |
| an ordinary user, without the namespace layer and without such a cgroup | `RLIMIT_NPROC` on the total of the user: the tasks the user runs outside the job + `--max-processes` + 1024, taken when the job starts and again every second | approximate: the job cannot push the user's task count above that number, nor into the last eighth of the user's own limit (`ulimit -u`); other programs of the user can always fork. The job can pass `--max-processes` by up to 1024 tasks (more if other programs of the user ended tasks within that second) until the launcher's count kills it. A `fork` in the job can fail with EAGAIN below the limit for up to a second after the user's other programs grew by more than 1024 tasks, and for longer when the user is near `ulimit -u` or the same numeric user id grows in another container; the launcher then prints `hotend-sandbox: the job cannot start another process or thread at the moment ...` on stderr, once. The job cannot change its own `RLIMIT_NPROC` in this mode (EPERM) |
| root, without such a cgroup | none | the launcher exits 125 and starts nothing |

The kernel counts three more things for the whole user account, and a job is kept from using
them up: inotify instances (a job gets none), queued signals and POSIX timers (1024), and POSIX
message queues (none). Two are not capped per job: epoll watches and pipe buffer memory
(`limitations` in the probe).

`--no-namespaces` skips the optional namespace layer; it exists so that the mandatory layers can
be tested alone. Without namespaces every guarantee of the probe still holds, with three
differences that the probe reports: for a user other than root the capability bounding set is
not emptied (the effective, permitted, inheritable and ambient sets are, and `no_new_privs`
keeps it that way); on kernels without Landlock signal scoping (ABI below 6) signals are
confined by seccomp instead: a process of the job may signal only the job's first process and
its own process group; and the process limit of an ordinary user is the approximate one of the
table above unless a cgroup carries it.

**Exit codes.**

| Code | Meaning |
| --- | --- |
| the program's code | the program ran and exited |
| 128+N | the program died from signal N (137 = SIGKILL, also from the CPU limit) |
| 124 | wall-clock limit reached; the whole process tree was killed |
| 125 | the sandbox could not be applied or the arguments were invalid; the program was not started. This includes a machine where no kernel mechanism can cap the number of processes (the table above), a launcher that could not find out whether one does (no `fork` works at all: the user or a cgroup around the launcher is at its process limit), and a user who has no room left under `ulimit -u` |
| 126 / 127 | the program could not be executed / was not found |
| 123, with `hotend-sandbox: process limit ...`, `hotend-sandbox: memory limit ...` or `hotend-sandbox: disk limit ...` on stderr | the launcher killed the tree for that limit |
| 128+N for SIGTERM, SIGINT, SIGHUP | the launcher received that signal and killed the tree; stop a job by sending SIGTERM to the launcher |

When the program exits, every process it left behind is killed before the launcher returns. The
same happens when the launcher itself dies, also from SIGKILL: a watchdog process that holds
nothing but a pipe to the launcher kills the job's process group when the pipe closes. So no
process of a job outlives its launcher, with or without namespaces. None of this, and no time
limit, holds against a kernel that has stopped scheduling the processes involved (a soft lockup;
seen once on a WSL2 machine): a launcher that outlives `--wall-seconds` by a wide margin means
the machine needs attention, not a retry. For as long as the job runs
the launcher holds the job folder and every `--ro` path open, which keeps Landlock's rules
attached on filesystems without stable inodes (9p, as WSL uses it for Windows drives). Launcher
diagnostics on stderr start with `hotend-sandbox: ` (the usage text does not). The codes are not authenticated: the
program can exit with 123 or 124 itself (the engine worker never passes either on).

**Profiles.** All three: no network, write only `--job`, read-only runtime, no privilege gain.
`engine` and `slicer` get no socket of any kind. `agent` gets one exception, because Python's
asyncio cannot create an event loop without it: anonymous `socketpair(AF_UNIX, SOCK_STREAM)` with
plain `send`/`recv` on it. It cannot create, bind, connect or accept any other socket and cannot
pass descriptors. Children of every profile talk to the core only over the stdio pipes they
inherit; every other inherited descriptor is closed.

**Inside the sandbox.** `chmod`, `chown` and `utime` succeed without effect; extended attributes
return `ENOTSUP`; symlinks, FIFOs and device nodes cannot be created and files in `--job` cannot be
executed; `/dev/shm` is absent, so Python `multiprocessing` locks and queues do not work (threads
and `subprocess` do); there is no user database and no name resolution; `inotify_init` fails with
EPERM, so a program that watches files has to poll; at most 1024 signals can be queued and 1024
POSIX timers exist at a time (EAGAIN beyond that).

**Probe.** `hotend-sandbox probe` exits 0 and prints one JSON object: `platform`, `backend`,
`usable` (false means every `run` will exit 125), `reason` (why not, with the fix for the user, or
null), `kernel`, `landlock_abi`, `seccomp`, `namespaces`, `namespaces_detail`, `root`,
`process_cap`, `guarantees` (for `network`, `filesystem`, `privileges`, `resources`,
`environment`: the mechanisms that provide it on this machine) and `limitations`.
`process_cap` says how the process limit is enforced: `default` and `without_namespaces` (what
a job gets with and without the namespace layer) each hold `mechanism`
(`rlimit-in-user-namespace`, `cgroup-pids`, `rlimit-on-user-total` or `none`), `enforcement`
(`kernel-exact`, `kernel-approximate` or `refused`) and `bound` (what that means, in words);
`rlimit_enforced` (whether the kernel applies `RLIMIT_NPROC` to the launcher's user; false for
uid 0 outside a container's user namespace, and false with the reason in `rlimit_detail` when the
launcher could not find out), `cgroup` (`cgroup v1 pids hierarchy`, `cgroup v2`, `cgroup v2,
threaded cgroups inside the launcher's own`, or null) and `cgroup_parent` (the cgroup in which
job cgroups are created, or null) and `cgroup_detail` (why no cgroup can be used). When
`default.mechanism` is `none`, `usable` is false. `hotend_sandbox::probe()` returns the same as
a struct.
Linux needs Landlock ABI 3 (kernel 6.2) and seccomp; macOS is implemented but untested and off by
default (`HOTEND_SANDBOX_MACOS_UNTESTED=1` turns it on; the library passes that one variable
of its own environment on to the launcher, which reads it, and to nothing else: the sandboxed
program's environment comes from the policy alone); Windows returns `NotSupported`. On
Windows every helper process is started with `CREATE_NO_WINDOW`, so a windowed application
opens no console window for it.

`HOTEND_UNSAFE_NO_SANDBOX=1` makes `hotend_sandbox::command` return an unconfined command, in
debug builds only, with a warning on stderr for every launch. Release builds ignore it. Every
job that ran that way has `detail.sandbox.mode: "off"` in the audit log (section 1), and
`hotend doctor` fails its `sandbox.read_only` check: it runs a sandboxed process that tries to
create a file in each folder the engine may only read, and reports any folder where that
worked. The same check warns when such a folder is on a 9p filesystem
(`hotend_sandbox::on_9p`).

## 8. UI <-> core (the facade)

Every UI command is an async method of `hotend_core::Core` (opened with
`Core::open(CoreConfig)`), named as in the table. The desktop application wraps each in a
Tauri command of the same name; `hotend` (section 8.1) calls the same methods, so every
feature is testable headless. Arguments and results are plain serde structs and enums (no
trait object crosses the facade); `Option` is `null`, a path is a string, enums are
snake_case strings or tagged objects as noted. The types are exported from the crate root.

| Command | Arguments | Result |
| --- | --- | --- |
| `projects_list` | | `Vec<Project {id, name, created_at_ms, updated_at_ms, current_version}>`, most recently changed first |
| `project_create` | `name` | `Project` |
| `project_get` | `project_id` | `ProjectDetail {project, versions, transcript}`; a transcript line is `{id, run_id, role, text, at_ms, remembered}` (section 1) |
| `versions_list` | `project_id` | `Vec<Version {project_id, number, created_at_ms, parent, origin, note, status, code_sha256, params}>`, oldest first |
| `version_get` | `project_id`, `number` | `VersionDetail {version, dir, result}` (`result` is the engine result, section 3.3) |
| `version_mesh` | `project_id`, `number` | the bytes of `mesh.json` |
| `version_revert` | `project_id`, `number` | `Version`: that version is the current one again; nothing is changed or removed, the next build continues from it |
| `questions_list` | `project_id`, `version: Option<u32>` (default: the current one) | `Vec<Question {requirement, text}>`: what the contract asks the user (section 4.1) |
| `measure_run` | `project_id`, `queries: Vec<JSON object>` (section 3.2 `measure`) | `Vec<JSON object>`, one answer per query, in order |
| `model_build` | `project_id`, `code`, `params`, `note` | `BuildReport {version: Option<Version>, result}`; the same path as the `write_model_code` tool |
| `parameter_set` | `project_id`, `name`, `value` | `BuildReport`; a refused value is a report without a version |
| `checks_run` | `project_id`, `checks: Option<Vec<String>>`, `sample_ranges` | the engine result of a check-only build of the current version |
| `views_render` | `project_id`, `views`, `section: Option<Section>`, `size` | `Vec<RenderedView {name, png}>` of the current version; as JSON `png` is base64 |
| `export_run` | `project_id`, `version: Option<u32>`, `formats`, `parts: Option<Vec<String>>`, `destination` folder | `Vec<ExportedFile {format, part, path, bytes, sha256}>` |
| `snapshot_create` | `project_id`, `name`, `version: Option<u32>` (default: the current one), `note` | `Snapshot {id, project_id, name, version, note, created_at_ms, printed_job_id, printed_sha256}` (below, "Snapshots"); refused (`invalid_input`) for a name taken in the project or not one plain line of 1 to 80 characters, a note over 500, (`not_found`) for a version the project does not have, (`no_model`) without a current version |
| `snapshots_list` | `project_id` | `Vec<Snapshot>`, oldest first |
| `snapshot_delete` | `project_id`, `name` | `Snapshot` as it was: the name is removed, its version and every file stay |
| `snapshot_restore` | `project_id`, `name` | `Version`: the snapshot's version is the current one again (`version_revert`); the later versions stay, and the next build has it as its `parent` |
| `snapshot_export` | `project_id`, `name`, `folder` (an existing folder, as an absolute path, that the person chose) | `SnapshotExport {snapshot, folder, files: [{kind: "step" \| "3mf" \| "printed" \| "manifest", source: "copied" \| "exported" \| "written", path, bytes, sha256}], printed: Option<PrintedFile {job_id, delivery, status, file_name, sha256}>, printed_missing: Option<MissingPrinted {job_id, sha256, message}>}`: the files written into a new folder `<folder>/<project> - <snapshot> (v<n>)/` (below); refused (`invalid_input`) when that folder already exists |
| `prepare_run` | `project_id`, `version: Option<u32>`, `PrepareOptions {copies: {part: n}, orientations: {part: "+x" \| "-x" \| "+y" \| "-y" \| "+z" \| "-z"}}` | `Preparation` (section 1): the version laid out for the printer and material in the settings; stored with the version, replacing an earlier one |
| `preparation_get` | `project_id`, `version` | `Option<Preparation>` |
| `slice_run` | `project_id`, `profile`, `plate: Option<u32>` | `SliceOutcome {record: SliceRecord, summary: SliceSummary, preparation: Preparation, output_path}` (`record.job` is `{plate, printer_id, material: {id, family}, pieces, prepared_at_ms}`: what was sliced, with the plate of the layout); prepares first when needed (section 6). Errors: `not_configured`, `failed_checks`, `no_plate`, the slicer's codes |
| `slice_get` | `project_id`, `version: Option<u32>` | `Option<SliceOutcome>`: the newest slice of the version as it is laid out now |
| `slicers_list` | | `Vec<SlicerInstallation>` (section 6): every slicer kind of the build, installed or not, with the printers it serves |
| `slicer_choose` | `kind`, `executable: Option<path>` | `Settings`: stores the program for that kind (when given) and, when the printer in the settings is sliced by another slicer, switches to the first printer this one serves (`prusa-slicer` -> `prusa-mk4s`) |
| `gauge_export` | `project_id: Option`, `cards: Option<Vec<CardRef {card, args}>>`, `nozzle_id: Option`, `destination` folder | `GaugeExport {gauges, plates, unplaced, warnings, files: [{plate, kind: "3mf" \| "view", path}]}`: the gauge plate of the given cards, or of the cards the project's current version uses |
| `calibration_get` | | `Calibration {printer_id, table: Option<JSON>}` (section 4.2) for the printer in the settings |
| `calibration_record` | `readings: Vec<GaugeReading {feature: "hole" \| "peg", drawn, measured}>`, `nozzle_id: Option` | `CalibrationUpdate {printer_id, material_id, nozzle_id, measured_at, changes, entry_provenance, table}`; `measured_at` is the core's clock (RFC 3339, UTC), never the caller's |
| `baseline_list` | | `Vec<ProvenBaseline {printer_id, plate_type, material_id, printed_on, source, values, recorded_at_ms}>`: the usable proven baselines of the data folder (below, "Proven baselines") |
| `baseline_invalid` | | `Vec<InvalidBaseline {printer_id, plate_type, material_id, problem}>`: the records of the file that cannot be used, and why (empty texts where the record names none) |
| `baseline_set` | `ProvenBaseline` (`recorded_at_ms` is ignored; the core dates the record) | `Option<ProvenBaseline>`: the record it replaced. Refused (`invalid_input`) for a value outside its field's hard range, a brim kind other than `none` or `outer`, a brim width that disagrees with the kind, a bed after the first layer hotter than during it, a level above 2, an unknown printer or plate, a day that is not a real `YYYY-MM-DD`, or a source that is not one printable line of 1 to 120 characters |
| `baseline_remove` | `printer_id`, `plate_type`, `material_id` | `BaselineRemoval {removed: Option<ProvenBaseline>, invalid: Vec<InvalidBaseline>}`: every record of the three is removed, the usable one and any that cannot be used; both empty when there was none |
| `baseline_remove_invalid` | | `Vec<InvalidBaseline>`: every record that cannot be used on its own (those `baseline_invalid` lists, a record without a printer, plate or material among them, but not the copies of a key recorded twice) is removed, and only those; usable records are written back as they were. Empty, and nothing written, when there was none |
| `printers_list` | | `Vec<PrinterInfo {id, name, model, serial, simulated, state: PrinterState, deliveries: Vec<DeliveryInfo>}>`. `DeliveryInfo {kind, name, enabled, disabled_reason, sends_file, starts_print, controls_print, live_status, reports_result, can, cannot, cost, needs}` lists only the ways this build can deliver, by the `kind` a `DeliveryChoice` names (`simulated_printer`, `bambu_connect`, `export_to_folder`), each in the words of its own `capabilities()` (section 9, "Delivery"). A way that is not built, such as direct control of a real printer, has no entry. `bambu_connect` has `enabled: false` and a `disabled_reason` on every host while the hand-off folder chosen in the settings cannot be used (missing, not a folder, under WSL not on a Windows drive, a comma in its Windows path), and under WSL while none is chosen |
| `print_propose` | `project_id`, `note` | `PrintJob` with `status: "pending"`, bound to the SHA-256 of the current version's sliced file |
| `print_jobs` | `project_id` | `Vec<PrintJob>`, newest first |
| `print_status` | `job_id` | `PrintJob {id, project_id, version, slice_sha256, note, status, delivery, created_at_ms, updated_at_ms, detail: {simulated, phase, headline, progress_percent, remaining_minutes, message, problems, findings, link, path}}` |
| `print_review` | `job_id`, `DeliveryChoice` (`{"kind": "simulated_printer"}`, `{"kind": "bambu_connect"}` or `{"kind": "export_to_folder", "folder"}`) | `PrintReview {job, delivery, simulated, printer, summary: PrintSummary, review_text, warnings, spools}`: the confirmation summary and the text a person confirms, both computed from the sliced file (section 8.2) |
| `print_confirm` | a `&ConfirmationAuthority` (not data: section 8.2), `job_id`, `shown`: the text the dialog or terminal displayed | `PrintJob` as delivered: `starting`, `handed_off` or `exported` |
| `print_cancel` | `job_id` | `PrintJob`: a pending job becomes `cancelled`; a job on the printer is stopped |
| `handoff_status` | | `HandOffStatus {host, chosen, folder, windows_folder, problem, opener, opens}`: the Bambu Connect hand-off as it is set up now |
| `print_project_make` | `project_id`, `plate: Option` | `EditableProject` (section 8.2) |
| `print_projects` | `project_id` | `Vec<EditableProject>`, newest first |
| `print_project_review` | `project_export_id`, `ProjectDestination` (`{"kind": "bambu_studio"}` or `{"kind": "folder", "folder"}`) | `ProjectReview {project, destination, file_name, handoff, review_text, warnings, notice}`; a folder destination is answered as its canonical path (on Windows without the `\\?\` prefix), and the review text names where the project goes. Refused before any dialog when the folder's path would not show as what it is on one line: a control character, any white space but the plain space (a tab, a line break, U+2028, U+2029, a no-break or ideographic space) or an invisible or bidirectional format character (U+200B to U+200F, U+202A to U+202E, U+2060 to U+206F, U+FEFF and the rest of Unicode's Cf) — `delivery_failed` for a folder, `handoff_folder` for the hand-off folder |
| `print_project_deliver` | a `&ConfirmationAuthority`, `project_export_id`, `shown` | `EditableProject` as handed over; refused (`project_stale`) when the plate's print plan changed since the review, and (`delivery_failed`, "review the project again") when the review text said that nothing opens the project and the opener now would, or the other way round |
| `audit_log` | `project_id: Option`, `limit` | `Vec<AuditEntry {id, at_ms, project_id, run_id, kind, name, args_sha256, outcome, detail}>`, newest first; without a project, every entry |
| `settings_get` | | `Settings {provider: Option<{kind, model}>, printer_id, material: {id, family, density_g_cm3}, max_turns, slicer_executables: {kind: path}, plate_type: Option<id>, effort, handoff_folder: Option<path>, previews}`. `previews` is where a sliced file's plate previews come from: `slicer` (the default: only what the slicer drew; the Linux Bambu Studio draws none headless, its GLFW being built for Wayland only) or `hotend_made` (when the slicer drew no `Metadata/plate_<n>.png`, Hotend's own renders of the plate from the layout, `prepare/plates/<n>/views/iso.png` and `top.png`, are scaled to Bambu Studio's sizes and put into the sliced file before it is hashed, checked and proven; the slice carries the warning `hotend_made_previews`, and a slice made so has a profile hash of its own). Every slice record's job carries the `previews` it was made with, so that the window can mark a slice made without Hotend-made previews while they are on as outdated ("Sliced before Hotend drew plate previews."); a record without the field counts as made with them only when it carries that warning. `plate_type` is the build plate on the printer (`cool`, `textured-pei`, `high-temp`, `engineering`); `null` until the person chooses one, and there is no default. `handoff_folder` is the folder a Bambu Connect hand-off places the checked file in (an absolute path; under WSL a folder on a Windows drive, required for a hand-off; elsewhere `null` means `<data-dir>/handoff`). Under WSL and on Windows its Windows path must hold no comma: Windows explorer, which opens a handed-off project, reads a comma in its argument as the end of the path, so such a folder is refused (`HandOffPathError::Comma`, `handoff_folder`). `effort` (`low`, `medium`, `high`, `xhigh`, `max`; `high` when the saved settings name none) is how much the model may think and spend on one answer. The egress writes it as `output_config.effort` into each generating request of an Anthropic run whose model Anthropic documents as accepting that level (the table in `crates/hotend-core/src/egress/effort.rs`, which the window's `providers.json` repeats and a test compares). For any other model name, for token counting and for every other provider the request carries no effort field and the provider's default applies; the agent cannot choose the level: an `output_config.effort` the agent wrote is removed from every request before anything is written (the rest of `output_config` stays; an `output_config` that held nothing else is removed whole rather than sent empty) |
| `settings_set` | `Settings` | nothing; refused (`invalid_input`) for an unknown printer, a limit out of range, a slicer path that is not absolute or a `plate_type` that is not a short lower-case id. `Settings.research` (`ResearchSettings`, docs/WEB_RESEARCH.md section 4) holds, besides the limits, `source` (`built_in` default, `keenable`, `tavily`, `off`), `acknowledged_at_ms` and `acknowledged_source` (the source the agreement was given for; `null` with an agreement from before sources existed means `built_in`; `off` is refused). `settings_set` never writes the agreement: the stored `acknowledged_at_ms` and `acknowledged_source` are kept whatever is sent (only `research_consent` / `research_consent_for` change them); a `source` changed here is therefore `needs_consent`. The desktop shell keeps the core's research settings whatever the window sends |
| `handoff_folder_check` | `folder` | nothing; refused (`handoff_folder`, with the reason) when the folder could not be used as the hand-off folder on the host set with `set_handoff_host`, as `handoff_status` would report it once the settings named it. Nothing is saved: a caller checks a candidate before it writes the settings |
| `provider_key_set` | `ProviderKind`, the key | nothing; write-only |
| `provider_key_delete` | `ProviderKind` | nothing |
| `provider_key_status` | `ProviderKind` | `bool`: whether a key is stored. No command returns a key |
| `research_status` | `project_id` | `ResearchStatus {state, message, search, fetch, searches_used, searches_cap, fetches_used, fetches_cap, searches_per_turn, fetches_per_turn, billed_searches, search_fee_micro_usd, result_bytes, result_bytes_cap, result_tokens_estimate, acknowledged, project, source, service_host, key_present, keyless}` (docs/WEB_RESEARCH.md sections 4 and 16). `state` is one of `on`, `off`, `needs_consent`, `budget_used`, `stopped`, `unsupported_provider`, `provider_refused`, `needs_key`. `acknowledged` is true only when the agreement covers the chosen `source`. `key_present` says whether a key for the chosen search service is stored (never the key; false for `built_in` and `off`). A key store that cannot be asked is `needs_key` (for Keenable too, with `keyless: false`: the keyless path is used only when no key is stored), with a `message` saying the key store could not be read; `message_send` still runs the message without research through the service |
| `research_source_options` | | `Vec<SearchSourceOption {source, available, reason, host, needs_key, key_present, key_from_environment, key_store, keyless, suggested, chosen, agreed}>` (`key_store`: where a typed key would be kept, `keychain` \| `file` \| `environment` \| `memory`; `environment` keeps none), one per source in the order `built_in`, `keenable`, `tavily`, `off`. With a provider that has no built-in search (`ollama`, `openai`, `google`, `openrouter`) `built_in` has `available: false` and a `reason`, and `keenable` is `suggested` |
| `research_source_set` | `SearchSource` (`built_in` \| `keenable` \| `tavily` \| `off`) | `Settings`. An agreement given for another source does not cover the new one: research is `needs_consent` until `research_consent_for` is called for it. Only a person's own action may call it |
| `research_consent` | `agreed` | `Settings`: records the agreement for the source chosen now, or withdraws any agreement |
| `research_consent_for` | `SearchSource`, `agreed` | `Settings`: records the agreement for that source; refused (`invalid_input`) when it is not the chosen source or is `off`. `agreed: false` withdraws any agreement. Only a person's own action (a terminal `yes`, the core's native dialog) may call it |
| `search_key_set` | `SearchService` (`keenable` \| `tavily`), the key | nothing; write-only, in the same secret store as the model key (`search-key.keenable`, `search-key.tavily`; with `HOTEND_SECRETS=env` read from `KEENABLE_API_KEY`, `TAVILY_API_KEY`) |
| `search_key_delete` | `SearchService` | nothing |
| `search_key_status` | `SearchService` | `bool`: whether a key is stored |
| `doctor_report` | `skip_self_tests` | `DoctorReport {ok, hotend, debug_build, checks: [{id, label, status, required, detail, fix}], sandbox, slicers, keys_from_environment}` (section 8.1; `keys_from_environment` lists the providers whose key the store in use reads from the environment, where it wins over a stored key: only with the environment and file stores); `hotend_core::doctor::report` gives the same without an open core |
| `message_send` | `project_id`, `text` | `MessageRun {run_id}`, a stream of `RunEvent`: `{"kind": "agent", "event"}`, then one of `{"kind": "finished", "stop_reason", "error"}` or `{"kind": "failed", "code", "message"}` |
| `run_cancel` | `run_id` | whether the run was still going |

**Errors.** Every command fails with `CoreError`; the interface receives `ApiError`
(`From<CoreError>`), a tagged enum of plain data: `{"kind", "message", "next_step"}` with
`kind` one of `not_found`, `invalid_input`, `not_configured`, `busy`, `no_model`, `secrets`,
`storage`, `internal`, or with a stable `code` as well: `engine` (the job codes of section
3.1), `slicer` (section 6), `agent`, and `print` (`not_pending`, `slice_changed`,
`plan_changed`, `gcode_rejected`, `not_reviewed`, `summary_mismatch`, `bed_type_not_chosen`,
`plate_changed`, `unsupported`, `no_spool`, `delivery_failed`, `handoff_folder`,
`project_stale`, `project_changed`, `not_importable`; with the linter's `findings` for
`gcode_rejected`). `not_importable` is a `bambu_connect` review or hand-off of a sliced file
that lacks an entry Bambu Connect needs (`Metadata/plate_<n>.gcode`, its `.md5`,
`Metadata/slice_info.config`, `Metadata/plate_<n>.png`): Bambu Connect's import dialog
accepts such a file and then imports nothing (seen with a headless Linux
slice, which has no plate preview). Nothing is placed. When the plate preview is missing
the words fit the settings now, and the error carries a stable sub-code `reason`: with
`previews` `hotend_made` and a slice record that does not say it was made with them,
`slice_before_previews`, "This slice was made before Hotend drew plate previews. Slice
again, then hand it off. ..." with the next step "Slice again, then hand it to Bambu
Connect." (the window offers `Slice again` in the refusal); with `previews` `slicer`,
`previews_off`, a message that says to switch on Hotend-made plate previews in Settings
and slice again. Otherwise there is no `reason` and the message names the missing entries.
Every `not_importable` next step ends with the short second line "Other ways: make the
editable Bambu Studio project, or save the file to a folder or a USB drive." `reason` is
left out of the JSON when there is none.
`slice_changed` is a sliced file that is missing or not the proposed one, or a version laid
out again after it was sliced; `plan_changed` is a job whose plate's print plan (the
settings, the spool, the nozzle or the proven baseline) changed after it was sliced (section
8.2). Both mean: slice again; the window says which. `message` says what happened and
`next_step` what the user can do; neither ever contains a key.

**Events.** `Core::events()` subscribes to one broadcast channel of `CoreEvent`, a tagged
enum: `{"channel": "run", "run_id", "project_id", "event": RunEvent}` for every event of every
agent run (the same values `message_send` streams), and `{"channel": "print", "job_id",
"project_id", "simulated", "event": PrintEvent}` with `PrintEvent` one of `{"type": "status",
"job": PrintJob}`, `{"type": "phase", "phase", "headline"}`, `{"type": "progress", "percent",
"remaining_minutes"}`, `{"type": "problem", "problem": {message, raw_code, meaning, blocking,
help_url}}`. A receiver that falls behind loses old events; `print_status` always has the
current state.

`Core::downgrade()` gives a `WeakCore` (`upgrade() -> Option<Core>`). `Core::tool_specs()` is
the agent's tool list and `Core::simulated_printer_wire_log()` what the simulated printer
received; both are for tests and the demo.

One run per project at a time (`busy` otherwise). Provider keys live in the OS keychain
(service `hotend`, entry `provider-key.<kind>`); with `HOTEND_SECRETS=env` set, the core
instead reads them from the providers' usual environment variables (`ANTHROPIC_API_KEY`,
`OPENAI_API_KEY`, `GOOGLE_API_KEY` or `GEMINI_API_KEY`, `OPENROUTER_API_KEY`) and cannot
store one.

**The file store (`HOTEND_SECRETS=file`).** For a machine without a keychain (WSL, a
headless Linux box) where keys should still be saved from Hotend. It is chosen only
explicitly; the keychain stays the default and `HOTEND_SECRETS=env` stays read-only. The keys
are kept in one JSON file, `HOTEND_SECRETS_FILE` (an absolute path) if set, else
`$XDG_CONFIG_HOME/hotend/secrets.json`, else `~/.config/hotend/secrets.json`. The core makes
the secrets folder, and any missing folder above it, with mode 700, and the file with mode
600. It opens the secrets folder once with `O_DIRECTORY | O_NOFOLLOW`, checks the open folder
(owner, mode), and opens, creates, renames and removes the file, its lock file and its
temporary files relative to that open folder (`openat`, `renameat`, `unlinkat`, each with
`O_NOFOLLOW`), so a folder swapped after the check cannot redirect a read or a write. It
replaces the file only atomically (a new file created exclusively in that folder, flushed,
renamed over it, the folder flushed), under an in-process lock and an advisory `flock` on
`.secrets.json.lock`, re-reading the file under the lock so that two Hotend processes never
lose each other's keys; temporary files a crashed writer left are removed under the lock. A
symbolic link is not followed at the secrets folder, the file, the lock file or a temporary
file; folders further up may be links (a linked `~/.config`, say). It refuses, without
changing anything: a file or secrets folder that other users could open (`chmod 600` /
`chmod 700` fixes it), that belongs to another user, or that is a link; a file with more than
one name (hard links); and a folder holding the secrets folder that other users can change
(`chmod go-w`), unless it is a root-owned sticky folder such as `/tmp`, or that belongs to a
user other than you or root. A file it cannot parse is reported as damaged without showing
its contents. The commands in these messages quote the path for the shell. Precedence: a key
in its usual environment variable (`ANTHROPIC_API_KEY`, `KEENABLE_API_KEY`,
`BAMBU_LAN_ACCESS_CODE`, ...) still wins for reading, so existing environment files keep
working; storing writes the file, and removing removes the file's entry only (`hotend key
delete` then says the variable is still read). `hotend key status`, `research_source_options`
(`key_from_environment`), `printer_lan_get` (`access_code`) and `doctor_report`
(`keys_from_environment`, for the model key) say which one a key comes from. Windows and other
systems without Unix permissions answer `unsupported`.

The file store is weaker than a keychain: any program running as the same user can read the
file, just as it can read an environment file holding the same keys. Use it where there is no
keychain, not instead of one.

`research_source_options` says for each source where a key typed in the window would be kept
(`key_store`: `keychain`, `file`, `environment` or `memory`; `environment` keeps nothing) and
whether the present key comes from the environment (`key_from_environment`);
`printer_lan_get` says the same of the access code (`access_code_store`). The window switches
the key fields off, with the reason and the way out, before anything is typed when the store
is `environment`.

**Print settings (`print_plan_get`).** `print_plan_get(project_id, version?, plate?)` returns
`PrintPlanView {project_id, version, plate, sha256, settings_source, blocked, decisions,
questions, warnings, notes, assumptions, checklist, facts, clearance_mm, nozzle_diameter_mm,
nozzle_size_set, revisions, baseline}` for plate 1 of the current version by default, preparing
the version first when it is not laid out for the settings. Each decision is `{field, value,
unit, source, rule, basis, sentence, envelope, asked, origin}`: `field` is Hotend's own name
(`adhesion.brim_width_mm`), never a slicer key; `value` `null` means "as the installed profile
has it". `source` may be `proven` (the proven baseline, rule `PRV-1`). `origin` says in words
where the value came from: "proven on your printer (<day>, <the person's label>)", "rule
<id>", "your settings" (the plate and spool the person set), "your choice", "the assistant's
choice", "the intent (<id>)", "print history (<id>)", "the fit calibration (<id>)" or "the
installed profile"; a value of the proven baseline never reads as a choice. `baseline` is
`{printer_id, plate_type, material_id, printed_on, source, fields}` when a proven baseline is
the floor of the plan (`fields`: the fields whose value it set because the rules alone were
less careful), else `null`. `blocked` is `{code, sentence}` (`bed_type_not_chosen`,
`spools_not_declared`, `facts_invalid`, `plate_material_conflict`, ...) and then there are no
decisions. `assumptions` say in fixed words what the plan takes for granted: the nozzle size
and the spool colour while they are not set, the balanced intent, the proven baseline when one
applies (`proven_baseline`), no print history and no calibration yet.
`settings_source` is "Hotend's print settings, plan <first 16 hex digits of sha256>", or "the
installed profiles as they are: the print plan is blocked (<code>)".

**Settings for the plan.** `Settings` gains `nozzle_diameter_mm` (0.2, 0.4, 0.6 or 0.8; `null`
until the person says, and 0.4 is then assumed) and `spool {slot ("1"; an AMS slot 1 to 16,
`external_main` or `external_auxiliary`), colour (`#rrggbb` or `null`: grey `#898989` is then
assumed and shown so), silk (false)}`. The spool's material is the settings' `material`. The
plan's other inputs: the plate is `plate_type`; the printer facts are the fitted nozzle size,
the largest layer height (0.7 of the nozzle size: 0.28 mm measured for 0.4 mm, the share a
judgment for the others), the bed limit (the lowest of the plate's, the machine's and the
filament family's in the printer data) and the slots (the AMS slots feed the main nozzle in
version 1, the outside holders the nozzle the printer data name). No slicer capabilities are
passed: the rules keep their verified defaults. The proven baseline of the settings' printer,
plate and material is an input as well (below).

**Snapshots.** A snapshot is a name for one version of a project: the maker takes the model
as it is at any time, keeps iterating, and later exports that state's files or goes back to
it. The store's `snapshots` table (migration 6) holds `id, project_id, name, version, note,
created_at_ms, printed_job_id, printed_sha256`. A name is one plain line of 1 to 80 characters (no control character, no
white space but the plain space, no invisible or bidirectional format character), unique in
its project without regard to ASCII case; a note is up to 500 characters on the same rules.
The row names an existing version of its project (a foreign key) and can never be moved to
another, nor its recorded print changed (a trigger); versions are immutable and never deleted, and the sweep never touches
them, so a pinned version and its model files stay exactly as they were. Deleting a snapshot
removes the row only.
- Printed versions. The printed file is decided when the snapshot is made: when a print job
  of the version has left Hotend by then (a delivery is recorded and the status is
  `exported`, `handed_off`, `starting`, `printing`, `finished`, `failed`, `not_started` or
  `cancelled`), the newest such job is recorded on the row as `printed_job_id` with its
  `slice_sha256` as `printed_sha256` (both null when the version had not been printed then).
  A later slice or print of the same version never changes what the snapshot exports, and a
  snapshot made before the version's first print has no printed file. Making a snapshot
  (`snapshot_create` and the agent's `snapshot_model` alike) keeps that file: the file with
  the recorded hash, looked for where the delivery placed it (the job's `detail.path`) and
  then in `versions/<n>/slice/<profile hash>/`, is copied (read without following a link, a
  plain file only) to `projects/<id>/snapshots/<snapshot id>/` and hashed again. An export
  uses exactly the recorded hash: the kept copy first, then the same original places (and
  keeps it then).
- Export. `snapshot_export` writes into a new folder `<folder>/<project> - <snapshot> (v<n>)/`
  (both names reduced to letters, digits and ` -_.,()+`; the part before ` (v<n>)` cut, on a
  character boundary, to at most 200 UTF-8 bytes and 200 UTF-16 units). It is refused
  (`invalid_input`: "already exists ... Choose another folder, or remove that one first")
  before anything is written when that folder, a file of that name, or an entry whose name
  differs from it only in letter case (two snapshot names such as `Lid*` and `Lid?`, or `Über`
  and `über`, can make the same folder) is already there, and the folder is made with a
  create that fails if it appeared in between. Inside it: `model.step` and `model.3mf`, each
  copied byte for byte from the version's own stored file where the version stores it
  (`versions/<n>/model.step` today; hash checked after the copy, `source: "copied"`) and made
  by `export_run` only for a format the version does not store (the 3MF today, `source:
  "exported"`; two exports of it differ in their random object UUIDs), the printed file when
  the snapshot recorded one (the recorded bytes, checked by SHA-256 after the copy; under its
  own name when that ends `.gcode.3mf` in any letter case and is a plain name, else
  `printed-<first 16 hex of the hash>.gcode.3mf`), and `snapshot.json` `{schema: 1, project:
  {id, name}, snapshot: {name, version, note, created, created_at_ms, printed_job_id,
  printed_sha256}, exported, files: [{name, kind, source, bytes, sha256}], printed,
  printed_missing}`. Every file is created new (never replacing one, never written through a
  link); sources are read without following a link and only when they are plain files.
  `folder` must be absolute and exist.
- Printed file missing. When the snapshot recorded a print and no file with its hash is found
  anywhere (kept copy, delivery place, slice folder), the export is not refused: it writes the
  model files and `snapshot.json`, and both the result's `printed_missing {job_id, sha256,
  message}` and the manifest's `printed_missing` say so (the message begins "printed file
  missing"); `printed` is null. The command line shows it as a warning on stderr.
- Going back. `snapshot_restore` is `version_revert` to the snapshot's version: the later
  versions stay (reachable by number and by their own snapshots), and the next build records
  the restored version as its `parent`, so `versions_list` shows where the line branched.
- Audit. `create` (with `by: "person"` or `"agent"`, and `printed_file`: the kept file's
  hash, null, or `"not found"`), `delete`, `export` (each file's kind, size and hash, the
  printed job or the missing one, or the error) and `restore` (with
  the version that was current) are `snapshot` entries with the snapshot's name and version.
- The shell offers `snapshot_create`, `snapshots_list`, `snapshot_delete`, `snapshot_export`
  and `snapshot_restore` (section 8.3); its `snapshot_export` takes only a folder the person
  picked in the session.

**Proven baselines.** A proven baseline records the sticking settings of a print the person
reported as good, for one printer, plate type and material: `adhesion.level` (0 to 2),
`adhesion.brim_kind` (`none` or `outer`), `adhesion.brim_width_mm`, both first layer speeds,
both bed temperatures, `cooling.part_fan_off_layers`, both auxiliary fan fields and
`shell.wall_loops`, each a Hotend field inside its hard range, never a slicer key. They are
kept per data folder in `proven-baselines.json`, `{"schema": 1, "baselines": [ProvenBaseline]}`
with the values keyed by field name, never in `Settings` and never shipped: a data folder
without the file has none, and the rules decide alone. The file is written whole (to a
temporary name, then renamed), holds at most 64 records, one per printer, plate and material,
and is checked whenever it is read. A file that cannot be read whole (not a plain file, too
large, not JSON of this layout, too many records) is an error (`invalid_input` naming the
file), never an empty list, so a broken file cannot make a plan less careful. Each record is
then checked on its own: one that cannot be used (a value out of its range, a printer this
build no longer has, a field it does not know, its printer, plate and material recorded twice)
is kept in the file as written, never taken by a plan, listed by `baseline_invalid` and shown as
a warning by `hotend baseline show`. It blocks only the plans of its own printer, plate and
material (`invalid_input` naming the record and how to remove it); plans of the others are
made as usual. A record that does not name all three (no printer, plate or material) applies
to no plan and blocks none; it stays in the file and is a warning like the others.
`baseline_remove` takes a record out by its printer, plate and material, `baseline_set` of the
same three replaces it, and `baseline_remove_invalid` (`hotend baseline remove --invalid`, the
command the warning for a record without a key names) takes out every record that cannot be
used on its own and never a usable one. A printer, plate and material recorded twice is not
taken out by it: each copy is a good record, so the warning names `baseline set` and
`baseline remove` with the key instead. `source` is the person's label for the print (one printable line of at most 120
characters, no invisible or reordering characters); it is shown beside the plan and never
reaches a sentence, a value or a file name. Setting and removing a record writes an audit entry
(`kind: "baseline"`, `name: "set"` or `"remove"`) with the key, the day and the values, not the
label (removing a record that could not be used writes `name: "remove_invalid"` with the key
and the problem). When a baseline exists for the printer, plate and material in the settings,
the plan of every plate takes it as its floor (docs/PRINT_SETTINGS.md, "Proven baselines"); the
plan's basis stores it (`PlanBasis.baseline`, left out when there is none, so earlier records
read as before), the room the layout keeps for brims follows the wider brim, and a slice made
before the baseline changed no longer counts as the slice of the layout. Every check of a plan
against the plate's current plan computes it with the baseline (the slice of the layout, the
print job's review and confirmation, the editable project's review and hand-over).

**Slicing with the plan.** `slice_run` (and the `slice` tool) computes the plate's plan, or
checks its stored record, before the slicer runs:
- A plan that is not blocked gives `SliceRequest.print_settings`; `nozzle_diameter_mm` is the
  fitted size. The slice folder's hash covers the profile, the plate, the plan's SHA-256 and
  the nozzle size, so a changed plan is a new slice.
- A blocked plan is refused with the plan's code and sentence when the backend would run a
  real slicer. Only the stand-in (every program it would run is the stand-in) slices it, with
  its own profiles, and its file can never become a print. A backend with no usable slicer is
  asked anyway and fails with its own reason. A result for a blocked plan that does not carry
  the stand-in's `fake_slicer` warning is refused with the plan's code.
- The slice record's job gains `plan_sha256` (`null` when the plan was blocked),
  `plan_blocked` (the code, or `null`), `nozzle_diameter_mm` and `nozzle_size_set`; every
  entry of `nozzles` carries the fitted size as `diameter_mm` (one size for every nozzle of
  the printer in this version), which is what a print job is declared and checked with.
- A stored slice made with a plan counts as the slice of the layout only while the plate's
  current plan has the same hash: changed settings make `slice_get` answer none and
  `propose_print` answer `no_slice` until the plate is sliced again.
- The slice report names the settings ("print settings: Hotend's print settings, plan
  <sha16>", with "(sliced by Hotend's stand-in slicer: not printable)" for the stand-in) and
  the nozzle size, with "(assumed: no size is set)" while it is assumed.

### 8.1 The `hotend` command

`hotend` is a thin layer over the methods above; it holds no logic of its own beyond
choosing a project, asking at the terminal and printing. Most commands take `--project` and
`--json`.

| Command | Facade |
| --- | --- |
| `doctor [--json]` | `hotend_core::doctor::report`, plus the row for the stand-in slicer |
| `demo [--out <dir>] [--simulate-print]` | the whole loop, see below |
| `project new\|list\|show` | `project_create`, `projects_list`, `project_get` + `print_jobs` |
| `build <model.py> [--param name=value]` | `model_build` |
| `param set <name> <value>` | `parameter_set` |
| `versions`, `revert <n>` | `versions_list` (the table's `from` column is each version's `parent`), `version_revert` |
| `snapshot create <name> [--version n] [--note text]`, `snapshot list`, `snapshot export <name> --to <dir>`, `snapshot restore <name>`, `snapshot delete <name>` | `snapshot_create`, `snapshots_list`, `snapshot_export` (`--to` must exist; it is passed as its canonical path), `snapshot_restore`, `snapshot_delete`; a name is matched without regard to ASCII case |
| `checks [--only a,b]`, `measure --query <json>`, `questions` | `checks_run`, `measure_run`, `questions_list` |
| `views`, `export` | `views_render`, `export_run` |
| `prepare [--version n] [--copies part=n] [--up part=+z]` | `prepare_run`; exit 1 when a piece did not fit |
| `slice [--profile id] [--plate n] [--fake]` | `slice_run` |
| `gauge [--card <json>] [--nozzle id] --out <dir>` | `gauge_export` |
| `calibrate [--reading hole:<drawn>=<measured>] [--nozzle id]` | `calibration_record`; without a reading, `calibration_get` |
| `baseline set --printer id --plate id --material id --printed-on YYYY-MM-DD --from-print text --level n --brim-kind none\|outer --brim-width mm --first-layer-wall-speed mm/s --first-layer-infill-speed mm/s --first-layer-bed C --layers-bed C --part-fan-off-layers n --aux-fan-first-layers % --aux-fan-max % --wall-loops n [--json]` | `baseline_set`; every value is required, none has a default; exit 2 when a value is refused |
| `baseline show [--json]`, `baseline remove --printer id --plate id --material id`, `baseline remove --invalid` | `baseline_list` and `baseline_invalid` (each record that cannot be used is a `warning:` line on standard error that names `hotend baseline remove --invalid`; `--json` prints the usable records only), `baseline_remove` (also removes a record of the three that cannot be used; exit 2 when there is none to remove), `baseline_remove_invalid` (`--invalid`, not with a key: removes every record that cannot be used and only those, and prints how many; 0 is not an error) |
| `printers` | `printers_list` |
| `print propose [--note text]` | `print_propose` |
| `print status [<job>]` | `print_status`, or `print_jobs` of the project |
| `print review <job> [--to simulated\|bambu-connect\|folder] [--out <dir>]` | `print_review`; confirms and sends nothing |
| `print confirm <job> [--to ...] [--out <dir>] [--time-scale n]` | `print_review`, the terminal prompt, `print_confirm`, then the job's events until it ends |
| `print cancel <job>` | `print_cancel` |
| `print project [--plate n] [--out <dir>] [--fake] [--json]` | `print_project_make` and where the project is; with `--out`, `print_project_review` for that folder, the terminal prompt and `print_project_deliver`. Opens nothing |
| `settings show`, `settings set [--printer] [--material] [--family] [--density] [--provider --model] [--effort level] [--max-turns] [--plate-type id] [--nozzle mm] [--spool-colour #rrggbb] [--spool-slot slot] [--slicer kind[=program]] [--handoff-folder path \| --clear-handoff-folder] [--previews slicer\|hotend-made]`, `settings slicers` | `settings_get`, `settings_set`, `slicer_choose`, `slicers_list`; `--handoff-folder` is checked first, as a hand-off on this host would use it (`handoff_folder_check`), before anything is written: when it cannot be used nothing changes (not the slicer or any other setting named with it), and the command says so and exits 2. Settings the core refuses after a `--slicer` was chosen are put back as they were before the command |
| `audit [--all] [--limit n]` | `audit_log` |
| `chat`, `key set\|status` | `message_send`, `provider_key_set`, `provider_key_status`; `key set keenable\|tavily` stores a search service's key (`search_key_set`) and `key status` lists both services (`--json`: `search_services: [{service, key}]`) |
| `research status\|on\|off\|source built-in\|keenable\|tavily\|off\|...` | `research_status`, `research_consent_for` (after a typed `yes` at a terminal, for the source that was explained), `research_set`, `research_source_set` and `research_source_options` (docs/WEB_RESEARCH.md sections 8 and 16) |

- Exit codes: 0 done; 1 the outcome is a failure (a model that does not build, a failed
  check, a run that did not end `done` or `budget`, a job that was not confirmed or did not
  finish, a missing hard requirement in `doctor`); 2 the command could not run (bad
  arguments, no such project, nothing configured, no terminal for a confirmation).
- Data directory: `--data-dir`, else `HOTEND_DATA_DIR`, else the platform's data folder.
- Slicing installs the backend with `hotend_slicer::install`. Settings exported from the
  user's slicer are read from `<data-dir>/slicer-profiles/` (the `SlicerConfig.profile_dir`
  layout of section 6) when that folder exists. `slice --fake` and `demo` point every slicer
  kind at `hotend-fake-slicer`; their results carry the `fake_slicer` warning.
- **A print is confirmed by a person at a terminal.** `print confirm` and `demo
  --simulate-print` refuse with exit code 2, before anything is opened, when standard input
  or standard output is not a terminal (a review that goes to a file is a review nobody
  read). They show the review, discard whatever was typed before the question was written
  (`tcflush`; `FlushConsoleInputBuffer` on Windows), ask, and take the process's
  `ConfirmationAuthority` only after the answer `yes` typed after the question; any other
  answer, and the end of the input, leaves the job pending. The review names the plate of
  the layout and its pieces (`PrintReview.plate`, `pieces`). There is no option, and no environment variable is read, that answers for the
  person (the argument parser is built without environment support, and a test walks the
  command tree for an option named like a pre-approval). `print project --out` asks the same
  way (both terminals checked before anything is opened or made, then the project's review
  text shown word for word, then a typed `yes`) before it saves a copy of the editable
  project into the folder; without `--out` it makes the project and prints where it is, its
  hash, the keys it lists as changed, its test slice and the sentence that a project opened
  this way is sliced again by Bambu Studio, so it is outside Hotend's check. `--fake` asks
  the stand-in, which makes no project and says so. No form of the command opens the project.
- `chat` takes the key only from the secret store (`--secrets keyring|env`, else
  `HOTEND_SECRETS`); `key set` reads it from a pipe or a hidden prompt, never from an
  argument. No command prints a key.
- `demo` runs the agent in scripted mode (`--script <file>`, section 5.2) with a script that
  is the built-in `cable_box_demo` up to the views, followed by `slice` and `propose_print`
  and its own closing text, with the fake slicer. The transcript then shows the prepared
  plate and the orientation of each part, the G-code check with the confirmation summary
  (`print_review` for the simulated printer) and the print step: without `--simulate-print`
  the job stays pending; with it, and after `yes` at the terminal, the job runs on the
  simulated printer (3600 simulated seconds per second) to `finished`, and what the
  simulated printer received is written to `simulated-printer-log.json`. Every line of that
  step is marked `[simulated]`. The core requires a provider for a run, so the demo records
  the placeholder provider `ollama` (the one kind that needs no key) with an empty in-memory
  secret store; no model request is made, and the audit log shows zero egress rows. `chat`
  has a hidden `--script` option that does the same for tests and restores the saved
  settings afterwards.
- `doctor --json` is the `DoctorReport`: `{ok, hotend, debug_build, checks: [{id, label,
  status, required, detail, fix}], sandbox, slicers}` with `status` one of `ok`, `warn`,
  `fail`, `skipped`, `info`. Required checks: `sandbox`, `python`, `sandbox.read_only`,
  `engine` (the engine builds a part in the sandbox) and `agent` (the real agent process,
  sandboxed, replays a written two-turn conversation with one `list_cards` call through the
  gateway; no model is asked). It exits 1 when a required check has status `fail`.

**Print settings on the command line.** `hotend plan show [--plate n] [--version n] [--json]`
prints the plan of a plate: which settings a slice uses, the nozzle size ("(assumed)" until
set), the proven baseline it rests on (with the day, the label and the fields it set), why it
is blocked, the questions, a table of field, value and origin, the warnings, notes and
assumptions, and the before-you-print list; `--json` prints `PrintPlanView`. `hotend plan
explain` prints one line per decision: `field = value [origin; rule, basis]: sentence`. `hotend settings set --nozzle <mm> --spool-colour <#rrggbb> --spool-slot <slot>`
sets the fitted nozzle and the spool (`external-main` and `external-auxiliary` are accepted
for the outside holders); `settings show` prints both, each with "(assumed: ...)" until set.
`hotend prepare` says how much room it kept for brims and prints each plate's plan in one
line (or BLOCKED with its code and sentence). Nothing here takes a slicer key or starts a
print.

### 8.2 Printing through the core

The core drives the printer layer of section 9 (`hotend_core::printing`). The only transport
that takes a file or a command is the simulated printer (`simulated-x2d`, serial
`20P0SIM00000001`): this build sends no file and no command to a real printer or to a printer
maker's cloud. What it can do with a real printer is read its status: a read-only listener on
the local network, which connects only when the user sets it up (section 9, "Printer status
(read-only)"). `printers_list` lists the three ways this build can deliver (`simulated_printer`,
`bambu_connect`, `export_to_folder`); direct control of a real printer over the LAN is not built
into the core and has no entry (section 8).

1. **Propose** (`print_propose`, or the agent's `propose_print` tool): a row with status
   `pending`, bound to the SHA-256 of the newest slice of the current version as it is laid
   out now. This is the last step a tool can take.
2. **Review** (`print_review`): only for a `pending` job. The core resolves the sliced file
   inside the version's slice folder, hashes it (`slice_changed` if it is not the proposed
   file, and also if the version was laid out again since the slice: the slice record's
   `job.prepared_at_ms` must be the `created_at_ms` of the stored preparation), and runs
   `check_job` with what was sliced and what the person chose: `DeclaredJob
   {plate: 1, plate_type, filaments: [{slot, material: the family in the slice's job record,
   nozzle, nozzle_diameter_mm}]}` (section 9). `plate_type` is the build plate recorded with
   the slice (`job.plate_type`, an id; section 6), looked up in the printer layer's plate
   rows by that id and declared by the slicer's name of the row. A slice without a recorded
   plate is refused with `bed_type_not_chosen`, which names the setting and the ids the check
   knows; a recorded plate the check has no row for (`high-temp`, `engineering`) is
   `unsupported`; and when the settings name another plate now than the one the version was
   sliced for, the review is refused with `plate_changed`. The plate is never taken from the
   file's own statement: the check holds the file to the declaration. `nozzle_diameter_mm`
   is the size recorded with the slice for the filament's nozzle (`job.nozzles`;
   `slice_changed` when the record has none). A file the linter refuses makes the job
   `rejected` (`gcode_rejected`, with the findings) and is never delivered. Otherwise the
   result is a `PrintReview {job, delivery, simulated, printer, plate, pieces, summary,
   warnings, spools}`. Besides the linter's warnings, `warnings` carries `built_for_other`
   (the version was built for another printer, material or calibration) and
   `nozzle_size_assumed` (the slice record's `job.nozzle_size_set` is not `true`: the
   person did not set the fitted nozzle's size, so the size the job is checked for is an
   assumption; a record older than that field counts as not set) and
   `hotend_made_previews` (the slice's warning of that code, "plate previews made by
   Hotend: ...": the pictures Bambu Connect and the printer show are Hotend's renders, not
   Bambu Studio's). A `bambu_connect` review of a file without the entries Bambu Connect
   needs is refused with `not_importable` before the hand-off folder is resolved. For a `bambu_connect`
   review the core resolves the hand-off folder first and `handoff` is `HandOffTarget
   {folder, windows_folder, opener, opens}`: where the file would go (its Windows form under
   WSL) and what opens the link (`opens` is false for the stand-in). The folder is the
   setting `handoff_folder`; without it, `<data-dir>/handoff` (made when missing), except
   under WSL, where a hand-off without a chosen Windows drive folder, or with a folder that
   is not one, is refused with `handoff_folder` and a sentence that says to choose one in
   Settings. A chosen folder that cannot be used (missing, not a folder, a comma in its
   Windows path) is refused the same way on every host. `printers_list` then lists
   `bambu_connect` with `enabled: false` and that reason, on every host. The confirmation
   places the file in the folder the review named. `plate` and `pieces` are the plate of the version's layout that was
   sliced (from 1) and the names of the pieces on it, from the slice record; this is the
   plate a confirmation names. `summary.plate` is the plate inside the sliced file, which is
   1 in every file Hotend slices, and is not shown. `summary` is the confirmation summary:
   `PrintSummary {file_name, sha256, size_bytes, plate,
   estimated_seconds, layers, filaments: [{slot, material, colour, grams, metres, nozzle_id,
   selected_in_gcode}], max_nozzle_c, max_bed_c, max_chamber_c, extent}`, copied from the
   printer layer's `JobSummary`, which is computed from the file only; `review_text`, the
   printer layer's `JobSummary::review_text()` as it is (section 9), which is the text a
   person confirms; the linter's warnings; and for the simulated printer the spool chosen
   for each filament (the first loaded tray of that family in an AMS that feeds the
   filament's nozzle and is not drying; `no_spool` otherwise). Printing
   through Hotend exists for `bambu-x2d` only (`unsupported` for another printer). The
   checked job is kept in memory for the confirmation; a review of the same job drops the
   one kept before it, so after a refused review there is nothing to confirm. After the
   checks above, a job whose slice was made with Hotend's print plan (`job.plan_sha256`) is
   reviewed only while that hash is still the plate's current plan, computed with the
   settings and the proven baseline as they are now (`Gateway::plan_is_current`): a
   baseline, spool, nozzle or setting changed since the slice refuses the review with
   `plan_changed` ("the print settings of this plate changed after this print was
   sliced"). The window then reads the slice and the layout again and says "The print
   settings of this plate changed since it was sliced. Slice it again."
3. **Confirm** (`print_confirm`): takes a `&ConfirmationAuthority`. The type has no public
   constructor and no `Deserialize`; `ConfirmationAuthority::take()` hands it out once per
   process, and the layer that draws the confirmation (the desktop application's native
   dialog, the CLI's terminal prompt) is the only code that takes it. The tool gateway holds
   none, so no tool call and nothing the webview sends can confirm. The job must have been
   reviewed since the core started (`not_reviewed`), and the settings must still name the
   plate the review declared (`plate_changed`). `shown` is the text the dialog or the
   terminal displayed to the person, all of it. The core looks for the review text of the
   reviewed job in it, in one piece and character for character, and hands that piece to
   the printer layer's `ConfirmationAuthority::confirm(request, shown)`, which gives a
   confirmation only for the request's own review text (section 9). A `shown` without the
   text, or with a text that differs in one character, is `summary_mismatch` and the job
   stays `pending`. The confirmation, and the start token minted from it, are bound to the
   SHA-256 of that text and of the file. The caller passes the very string it displayed:
   the CLI prints `shown` and then asks, the desktop shell gives the dialog and the core
   the same text. The version must not have been laid out again since the review
   (`slice_changed`), and the job's plan must still be the plate's current plan, as for the
   review (`plan_changed`; checked before anything else, but a missing review, another
   plate or another layout is named first when one of them holds); a refused confirmation
   uses the review up. The job is claimed before anything is sent: one
   conditional update moves it from `pending` to `starting` (`not_pending` for the loser of
   a race, which sends nothing). For the simulated printer the claim, the upload and the
   start happen under the printer's lock; a delivery that then fails gives the claim back
   (the job is `pending` again), and one that succeeded is always followed. `print_cancel`
   of a job that is `starting` or `printing` on the simulated printer takes the same lock
   and sends `stop`: a cancel that races a confirmation either wins before the claim (the
   job is `cancelled` and nothing was sent) or stops the print that was started; a started
   print is never left recorded as `cancelled` and running.
4. **Deliver**, by the reviewed `DeliveryChoice`: `simulated_printer` uploads through
   `PrinterDelivery` (under the name `CheckedJob::remote_name()`, section 9), mints the start token from the confirmation
   (`ConfirmationRequest::start_print`, `StartToken::mint`) and calls `start_print`; the job
   is `starting`. `bambu_connect` (listed first by the window: the way a Bambu printer
   gets a print without Developer Mode) places the checked bytes in the hand-off folder the
   review named, reads them again, makes the `bambu-connect://import-file` link (section 9,
   "The Bambu Connect hand-off") and gives it to the core's `LinkOpener`; the job is
   `handed_off` and Hotend learns nothing more. The job's detail holds `link` (as given to
   the opener), `path` (as Hotend sees the file), `windows_path` (its Windows form, under
   WSL and on Windows: the path the link names), `opened` (true only when a real opener
   started; false for the stand-in or when the opener failed, whose reason is in `message`)
   and `opener` (`LinkOpener::describe`). Its `print`/`handed_off` audit entry records
   `opener`, `opener_started` and `opener_error` (section 1), without the link or the path.
   A failed opener leaves the job `handed_off`: the file is in place and the message says to
   import it in Bambu Connect. `export_to_folder` writes
   `<project>-v<n>.gcode.3mf` into the folder; the job is `exported`. A hand-off or export
   confirmation cannot mint a start token.
5. **Follow** (simulated printer): a task polls the printer's status. Until the start
   watchdog (section 9) reports `Started` the job is `starting`; `NotStarted` after the
   deadline (90 s of the printer's clock) makes it `not_started` with the reason, `Failed`
   makes it `failed`. Then `JobTracker` events move it to `printing` and to `finished`,
   `failed` or `cancelled`. Every change is stored, audited (`print`) and sent as a
   `CoreEvent`. `PrinterOptions {time_scale, start_deadline, poll_interval,
   stays_idle_after_start}` in `CoreConfig` set the simulated clock; the default is 60
   simulated seconds per second. The simulated print lasts as long as the slice's
   estimated time (`SliceSummary.time_s`): for a real slice that is the time the file
   states, and the stand-in's file states none (it prints nothing), so its estimate
   keeps the simulated run long enough to follow.

**The opener and the host.** The core holds an `Arc<dyn LinkOpener>` and a `HostKind`.
Until the application sets them (`Core::set_link_opener`, `Core::set_handoff_host`) the
opener is the stand-in, which opens nothing, and the path is the local one: the tests and
the command line never open anything. The desktop application sets
`platform_opener(HostKind::detect())` and that host; the development bridge sets the
stand-in (`StandInOpener::from_env`) and the detected host. `hotend print confirm --to
bambu-connect` makes the path for the detected host, prints the link and the Windows path,
and says that it opens nothing. `Core::handoff_status()` answers `HandOffStatus {host,
chosen, folder, windows_folder, problem, opener, opens}`.

**The editable Bambu Studio project** (`hotend_core::printing::project`, the "Open in Bambu
Studio" fallback). A project opened this way is sliced again by Bambu Studio, so it is
outside Hotend's check; every text about it says so in these words
(`hotend_core::PROJECT_OUTSIDE_CHECK`) and none says that the project was checked. Four
facade methods, none reachable from a tool; the window's part is in section 8.3:

1. `print_project_make(project_id, plate?) -> EditableProject` makes the project of one
   plate of the current version (plate 1 by default) for its current layout and print plan.
   Refused with the stand-in slicer (`stand_in_slicer`, "made by the real Bambu Studio
   only") or no slicer (`slicer_not_installed`); for a blocked plan (the plan's code); and
   while there is no slice of the same plan (`job.plan_sha256`) of the same layout
   (`job.prepared_at_ms`) for that plate by a real slicer (`no_slice`). The G-code check of
   section 9 then runs again on that slice's file, report only (no job is marked), for the
   job the slice was made for, and a refusal is `gcode_rejected`: a plan whose file the check
   refuses never becomes a project. Only then the backend's `export_project` runs (section
   6), into `versions/<n>/slicer-project/<plate>-<first 16 hex digits of the plan's hash>/`
   with the same process profile as the slice. Its answer is held to section 6's rules, and
   its test slice (run C) must pass the G-code check too, held to the same job
   (`gcode_rejected`, "Bambu Studio's own slice of the editable project did not pass").
   The project is recorded with its hash (store table `project_exports`, migration 5: id,
   project, version, plate, plan hash, the slice's profile hash and SHA-256, folder, SHA-256,
   the backend's summary, the last hand-over) and audited (`print`/`project_made`).
   `EditableProject` is `{id, project_id, version, plate, plan_sha256, slice_sha256, path,
   sha256, keys, test_slice, proof, notice, delivery, detail, created_at_ms,
   updated_at_ms}`. `print_projects(project_id)` lists them, newest first.
2. `print_project_review(id, destination) -> ProjectReview` checks everything again: the
   plan is still the plate's plan (with the proven baseline) and not blocked, the slice record still has the recorded
   hash and its file passes the check, the layout is the one sliced, the settings still name
   the plate (`plate_changed`), the test slice passes the check and has its recorded hash,
   and the project file is a regular file with the recorded hash (`project_stale`,
   `project_changed` otherwise). `destination` is `{"kind": "bambu_studio"}` (the hand-off
   folder of a Bambu Connect hand-off, resolved as for a review, with `handoff` naming it and
   the opener) or `{"kind": "folder", "folder"}` (an existing folder; nothing is opened; the
   review resolves it to its canonical path, without the `\\?\` prefix Windows gives one, so
   `D:\Models` and not `\\?\D:\Models`, and `ProjectReview.destination` names that path; the
   hand-off is compared and shown the same way). The folder's path goes into the text the
   person confirms, so it must show as what it is on one line (`ConfirmationRequest::project`
   takes only such a line, `hotend_printer::is_plain_line`): a path with a control
   character, any white space but the plain space (a tab, a line break, U+2028, U+2029, a
   no-break space) or an invisible or bidirectional format character (U+200B to U+200F,
   U+202A to U+202E, U+2060 to U+206F, U+FEFF and the rest of Unicode's Cf) is refused here,
   before any dialog (`delivery_failed` for a folder, `handoff_folder` for the hand-off
   folder), with the reason and "choose or rename the folder". `ProjectReview {project,
   destination, file_name, handoff, review_text, warnings,
   notice}`: `file_name` is the test slice's label (the project's name and version, cleaned
   as for a sliced file) and the first 32 hex digits of the project's SHA-256, with `.3mf`;
   `review_text` is the printer layer's `ConfirmationRequest::project` text (section 9): the
   project's SHA-256, the destination line (`Where it goes: saved in the folder <path>;
   nothing is opened.`, or `Where it goes: saved in the hand-off folder <Windows path, else
   path>; nothing is opened.` with the stand-in opener, `..., then the system is asked to
   open it with the program for .3mf files (Bambu Studio).` with a real one), the sentence
   above, and the test slice's review text introduced as a test of the project, not of what
   Bambu Studio will make of it. A new review replaces the last one, also when it fails, so
   a review for another destination invalidates a yes given for the earlier one.
3. `print_project_deliver(&ConfirmationAuthority, id, shown) -> EditableProject` hands a
   reviewed project over. As for `print_confirm`, only the layer that shows the confirmation
   holds the authority (the CLI's terminal prompt, the desktop shell's native dialog), and
   `shown` must contain the review text in one piece, word for word (`summary_mismatch`, and the
   review is used up). Because the review text names the destination, a `shown` from a dialog
   for another destination does not contain it. The project's plan must still be the plate's
   plan with the proven baseline (`project_stale`, and the review is used up), the settings must
   still name the plate and the layout must be the one reviewed. For `bambu_studio` the opener
   must still be of the kind the review text named: a review that said "nothing is opened" (the
   stand-in) is not handed to a real opener, and one that said the system is asked to open it is
   not handed to the stand-in (`delivery_failed`, "review the project again"; nothing is placed,
   the review is used up). The printer layer's confirmation is redeemed for that project's hash,
   that destination and that test slice (`HumanConfirmation::redeem_for_project`), the project
   file is read once and its bytes held to the recorded hash, written to the destination under
   `file_name` through a partial file that is renamed into place, and read again
   (`project_changed` and removed otherwise; a file is removed only while its folder is still
   the directory that was checked, never through a folder that became a link). The folder of a
   `folder` destination must still resolve to the path the review named. For `bambu_studio` the
   folder must still be the folder that was chosen, the placed file is resolved with
   `HandOffFolder::placed_project`, read again right before the opener is asked, and given to
   the core's `LinkOpener::open_project` (the stand-in until the application sets the real one).
   The record's `delivery` is `bambu_studio` or `folder` and its `detail` (the fields of
   `PrintJobDetail`) holds `message`, `path` (as Hotend sees the file), `windows_path` (its
   Windows form, under WSL and on Windows), `opened` (true only when a real opener started;
   false for the stand-in or a failed opener) and `opener`; audited
   (`print`/`project_handed_over`, with `opener`, `opener_started` and `opener_error` when the
   opener was asked: section 1). A project confirmation mints no start token and releases no
   Bambu Connect link.

### 8.3 The desktop shell (`apps/desktop`)

The window reaches the core through Tauri commands generated from one table
(`apps/desktop/src-tauri/src/ipc.rs`). tauri-specta writes the TypeScript binding
(`apps/desktop/src/lib/core/bindings.ts`, committed; `tests/bindings.rs` fails when it is
stale, `pnpm bindings` rewrites it). A command has the name of its facade method. On the wire
its arguments are named in camelCase (`project_id` is sent as `projectId`, Tauri's convention;
the binding hides this); field names inside values stay as the core writes them (snake_case).
A failed command rejects with the `ApiError` value. The commands are those of the table in
section 8 with these differences:

| Command | In the shell |
| --- | --- |
| `version_mesh` | answers the text of `mesh.json` (UTF-8), not bytes |
| `export_run`, `gauge_export` | `destination` must be a folder the person picked with `folder_pick` in this session (`invalid_input` otherwise; a program picked with `file_pick` is not a folder) |
| `snapshots_list`, `snapshot_create`, `snapshot_delete`, `snapshot_restore` | as in section 8, "Snapshots": `project_id`, and `name`, `version` (null: the current one) and `note` where the facade takes them; answers `Snapshot` (`snapshot_restore`: the `Version` made current) |
| `snapshot_export` | `project_id`, `name`, `folder` -> `SnapshotExport`. `folder` must be a folder the person picked with `folder_pick` in this session under exactly the title "Save snapshot files" (`grants::SNAPSHOT_TITLE`, the window's `S.snapshots.exportTitle`), granted as `Picked::SnapshotFolder` and as nothing else; a folder picked under any other title (an export's, the hand-off folder's) is refused with `invalid_input`, "was not chosen as the folder for snapshot files", and a snapshot folder is not a folder for an export or the hand-off folder. The files go into a new folder below it |
| `printer_lan_get`, `printer_lan_set` | `printer_lan_set` takes `ip` (a private-range IPv4 address) and `serial`. Both answer the core's `PrinterLanSetup` with one field added by the shell, `access_code_present: bool` (true when an access code comes from the environment or the secret store). Never the code |
| `printer_lan_clear`, `printer_lan_key_delete`, `printer_certificate_forget`, `printer_watch_start` | -> nothing. `printer_watch_start` follows the printer with the core's read-only listener (it has no way to publish); its reports arrive on the event stream as `{"channel": "printer", "status": LanPrinterStatus}`. Without an address, or without a confirmed certificate or Bambu Lab's certificate authorities, it is refused before anything is connected to |
| `printer_watch_stop`, `printer_watch_latest` | -> whether a session was running; the latest `LanPrinterStatus` or `null` |
| `printer_lan_key_set` | `code` -> nothing. Write-only: no command returns the code; `printer_lan_get` says only whether one is there (`access_code_present`) |
| `printer_certificate_check` | no arguments -> `{confirmed: bool, setup}`. The shell takes the first look itself (`printer_certificate_probe`), shows the native dialog (title "Trust this printer's certificate?", button "Trust this certificate", Cancel the default and focused button, the confirming button inert for the first second) with the address, serial, whole fingerprint, subject, issuer (each on one labelled line) and the core's note as written, and calls `printer_certificate_confirm` with that look's own fingerprint only on the confirming button. Cancel, closing the dialog and a dialog that could not be shown store nothing. `printer_certificate_confirm` and `printer_certificate_probe` are no commands of the window, and no command takes a fingerprint. Refused with `busy` while another confirmation dialog is open, `not_configured` when the shell cannot draw the dialog (before the printer is looked at) |
| `research_source_options`, `research_source_set` | -> `SearchSourceOption[]`; `source` (`built_in`, `keenable`, `tavily`, `off`) -> `Settings`. An agreement given for another source does not cover the new one |
| `research_consent_ask` | `source` -> `{agreed: bool, options: SearchSourceOption[]}`. Refused with `invalid_input` before any dialog when `source` is not the source chosen in the settings, or is `off`. Otherwise the shell shows the native dialog (title "Agree to web research through <Keenable, Tavily or your AI provider>?", button "Agree", Cancel the default and focused button, the confirming button inert for the first second) whose body is the core's agreement text for that source as written (`research::consent_request`, which names who receives the queries and their host), and calls `research_consent_for(source, true)` only on the confirming button; the core refuses it again if the chosen source changed meanwhile. Cancel, closing the dialog and a dialog that could not be shown record nothing. `research_consent`, `research_consent_for` and `research_set` are no commands of the window; `settings_set` keeps the core's research settings whatever the window sends. `busy` and `not_configured` as for `printer_certificate_check` |
| `research_consent_withdraw` | -> nothing; `research_consent_for(<chosen source>, false)`, which ends any agreement |
| `search_key_set`, `search_key_delete`, `search_key_status` | `service` (`keenable`, `tavily`), and `key` for `search_key_set` -> nothing, nothing, `bool`. Write-only: no command returns a key |
| `slicer_choose`, `settings_set` | a slicer program must be a path the person picked with `file_pick` in this session (a folder picked with `folder_pick` is not accepted), or the one already stored |
| `print_review` | an `export_to_folder` delivery needs a picked folder; refused with `busy` while a confirmation dialog is open |
| `print_confirm` | arguments `job_id` and `DeliveryChoice` only, answer `{confirmed: bool, job: PrintJob}`; see below |
| `settings_set` (hand-off folder) | a `handoff_folder` other than the stored one must be a folder the person picked with `folder_pick` in this session under the hand-off picker's own title, "Choose the Bambu Connect hand-off folder" (granted as `Picked::HandOffFolder`); a folder picked under any other title, an export's for instance, is refused (`invalid_input`, "was not chosen as the Bambu Connect hand-off folder", with "Choose the hand-off folder with its Choose button in Settings, Printer."); `null` clears it |
| `print_project_review` | a `folder` destination needs a picked folder; refused with `busy` while a confirmation dialog is open |
| `print_project_confirm` | arguments `project_export_id` and `ProjectDestination` only, answer `{confirmed: bool, project: EditableProject}`. Like `print_confirm` it reviews the project itself, shows the native dialog (title "Save this project and open it in Bambu Studio?", "Save this project for Bambu Studio?" with the stand-in opener, or "Save this Bambu Studio project?" for a folder), puts the `review_text` below the heading "What Hotend checked of this project. This is what you confirm:", and calls `print_project_deliver` only on the confirming button, with the dialog's body. There is no `print_project_deliver` command |
| `message_send` | answers `{run_id}` only; the run's events arrive on the event stream |
| `doctor_report` | also answers when the core could not be opened |
| `folder_pick`, `file_pick` | `title` -> the path picked in the system's dialog, or `null`. The only source of paths the shell accepts, each for what it was picked as (a folder to write into, the hand-off folder, or a slicer program). A folder picked under exactly "Choose the Bambu Connect hand-off folder" (`grants::HANDOFF_TITLE`, the window's `H.handoff.pickTitle`) is granted as the hand-off folder and as nothing else, so it is not a folder for an export either; a folder picked under exactly "Save snapshot files" (`grants::SNAPSHOT_TITLE`) is granted as the snapshot export folder and as nothing else; a folder picked under any other title is a folder to write into and never the hand-off or snapshot folder. The dialog shows `title` only when it is one of the interface's own picker titles (`grants.rs`: `FOLDER_TITLES`, and "Choose the <slicer> program" for a known slicer); any other text is replaced by "Choose a folder" or "Choose a slicer program" |
| `open_link` | `link`, one of `anthropic_keys`, `openai_keys`, `google_keys`, `openrouter_keys`, `ollama_download` -> nothing. The shell holds the address; it opens `https` addresses of the listed hosts only, in the system browser. The window cannot pass an address |
| `events_subscribe` | a Tauri channel -> nothing; the channel then receives every `CoreEvent` as JSON (a followed printer's reports as `{"channel": "printer", "status"}`, the core's own form), and `{"channel": "lagged", "missed": n}` when the window read too slowly and `n` events were dropped. One subscription per window: a new one ends the last |

`print_confirm` is the only way a print is confirmed from the window, and the window's part in
it is to ask. The shell computes the review itself (`Core::print_review`), draws a native
dialog with the facts of that review (the text of DESIGN_SPEC 4.6, built in `confirm.rs`
from the `PrintReview`, never from anything the window sent), and only when the person
presses the confirming button takes the process's `ConfirmationAuthority` and calls
`Core::print_confirm` with the body of the dialog. Below the dialog's own lines, under one
heading, the body holds the review's `review_text` word for word
(`confirm::with_checked_text`): the prompt and the core are given the same text, and the
core confirms only when the review text is in it. The dialog's last line names the file and
a short "check code" of its SHA-256. The dialog and the window's review show one job and a person compares them, so both
write a number by the same rule: a time to the nearest minute
(`src-tauri/tests/fixtures/durations.json` holds the cases the shell's and the window's
`duration` must both pass), grams with one decimal and whole degrees, a tie rounded up. The
dialog uses the window's words: the nozzle by name, a spool's colour as a word, and for a
placeholder of the stand-in slicer (the version's stored slice is the reviewed file and has
the `fake_slicer` warning) "about" before the time, the stand-in's estimates of time, layers
and grams from that slice record in place of what its file states (it states 0 s and no
layers; the checked review text below keeps the file's own words), and the window's sentence in place of the
file check's `no_moves` warning. The dialog names the plate of the layout (`PrintReview.plate`)
and, when the file heats one, the chamber ("Chamber up to"). Text from outside (the project's
name, what the file says of itself, a folder, a warning) is one line each: control
characters, every kind of white space and the separators U+2028 and U+2029 become one
space, invisible and bidirectional format characters are dropped, and each is cut to a
fixed length; at most 6 filaments and 6 warnings get a line each and the rest are counted
("and N more"). The dialog itself (`dialog.rs`) is drawn with the toolkit directly, not
with the dialog plugin: Cancel is the default button and holds the keyboard focus, so
Return, Space and Escape on a freshly opened dialog answer Cancel; the confirming button
does nothing for the first second; the text is never a format string or markup; and a body
taller than the screen scrolls inside the dialog, so the buttons stay on screen (GTK on
Linux, checked by `src-tauri/tests/confirm_dialog.sh` under Xvfb; on Windows and macOS the
dialog is `rfd`'s with Cancel as its first, default button, type-checked and not yet run
there). "Cancel", closing the dialog and Escape answer `confirmed: false`
with the job still `pending`. While the dialog is open, `print_review` and a second
`print_confirm` answer `busy`, so the review the core checks is the one on screen. The
authority, the text and every other input of the core's confirmation stay in the shell: no
command takes or answers them, and unknown arguments are refused. A shell that cannot draw
the dialog answers `not_configured`. The window can show `PrintReview.review_text` too; it
cannot confirm with it. The build plate is chosen in the window's printer settings
(`settings_set` with `plate_type`).

For a Bambu Connect hand-off the dialog says what the code does. With a real opener its
title is "Save this file and open it in Bambu Connect?" and its button "Save and open Bambu
Connect"; its lines say that Hotend saves the checked file to the folder named below and
asks Bambu Connect to import it, and that the person chooses the printer and presses Print
in Bambu Connect. With the stand-in (the bridge, the tests) the title is "Save this file for
Bambu Connect?", the button "Save", and a line says that Bambu Connect is not opened. Either
way a `Folder:` line names the hand-off folder as Bambu Connect reads it (the Windows form
under WSL). Neither the title nor the button says "Start", "Print" or "Send". The window's
print review lists the ways in this order: Bambu Connect (shown, not selectable, with the
core's reason while it is not available), the editable Bambu Studio project (selectable except with the stand-in
slicer, always with the sentence that a project opened this way is sliced again by Bambu
Studio, so it is outside Hotend's check; the window makes it with `print_project_make`,
offers the hand-off folder or a picked folder, saves only through `print_project_confirm`,
and afterwards shows where it was saved and whether Bambu Studio was asked to open it), the
simulated printer, a folder or USB drive. It names the hand-off
folder before the confirmation, lists the numbers from the web that nobody confirmed as the
dialog does ("From the web: <label> <value> <unit> (from <host>, not confirmed)", six lines
at most, the rest counted), and after a hand-off says whether Bambu Connect was asked to
import the file (`detail.opened`), the file's path (its Windows form where it has one) and
what to do in Bambu Connect. The settings' printer page has the "Bambu Connect hand-off
folder": picked with `folder_pick`, stored with `settings_set` (`handoff_folder`), shown with
its Windows form, with the core's reason when a hand-off cannot be made with it. The
application gives the core this platform's opener; the window cannot name a link or a file
to open.

The window's capability (`capabilities/main.json`) is for the window `main` and grants one
permission per command above and nothing else: no plugin, no file system, no shell, no window
or event API. The content policy is in `tauri.conf.json` (`default-src 'none'`, scripts and
styles from the application only, no `eval`, no remote source); the window cannot navigate
away from the application and cannot open another window. Tauri puts that policy only on
files it serves itself (a release build). A debug build loads the interface from
`devUrl`, `http://localhost:1420`, and runs whatever page answers there with every command
of the window; the policy it then runs under is the header of Hotend's own development
server (`apps/desktop/csp.ts`, `devPolicy`: the window's policy, plus a nonce that is new
at each start of the server for the tags Vite writes, plus `'self'` and
`ws://localhost:1420` in `connect-src` for hot reloading). A debug window is for a scratch
data folder only. The application's address is
`tauri://localhost` on Linux and macOS and `http(s)://tauri.localhost` on Windows and
Android; each form is accepted only on its own platform (elsewhere it is an ordinary
address of the computer).

**Installed layout.** An installed (release) application finds its parts by position, not
by environment: the Python environment is the folder `python` under the application's
resource folder, with the interpreter at `python/bin/python` (`python/python.exe` on
Windows) and the `hotend_*` packages installed in it
(`PythonRuntime::packaged_interpreter`); the sandbox launcher is `hotend-sandbox` beside
the main executable. `HOTEND_PYTHON` still names another interpreter when set, and a
debug build uses the checkout. `src-tauri/tauri.bundle.conf.json` is the bundle
configuration for that layout (`pnpm tauri build --config src-tauri/tauri.bundle.conf.json`):
`externalBin` `binaries/hotend-sandbox` (Tauri places it beside the executable) and the
resource folder `bundle/python/` as `python/`. It is a file of its own because its sources
exist only after the packaging steps; no step in this repository produces them yet, and no
bundle has been built. `bundle.icon` lists `icon.ico` (Windows resource) and `icon.icns`
(macOS); `bundle.macOS.minimumSystemVersion` is 13.3. When the environment is missing the
doctor's fix for an installed build is to install Hotend again.

**Development bridge.** `hotend-bridge` (cargo feature `dev-bridge`; the build script refuses
the feature in a release profile) serves the same table to a browser so that Playwright can
drive the interface: `POST http://127.0.0.1:<port>/invoke/<command>` with the named arguments
as a JSON object answers 200 with `{"ok": true, "data"}` or `{"ok": false, "error": ApiError}`
(404 for an unknown command, 400 for bad arguments); `GET /events` is a server-sent event
stream with one event of `events_subscribe` per `data:` line; `GET /health` answers
`{"hotend_bridge": 1}`. Every request but `/health` needs `Authorization: Bearer <token>`;
the token is 32 random bytes, new at each start, printed with the address as one JSON line on
standard output (`{"hotend_bridge": 1, "url", "token", "data_dir", "answers_dialogs"}`). The
bridge listens on 127.0.0.1 only and refuses a `Host` that is not its own address and an
`Origin` that is not a loopback `http` page (403). It runs a scripted agent, the stand-in
slicer and the simulated printer in a temporary data directory. It answers `print_confirm`
with `not_configured` and every picker with `null`, unless it was started with
`--answer-dialogs-for-tests`, which answers every confirmation with "confirm" and every folder
picker with a new folder inside its data directory. `hotend-confirm-dialog` (same feature)
shows the real dialog for a cable box job on the simulated printer and prints what
`print_confirm` answered, for checking the dialog by hand or under a virtual display
(`--name` sets the project's name, `--asks n` asks again after an answer that did not
confirm, `--text <file>` shows the dialog alone with that text).

## 9. Printer layer (`hotend-printer`)

The crate `hotend-printer` is the only code that talks to a printer or prepares a file for one.
`hotend-core` drives it; the agent and the webview never hold any of its values. Nothing in a
default build of this crate opens a network connection: the printer is `SimulatedPrinter`. The
real transport is behind two cargo features, both off by default. `lan` adds the commanding side
(`LanPrinter`, uploads and commands, with `suppaftp`), which connects to loopback addresses only.
`lan-status` is the read-only side, the status listener (`lan::StatusListener`, see "Printer
status (read-only)" below), which connects to the one private-range address the user configured
and has no way to publish. `hotend-core` turns on `lan-status`, never `lan`.

**Printer facts.** `crates/hotend-printer/profiles/bambu.toml` (`schema = 5`, `revision`) holds,
per printer: identity (model code, serial prefixes), what a sliced file for the printer says and
holds (`slice_file`: `printer_models`, `printer_settings_ids`, `nozzle_diameters_mm`, the entry
names `entries` and `required_entries`), the build plates (`[[printer.plate]]`: `id`, `name`,
optional `json_name`, `bed_max_c`, optional `z_trim_mm`; the plate table below), the nozzles
(`ids = {device_extruder, slicer_extruder}`, `reach`, optional `travel`, `max_speed`,
`external_tray`) and machine limits; per protocol family: transport, storage, discovery, the
`project_file` rules, AMS numbering, speed presets, known error codes and the G-code table with
its limits (`family.gcode`, below); and temperature limits per filament family, each with its
`aliases`. Every block has a `verified` flag. `verified = true` means the fact was observed in
real output, and there are two kinds. "On a printer": none so far (a print has gone through
Bambu Connect to an X2D; no fact was measured there). "In a slice": read from a file the real
slicer wrote. So far that is three files, all sliced on 2026-11-17 by Bambu Studio 02.09.01.70
for the X2D (0.4 mm nozzles, one PLA filament on the main nozzle): one for the Cool Plate and
two for the Textured PEI Plate. The G-code table, the section markers, the travel box of the
main nozzle, the `slice_file` block, the two plate rows and the numbers of
`[family.gcode.limits]` are verified in this sense; they say what the slicer writes, not what
the printer does. Each limit is recorded with what was seen and the margin added. Everything
else is `verified = false`. This file is the source of the X2D's nozzle ids, reach and speed
limit: `hotend-core` reads them from it and `hotend-slicer`'s `printers.toml` is generated from
the core (sections 1 and 6); the tests `nozzle_facts_agree_with_the_slicer_crate`
(`hotend-printer`) and `the_nozzle_blocks_are_generated_from_the_core_profiles`
(`hotend-slicer`) fail on drift. Code maps nozzles by id only: slicer extruder 1 = device
extruder 1 (main), slicer extruder 2 = device extruder 0 (auxiliary).

**The plate table.** A print is checked for one build plate, the one the job declares, and a
plate without a row cannot be declared:

| `id` | `name` (`curr_bed_type`) | `json_name` (`plate_<n>.json`) | `bed_max_c` | `z_trim_mm` | Measured in |
| --- | --- | --- | --- | --- | --- |
| `cool` | `Cool Plate` | `cool_plate` | 45 (seen 35 for PLA; the margin is chosen) | 0.022 | the first slice |
| `textured-pei` | `Textured PEI Plate` | `textured_plate` | 120, the machine's limit, so the filament's limit binds (seen 65, then 60 for PLA) | 0.002 | two slices |

`id` is Hotend's own id: the `plate_type` of the settings, the id recorded with a slice
(section 6) and the id of the same plate in the slicer's `system-profiles.toml`
(`PrinterProfile::plate_by_id`). The file must state the plate's `name`, its `json_name`
where `plate_<n>.json` states a `bed_type`, command no bed temperature above `bed_max_c` or
the filament's limit, and set no Z trim but 0 or the plate's `z_trim_mm` (the argument word
`plate_z_trim` of the `G29.1` row). A real slice is therefore acceptable only for the plate
it was sliced for: each textured slice as a Cool Plate job fails on the name, the bed
temperature and the trim, and the Cool Plate slice as a textured job on the name and the
trim. The High Temp and Engineering plates can be sliced for (section 6) and have no row
here, so a print for them is refused at the review (`unsupported`).

**The path of a job.** Each step yields the only value the next accepts:

```
check_job(path, name, DeclaredJob, PrinterProfile, ArchiveLimits)      -> CheckedJob
Delivery::deliver(&CheckedJob)                                         -> Delivered
ConfirmationRequest::start_print(&CheckedJob, &PrinterIdentity)        (the dialog shows .review_text())
ConfirmationAuthority::confirm(request, shown_text)                    -> Result<HumanConfirmation, TokenError>
StartToken::mint(HumanConfirmation, ttl)                               -> StartToken
start_print(&mut dyn Printer, StartToken, &CheckedJob, &UploadedJob,
            &[FilamentSource], StartOptions)                           -> StartWatchdog
```

- `DeclaredJob {plate, plate_type, filaments: [{slot, material, nozzle,
  nozzle_diameter_mm}]}` is what the application sliced with and what the person chose:
  `plate_type` is the build plate by the slicer's name (`Cool Plate`), `slot` the slicer's
  1-based filament number, `nozzle` the **device** extruder id, `nozzle_diameter_mm` the
  size of that nozzle. The plate type must be a `[[printer.plate]]` of the profile, the
  material a `[[filament]]` family or one of its aliases (nothing matches by prefix), the
  size one of `slice_file.nozzle_diameters_mm` (`invalid_declaration`, `unknown_filament`).
- An archive that holds any `.gcode` entry besides the declared plate's (any case), or two
  entries whose names differ only in case, is `JobError::Unusable` before anything else is
  read: G-code that was not checked does not ride along to a hand-off or an export. A
  warning compares the file's predicted time with what its own progress command tells the
  printer: `time_understated` (the first `M73 R` is more than twice the predicted time plus
  ten minutes). The stated filament length is held to what the commands push as an error
  (`filament_amount_mismatch`, below).
- `check_job` reads the file **once**. The path must be a regular file that is not itself
  a symbolic link (a folder on the way to it may be one), with one hard link, not empty
  and within `ArchiveLimits.max_file_bytes`; a named pipe or a device is refused before
  it is opened for reading. The bytes are read from start to end into memory and hashed
  while they are read. The archive checks, the G-code check, the summary and every
  delivery use those bytes and never the file again, so the hash, the verdict, the review
  text and what is sent are of the same bytes. The file on disk is read again only to
  refuse a job whose file was changed since (`JobError::Changed`).
- The archive (`archive::Archive`) has one reading: at most `max_entries` entries; names
  that are plain relative paths of printable ASCII, no two alike when case is ignored,
  none both a file and a folder; stored or deflated entries only, none encrypted or a
  symbolic link; the entries tile the file from its first byte to the directory with no
  gap and no overlap; each local header and data descriptor says what the directory says;
  no archive or entry comment; nothing after the end record; declared sizes and ratios
  within the limits; an entry that holds more or fewer bytes than it declares is an
  error. `ArchiveLimits::default()` is 128 MiB for the file, an entry and the G-code,
  512 MiB for all entries, 256 entries, a 1 MiB directory, ratio 200. Nothing is
  extracted. Failures are `JobError::Archive`.
- The archive may hold only names of `slice_file.entries` for the declared plate, and must
  hold `slice_file.required_entries` (`Metadata/plate_<n>.gcode`, its `.md5`,
  `Metadata/slice_info.config`). Any other entry, a second plate included, is
  `JobError::Unusable`: nothing travels to a printer unread.
- `check_job` then lints `Metadata/plate_<plate>.gcode` and compares what the file states
  with what its commands do and with the declaration. It fails with
  `JobError::Rejected(LintReport)` on any error of the G-code check (below) or of this
  comparison:
  - `gcode_md5_mismatch`: the `.md5` entry is not 32 hex digits or not the G-code's MD5;
  - `slice_info`: the account is not well-formed, describes no plate, more than one, or
    another plate than the declared one;
  - `time_mismatch`: no stated time, a stated time below the time the moves take at
    their commanded speeds (`GcodeStats.min_seconds`), or above that time times
    `time_max_factor` (4) plus `time_overhead_s` (1800);
  - `filament_amount_mismatch`: a filament's stated length further from the sum of its E
    moves than `length_tolerance` (2 %) plus `length_slack_mm` (10), a stated weight that
    implies a density outside `density_g_cm3` (0.9 to 1.5), or no stated length;
  - `filament_mismatch`: a filament without a stated material, or with a material of
    another family than declared; `undeclared_filament`; `filament_map_mismatch` (no map,
    or another nozzle than declared); `nozzle_mismatch`; `wrong_printer` (the model id of
    the account, the printer preset in `project_settings.config`); `plate_mismatch`
    (`curr_bed_type` in `project_settings.config`, `bed_type` in `plate_<n>.json`).
  An absent statement is an error, not an agreement.
- `JobSummary {file_name, sha256, size_bytes, plate, estimated_seconds, min_seconds, layers,
  filaments: [{slot, material, colour, grams, metres, measured_metres, nozzle, nozzle_role,
  nozzle_diameter_mm, selected_in_gcode}], max_nozzle_c, max_nozzle_reference_c, max_bed_c,
  max_chamber_c, extent, printer_name, stated_printer, plate_type, configuration: [{kind,
  command, section, count, what}], unplaced_moves}`. Measured from the commands:
  temperatures, `extent`, `layers`, `min_seconds`, `measured_metres`, `configuration`,
  `unplaced_moves`. Stated by the file and held to a measurement: `estimated_seconds`,
  `metres`, `grams`. Stated by the file and only shown: `colour`. Names from the profile's
  closed lists, after the file's statement was matched against them: `printer_name`,
  `stated_printer`, `plate_type`, `material`. `plate`, the nozzle and its size are the
  declared ones, which the file was held to. `file_name` is the file's name on disk cut to
  60 characters, with every character but letters, digits, `.`, `_` and `-` replaced by
  `_`; the application's label is not in the summary. No free text of the file or of the
  application reaches the summary. It cannot be constructed or deserialised by other code.
- `JobSummary::review_text()` (also `ConfirmationRequest::review_text()`) is the text a
  person confirms, built from the summary alone: file and printer; the time the file
  states and the least time its moves take; the layers counted from the commands; each
  filament with material, nozzle, nozzle size, the length the commands push, and what the
  file states (length, weight, colour); the highest nozzle, bed and chamber temperature
  the commands set; the build plate chosen for the job; that every heater is off when the
  file ends; the printed area; every accepted command that changes limits or stored
  configuration with its count, its place and what it does (or "unknown"); the number of
  moves made from a position only the printer knows; a line saying that what the
  printer's screen shows for the job is read by the printer from the file and was not
  checked; and the SHA-256. Nothing the agent wrote is in it. The core hands it on as
  `PrintReview.review_text` (section 8.2); a confirmation needs this exact text (Start
  token, below), so the terminal and the desktop dialog show it as it is.
- `CheckedJob::remote_name()` is the cleaned label, `-`, the first 32 hex digits of the
  file's SHA-256 and `.gcode.3mf`: the name of the file on the printer and of a placed
  copy. Two different files never share it.

**The G-code check** (`lint_gcode`, called by `check_job`). It reads the G-code as hostile
input. The threat model is at the top of `crates/hotend-printer/src/lint.rs`: a tampered or
substituted file; custom G-code injected through a profile; a slicer bug or wrong profile
that drives a nozzle outside its reach or into the plate; temperatures above the limits
and heaters left on; firmware and configuration commands; a file that says it is for
another printer, build plate, nozzle size or filament, or does not say; plastic where no
print puts it. It does not simulate acceleration, cooling or collisions with the print, it
does not know the shape that was meant, and it cannot know what a vendor command does
inside the firmware. A file's statements are part of the file: comparing them catches a
wrong profile or a wrong file, not a file whose statements were written to match, which
is still held to every other rule.

- **One command per line.** The first word is `G` or `M` and a number with an optional
  sub-code (`G150.1`, `M620.11`), or `T` and a number. Every later word is a parameter, one
  letter and its value, whatever the letter (`M1006 ... M50 N50`, `M204 P R T`, `T0 H-1`).
  Refused as two commands or as ambiguous: a `G` word after the command, an `M` or `T` word
  that no row of the command lists, a number glued to a letter, a value without a letter, a
  letter given twice, parentheses, control and non-ASCII characters. Letters are
  case-insensitive, `N` line numbers and `*` checksums are stripped, a lone carriage return
  ends a line. A line with a command is at most 4096 bytes. A line that is only a comment
  may be longer (the slicer writes whole routines into config comments); the excess is
  skipped and capped at 1 MiB in total (`comment_too_large`).
- **The table.** `[[family.gcode.command]]` rows have `codes`, `sections` (`start`, `body`,
  `end`; all three when absent), `class`, either `forms` (the exact sets of parameter
  letters; moves and `G92` take `letters`, any subset) and `args` (a limit for every value:
  `[low, high]`, `{one_of = [...]}`, `{up_to = n}`, `"flag"`, `"nozzle_temp"`, `"bed_temp"`,
  `"chamber_temp"`, `"nozzle_temp_ref"`, `"nozzle"`, `"filament_a"`, `"service_tool"`,
  `"plate_z_trim"`: 0 or the `z_trim_mm` of the declared build plate),
  `moves` (the nozzle is somewhere unknown afterwards), `stays_put` (why a command of unknown
  meaning does not move it), `requires = "relative"`, `config` and `max_count`, `verbs` (for
  `M1002`), `mask_chars`, `what`, `basis` (`standard`, `stated`, `inferred`, `unknown`) and
  `verified`. A line passes only if it matches one row: command word, section, exact letter
  set, every value within its limit. Loading the file refuses a row with a parameter that
  has no limit, a row of unknown meaning that is taken to leave the nozzle in place without
  a reason, a configuration row without `max_count`, `G` as a parameter letter and two rows
  for the same command, section and form. `[[family.gcode.deny]]` names firmware, stored
  settings, configuration, network, storage and pin commands; they are refused as
  `denied_command` wherever no row accepts them. Anything else not in the table is
  `unknown_command`.
- **Sections.** Bambu Studio writes four marker comments: `; EXECUTABLE_BLOCK_START`,
  `; MACHINE_START_GCODE_END`, `; MACHINE_END_GCODE_START`, `; EXECUTABLE_BLOCK_END`. They
  divide the file into the start routine, the print and the end routine, and the check uses
  them only to choose the rows that apply. They grant no trust, because whoever writes the
  file writes the markers. Each marker counts once and in order; the first only before any
  command; a marker anywhere else is `section_marker`, and so is a command after the last
  one. A file without markers is the print from its first line to its last. A file with
  markers that ends before the last one is `truncated`. The start and end routines may not
  lay plastic along a path (`printing_outside_body`) and are limited to
  `start_max_commands = 600` and `end_max_commands = 200` (`section_too_long`; the real
  routines have 278 and 77). What a forger gains by writing markers is therefore the stock
  routine's own commands at values no larger than the stock ones, each as often as the table
  allows, and its five moves from a position the file does not give, none of which can
  close the gap to the bed (Motion, below); the test
  `the_most_a_forged_start_routine_gains_is_the_stock_commands_at_their_stock_values`
  states that case and shows each next step failing.
- **Configuration commands.** A row with `config` (`travel_limits`: `M211`; `stored_settings`:
  `M500 D1`; `motion_limits`: `M201`, `M203`, `M204 P R T`, `M205`, `M201.2`; `motor_current`:
  `M17` with `D`, `S`, `Z`, `R`; `z_offset`: `G29.1`) passes only in the start or end routine,
  in the exact form and with the values that were seen (motion limits up to the values of
  the stock machine profile), at most `max_count` times (`too_many`). Each accepted one is
  recorded in `GcodeStats.configuration`, reported in one warning `configuration_commands`
  and listed in the review text. In the print, or in any other form, they are refused.
- **Motion.** A move that lays plastic while moving in X or Y must stay inside the `reach` of
  the nozzle that makes it, at a known height (`extrusion_outside_reach`, `unverifiable_move`).
  Every other move must stay inside that nozzle's `travel` box, or its reach when it has
  none (`out_of_reach`). The travel box of the main nozzle was measured in the real slice: it
  is the reach plus 0.2 mm in front of the plate in Y, where the start routine draws its
  load line. Arcs are checked over their whole path, relative moves are followed, and the
  extrusion counter is followed in both extrusion modes. `G92` may reset `E` only; with X,
  Y or Z it is `unexpected_parameters`, because every later coordinate would mean another
  place. After homing, a
  tool change, a firmware routine (`moves = true`) or conditional lines that moved (`M622` to
  `M623`, `M620 S..` to `M621 S..`), the position is unknown. From there an absolute move is
  checked by its target and plastic cannot be laid. An absolute target (a move, a purge
  line, a probing area) is accepted only on an axis the file has homed with a `G28` outside
  conditional lines (`unverifiable_move`), and a `G28` that homes Z without X and Y needs X
  and Y homed first (`command_out_of_place`). Measured in the three real slices: the stock
  start routine homes with `G28 X T300 R`, moves Y to absolute places, then homes Z with
  `G28 Z P0 T400`; so in the start routine, and only for that form, homing X also homes Y.
  A relative move from there (an
  "unplaced" move) is the one kind nothing can check, while the stock start routine has
  the printer's own travel limit check switched off. `[family.gcode.unplaced]` holds it
  to the direction and the distances of the stock routine; every range is `[furthest in
  the negative direction, furthest in the positive direction]` in mm, and a lower Z is a
  smaller gap between nozzle and bed:
  - `sections` (`start`): only there, and without an `E` word;
  - `plain`: what one `G0` or `G1` may cover (X 0, Y -16 to 0, Z 0 to 6). It never lowers Z;
  - `guarded`: what one guarded move (`G380`) may cover (Z -12 to 22);
  - `net`: where the sum of the moves on an axis must stay, counted from where the printer
    itself last left that axis (X 0, Y -16 to 0, Z 0 to 22). The sum for Z never goes below
    0, so a guarded move lowers the nozzle only as far as a move lifted it just before,
    and homing, a firmware routine, a tool change or conditional lines in between end
    that allowance;
  - `total_mm` (Y 16, Z 45) and `max_count` (5): the distance and the number of such moves
    in a file, which are the stock routine's own.
  A move outside `plain`, `guarded` or `net` is `unverifiable_move`, one over `total_mm` or
  `max_count` is `too_many`, and each accepted one is counted in `unplaced_moves`. Loading
  the profile refuses a table in which a plain move or the sum may lower Z. A guarded
  move from a known position must also end inside the travel box. `G29` (probing area)
  and `G130` (load line) are checked by their coordinates against the travel box. Homing
  after plastic has been laid is refused.
- **Tools.** `T<n>` selects project filament n counted from 0 and must be a declared
  filament (`undeclared_tool`); the form `T0 H-1` is accepted with `H-1` only. The numbers
  65279 and 65535 are not filaments. They pass only in the end routine, as `T65279` or
  `T65535` and as the `S` value of `M620 S<n> B` / `M621 S<n> B`. Inside conditional lines
  only the filament the switch is for may be selected (`branch`).
- **Height and plastic** (`[family.gcode.limits]`; each number with what the real slice
  showed and the margin). No move, load line or probing height names a Z below `min_z_mm`
  (0.2, the first layer; seen 0.2, no margin) (`too_close_to_bed`), and `G29.1 Z` is 0 or
  exactly the trim of the declared build plate (Cool Plate 0.022, Textured PEI Plate 0.002;
  the plate table above). Plastic is laid at most `max_layer_step_mm` (0.32; seen 0.2) above the
  highest plastic laid before, or above the plate for the first (`extrusion_in_the_air`).
  A move pushes at most `max_e_per_mm` (0.1 mm/mm; seen 0.0634) (`over_extrusion`).
  Without a move in X or Y, filament is pushed only as far as it was pulled back before,
  plus `in_place_slack_mm` (0.001) (`extrusion_in_place`), and pulled back at most
  `max_retracted_mm` (4; seen 3.4) in all (`retraction_too_long`). The stock routine's
  purge and prime are firmware routines and the load line, rows of the start routine
  only. An `E` value is read only after `M82` or `M83`, and never as an absolute value
  under `G91` (`ambiguous_extrusion`). Filament is pushed only while a command the
  printer cannot skip has set the printing nozzle to `min_extrude_c` (170) or more
  (`cold_extrusion`). In a job with several filaments, plastic is laid only after the
  file has selected one, so that each amount is counted for a filament
  (`unassigned_extrusion`). `F` is at most `max_feed_mm_min` (60000; `value_out_of_range`). A
  file of more than `max_lines` (5,000,000; seen 42,200) lines is not read to the end
  (`gcode_too_large`).
- **Temperatures.** `M104` (`S`, `S A`, `S T<device extruder id>`), `M109` (`S`, `S A`),
  `M140`, `M190`, `M141`, `M191` are held to the declared filament and the machine, and the
  bed also to the `bed_max_c` of the declared build plate (Cool Plate 45, seen 35; Textured
  PEI Plate 120, seen 65) (`temperature_over_limit`). Heating a nozzle no filament of the job is on is
  `undeclared_nozzle`. Vendor values that may be a nozzle temperature (`M620.10 T` and `P`,
  `G150 T`, `G383 T`, `G383.3 T`, `M620.17 S`) are held to the same limits and reported apart
  as `max_nozzle_reference_c`. The end routine may not heat (`heating_in_end_routine`). When
  the file ends, every nozzle, the bed and the chamber must have been switched off by a
  command outside conditional lines, each heater by its own command (`heaters_left_on`).
- **What the file says about itself.** The config comments `printer_model`,
  `printer_settings_id`, `curr_bed_type` and `nozzle_diameter` and the layer count
  (`; total layer number: n`) are read wherever they stand in the file; a second statement
  that says something else is an error. `printer_model` must be there and be one
  of `slice_file.printer_models`; `printer_settings_id`, where stated, one of
  `printer_settings_ids` (`wrong_printer`). `curr_bed_type` must be there and be the
  declared `plate_type` (`plate_mismatch`). `nozzle_diameter` must be there and give the
  declared size for every filament's nozzle (`nozzle_mismatch`). A stated layer count must
  be the number of heights at which the commands lay plastic (`layer_count_mismatch`).
  `M1002 set_filament_type` must name a declared filament (`filament_mismatch`).
- A `Finding` is `{severity, code, line, message, excerpt}`. Error codes: `unknown_command`,
  `denied_command`, `command_out_of_place`, `unexpected_parameters`, `value_out_of_range`,
  `too_many`, `multiple_commands`, `ambiguous_syntax`, `malformed_number`, `text_argument`,
  `line_too_long`, `out_of_reach`, `extrusion_outside_reach`, `printing_outside_body`,
  `unverifiable_move`, `malformed_arc`, `branch`, `temperature_over_limit`, `undeclared_tool`,
  `undeclared_nozzle`, `filament_mismatch`, `section_marker`, `section_too_long`, `truncated`,
  `wrong_printer`, `plate_mismatch`, `nozzle_mismatch`, `layer_count_mismatch`,
  `too_close_to_bed`, `extrusion_in_the_air`, `over_extrusion`, `extrusion_in_place`,
  `retraction_too_long`, `ambiguous_extrusion`, `cold_extrusion`, `unassigned_extrusion`,
  `heating_in_end_routine`,
  `heaters_left_on`, `comment_too_large`, `gcode_too_large`, `invalid_declaration`,
  `unknown_filament`, and from `check_job` `slice_info`, `gcode_md5_mismatch`,
  `time_mismatch`, `filament_amount_mismatch`, `filament_map_mismatch`,
  `undeclared_filament`. Warnings: `configuration_commands`, `no_moves`. A message repeats
  a name from the file or the declaration only when it is a plain name (at most 32
  letters, digits, spaces and `-_+./`); otherwise it says so instead.

The real files never enter the repository. `tests/real_slice.rs` runs the check on them when
`HOTEND_REAL_SLICE_DIR` names a folder that holds `plate_1.gcode.3mf` (the Cool Plate
slice) and the two textured slices
(`textured-a.gcode.3mf` and `textured-b.gcode.3mf`): each is acceptable for its own plate
and refused for the other, and harmful values next to the measured ones are refused. It
is ignored, with the reason, by a plain `cargo test`; run with `-- --ignored` it fails when
the variable is not set or a file is not there. `cargo run -p hotend-printer --example check_sliced -- <file.gcode.3mf>`
prints the verdict, the review text and what was measured.

**Start token.** `ConfirmationAuthority::take()` returns the authority once per process.
The application takes it at start-up and gives it only to the code that draws the native
confirmation dialog (`print_confirm`, section 8.2); the tool gateway and the webview command
handlers never hold it. `HumanConfirmation` and `StartToken` have no public constructor, no
`Clone` and no `Deserialize`. `ConfirmationAuthority::confirm(request, shown)` takes the
text the dialog displayed and refuses any text but `request.review_text()`
(`TokenError::NotWhatWasShown`); this cannot prove what was on the screen, it makes the
dialog code say what it drew. `HumanConfirmation::record()` is `ConfirmationRecord {sha256,
review_sha256, plate, remote_name, printer, confirmed_at}`. A token is bound to the job's
identity (the file's SHA-256 and size, the plate, the name on the printer, the SHA-256 of
the declaration and the SHA-256 of the review text), the printer's serial and an expiry (at
most 300 s; a confirmation must be minted within 120 s). `start_print` consumes the token,
reads the file on disk again and refuses with `TokenError::{WrongFile, WrongJob,
WrongPrinter, Expired, FileChanged, Unreadable}`: `WrongJob` is another job of the same
file (another plate, declaration, review text or label). It also refuses an `UploadedJob`
that is not this job's upload to this printer (hash, size, name and serial;
`StartError::NotUploaded`). The file is stored on the printer as
`CheckedJob::remote_name()`, `<label>-<first 32 hex digits of the SHA-256>.gcode.3mf`,
and the start command names that: a later upload under the same label is another file
and cannot take the confirmed one's place (`NotUploaded`), a job checked for another model, a busy printer, and filament
sources that
do not match what the printer reports (`StartError::Mismatch`: empty tray, other material
family, an AMS or external holder that feeds another nozzle than the job was sliced for).
A confirmation for a hand-off (`ConfirmationRequest::hand_off`) cannot mint a start token.

An editable Bambu Studio project is handed over on a confirmation of its own:
`ConfirmationRequest::project(test_slice, project_sha256, destination)` (`None` unless the
hash is 64 hex digits and `destination` is one non-blank plain line of at most 1000 bytes:
`is_plain_line`, no control character, no white space but the plain space, so no tab, line
break, U+2028 or U+2029, and no invisible or bidirectional format character of
`is_hidden_format`, U+200B to U+200F, U+202A to U+202E, U+2060 to U+206F, U+FEFF and the
rest of Unicode's Cf) is built from the checked test slice that Bambu Studio's command line
made from that very project file. Its `review_text()` is, line by line: "Editable Bambu
Studio project. SHA-256 of the project file: <hash>.", "Where it goes: <destination>",
`PROJECT_OUTSIDE_CHECK` ("A project opened this way is sliced again by Bambu Studio, so it is
outside Hotend's check."), a sentence that the lines below are about Hotend's test slice of
the project only, and the test slice's own review text. The identity of the confirmation adds
the project's hash and the destination to the test slice's, and its review hash is that of
the whole text, so `confirm` refuses a dialog that showed only the test slice's text, or the
text of the same project going somewhere else.
`HumanConfirmation::redeem_for_project(test_slice, project_sha256, destination)` is the only
use of such a confirmation: it refuses a start, a Bambu Connect hand-off or an export
confirmation (`NotForThis`), another project, another destination or another test slice
(`WrongFile`, `WrongJob`),
an old one (`Expired`) and a test slice whose file changed on disk. A project confirmation
mints no start token (`NotForAPrint`) and releases no Bambu Connect link (`NotForThis`). The
caller writes the project's bytes and holds them to the hash itself (section 8.2).

**Commands.** `Command` is closed: `StartPreparedJob` (constructible only inside
`start_print`), `Pause`, `Resume`, `Stop`, `SetSpeed(Silent|Standard|Sport|Ludicrous)`,
`Light(bool)`, `RequestStatus`. There is no raw G-code command; a test serialises every
variant and fails on `gcode_line`. On the wire (topic `device/<serial>/request`; every request
also carries a `sequence_id`):

| Command | JSON | QoS |
| --- | --- | --- |
| start | `{"print": {"command": "project_file", "param": "Metadata/plate_<n>.gcode", "url": "ftp:///<remote name>", "subtask_name", "use_ams", "ams_mapping", "bed_leveling", "flow_cali", "vibration_cali", "layer_inspect", "timelapse", ...}}` (flags as booleans; a profile can ask for 0/1 and for `ams_mapping2`) | 0, never repeated |
| pause, resume, stop | `{"print": {"command": "pause" \| "resume" \| "stop"}}` | 1 |
| speed | `{"print": {"command": "print_speed", "param": "1".."4"}}` | 0 |
| light | `{"system": {"command": "ledctrl", "led_node": "chamber_light", "led_mode": "on" \| "off", ...}}` | 0 |
| status | `{"pushing": {"command": "pushall"}}` | 0 |

`FilamentSource` is `Ams {unit, tray}`, `External(tray id)` or `Unused`, one per declared
filament in slot order.

**Reports.** `protocol::report::parse` reads a payload from `device/<serial>/report` as
hostile input: at most 1 MiB, unknown fields ignored, a field of the wrong type read as
absent, lists and strings cut, control characters removed, never a panic. `PrinterStatus`
merges partial reports: `gcode_state`, stage, progress, remaining minutes, layers, job name,
nozzle temperatures keyed by device extruder id, the active nozzle, bed, chamber,
`print_error`, HMS codes, AMS units (`id`, `high_temperature`, the `nozzle` each unit feeds,
humidity, drying time, trays with material, colour as `#RRGGBBAA`, remaining percent),
external spools, speed level, light, storage. `phase()` is one of `idle`, `preparing`,
`heating`, `printing`, `paused`, `finished`, `failed`, `offline`, `unknown`.

**After the start.** `StartWatchdog::observe(status, waited, profile)` returns `Waiting`,
`Started {phase}`, `NotStarted {reason: ams_drying | stayed_idle | other_job | no_reports,
message}` (after the deadline, 90 s by default) or `Failed {problems}` (an error that was not
there before the start, for example `not_authorised` outside Developer Mode). An accepted
command is never taken as a started print. `JobTracker::observe` yields `JobEvent`s
(`phase`, `progress`, `problem`, `ended` with `finished | cancelled | failed`). Every
`Explanation {message, raw_code, meaning, blocking, help_url}` keeps the printer's own code
(`HMS_0300_0100_0001_0007`, `0300_400C`) beside the plain sentence.

**Delivery.** One trait, three paths; `capabilities()` returns `{kind, name, sends_file,
starts_print, controls_print, live_status, reports_result, can, cannot, cost, needs}` for
the interface (`kind`: `simulated`, `lan`, `bambu_connect` or `export`; `needs` may be
empty). A `PrinterDelivery` has the capabilities of the printer it delivers to
(`Printer::capabilities()`): `Capabilities::simulated()` for the simulated printer, whose
sentences are about the simulation only (it runs the job, shows progress, stops on request;
it makes nothing), and `Capabilities::lan()` for a real printer in Developer Mode. A
printer never borrows another's sentences.

| Path | Type | Result |
| --- | --- | --- |
| A printer Hotend talks to (simulated, or LAN in Developer Mode) | `PrinterDelivery` (any `Printer`) | `Delivered::OnPrinter(UploadedJob)` |
| Bambu Connect | `BambuConnectHandOff` (a `HandOffFolder`) | `Delivered::HandOff`; `HandOff::link(job, HumanConfirmation)` returns a `BambuConnectLink`; the caller gives it to its `LinkOpener` |
| Export | `ExportToFolder` (folder or USB drive) | `Delivered::Exported {path}` |

A placed or uploaded file is written from the job's own copy of the checked bytes (never
from the file on disk again) under `CheckedJob::remote_name()`, hashed while written and
discarded unless it has the job's hash. `HandOff::link` is given only against a
confirmation of that job and only while the placed file still holds those bytes. The
start command's `md5` field is sent empty and `bed_type` as `auto`; what the printer
compares is not known (README checklist).

**The Bambu Connect hand-off.** Bambu Connect runs on Windows and macOS and imports a file from
the documented link `bambu-connect://import-file?path=<path>&name=<name>&version=1.0.0`. The
form that works was measured with Bambu Connect on Windows on 2026-11-18: the Windows path with
backslashes, encoded as `encodeURIComponent` encodes it. A print has gone this way from the
application to an X2D; the form on macOS was not tried.

- *The folder* (`handoff::HandOffFolder::new(folder, host)`): an existing directory,
  canonicalised once when it is chosen. `HostKind` is `wsl`, `windows`, `macos` or `linux`
  (`HostKind::detect()`: the build for Windows and macOS; on Linux, WSL when
  `/proc/sys/kernel/osrelease` contains "microsoft" or `WSL_DISTRO_NAME` is set;
  `HOTEND_HANDOFF_HOST` names the kind for tests and tools). Under WSL the folder must be a
  directory strictly below `/mnt/<one drive letter>/` (a drive's root is refused, and so is a
  link that leads off the drives), every name in it must be one Windows accepts, and its
  Windows form must be at most 240 characters; otherwise `NotOnWindowsDrive`, whose sentence
  says to choose a Windows drive folder in Settings. On Windows the local path is used
  without the `\\?\` prefix of a canonical path; a network or device path is refused. Under
  WSL and on Windows a folder whose Windows form holds a comma is refused (`Comma`, whose
  sentence says to choose a folder without one): explorer.exe, which opens a placed project,
  reads a comma in its argument as the end of the path (the link itself encodes a comma). On macOS and Linux the local path is
  used.
- *The translation* (`handoff::wsl_to_windows`, pure: no `wslpath`, no shell, nothing is
  read): `/mnt/d/Models/Hotend/a.gcode.3mf` is `D:\Models\Hotend\a.gcode.3mf`. Refused:
  a relative path, `..` or `.`, anything not under `/mnt/<letter>/`, a name with
  `<>:"/\|?*` or a control character or ending in a dot or a space, a reserved device name
  (`CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, also with an extension, any case)
  and a result longer than 240 UTF-16 units.
- *The placed file* (`HandOffFolder::resolve`): after the copy is in place its canonical path
  must be a regular file directly inside the chosen folder under the name it was placed as;
  a link that leads elsewhere (even to the same bytes) is refused. Its name is
  `remote_name()` (the cleaned label and 32 hex digits of the SHA-256). The folder is checked
  again before the copy: a folder that has become a link since it was chosen is not written
  into, and a partial or refused copy is removed only while the folder is still that
  directory (never through a link).
- *The link* (`delivery::BambuConnectLink`): only `HandOff::link` makes one (no public
  constructor, no `Deserialize`). Before it does, it redeems the hand-off confirmation for
  that job, reads the placed file again (it must hold the confirmed bytes) and resolves it
  again in the folder. The text always starts with `bambu-connect://import-file?path=`; the
  path is the Windows form under WSL and on Windows, the local path elsewhere; `name` is the
  job's label restricted to letters, digits, space, `.`, `_` and `-`, at most 64
  characters ("Hotend print" when nothing is left); both are encoded as
  `encodeURIComponent` encodes them, so no path or name can add a parameter or end the link.
- *The opener* (`opener::LinkOpener`): `open(&BambuConnectLink)`, `open_project(&PlacedProject)`
  (a `.3mf` project placed in the hand-off folder, opened with the program the system associates
  with it, for the "Open in Bambu Studio" fallback; the core asks it only after a confirmed
  project hand-over, section 8.2), `describe()` and `is_stand_in()`. `StandInOpener` opens
  nothing: it records each link (a project as `open-project <path>`) in memory and, when
  `HOTEND_STAND_IN_OPENER` names a file, as one line appended to it. The real openers start one
  program by its absolute path, through `std::process::Command`: no shell, no `cmd /c start`, no
  PowerShell. What they start is an `OpenCommand {program, args, current_dir}` built by a pure
  function (`WindowsFromWsl::link_command`, `::project_command`, and the same on
  `NativeWindows`), which the tests compare without running anything. `WindowsFromWsl` opens the
  link with `/mnt/c/Windows/System32/rundll32.exe` and the two arguments
  `url.dll,FileProtocolHandler` and the link, and a project with `/mnt/c/Windows/explorer.exe`
  and its Windows path as the one argument; both start in `/mnt/c/Windows` (`C:\Windows`),
  because a program started from WSL otherwise starts in `\\wsl.localhost\<distribution>\...`, a
  directory Windows programs refuse. `FileProtocolHandler` hands the text after it, as it is, to
  the Windows shell (`ShellExecute`, what Win+R does), which starts the program registered for
  `bambu-connect:` with the link as its `%1`. The link does not go to explorer.exe: started on
  the WSL share of a Windows 11 machine it opened a folder window instead of Bambu Connect,
  although explorer.exe received the link as one argument with its `&` intact. Before it starts
  rundll32 the opener checks the link again: only letters, digits, ``-_.!~*'()``, `%` followed
  by two hex digits and the link's own `:/?=&` (no space, quote, `^`, `,`, non-ASCII), otherwise
  `OpenError::Spawn` and nothing starts; a link the core built always passes, since the path and
  name are encoded as `encodeURIComponent` encodes them. A project path is not encoded: it is
  one argument, which WSL's interop (and Rust on Windows) quotes when it holds a space, and
  explorer.exe is not a shell, so `&`, `^` and `%` in it are plain characters; the comma
  explorer.exe would read as the end of the path is refused when the folder is chosen.
  `NativeWindows` starts the same programs under `%SystemRoot%` (`System32\rundll32.exe`,
  `explorer.exe`), in `%SystemRoot%`. `MacOs` runs `/usr/bin/open` with the link or the project
  path, and on Linux `NoBambuConnect` refuses. Success is the start of the program: no exit
  status is read (explorer.exe exits with 1 even when it did what it was asked).
  `platform_opener(host)` gives the real one, and the stand-in in this crate's test build or
  while `HOTEND_STAND_IN_OPENER` is set (`real_openers_allowed`, which every real opener checks
  again before it starts anything).
- *What is never done*: no link is built from text a person, a project, a file or the
  agent supplies; nothing is opened before the confirmation or for a file that did not pass
  the check (there is no `CheckedJob` for it, so no `HandOff` and no link); no shell runs; no
  file is written outside the chosen folder; nothing is sent to a printer: the person chooses
  the printer and presses Print in Bambu Connect.

**LAN transport** (feature `lan`). `lan::Endpoint` accepts loopback addresses only.
`lan::pair(endpoint, confirmed_serial, authorities)` makes one TLS 1.2 handshake, requires a
certificate whose common name is the serial the user confirmed and whose chain ends at one of
the certificate authorities in `authorities`, a PEM file the user supplies with Bambu Lab's
printer authorities (`CertificateProblem::UntrustedIssuer` otherwise: a self-signed
certificate that only names the serial is not paired). Hotend does not ship that file (see
docs/DEPENDENCIES.md). Every certificate in the file must be one of Bambu Lab's known
authorities, by SHA-256 (`tls::BAMBU_LAB_CA_SHA256`); a path that is not a plain file of at
most 1 MiB holding only such certificates gives `LanError::NoIssuers` before anything is
connected to. It returns `Pairing
{serial, certificate_sha256}` (not a secret; storable). `pair_with_issuers` takes other
authorities (`Issuers::from_pem`), for the tests' stand-in printer. A start whose filament
source is an AMS HT is refused by the LAN printer (`NotSupported`): the flat `ams_mapping`
value for it (`flat_ht = "slot_16"`, unverified) has not been confirmed on a printer. `LanPrinter::connect(endpoint, &pairing,
AccessCode, profile, name)` accepts only that certificate on MQTT (8883, user `bblp`) and on
implicit FTPS (990, data connection encrypted and always to the control connection's
address). `AccessCode` has no `Display`, `Serialize` or `Clone` and a redacted `Debug`. A
session that drops is not re-opened and queues nothing. `discovery::parse_announcement` parses
an SSDP announcement (UDP 2021); no socket is opened for discovery.

### Printer status (read-only)

Hotend can read a real printer's status, and does nothing more with it: no command, no
`pushall`, no file, Developer Mode off. It connects only when the user sets it up. The pieces,
from the printer layer up:

**Listener** (`hotend_printer::lan::listen`, feature `lan-status`; `lan` includes it).
`StatusListener::connect(target, trust, AccessCode, profile, ListenOptions)` opens MQTT 3.1.1 over
TLS 1.2 to one `ListenTarget` and subscribes to `device/<serial>/report` only. The session is a
small loop over the TLS stream built on rumqttc's packet codec, not rumqttc's client: the only
function that writes packets takes `Outgoing`, a closed list of CONNECT (user `bblp`, the access
code as password, clean session), SUBSCRIBE (that one topic, QoS 0, packet id 1), PINGREQ (every
half keep-alive; default keep-alive 30 s) and DISCONNECT (when the listener is dropped). The type
has no publish method and holds no client that has one; a QoS 1 report is not acknowledged (a
PUBACK would be a fifth packet). Reports go through `report::parse` (hostile input; a payload over
1 MiB or not a JSON object is dropped and counted in `ListenerState::dropped_reports`) and are
merged by `PrinterStatus::apply`, so partial reports build the status up until the printer sends a
complete one on its own. A printer silent for twice the keep-alive, a closed connection, a refused
sign-in (`LanError::AccessCodeRefused`) or a refused subscription
(`LanError::SubscriptionRefused`) ends the session; it is never re-opened by itself
(`ListenerState::ended` says why). Proof: `tests/lan_status.rs` runs a whole session (sign-in,
partial reports, keep-alive, end) against a stand-in broker on loopback that records the type of
every packet it receives, and asserts that Hotend sent only CONNECT, SUBSCRIBE, PINGREQ and
DISCONNECT, zero PUBLISH, and nothing on any topic; a unit test scans the listener's source for
any packet constructor but those four; a `compile_fail` doctest calls a publish method that does
not exist.

**Address allow-list.** `ListenTarget::with_port` accepts an IPv4 address in 10/8, 172.16/12
or 192.168/16, or loopback (the tests' stand-in); everything else is `LanError::NotPrivate`.
`parse_listen_address` reads what the person typed: four numbers only (names are never looked
up, ports and IPv6 are refused: `LanError::NotAnAddress`, which does not repeat the text), and
loopback is refused there. The core stores one address and serial
(`PrinterLanAddress {ip, serial}`, store key `printer_lan`, not part of `Settings`).

**TLS, never unchecked.** The certificate must name the configured serial as its common name
(one common name only), and the server must prove it holds the key (the handshake completes
before CONNECT, which carries the code, is written: a refused certificate never sees it). Then
one of two modes:
- *CA file*: `Trust::Authorities(Issuers)`: the chain must end at one of Bambu Lab's printer
  authorities, from a file the user supplies, `<config>/bambu-ca.pem` (`<config>` is
  `HOTEND_CONFIG_DIR`, else the user's configuration folder plus `hotend`:
  `~/.config/hotend` on Linux). The core resolves and reads it with
  `Issuers::bambu_lab_from_file`: every certificate in it must be one of the five known by
  SHA-256 (`tls::BAMBU_LAB_CA_SHA256`). A file that is there but fails that is refused, never
  ignored in favour of a pin. Hotend does not ship the file: the certificates are Bambu Lab's
  and the collection that publishes them (OpenBambuAPI) is under the GFDL. (Protocol facts such as
  field names also come from OpenBambuAPI's notes; no text or file of it is in this repository.)
- *Pin*: without the file, `probe_certificate(target)` makes a TLS handshake only (nothing is
  sent, not even CONNECT) and returns `CertificateSeen {sha256, subject, issuer, serial}` for a
  certificate that names the serial, whoever issued it. The core keeps that first look;
  `printer_certificate_confirm(sha256)` stores the pin (`CertificatePin {ip, serial, sha256,
  subject, confirmed_at_ms}`, store key `printer_lan_pin`) only when `sha256` is the
  fingerprint the last first look returned for the address and serial set now, and it is the
  only call that writes that key. Later sessions use `Trust::Pinned(sha256)`: another
  certificate is `CertificateProblem::Changed` ("not the one this printer had when it was
  paired"). A pin is for its address and serial; changing either makes the setup
  `unconfirmed` again.
- *What the subject proves*: nothing alone. Its common name is the serial number, which the
  printer announces on the network, so any device can make a certificate naming it. The CA
  chain proves Bambu Lab issued it; a pin proves only that it is the certificate confirmed the
  first time (trust on first use), so the first confirmation belongs on a trusted network.
  The core says this with every first look (`PrinterCertificate::note`).

**Access code.** Read by the core from the environment variable `BAMBU_LAN_ACCESS_CODE` first,
then from the secret store (`printer-lan.access-code`, set write-only with
`printer_lan_key_set`, like a provider key). The variable can be loaded from a file the user
creates, for example `~/.config/hotend/bambu-lan.env` (mode 600, one line
`BAMBU_LAN_ACCESS_CODE=<code>`). The code is held as `AccessCode` (no `Display`, `Serialize` or
`Clone`, redacted `Debug`, wiped on drop), written once into the CONNECT packet and wiped from
the packet and its buffer right after; it is in no setting, audit row, event, error or log line
(`printer_lan_key_status` says only `environment`, `secret_store`, `missing` or `unreadable`).
The text read from the variable is moved into the `AccessCode` without a copy. No program Hotend
starts itself inherits the variable: the sandbox starts every child with an empty environment, the
Python prefix query runs with an empty one, and the programs started for a link, a file or the
system browser have it removed with the API keys and `HOTEND_SECRETS_FILE`
(`hotend_printer::secret::SECRET_ENV`). It stays in Hotend's own process environment: removing
it there (`std::env::remove_var`) is unsound once other threads run. The web view's own helper
processes, which the toolkit starts, inherit that environment. The command line's prompt and piped
input are read into buffers wiped on drop.

**Core facade.** `printer_lan_get` (`PrinterLanSetup {address, trust: ca_file | pinned |
unconfirmed, ca_file, pin, access_code, access_code_store, watching}`; `access_code_store` is
where a typed code would be kept: `keychain`, `file`, `environment`, which keeps none, or
`memory`), `printer_lan_set(ip, serial)`,
`printer_lan_clear`, `printer_lan_key_set(code)` / `printer_lan_key_delete` /
`printer_lan_key_status`, `printer_certificate_probe` (`PrinterCertificate {ip, serial,
sha256, subject, issuer, note}`), `printer_certificate_confirm(sha256)`,
`printer_certificate_forget`, `printer_status_read(wait)` (one session: waits up to `wait`,
at most 120 s, for a report, then DISCONNECT; while a watch runs, its latest status),
`printer_watch_start` / `printer_watch_stop` / `printer_watch_latest`. Address, certificate
check and access code are settled before anything is connected to; each refusal is
`not_configured` with what to do. `printer_watch_start` checks, connects and records the
session under one lock: while a session runs, another start is refused (`invalid_input`,
"already running ..."), so two starts at once never open two sessions. Every step is audited
(kind `printer_lan`: `address_set`, `certificate_seen`, `certificate_confirmed`,
`access_code`, `session_open`, `session_end`, with address, serial and fingerprint, never the
code).

`printer_certificate_confirm` must only ever be called after a person confirmed the
fingerprint themselves: in a native dialog of the application (not web content) or by
typing `yes` at a terminal (the command line). It must never be an IPC command the webview
can call, directly or through another command: a confirmation the page could send would let
any script in the window trust a certificate nobody looked at.

**Status model and events.** `LanPrinterStatus {state: PrinterState, nozzles: [{id,
current_c, target_c}], bed, chamber: {current_c, target_c}, reports, job_id, matched_name,
connected, ended}`: `state` is the simulated printer's `PrinterState` (phase, headline,
progress, minutes left, layer and total, job name, problems: HMS codes and the print error
with their meaning where `explain` knows it). Every report is sent as
`CoreEvent::Printer {status}` (channel `printer`). A report is tied to a Hotend job only when
the printer's job name (`subtask_name` when the report gives one, else the stem of
`gcode_file`, never both; folders and the `.gcode.3mf` / `.3mf` / `.gcode` ending dropped)
is exactly, case included, the name of the file a Bambu Connect hand-off placed, and exactly
one `handed_off` job has it. Once a session (`printer_watch_start` to its end) matched a job
it stays with that job: a later report naming another job is the printer's status only
(`job_id: null`), does not update the matched job and does not move the match to another
job; a new session matches afresh. That job then
gets the events a simulated print has (`CoreEvent::Print`: `status` once, `phase`, `progress`,
`problem`), and its detail the phase, headline, progress, time left, problems and a message
saying the values come from the printer's own reports; its status stays `handed_off`
(Hotend did not start it, and a name is not proof that the print is the checked file). Any
other report is the printer's status, `job_id: null`, and claims nothing. What the printer
reports as `subtask_name` for a job Bambu Connect started is not yet seen on an X2D.

Residual risk of the match: a report is trusted as far as the certificate check goes. In pin
mode the pin proves only that the device holds the key of the certificate the person
confirmed, not that it is a Bambu Lab printer, so a device holding the pinned key could
report our job's name and claim that job finished (or failed). Nothing acts on such a
report: the job's status stays `handed_off`, only its detail (phase, progress, problems,
message, which says the values come from the printer's own reports) changes, and no
command, file or further step follows from it.

**Command line.** `hotend settings set --printer-lan-ip <ip> --printer-serial <serial>`
(both together; `--clear-printer-lan`), shown by `settings show`; `hotend printer status
[--watch] [--wait <s>] [--json]` (the first time, the certificate is shown and trusted only
after `yes` typed at a terminal, anything typed before the question discarded); `hotend
printer access-code set|delete|status` (the code from a pipe or a prompt without echo, never
from the command line); `hotend printer forget-certificate`.

**Not built.** An agent tool: the agent gets no printer status. The window's commands are in
section 8.3.
