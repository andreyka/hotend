# hotend-slicer

Hotend does not slice. This crate finds a vendor slicer the user installed, runs its
command line inside the sandbox with settings files Hotend wrote into a job folder, and
reads what the slicer wrote as hostile input. The contract is `docs/PROTOCOL.md` section 6.
This file says how the Bambu Studio side works, what has been proven and how, what has not,
and what to run on a machine that has the real program.

## What was measured

On 17 November 2026 Bambu Studio 02.09.01.70 was run by hand from PowerShell on Windows and
sliced one plate for the X2D. The sliced file and the three `result.json` files
of that session are the reference for everything below. They hold the vendor's start and end
G-code and profile text, so they are not in this repository. No test fixture, document or
source file here contains a run of their lines; fixtures are written by hand from the facts
only (command words, key names, markers, number ranges).

Facts taken from that session:

- The Windows program prints nothing, also with `--debug`, and exits with 0. The only
  account of a run is `result.json` in `--outputdir`. `--info` alone writes its
  `result.json` into the working directory.
- `--slice 0 --arrange 0` slices a plain 3MF where its parts lie. Without `--arrange 0` the
  program arranges the parts itself.
- System profiles passed as they are installed give a "successful" slice with a filament
  density of 0, a weight of 0.00 g and an empty filament id. Flattened profiles give the
  right result.
- The command line slices for the Cool Plate (bed 35 C for PLA) unless it is told the plate.
  The measured run did not tell it: its file states `Cool Plate` because that is the default.
- The `bbox` of an object in `result.json` is not the printed size.
- The command line of the run was `--debug 3 --slice 0 --load-settings <machine;process>
  --load-filaments <filament> --arrange 0 --outputdir <dir> --export-3mf plate_1.gcode.3mf
  <input>`, every path absolute.

## How a Bambu Studio slice runs

1. **The job is checked** before any program is looked for: paths, ids, the nozzle rules of
   the printer, the plate type. A real slice needs a plate that somebody chose (see "The
   plate type"). A 3MF input must hold the parts of a plain model and nothing else: a
   slicer's project file carries settings, per-object settings and G-code for single layers,
   and whether the command line applies them has not been measured. Such a file is refused
   with `input_refused` and never reaches the slicer (`src/input.rs`).
2. **The profiles of the installation are flattened into the job folder.** The base of every
   real slice is a system profile that the installed program ships, in
   `resources/profiles/BBL/{machine,process,filament}/`. `profiles/system-profiles.toml`
   maps Hotend's printer, process preset and material ids to profile names, and a job can
   select a profile only through an id of that table. An id it does not list is refused
   (`invalid_job`), so nothing the agent passes as `profile` can name a file of the
   installation. Each profile is flattened: its `inherits` chain from the root down, then the
   templates it names in `include`, then its own keys, each step overriding the one before.
   The three results are written to `<job>/profiles/machine.json`, `process.json` and
   `filament_<slot>.json`.
3. **Hotend's settings are applied** through the keymap (`profiles/keymap-bambu-studio.toml`,
   `src/keymap.rs`): the plate type of the job (`curr_bed_type`), the `overrides` table of a
   process preset in `profiles/system-profiles.toml` (none today) and, with a plan, the plate's
   print settings. A key outside the keymap, a key on the deny list `keymap::DENIED`, a value
   outside its field's hard range and a value never measured with the real program (unless the
   installed profile already holds it) are refused, when the table is loaded and again by the
   helper. A project export adds the nozzle inventory to the machine profile. Every value that
   was set is named in the warning `profile_overrides` of the result.
4. **The program runs in the sandbox** with the job folder as its working directory, its
   home and its only writable place:

   ```
   bambu-studio --debug 3 --slice 0
       --load-settings "<job>/profiles/machine.json;<job>/profiles/process.json"
       --load-filaments "<job>/profiles/filament_1.json"
       --filament-map-mode=Manual --filament-map=1
       --arrange <0|1> --outputdir <job>/output --export-3mf plate_1.gcode.3mf
       <job>/input/plate.3mf
   ```

   Without the third line of options this is the measured command line, word for word. The
   plate goes into the process settings, not onto the command line (`[bed_type_option] use =
   false`; the Linux build sliced for all four plates that way on 18 November 2026). The two
   filament map options have a `verified` flag (`filament_map` in `profiles/printers.toml`). The real
   program says nothing when it fails, so the error of a real run that fails or leaves no
   result names the parts of its command line that are still unconfirmed, and the profile
   names that are, as the first suspects. Nothing the program prints is used to decide
   anything.
5. **`<job>/output/result.json` is read as hostile input.** A missing file is the error
   `result_missing`. A `return_code` other than 0 is the error `slicer_failed` with the
   slicer's `error_string`. Only then is the sliced file opened.
6. **The sliced file is read as hostile input**, the G-code first: version, total time,
   layer count, filament length and weight from its header; the plate type and the plate
   temperatures from its settings block; the highest bed and nozzle temperature and the
   printed extent from its commands. `Metadata/slice_info.config` supplies the filament map
   and the slicer's warnings, and any number the header lacks. `result.json` is compared
   with the file and a difference is reported (`summary_disagrees`); the file's numbers
   stand, because the file is what the printer gets.
7. **The file is compared with the job.** It is refused
   - when the version it states is one the version policy refuses, whatever the program said
     when it was asked (`slicer_version_refused`);
   - when it was sliced for another plate than the job asked for, or does not state its
     plate at all (`bed_type_not_applied`);
   - when its commands heat the bed more than 1 C above what its own settings give for that
     plate, or heat it while the settings do not state the plate's temperature
     (`bed_temperature_not_applied`). A bed colder than the settings is the warning
     `bed_temperature_low`;
   - when it prints outside the reach of the nozzle that prints (`out_of_reach`);
   - when the parts were moved although `arrange` was off: the printed box must cover the
     box of the placed parts to within 3 mm and must not reach more than 15 mm beyond it
     (`positions_not_kept`). With several filaments a prime tower stands apart, so there the
     excess is the warning `printed_outside_parts`;
   - when `arrange` was off and the file does not show where it prints, or shows it only in
     the outline the slicer recorded beside the G-code (`positions_unknown`);
   - when its filament map differs from the job's (`hostile_output`);
   - when it states a filament density or weight of 0 for filament that is used
     (`unusable_output`, the mark of settings that were not flattened).

### Where the profiles are parsed, and why

Files Hotend did not write are parsed only inside the sandbox, never in the core. The installed
profiles are such files: they belong to a program the user installed, any other program on the
machine can change them, and they decide what G-code is produced. So the core does not open
them. `hotend-profile-flatten`, a small program built from this crate (`src/flatten.rs`), runs
under the same `slicer` sandbox profile as the slicer. It can write into `<job>/profiles` and
nowhere else, and it is given two read-only paths: the vendor's profile folder and its own
program file. It cannot read the model, the rest of the job folder, or the folder its program
lies in (Hotend may be installed in a folder it shares with other things). The core writes it a
request (`profiles/flatten-request.json`), and reads back only `profiles/flatten-report.json`:
at most 256 KiB, strictly typed, with the names, chains and key counts of what was written and
the keys that were left out. The flattened files themselves go from the helper to the slicer
without the core parsing them.

The helper treats the tree as hostile:

- a profile is found by comparing its name with the file names really present in the role's
  folder, so no path is built from a name. A name with a path separator, `..`, a leading dot
  or a control character is refused before the lookup;
- symbolic links are never followed, at any level: a linked profile file, a linked role
  folder (`machine`, `process`, `filament`) and a linked vendor folder are errors, and the
  vendor folder must lie directly in the profile folder;
- a profile is used only as what it is: it must say `type` equal to the role it is asked for
  and `instantiation` true (a base profile is not a preset), and a process or filament
  profile that lists compatible printers without the machine is refused
  (`profile_unusable`). One that lists none is used with the warning
  `profile_compatibility`;
- what the printer is told around and between the layers comes from the machine profile
  only. The slicer loads the process settings after the machine's and the filament settings
  after both, so a process profile may carry no key ending in `_gcode`, a filament profile
  none but `filament_start_gcode` and `filament_end_gcode`, and no profile `post_process` (a
  program the slicer would run). Such a key with content stops the job
  (`profile_unusable`, naming the profile and the key). One without content is left out and
  reported (`dropped` in the report, warning `profile_keys_dropped`);
- every file is capped at 1 MiB, the nesting of `inherits` and `include` at 16, the files
  read for one profile at 64, the templates of one profile at 16, the keys at 4096, and a
  circle through `inherits` or `include` is found and named;
- values are carried as the bytes the file had. A number is not reformatted, an object or a
  `null` is passed through, and nothing in a profile is interpreted or executed;
- either all files of a request are written or none.

A profile that is not there is the error `profile_not_found`. It names the profile, the
folder that was searched and similar names that are there. Any other problem is
`profile_unusable`.

No system profile is copied into this repository. The profiles in `profiles/demo/` are
written for Hotend and are only ever given to the stand-in slicer and to PrusaSlicer. A
real Bambu Studio whose profile folder is not found is not run at all.

### The plate type

A job states the plate that is on the printer: `SliceJob::bed_type`, one of `cool`,
`textured-pei`, `high-temp`, `engineering`. In the application the plate is the setting
`plate_type` (`hotend settings set --plate-type textured-pei`, or the printer settings of the
desktop window): the core passes it with every slice request (`SliceRequest::plate_type`) and
the backend makes it the job's `bed_type`. There is no environment variable for it. A job that
names none gets `SlicerConfig::default_bed_type`, which nothing in the application sets. **There
is no further default for a real slice.** Which plate is on the printer is a fact only the
person at it knows, and a bed heated for the wrong plate is exactly what this check exists to
prevent, so a real Bambu Studio is not run until the job or the settings name a plate
(`bed_type_not_chosen`). Only the stand-in, whose file cannot be printed, then slices for the
table's `stand_in_bed_type` (`textured-pei`).

The plate is passed as `curr_bed_type` in the flattened process settings. The sliced file must
state the same plate, and a file that states none is refused like one that states another. The
bed temperature its commands set is compared with the one its own settings give for that plate.
`SliceResult::bed` reports the plate and the highest bed temperature the file commands, and the
backend hands it to the core as `SliceSummary::bed` (`{id, plate_type, temperature_c}`). The
core records the plate's id with the slice only when the settings named a plate and `bed.id` is
that plate; a print is later checked for the recorded plate and for no other (docs/PROTOCOL.md,
sections 6 and 8.2). For a real slicer the backend also adds the warning `bed_plate` ("this file
was sliced for the Textured PEI Plate with a bed temperature of 55 C; that plate must be on the
printer").

The plates a print can be checked for are rows of the printer layer's table
(`crates/hotend-printer/profiles/bambu.toml`, `[[printer.plate]]`: `cool` and
`textured-pei`), by the same ids and names as here; `tests/plate_names.rs` fails when the
two tables differ. A slice for `high-temp` or `engineering` can be made and exported, and
cannot be sent to a printer through Hotend.

The option `--curr-bed-type` is not used (`[bed_type_option] use = false` in
`profiles/system-profiles.toml`); the Linux build took the plate from the process setting
alone. The names `Cool Plate` and `Textured PEI Plate` are measured (`verified = true`): the
sliced file states the plate and heats the bed to that plate's keys. The other two names are
guesses from the program's own naming of its plates (`verified = false`).

## The stand-in slicer

`hotend-fake-slicer` (`src/fake/`) stands in for both slicers in tests and in the demo. In
the Bambu Studio style it now behaves the way the real program was measured to:

- it prints nothing, whether it succeeds or fails;
- it writes `result.json` in the real shape into the output folder (into the working
  directory when it has none, and for `--info`), also for a failure;
- it writes `plate_<n>.gcode` and the `.gcode.3mf` into the output folder;
- it moves the parts to the middle of the bed unless it is told `--arrange 0`;
- it does not resolve `inherits` or `include`. A filament file without a density of its own
  gives 0.00 g and an empty filament id, which is what the real program did. A regression in
  the flattening therefore fails
  `a_slice_with_settings_that_lost_their_parents_is_refused`;
- it slices for the Cool Plate unless `curr_bed_type` says otherwise, and states the plate
  and the plate's temperature in its file.

Its "G-code" is still comments only. A printer that received it would do nothing, and the
file says so in the places a real file states what it prints:

- the header and `Metadata/slice_info.config` state a time of 0 s, no filament and no
  layers (there is no `; total layer number` line), because that is what the file does;
- the archive carries `Metadata/plate_<n>.gcode.md5`, the MD5 of the G-code entry in upper
  case (`src/fake/md5.rs`, checked against the RFC 1321 test vectors), as a real file does;
- the settings block states `printer_model` and `nozzle_diameter` from the machine settings
  it was given (the demo machine profile names `Bambu Lab X2D`), next to the plate.

With that the G-code check of `hotend-printer` accepts the file as what it is, a file
without moves (warning `no_moves`), by the same rules as a real slice: the check has no
exception for the stand-in. The checked review text of such a job says what the file
states: 0 s and no layers. The numbers above it, in the window's review and in the
native dialog, are the stand-in's estimates from the slice record the core keeps for that
very file (same SHA-256 and the `fake_slicer` warning), marked as rough estimates of the
stand-in; for any other file they are the file's own.

The estimates (time, layers, filament length and weight, height, from the mesh volume and
height) stand apart, in a labelled comment block between `; HOTEND_ESTIMATE_BLOCK_START`
and `; HOTEND_ESTIMATE_BLOCK_END`. The adapter reads that block only when the
installation is the stand-in (`Parsed::use_stand_in_estimate`, called for
`installation.fake`) and then reports the estimates as the slice's numbers, with the
`fake_slicer` warning. A file from any other program cannot have its numbers replaced
this way. `result.json` carries the estimates as before.

## Versions

`profiles/slicer-versions.toml` records 02.09.01.70 as the measured release. It still has
the verdict `warn`, because Hotend has not yet passed its regression suite with it. Every other
Bambu Studio version is `warn` with the reason that it is not the measured one. When the steps
below have passed, set the measured release to `allow` and the catch-all rule to `refuse`.

Detection reads a version by running `<program> --help` inside the sandbox. The Windows
program prints nothing, so its version is unknown before a slice. After every run of a real
Bambu Studio, Hotend takes the version from the first line of the G-code and applies the
policy to that statement before the file is used, also when `--help` stated a version: the
program that was asked may not be the one that sliced. A refused version is the error
`slicer_version_refused`. The result reports the file's version. A version that `--help`
did state and that differs from the file's (or a file that states none) gives the warning
`slicer_version_mismatch`.

For a Windows backend that wants the version before the first slice, read the version resource
of `bambu-studio.exe` (inside the sandbox helper, because the program file is not Hotend's
either) or the `DisplayVersion` of the program's uninstall entry in the registry. Step W4 below
records what they contain. Reading the sliced file stays the check that cannot be skipped,
because it is the statement of the program that really ran.

## What is proven, and by which command

All commands are run in the repository root.

| Claim | Command |
| --- | --- |
| The real sliced file is read with the measured numbers: 710 s, 26 layers, 4.52 g, 1490.77 mm, Cool Plate, bed 35 C, nozzle 220 C, printed extent X 2.215 to 133.393 and Y 2.21 to 95.391, version 02.09.01.70 | `HOTEND_REAL_SLICE_REF=<recording> cargo test -p hotend-slicer --test real_reference -- --ignored` |
| The three real `result.json` shapes are read (slice with kept positions, slice with unflattened profiles, `--info`) | same |
| The recorded run, replayed through the sandbox, passes every check that follows a real run, and is refused for another plate or other positions | same |
| Flattening: child over parent, templates, cycles, depth, file count, size, names that leave the folder, links at every level, malformed files, all or nothing, presets only, G-code and scripts from the machine profile only, typed override values | `cargo test -p hotend-slicer --lib flatten` |
| The helper flattens a tree through the sandbox and the stand-in slices with the result; hostile trees stop the job before the slicer runs; only ids of the table select a profile; the reader sees the vendor folder and its own file and writes into `profiles/` only | `cargo test -p hotend-slicer --test fake_pipeline` |
| A silent program is judged by `result.json` and the sliced file; a file for another plate, without a plate, with a hotter bed, with moved or unlocatable parts or of a refused version is refused; a real slice is not run without a chosen plate; a failed real run names what is unconfirmed; a 3MF that carries more than the model is not handed to the slicer | `cargo test -p hotend-slicer --test sandboxed_run` |
| The measured part of the command line is unchanged, word for word, and every addition is named as unconfirmed | `cargo test -p hotend-slicer --lib command` |
| The stand-in's behaviours | `cargo test -p hotend-slicer --lib fake` |
| From a built model to a finished job on the simulated printer, with the stand-in | `cargo test -p hotend-slicer --test print_flow` |

`<recording>` is the folder with `plate_1.gcode.3mf` and `real-cli-logs/`. The tests that
need the sandbox, and `print_flow`, which also needs Hotend's Python environment
(`uv sync --frozen` in `python/`), fail where those are missing. They are skipped only with
`HOTEND_TEST_ALLOW_SKIP=1`, and then say so.

## What is not proven

The Linux build of 02.09.01.70 was run in Hotend's sandbox on 18 November 2026 (the recipe of
the test slices, a plate without a brim, the bundled example and an editable project;
`docs/PRINT_SETTINGS.md`, "In the slice"). Each of those runs is a gated test in
`tests/real_bambu_studio.rs`. The points below rest on the wiki page, on the measured session
and on the stand-in until the steps at the end of this file were run on a given installation:

1. That the Linux program starts inside the sandbox on that machine: no display, no network,
   home in the job folder.
2. Where a Linux installation keeps `resources/profiles`. Hotend looks next to the program
   (measured on Windows) and one folder above it.
3. Whether the Linux program prints `BambuStudio-<version>` for `--help`.
4. `--curr-bed-type=<name>` at all. The option is the wiki's own example and is not passed; the
   plate names `High Temp Plate` and `Engineering Plate` are unverified as well.
5. `--filament-map-mode=Manual --filament-map=<numbers>`. The measured Windows run passed
   neither. Its file shows that both settings exist and that the automatic mode put the one
   filament on slicer extruder 1.
6. `--arrange 1` and a plate number other than 0 after `--slice`. The measured run had
   `--arrange 0` and `--slice 0`; the core's backend uses those two as well.
7. What the real program does on failure: whether it writes `result.json` and which exit
   code it gives. Hotend handles both a result file with an error and none at all.
8. A file with more than one filament: the header's lists and the map of two nozzles.
9. Every profile name in `profiles/system-profiles.toml` with `verified = false`: the fine
   process, PETG, TPU.
10. One object in the measured input was assigned extruder 2 in the slicer's model
    settings, and the slice still used filament 1 only. What decides the filament of an
    object in a plain 3MF is not understood.
11. The Windows program cannot be run by Hotend at all: the sandbox has no Windows backend.
12. That the installed profiles pass the checks Hotend now makes on them: a leaf profile
    says `type` and `instantiation` the way the stand-in tree does, and no process or
    filament profile of the vendor carries a G-code key or `post_process` with content
    where Hotend forbids it. If one does, the slice stops with `profile_unusable` and names
    the profile and the key. That is a finding to look at, not a check to loosen.
13. Which settings win when a process settings file carries `machine_start_gcode` (step W5),
    and whether the command line applies the settings and the per-layer G-code a 3MF input
    carries (step W6). Hotend refuses both, so the answers only say whether the refusals
    are necessary or merely cautious.
14. How far outside its parts a real slice prints. The measured file has no brim and no
    skirt. Its settings allow a brim of 5 mm (`brim_type = auto_brim`), and Hotend allows
    15 mm before it refuses a one-filament file.
15. Whether an empty G-code key in a real process or filament profile makes the warning
    `profile_keys_dropped` appear on every slice. If it does, it tells nobody anything and
    should become a line of the report only.

## Checking against a real Bambu Studio

Keep the printed output of every step. Nothing here contacts a printer.

### Linux build inside the sandbox (WSL)

L1. Install the web view library the program needs, and check it:

```
sudo apt-get install -y libwebkit2gtk-4.1-0
ldconfig -p | grep webkit2gtk-4.1
```

L2. Get the Linux build of 02.09.01.70 from Bambu Lab yourself (Hotend never downloads a
slicer). The sandbox cannot mount an AppImage, so unpack it into a folder of its own, not
into Downloads (a program in a shared folder may read only its own file):

```
mkdir -p ~/apps && cd ~/apps
/path/to/Bambu_Studio.AppImage --appimage-extract && mv squashfs-root BambuStudio
find ~/apps/BambuStudio -type f -name 'bambu-studio*'
find ~/apps/BambuStudio -type d -path '*profiles/BBL'
```

L3. Point Hotend at what the two `find` lines printed. The second variable is only needed
when the profile folder is not `resources/profiles` next to the program or one folder up:

```
export HOTEND_BAMBU_STUDIO=~/apps/BambuStudio/bin/bambu-studio
export HOTEND_BAMBU_PROFILES=~/apps/BambuStudio/resources/profiles
```

L4. Run the tests that use the real program:

```
cargo test -p hotend-slicer --test real_bambu_studio -- --ignored --nocapture --test-threads 1
```

What each test settles:

- `the_installed_bambu_studio_is_found_with_its_profiles`: points 1 to 3. The printed
  `Installation` shows the version or the note that says why there is none. A program that
  cannot start is refused with the loader's words.
- `the_real_bambu_studio_slices_a_placed_box_for_the_measured_plate`: points 1, 5, 12, 14 and
  15, the flattening against the real tree, and that `--arrange 0` keeps a placed box. It prints
  the printed extent and every warning. Compare the printed `flatten-report.json` with the
  measured chains and key counts: machine `fdm_machine_common`, `fdm_bbl_3dp_002_common`, `Bambu
  Lab X2D 0.4 nozzle` (116 keys); process four profiles ending in `0.20mm Standard @BBL X2D`
  (211 keys, or 212 if `curr_bed_type` was not among them); filament four profiles ending in
  `Bambu PLA Basic @BBL X2D 0.4 nozzle` (142 keys).
- `the_real_bambu_studio_slices_for_the_plate_hotend_asks_for`: point 4, for the plate
  names. It prints the bed temperature of each. For PLA the textured plate should give 55 C.
- `the_real_bambu_studio_reports_a_model_it_cannot_slice`: point 7. It prints which of the
  two ways the program fails.

If the filament map options are refused (point 5), the second test fails with `slicer_failed` or
`result_missing`, and the message lists them as the first suspects. Then run the program by hand
once without them and once with each, and correct the spelling in `profiles/printers.toml`
(`filament_map`). When they have worked, set their `verified` flags to `true`: each plate name
that was sliced with, the filament map rule.

L5. Run the whole flow with the real program (no `--fake`) in a project with a built
model, first for the Cool Plate, then for the plate that is on the printer. The build
puts `hotend-sandbox` and `hotend-profile-flatten` next to the `hotend` program:

```
cargo build --workspace
target/debug/hotend slice
target/debug/hotend settings set --plate-type cool
target/debug/hotend slice
target/debug/hotend settings set --plate-type textured-pei
target/debug/hotend slice
```

The first slice must fail with `bed_type_not_chosen` without running the slicer. The third
result must carry the warning `bed_plate` naming the textured plate, and every result the
warning `profile_overrides` naming the plate that was set. Open the `.gcode.3mf` in Bambu
Studio's preview and check the plate, the temperatures and the position of the part before
anything is printed.

L6. Slice a plate with PETG and one with the 0.12 mm preset. A wrong name gives
`profile_not_found` with the names that are installed; correct
`profiles/system-profiles.toml` and set `verified = true` for each name that worked
(point 9).

### Windows program

Hotend cannot run it (point 11), so these steps are by hand, in PowerShell, with the
profiles Hotend's helper flattens in WSL.

W1. In WSL, flatten the Windows installation's profiles with Hotend's helper. Write
`request.json`:

```json
{ "root": "/mnt/c/Program Files/Bambu Studio/resources/profiles/BBL",
  "machine": "Bambu Lab X2D 0.4 nozzle",
  "items": [
    { "role": "machine", "name": "Bambu Lab X2D 0.4 nozzle", "output": "machine.json" },
    { "role": "process", "name": "0.20mm Standard @BBL X2D", "output": "process.json",
      "overrides": { "curr_bed_type": "Textured PEI Plate" } },
    { "role": "filament", "name": "Bambu PLA Basic @BBL X2D 0.4 nozzle",
      "output": "filament_1.json" } ] }
```

```
cargo build -p hotend-slicer
mkdir -p /mnt/c/Users/<you>/hotend-flat
target/debug/hotend-profile-flatten request.json /mnt/c/Users/<you>/hotend-flat; echo $?
cat /mnt/c/Users/<you>/hotend-flat/flatten-report.json
```

Exit code 0 and the chains and key counts of L4 are expected, and every `dropped` list
should be empty (points 12 and 15). Exit code 3 with the code `not_a_preset` or
`forbidden_key` in the report is a finding about the installed profiles: keep the report.

W2. In PowerShell, slice with them, with the plate type option and the two options for the
filament map added to the measured command line:

```
& "C:\Program Files\Bambu Studio\bambu-studio.exe" --debug 3 --slice 0 `
  --load-settings "C:\Users\<you>\hotend-flat\machine.json;C:\Users\<you>\hotend-flat\process.json" `
  --load-filaments "C:\Users\<you>\hotend-flat\filament_1.json" `
  "--curr-bed-type=Textured PEI Plate" --filament-map-mode=Manual --filament-map=1 `
  --arrange 0 --outputdir "C:\Users\<you>\hotend-out" --export-3mf plate_1.gcode.3mf `
  "<your plate>.3mf"
```

W3. Back in WSL, let Hotend read the result:

```
cargo run -q -p hotend-slicer --example read_sliced -- \
  /mnt/c/Users/<you>/hotend-out/plate_1.gcode.3mf /mnt/c/Users/<you>/hotend-out/result.json
```

Expected: plate type `Textured PEI Plate` with Hotend's id `textured-pei`, bed 55 C for
PLA, filament map `[1]`, the printed extent where the plate has its parts, `return_code` 0.
If the plate type reads `Cool Plate`, the option was ignored (point 4). Repeat W2 with a
broken input file and note the exit code (`$LASTEXITCODE`) and whether a `result.json`
appears (point 7).

W4. Record what a Windows backend could read the version from without running the program:

```
(Get-Item "C:\Program Files\Bambu Studio\bambu-studio.exe").VersionInfo | Format-List
Get-ItemProperty HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\* |
  Where-Object DisplayName -like "Bambu Studio*" | Select-Object DisplayName, DisplayVersion
```

W5. Which settings win (point 13). Copy `process.json` to `process-probe.json` and add
one key to the copy: `"machine_start_gcode": "; HOTEND PROBE"`. Slice as in W2 with the
copy in place of `process.json`, into another output folder. Then, in WSL:

```
unzip -p /mnt/c/Users/<you>/hotend-out-probe/plate_1.gcode.3mf Metadata/plate_1.gcode \
  | grep -c "HOTEND PROBE"
```

A count above 0 means a process file can replace the machine's start routine. Do not print
that file: it has no start routine. Hotend's helper never writes such a process file: a
process profile that carries the key with content is refused with `forbidden_key`.

W6. What a 3MF input brings along (point 13). In Bambu Studio's window, open a model, set
the layer height to 0.28 mm, add a pause at some layer in the preview, and save the project
as a 3MF. Slice that file as in W2 (with the 0.20 mm profiles) and read the result as in
W3. 36 layers instead of 50 for a 10 mm part, or a pause command in the G-code, mean the
command line applies what the file carries. Then give the same file to Hotend as an input:
it must be refused with `input_refused`, naming the entries under `Metadata/`.

## Shipping

`hotend-profile-flatten` must lie next to the running program, like `hotend-sandbox` and
`hotend-fake-slicer`. `cargo build -p hotend-slicer` and `cargo build --workspace` build
it. Without it a real Bambu Studio slice stops with `profile_unusable` and says so.
