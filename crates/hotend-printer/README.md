# hotend-printer

The printer layer: checking a sliced file, getting it to a printer, starting it only after a
human confirmed it, and following the print. The module table and the path of a job are in
`src/lib.rs`; the wire contract is `docs/PROTOCOL.md` section 9.

Nothing in a default build opens a network connection. The application runs a print on
`SimulatedPrinter`, places it for Bambu Connect or writes it to a folder; every test uses
`SimulatedPrinter` or a stand-in. The real transport is behind two features, both off by
default. With `lan`, the side that uploads and sends commands (`lan::LanPrinter`) connects to
loopback addresses only (`lan::Endpoint`), where the tests run a stand-in printer inside the test
process. With `lan-status`, the read-only status listener (`lan::StatusListener`) connects to
one private-range address the user configured, subscribes to the report topic and has no way to
publish; `hotend-core` turns on `lan-status`, never `lan`. Its tests (`tests/lan_status.rs`,
built only with `--features lan`, which includes the listener) run against the same loopback
stand-in and prove that a whole session sends no PUBLISH.

```
cargo test -p hotend-printer                    # default build, no socket
cargo test -p hotend-printer --features lan     # adds the transport against loopback stand-ins

# The check on one sliced file: verdict, the text a person confirms, what was measured.
cargo run -q -p hotend-printer --example check_sliced -- /path/to/plate_1.gcode.3mf

# The same file through the tests. A plain `cargo test` reports them as ignored; run with
# `--ignored` they fail when the variable is not set or a file is missing.
HOTEND_REAL_SLICE_DIR=/path/to/folder cargo test -p hotend-printer --test real_slice -- --ignored --nocapture
```

## The check of a sliced file

`check_job` reads the sliced file before a person may confirm a print. The person sees a
short text; nobody reads forty thousand lines of G-code. The check stands in for that
reading.

### One file, read once

The file is read once, from start to end, into memory, and hashed while it is read.
Everything after that uses those bytes: the archive checks, the G-code check, the text a
person confirms, the upload, the export and the hand-off copy. The file on disk is looked
at again only to refuse a job whose file was changed since. So the hash, the verdict, the
text and what is sent are of the same bytes, whoever writes the file meanwhile
(`the_hash_the_verdict_and_the_text_are_of_the_same_bytes`, `tests/job.rs`). A symbolic
link, a file with two names, a folder, a named pipe and an empty file are refused before
anything is opened for reading. The limits (`ArchiveLimits`: 128 MiB for the file and for
the G-code, 5,000,000 lines in the profile) are about 120 times the real slice; a file at
those limits is checked in about 35 s in a debug build.

The archive must have one reading: its entries tile the file with no gap and no overlap,
every header says what the directory says, no two names differ only in case, and there are
no comments (`src/archive.rs`). It may hold only the entries a file sliced for the declared
plate has (`printer.slice_file.entries`): the 21 names of the real slice. Anything else is
refused, a second plate included, because the printer would be handed G-code nobody read.
The G-code must have the MD5 the file states for it.

### What it defends against

1. A tampered or substituted file. Whoever can write the file can write anything in it,
   comments and section markers included.
2. Custom G-code injected through a profile. It lands in the start routine, the end routine
   or between layers.
3. A slicer bug or a wrong profile that drives a nozzle outside its reach or into the
   plate. The stock start routine switches the printer's own travel limit check off
   (`M211 X0 Y0 Z0`), so this check is the only one left.
4. Temperatures above the limit of the material, the build plate or the machine, in the
   plain commands or in a vendor command that carries one, and a heater left on.
5. Firmware and configuration commands.
6. A file that says it is for another printer, build plate, nozzle size or filament than
   the job declares, or that does not say. This catches a wrong profile or a wrong file.
   It cannot catch a file whose statements were written to match; that file is still held
   to every other rule.
7. Plastic where no print puts it: pushed without moving, pushed faster than a nozzle lays
   it down, laid into the air, or pushed through a nozzle no command has heated.
8. A file that states one thing and does another: the stated time, filament length and
   weight, layer count and materials are compared with what the commands do.

It does not simulate acceleration, cooling or collisions with what is already printed, it
does not know the shape that was meant (a file that prints another object within every
rule passes, and the text then shows that object's time, amount and box), and it cannot
know what a vendor command does inside the firmware. For those it guarantees only that the
line is one the stock routine uses, with values no larger than the stock ones.

### How it works

- One command per line. The first word is the command (`G1`, `M620.11`, `T0`), every later
  word is a parameter whatever its letter. This dialect really writes `M1006 ... M50 N50`,
  `M204 P R T` and `T0 H-1`. Lines that firmware could run as two commands stay refused.
- Every line must match a row of the table in `profiles/bambu.toml`: the command word, the
  section, the exact set of parameter letters and a limit for every value. There is no
  opaque parameter. Each row says where the command was seen, what it does or "unknown",
  and whether it was observed in the real slice.
- The file's section markers choose which rows apply. They grant no trust, because the
  attacker writes them too. The start routine opens only before the first command and
  closes once, the end routine opens once, neither may lay plastic along a path, both are
  limited in length, and their rows are the stock commands at the stock values, which the
  check would accept from anyone. The most a forger gains is the stock routine's own
  commands in another order, and of its moves from an unknown position only ones that
  cannot close the gap to the bed. The test
  `the_most_a_forged_start_routine_gains_is_the_stock_commands_at_their_stock_values`
  (`tests/stock_routine.rs`) states that case and shows each next step failing.
- Commands that change limits or stored configuration (`M211`, `M500 D1`, `M201`, `M203`,
  `M204 P R T`, `M205`, `M201.2`, `M17` with a parameter, `G29.1`) pass only in the exact
  form and place they were seen, each is counted, and all of them are listed in the text
  the person confirms (`JobSummary::review_text`) and in one warning
  (`configuration_commands`). In the print itself, or in any other form, they are errors.
- A move that lays plastic must stay inside the reach of the nozzle that makes it. Every
  other move must stay inside that nozzle's travel box. After homing, a tool change or a
  firmware routine the position is unknown: an absolute move is checked by its target
  and plastic cannot be laid. `G92` may reset the extrusion counter only; on X, Y or Z it
  is refused, because every later coordinate would mean another place.
- An absolute target (a move, a purge line, a probing area) is accepted only on an axis
  the file has homed with a `G28` outside lines the printer may skip; before that the
  printer's coordinates are wherever its axes stopped (`unverifiable_move`). A `G28` that
  homes Z without X and Y needs X and Y homed first, because the probe meets the bed at
  the X and Y the nozzle is at (`command_out_of_place`). Measured in the three real
  slices: the stock start routine homes with `G28 X T300 R`, moves Y to absolute places,
  then homes Z with `G28 Z P0 T400`. So in the start routine, and only for that form,
  homing X also counts as homing Y.
- A relative move from an unknown position is the one kind of move nothing can check,
  and the stock routine makes five of them while the printer's own travel limit check is
  off. They pass only in the start routine, without an `E` word, and only as the stock
  routine makes them (`[family.gcode.unplaced]` in the table):
  - a plain move (`G0`, `G1`) goes at most 16 mm to a lower Y or 6 mm to a higher Z, and
    never to a lower Z, which is towards the bed;
  - a guarded move (`G380`) may go to a lower Z, but only as far as a move lifted the
    nozzle just before. Homing, a firmware routine, a tool change or conditional lines in
    between end that allowance, because the printer has then moved by itself;
  - all such moves of a file together cover at most 16 mm in Y and 45 mm in Z, in at most
    five moves. Those are the stock routine's own numbers.

  So no such move, and no chain of them, is written to end closer to the bed than where
  the printer left the nozzle. The test
  `moves_from_a_position_the_file_does_not_give_cannot_close_the_gap_to_the_bed_or_add_up`
  has the files that must fail, among them twelve moves of 22 mm towards the bed.
- Every temperature form is held to the declared filament and the machine, the vendor ones
  included, and the bed also to the declared build plate. Heating a nozzle the job does
  not use is an error. The end routine may not heat, and when the file ends every heater
  it switched on must have been switched off by a command the printer cannot skip.
- Height and plastic (`[family.gcode.limits]`; each number is recorded there with what the
  real slice showed and the margin that was added):

  | Rule | Limit | Seen in the real slice | Margin and why |
  | --- | --- | --- | --- |
  | No move names a Z below the first layer | 0.2 mm | lowest Z 0.2 mm, travels included | none: lower is a nozzle closer to the plate than the slicer ever puts it |
  | The Z trim (`G29.1 Z`) | 0, or the `z_trim_mm` of the declared build plate, exactly | Cool Plate: 0, then 0.022. Textured PEI Plate (two slices): 0, then 0.002 | none: a trim is a height like any other, a negative one lowers every layer, and the other plate's trim is refused |
  | Plastic at most one layer step above the highest plastic before | 0.32 mm | 0.2 mm steps | 80 % of the 0.4 mm bore, the tallest layer such a nozzle is given |
  | Filament per millimetre of path | 0.1 mm/mm | at most 0.0634 | 1.6 times: the widest line (0.75 mm) at the tallest layer (0.32 mm) |
  | Filament pushed without moving | what was pulled back, plus 0.001 mm | pulls sum to 0.39999 mm, push 0.4 mm | a hundred times the rounding that was seen |
  | Filament pulled back in all | 4 mm | 3.4 mm | room for the 0.8 mm retraction the file states instead of the 0.4 seen |
  | Nozzle commanded hot before filament is pushed | 170 C | 220 C | the usual cold-extrusion guard of FDM firmware |
  | `F` of a move | 60000 mm/min | 60000 | none: it is the machine's stated limit |
  | Bed on the Cool Plate | 45 C | 35 C | 10 C, chosen here, below the 55 C the file names for hot plates |

  The stock routine's purge and prime are firmware routines and one purge-line command.
  Each is a row of the start routine with the stock values, so they pass there and
  nowhere else; a plain `G1 E...` in place is held to the rule above in every section.
- What the file says about itself is held to its commands and to the job
  (`cross_check` in `src/job.rs`). Except where the table says otherwise, an absent
  statement is an error, not an agreement.

  | Statement | Held to | Tolerance |
  | --- | --- | --- |
  | print time (`prediction`) | the time the moves take at their commanded speeds | at least that time; at most 4 times it plus 30 min (seen: 710 s stated, 262 s of moves) |
  | filament length (`used_m`) | the sum of the E moves of that filament | 2 % plus 10 mm (seen: 1490 mm stated, 1487.37 mm measured) |
  | filament weight (`used_g`) | the measured length at 1.75 mm | a density of 0.9 to 1.5 g/cm3 (seen: 1.26) |
  | layer count | the heights at which plastic is laid | equal |
  | material of each filament | the declared material | the same family or one of its aliases; nothing matches by prefix |
  | filament map, nozzle size | the declared nozzle and size | equal |
  | printer, build plate | the profile's names for this printer, the declared plate | equal and stated; a second statement that differs is an error, wherever it stands |
  | printer preset | the profile's names | equal wherever the file states it (G-code comments, `project_settings.config`) |
  | MD5 of the G-code | the G-code | equal |

  The time bound is loose on purpose: acceleration, heating and the printer's own
  routines are not simulated, so the stated time may be well above the moves' time. It
  cannot be below it, and a file cannot state a time or an amount that its commands do
  not come near.

The full rules are at the top of `src/lint.rs` and `src/job.rs` and in `docs/PROTOCOL.md`
section 9. `tests/stock_routine.rs` has, for each harm above, files that must fail.

### What a person confirms

`JobSummary::review_text` is built from the summary alone, and the summary takes no free
text from the file or from the application. Each line says where its value comes from:
measured from the commands (temperatures, the printed box, layers, the filament pushed,
the least time the moves take), stated by the file and held to a measurement (time,
length, weight), stated by the file and only shown (colour), or a name from the printer
profile's closed lists that the file's statement was matched against (printer, build
plate, material). The file's name is shown in quotes, cut to 60 characters, with every
character but letters, digits, `.`, `_` and `-` replaced; the application's label is not
shown. Refusals repeat a name from the file only when it is a plain name.

A confirmation is of that text: `ConfirmationAuthority::confirm` takes the text the dialog
displayed and refuses any other, and the token is bound to the job (hash and size of the
file, plate, declaration, hash of the review text, name on the printer), the printer's
serial and an expiry. A token for one job does not start another job of the same file.

On the printer the file is named `<label>-<first 32 hex digits of its SHA-256>.gcode.3mf`,
so a second job with the same label cannot take the confirmed file's place, and the start
command names exactly the file this job uploaded.

### What the real slice showed (2026-11-17)

One real file has been checked: sliced on 2026-11-17 by Bambu Studio 02.09.01.70 for the
X2D, 0.4 mm nozzles, one PLA filament on the main nozzle, Cool Plate, 26 layers. The file
is not in this repository (it contains Bambu Lab's own routines). What it showed:

- [x] 2026-11-17: the file is acceptable, with 0 errors and 1 warning. The warning is
      `configuration_commands`, the list of the 16 accepted configuration commands of the
      stock start and end routines. It is meant to be shown, not to be fixed.
- [x] 2026-11-17: the layout of a real `.gcode.3mf`: 21 entries, each followed by a data
      descriptor; `Metadata/plate_1.gcode` and `Metadata/plate_1.gcode.md5` (32 upper-case
      hex digits, the MD5 of the G-code); `Metadata/slice_info.config` with `prediction`,
      `filament_maps` (slicer extruder per filament) and one `<filament>` per used filament
      with `type`, `color`, `used_m`, `used_g` and `nozzle_diameter`;
      `Metadata/project_settings.config` and `Metadata/plate_1.json`, which name the
      printer preset and the build plate again.
- [x] 2026-11-17: the section markers and the sizes of the routines (278 commands in the
      start routine, 77 in the end routine, 38,695 in all, 42,200 lines).
- [x] 2026-11-17: every command word and parameter form of the file is a row of the table
      (`verified = true` on that row). Rows kept from other firmware that the file does not
      use are `verified = false` (`G2`, `G4`, plain `G28`, `M82`, plain `T<n>`).
- [x] 2026-11-17: the travel box of the main nozzle. Every coordinate written in the start
      and end routines is inside the reach except one, the start of the load line at
      Y -0.2. The purge chute, the wiper and the cutter are reached by firmware routines
      that carry no coordinates.
- [x] 2026-11-17: the moves from a position the file does not give. There are five, all
      in the start routine and none with an `E` word: a guarded lift of 22 mm with a
      guarded lowering of 12 mm directly after it, 16 mm to a lower Y, and plain lifts
      of 5 and 6 mm. The table's limits are these numbers.
- [x] 2026-11-17: tools. The file selects its one filament as `T0 H-1` inside
      `M620 S0A H-1 B` / `M621 S0A B`, and the end routine uses `T65279` and `T65535`
      inside `M620 S<n> B` / `M621 S<n> B`. `slots_used` is `[1]`. `T255`, `T1000` and
      `T1100` were not seen and are no longer accepted.
- [x] 2026-11-17: heights and plastic. The lowest Z is 0.2 mm; plastic is laid at 26
      heights, 0.2 mm apart, up to 5.2 mm; the moves push 1487.37 mm of filament, at most
      0.0634 mm per millimetre of path; nothing is pushed in place beyond what was pulled
      back; the highest `F` is 60000; `G92` is used on `E` only; `M83` stands before the
      first `E`. The end routine switches the nozzle, the bed and the chamber off.
- [x] 2026-11-17: the stated time is 710 s; the moves take 262 s at their commanded speeds.
- [x] 2026-11-17: the review text of the file: the file says 11 min 50 s (moves at least
      4 min 22 s), 26 layers counted from the commands, PLA on the main nozzle (id 1),
      0.4 mm, the commands push 1.49 m (the file says 1.49 m, 4.52 g, colour #00AE42),
      nozzle up to 220 C (a vendor setting names 240 C), bed up to 35 C, Cool Plate,
      chamber off, every heater off at the end.
- [x] 2026-11-17: the file names its printer as `Bambu Lab X2D`, its preset as
      `Bambu Lab X2D 0.4 nozzle` and its plate as `Cool Plate` (`cool_plate` in
      `plate_1.json`).

What a slice cannot show: what the printer does with any of it. "Verified in a slice"
means the slicer writes it, not that the firmware behaves as the row's `what` says.

### What the two textured slices showed (2026-11-18)

Two more real files were measured: sliced on 2026-11-17 by Bambu Studio 02.09.01.70 for
the X2D, 0.4 mm nozzle, one Generic PLA with Hotend's settings on the main nozzle,
Textured PEI Plate; two ring-shaped test prints of 37 and 344 layers. They are not in
this repository either. The table needed exactly two more values for them, and nothing else
was widened:

- [x] 2026-11-18: the Z trim belongs to the plate. Both files clear the trim (`G29.1 Z0`)
      and set 0.002 where the Cool Plate slice sets 0.022. The plate's row now carries
      its trim (`z_trim_mm`), and the `G29.1` row takes 0 or the trim of the plate the job
      declares (`plate_z_trim`). The Cool Plate's 0.022 in a job for the Textured PEI
      Plate is refused, and so is 0.002 in a job for the Cool Plate.
- [x] 2026-11-18: the `M142` of the print (once per file, before the second layer) has `U0.3`
      where the first slice has `U0.7`. In each file that is one hundredth of the
      `additional_cooling_fan_speed` the file states (30 and 70), so the value follows the
      filament's settings and not the plate. The row takes 0.3, 0.5 (measured in a later slice)
      or 0.7 and no other value; what the printer does with `U` was not observed.
- [x] 2026-11-18: the plate's names: `Textured PEI Plate` in `curr_bed_type`,
      `textured_plate` in `plate_1.json`. The bed is commanded to 65 C in the start
      routine and for the first layer and to 60 C after it.
- [x] 2026-11-18: both files are acceptable for a job on the Textured PEI Plate, with 0
      errors and the one warning `configuration_commands`; every other row and limit
      written from the first slice passes them unchanged. Measured: 1152 s stated (moves
      713 s), 37 layers, 5.38 g and 1.80 m for the first; 13,066 s stated (moves 9323 s),
      344 layers, 100.42 g and 33.67 m, up to 68.8 mm high, for the second; nozzle up to
      220 C; five moves from an unknown position, as in the first slice.
- [x] 2026-11-18: a file is refused for the other plate. Each textured slice as a Cool
      Plate job has 10 errors (the plate's name three times, the bed above 45 C six
      times, the trim once); the first slice as a Textured PEI Plate job has 4 (the name
      three times, the trim once).
- [x] 2026-11-18: nineteen harmful changes to a textured slice are each refused with one
      error: other trims (the Cool Plate's, larger, negative), other fan shares (0.31,
      0.5, 1, 3, negative, none), other values of the same `M142`, and a bed above the
      filament's or the machine's limit (`tests/real_slice.rs`).

### What will be refused until it has been seen

The table holds what three files showed: the first slice and two slices for the Textured
PEI Plate (below). A real file of another kind will probably contain forms it does not
have, and the check refuses those (it fails closed). Expect this for:

- a second filament or the auxiliary nozzle (other `M620`/`M621` forms, tool changes in
  the print, a prime tower, the auxiliary nozzle has no travel box);
- another material (some rows hold exact values that probably depend on it, for example
  `M142` and the upper limits of the `F` values of `M620.10`, `G130` and `M983.3`);
- another share of the auxiliary fan: the `U` of the `M142` in the print passes only as 0.7, 0.5
  or 0.3, the values seen (one hundredth of the `additional_cooling_fan_speed` each file states,
  70, 50 and 30). Hotend's print cards also cap that fan at 20 and 0 percent (`hotend-settings`,
  `data/print-cards.toml`); if `U` follows the setting, a file sliced with one of those is
  refused until such a file has been seen;
- another build plate: the Cool Plate and the Textured PEI Plate were seen. A file is
  acceptable only for the plate it was sliced for: its name, its word in
  `plate_<n>.json` and its Z trim must all be the declared plate's. The High Temp and
  Engineering plates have no row, so a job for them cannot be declared;
- a first layer thinner than 0.2 mm, layers taller than 0.32 mm, spiral ("vase") prints
  whose Z rises during a move from below the first layer, lines wider than about 0.75 mm,
  a bed above 45 C on the Cool Plate, a bed above the filament's limit on the Textured PEI
  Plate (70 C for PLA; 65 C was seen);
- a slicer that pushes filament in place for more than it pulled back (extra prime after
  a travel), or pulls back more than 4 mm;
- a start routine that makes other moves from an unknown position than the five seen
  (another distance, one more move, a plain move to a lower Z);
- another nozzle size (`H0.4` and `A0.4` are exact values in three rows, and the profile
  lists 0.4 mm only);
- more than one plate in a file, or an entry name the real slice did not have;
- more labelled objects than the 12 character mask of `M624` and `M639` holds;
- slicer options that add commands the file did not have.

Each refusal names the line and the row or rule it missed. The remedy is a new row or a
measured number in `profiles/bambu.toml` with what was seen, not a wider rule.

### To try with your own slice

Nothing here contacts a printer; every step only reads a file.

1. Slice the same model with the same profile in Bambu Studio and run the example on the
   result. Expected: `ACCEPTABLE: 0 errors, 1 warning(s)`, and a review text with the
   right time, filament, nozzle, temperatures and plate. Compare each of those lines with
   what Bambu Studio shows for the plate.
2. Run `HOTEND_REAL_SLICE_DIR=<folder> cargo test -p hotend-printer --test real_slice --
   --ignored --nocapture`. For a new slice the measured numbers are not compared (they are
   only when `plate_1.gcode.3mf.sha256` beside the file holds its hash); the general
   assertions, the thirty-two harmful changes to the G-code and the changes to the archive
   are. The folder holds `plate_1.gcode.3mf` (the first slice) and the two textured slices as
   `textured-a.gcode.3mf` (37 layers) and `textured-b.gcode.3mf` (344 layers). With the variable
   unset or one of them missing the tests fail; a plain `cargo test` reports them as ignored.
3. Slice other jobs and run the example on each: PETG and TPU on the main nozzle, a job on
   the auxiliary nozzle, two filaments, the Textured PEI plate, two and more objects on the
   plate, a tall part, a part at the edge of the plate, a 0.12 mm and a 0.28 mm layer
   height. Each refusal is a line or a number the table has not seen. To report one, give
   the findings (code, line number, message), not the file.
4. Put a line of custom start G-code into a copy of the machine profile (for example
   `M211 S0` or `M104 S300`), slice, and check that the example refuses the file. Do the
   same with the three lines `G91`, `G1 Z-5`, `G90` placed directly after a `G28` of the
   start routine: the example must refuse the file with `unverifiable_move`. This only
   slices and reads the file; do not print it.
5. Compare the list of configuration commands in the review text with the start and end
   G-code Bambu Studio shows for the printer. Every command there that changes limits or
   stored settings should be in the list, and nothing else.

## What has not been checked against a real X2D

A print has gone from the application through Bambu Connect to an X2D. Beyond that, no fact in
this crate has been observed on a printer: the read-only status is tested against a stand-in
broker only, and nothing the printer did with a file was measured. Each fact is data in
`profiles/bambu.toml` with `verified = false`, or `verified = true` in the "in a slice" sense
explained at the top of that file, or is listed here. This is the checklist for the first
session with a real X2D; that session needs a deliberate change to `lan::Endpoint::allowed`.
Tick a fact by setting its `verified` flag and bumping the file's `revision`.

Identity and discovery
- [ ] The model code is `N6` and serial numbers start with `20P` (`printer.identity`).
- [ ] The printer announces itself over SSDP on UDP 2021 with the service type and the
      header names `discovery::parse_announcement` reads (`family.discovery`). No socket
      code exists for discovery; only the parser.

Nozzles (`printer.nozzle`)
- [ ] Device extruder id 1 is the main (direct-drive) nozzle and slicer extruder 1; device
      extruder id 0 is the auxiliary (Bowden) nozzle and slicer extruder 2.
- [ ] Reach: main 256 x 256 x 260 mm; auxiliary X 20.5 to 256, Y 0 to 256, Z 0 to 256.
- [ ] The auxiliary nozzle's 200 mm/s limit (recorded; the linter holds `F` to the machine's limit only).
- [ ] External spool holder 254 feeds the main nozzle and 255 the auxiliary one.
- [x] 2026-11-17 (in a slice): where the main nozzle is sent outside its reach by
      coordinates. Only to Y -0.2, by the load line. The `travel` box says so.
- [ ] Where the firmware routines of the start and end routines (wipe, purge, cut, the
      filament switch) move the nozzle. The file does not say, and nothing here checks it.
- [ ] Where the nozzle is when the start routine makes its five moves from a position
      the file does not give, and what stops a guarded move (`G380 S2`). The check only
      guarantees that those moves are the stock ones and that none is written to end
      closer to the bed than where the printer left the nozzle. A forged file can still
      make the stock moves from another unknown position: 16 mm to a lower Y, and up to
      45 mm of Z travel in all.
- [ ] The travel box of the auxiliary nozzle. None was measured, so every move of that
      nozzle is held to its reach.
- [ ] Machine limits: nozzle 300 C, bed 120 C, chamber 65 C (`printer.limits`).

Connection (`family.transport`, `src/lan`)
- [ ] MQTT over TLS on 8883 and implicit FTPS on 990, both TLS 1.2 only, with a cipher
      suite rustls (aws-lc-rs) offers.
- [ ] User `bblp`, the 8-character access code as password, on both.
- [ ] The certificate's common name is the serial number, and MQTT and FTPS present the
      same certificate (one pin is used for both).
- [ ] The certificate chains to one of Bambu Lab's certificate authorities, read from a
      PEM file the user supplies (`lan::pair(endpoint, serial, authorities)`; Hotend does
      not ship Bambu Lab's certificates, see docs/DEPENDENCIES.md; every certificate in the
      file must be one of the five whose SHA-256 is in `lan::tls::BAMBU_LAB_CA_SHA256`). Pairing requires it and
      then pins the certificate. Not known: which authority signs an X2D's certificate, whether the
      printer sends the intermediate certificates, and whether rustls' strict reading of
      X.509 accepts the printer's certificate (version 3, usable for server authentication).
- [ ] The printer completes the handshake without an SNI extension (the server name sent
      is its IP address; the common name is checked against the serial by hand).
- [ ] The flat `ams_mapping` value of an AMS HT tray is 16-23 (`family.ams.flat_ht`). Until
      it is seen to work, the LAN transport refuses to start a job from an AMS HT.
- [ ] Topics `device/<serial>/report` and `device/<serial>/request`; a `pushall` request is
      answered with a full status; reports fit in 1 MiB.
- [ ] Status listener (`lan::StatusListener`): the printer accepts a client that only
      subscribes (no `pushall`) in LAN Only mode with Developer Mode off, sends reports on its
      own often enough to follow a print, grants the QoS 0 subscription, answers PINGREQ, and
      serves it next to Bambu Connect. What `subtask_name` / `gcode_file` say for a job Bambu
      Connect started (the core matches a handed-off job by the placed file's name).
- [ ] The first look (`lan::probe_certificate`) shows the same SHA-256 on every connection
      (the pin then holds across reboots and firmware updates, or not).
- [ ] Commands are accepted only in LAN Only mode with Developer Mode on; outside it the
      printer still reports status and answers with print error `0502_4007`.
- [ ] FTPS: `PBSZ 0` / `PROT P` are accepted, passive mode works with the data connection
      to the printer's own address, a TLS session that is not resumed on the data connection
      is accepted, `SIZE` answers, and an upload of several hundred MB completes.
- [ ] FTPS reaches the USB drive only, a file uploaded to its root is started as
      `ftp:///<name>`, and nothing can be started from the built-in storage this way
      (`family.storage`).

Reports (`src/protocol/report.rs`, `src/status.rs`)
- [ ] `gcode_state` values and the `stg_cur` stage numbers and their grouping into
      preparing / heating / printing.
- [ ] Dual-nozzle temperatures in `device.extruder.info[].temp` (low 16 bits current, high
      16 bits target), the active extruder in bits 4-7 of `device.extruder.state`, bed in
      `device.bed.info.temp`, chamber in `device.ctc.info.temp`.
- [ ] AMS: unit ids 0-3, AMS HT ids 128-135 with one tray, the nozzle a unit feeds in bits
      8-11 of its hex `info` (14 = not bound), `dry_time` > 0 while drying, external spools
      in `vir_slot` with ids 254 and 255 (`family.ams`).
- [ ] HMS entries as `attr` / `code` pairs; severity in `code >> 16`, module in
      `attr >> 24`; the wiki address built from the code.
- [ ] The known codes: `0300_400C` cancelled, `0700_8012` filament mapping,
      `0502_4007` not authorised, HMS `0300_0100_0001_0007`
      (`family.known_code`).
- [ ] A start that is accepted while the AMS dries leaves the printer idle with no error
      (the behaviour `StartQuirk::AcceptedButStaysIdle` simulates and the watchdog reports).

Commands (`src/protocol/command.rs`, `family.project_file`, `family.speed_levels`)
- [ ] `project_file`: `param` `Metadata/plate_<n>.gcode`, the four calibration flags as
      booleans (or 0/1 integers), the key spelled `bed_leveling`, `use_ams`, `ams_mapping` (and
      whether it wants `ams_mapping2`), and an empty `md5` being accepted.
- [ ] The flat `ams_mapping` value of an AMS HT tray: the sources disagree (unit id 128-135
      or 16-23); `flat_ht` selects it and is set to 16-23.
- [ ] How an external spool and an unused filament are written in both mappings.
- [ ] `pause`, `resume`, `stop` at QoS 1; `print_speed` with `"1"`..`"4"`; `ledctrl` for
      `chamber_light`; `pushing.pushall`.

Sliced file and G-code (`src/job.rs`, `src/lint.rs`, `family.gcode`, `filament`)
- [x] 2026-11-17 (in a slice): the layout of a real `.gcode.3mf` and the command table,
      see "What the real slice showed" above.
- [ ] That a printer does with the file what the check assumes. A print has gone to an X2D through
      Bambu Connect; the check itself only ever sees the file.
- [ ] What the commands of unknown meaning do (`basis = "unknown"` in the table, among them
      `M500 D1`, `M17 S`, `M971`, `M622.1`, the `T65279` and `T65535` tools).
- [ ] That `T` on `M104` names a nozzle by device extruder id. Inferred: the one filament
      is on device extruder 1 and the only nozzle heated by number is `T1`.
- [ ] That `M620.10 T` and `P`, `G150 T`, `G383 T` and `M620.17 S` are temperatures. They
      are held to the nozzle limits in any case.
- [ ] The temperature limits per filament family (`[[filament]]`) against Bambu's profiles.
- [x] 2026-11-17 (in a slice): `Metadata/plate_<n>.gcode.md5` is the MD5 of the G-code;
      the check compares the two.
- [ ] Whether the printer compares anything with the file it runs. The start command's
      `md5` field is sent empty, and its meaning (MD5 of the archive or of the G-code) is
      not known. Until it is, what protects the file between upload and start is its
      name on the printer, which carries its hash, and the printer's own storage.
- [ ] The start command sends `bed_type` as `auto`. Whether the printer compares the
      plate on the bed with the file's plate, and what it does when they differ.
- [ ] Whether the printer checks the nozzle size a file was sliced for against the nozzle
      that is fitted. The check compares the file with the declared size only; nothing
      here reads the fitted size from a report.
- [ ] Whether this printer refuses to push filament through a cold nozzle. The check
      requires a command for 170 C or more before any push in any case.
- [x] 2026-11-18: the Textured PEI Plate, from two real slices: its name in
      `curr_bed_type`, its word in `plate_<n>.json` (`textured_plate`), its Z trim
      (0.002) and its bed temperatures for PLA (65 C, then 60 C). In a slice only: what
      the printer does with the trim was not observed.
- [ ] The Textured PEI Plate's ceiling is the machine's bed limit (120 C), so only the
      filament's limit binds. No slice of a hot plastic on it was seen.
- [ ] The Cool Plate's ceiling of 45 C: a margin chosen here, not a figure from Bambu.

Hand-off
- [x] Bambu Connect accepts `bambu-connect://import-file?path=&name=&version=1.0.0` for a
      `.gcode.3mf` sliced for an X2D, with the Windows path (backslashes) encoded as
      `encodeURIComponent` encodes it: measured on 2026-11-18 with Bambu Connect on Windows,
      opened once by hand (it imported the file; no printer was chosen).
- [ ] Hotend's own opener (`opener::WindowsFromWsl`) opening that link: built; the tests
      here use the stand-in only.

## Not built

- Camera stream, the file tunnel on port 6000, listing or deleting files on the printer.
- Jobs on the printer's built-in storage.
- A socket listener for discovery.
- Correlating a command with the printer's reply (`sequence_id`); the outcome is read from
  status reports.
- Bambu's HMS message catalogue: codes get a general sentence from module and severity.

## Shared code

`archive` is the hardened `.gcode.3mf` reader. It is a port of `hotend-slicer`'s private
reader (`hotend-slicer` depends on `hotend-core`, which depends on this crate). `hotend-slicer`
should use `hotend_printer::archive` (or both should move to a small shared crate) so the
limits live in one place.
