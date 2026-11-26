# Acceptance scenarios: evaluation briefs

Two products as evaluation cases for the design agent. Each case has: the brief as a user types
it, what the run is given besides the words, and pass criteria that the engine computes from
`result.json` and the exported files, so that a run with a real model through `hotend chat` can
be scored without a person.

Both scenarios pass at engine level with a model script in the repository
(`tests/models/scenarios/<key>/model.py`, tested by `tests/scenarios/`).

How a criterion is written: `where it is read` then `what must hold`. "Measured" means
measured by `tests/scenarios/scenario_harness.py` (rays through the solids of `model.brep`,
triangles of the plate file), not taken from the model's own checks. Every criterion marked
(engine) is a field of the build, `prepare` or `gauge` result; (measured) is computed by the
harness; (judge) needs a person or a vision model and is not part of the automatic score.

## 1. A soft figure in a steamer basket (`dumpling-steamer`)

### Brief

> Design a round steamer basket with a lid for the soft figure in the attached file. Take the
> sizes from the file.

Follow-up, typed after the first version passes:

> One more basket, for the figure in this second file.

What an agent that asks is told, and nothing more: with the cover off, the figure is seen above
the basket's edge, and more of it is below the edge than above.

Variant without a file (measurements the user gives):

> My soft figure is 64 mm wide, 65 mm deep and 46 mm tall, measured with a caliper. Design a
> round steamer basket with a lid for it.

### What the run is given

| Input | Value |
| --- | --- |
| Reference `dumpling`, kind `soft` | A stand-in made by `tests/reference_stand_in.py: stand_in_mesh(radius=31.0, height=46.0, pleats=9)`, written as STL: 63.9 x 64.7 x 46 mm. The sizes are invented |
| Second reference for the follow-up | The same formula with `radius=18, height=28, pleats=5`, stretched 1.25 in X, written as 3MF: about 45.2 x 37 x 28 mm |
| Printer | Bambu Lab X2D, both nozzles with their reach (`tests/models/scenarios/dumpling-steamer/reference.json`) |
| Material of the holder | PLA (the brief does not say; the agent must state its assumption or ask) |
| Calibration | none (uncalibrated printer) |

A run may also be given the user's own reference file. Such a file may be somebody else's
design: it is never copied into the repository, a fixture or a deliverable other than pictures;
a run puts a copy into its job folder and removes it, with `mesh.json`, afterwards. When a file
holds several objects or plates, a correct run names plate and object (the `references` op answers
`needs_selection` and lists them) and never unites them.

### Pass criteria

Reading the reference
1. (engine) `references[0].provenance` is `measured`, `used` is true, and `size` is within
   0.05 mm of the size in the table above. For a file with several objects `source.plate` and
   `source.object` are the ones the request names.
2. (engine) `questions` holds `reference_dumpling` (is this the thing, at the size you print:
   confirm with a caliper).

The fit
3. (engine) One `reference_fit` result with `data.kind` = `pocket` per figure, each `pass`
   with `clearance_proven` and `path_clear` true and `problems` empty.
4. (engine) The pocket's `fit_class` is `cradle` or `loose` and comes from the resolver: no
   `fit_pairs` result of kind `unverified` names a reference clearance, and `data.target`
   (the printed gap per side) is between 0.4 and 4.0 mm: a soft thing is neither squeezed nor
   lost in its room.
5. (measured) The smallest circle around the placed figure, against the inside of the
   basket: the air per side is not less than `data.expected` (less 0.02 mm) and not more than
   `data.expected` + 0.25 mm; the bore does not narrow between the floor and the edge by more
   than 0.005 mm; the figure rests on the floor (within 0.01 mm); no vertex of the figure
   is inside a part.
6. (measured) With the cover on, nothing is closer than 2.0 mm above any point of the
   figure, and the script's own statement of that distance is within 0.02 mm of the
   measurement.

The holder
7. (engine) `status` is `ok`. `valid_solid`, `manifold`, `single_solid_per_part`,
   `fits_build_volume`, `min_wall` and `parts_do_not_intersect` pass for every part; no
   `fit_pairs` or `contract` result is `fail`.
8. (engine) At least two parts: a body that carries the figure and a cover that comes off
   (the cover is in the pocket's `removable`).
9. (engine) Outside diameter of the body divided by the diameter of the circle around the
   figure is between 1.15 and 1.35.
10. (engine) Every guess is asked: with `checks.contract`, each requirement that rests on a
    guess is `warn` with "rests on a guess" and has an entry in `questions`; no requirement
    is tagged `measured` or `calibrated` on an uncalibrated printer and reported as `pass`.

Printing
11. (engine) `prepare` returns `ok`, one plate, `unplaced` empty, every check `pass`.
12. (measured) On the triangles of `plates/1/plate.3mf`: every piece lies on z = 0 inside the
    main nozzle's reach (256 x 256 x 260 mm), the pieces' boxes are at least 5 mm apart, and
    less than 4 mm2 per piece hangs over more steeply than 45 degrees (no support).

The reference stays out of the product
13. (measured) `export/model.3mf`, the plate and `model.step` hold exactly the printed parts:
    object names equal part names, each mesh encloses its part's `volume` within 0.1 %, the
    STEP has one exact solid per part and no tessellated entity. `mesh.json` lists the
    figure under `hotend.references` only.

Re-fit (the follow-up message)
14. (engine + measured) The same script, byte for byte, built with the second reference meets
    criteria 1 to 13 and 15. The agent's turn for the follow-up contains no `write_model_code`
    call that changes a size by hand: the sizes follow the reference.

Where the basket's edge is
15. (measured) With the cover off, the edge of the body is between 60 % and 80 % of the
    figure's height above the figure's base, so that between a fifth and two fifths of it
    stand above the edge to be taken by. The body's wall has no window or cut-out.

Not scored automatically
16. (judge) It reads as a steamer basket: round, banded rims, slatted or woven floor or lid, a
    lid with a knob or handle. With the lid off the figure is seen above the basket's edge. It is
    an original design: no outline, mesh or dimension table of an existing model is in the script.

Budget for an agent run: at most 12 builds (`write_model_code` and `set_parameter` calls) for
the first message and 2 for the follow-up; outcome `done`.

### Run it (engine level)

```
cd python && uv run pytest -q tests/scenarios/test_scenario_dumpling_steamer.py
```

## 2. Case for a phone (`sample-phone-case`)

### Brief

> Generate a case for the sample phone. Figure out its sizes yourself and suggest a case
> with correct geometry and size that can be 3D printed on my printer.

### What the run is given

| Input | Value |
| --- | --- |
| Device data | The shipped component `sample_phone` (`hotend_fit/components/sample_phone.json`): an invented device of 80 x 130 x 10 mm with buttons, ports, camera, keep-outs and case rules, every number with tolerance, provenance, confidence and source. 29 of the numbers the scenario's model relies on are not verified and must come back as questions. A case for a real phone needs a local component file (PROTOCOL section 4.7); this scenario does not |
| Reference file | none |
| Printer | Bambu Lab X2D, both nozzles with their reach |
| Material | TPU 95A (`{"id": "tpu-95a", "family": "TPU"}`). The brief does not name it; an agent that chooses a rigid material must not use a stretch fit |
| Calibration | none (uncalibrated printer) |

A run that looks the phone's size up on the web instead of using the component fails
criterion 1: a lookup has no tolerance and no data sheet behind it.

### Variant: a short request, web research off

> sample phone case using PLA

Web research is not agreed, so the run has no web tools. The harness gets the facts itself
(prompt, steps 1 and 5, and "Bought devices"):

- (agent) The first tool call is `list_components`, which lists `sample_phone` with its size,
  sources and the 29 numbers that are not verified (camera plateau 7, flash 4, ...); the model
  uses `ctx.component("sample_phone")` and types no number of the phone.
- (agent) The answer says that the sample phone's numbers are invented, names the features whose numbers are not verified and what that means for the fit (the camera
  opening), and offers research or a test print of that area or the gauge, never a ruler.
  Questions are real choices (wall thickness, colour), at most three, each with a recommended
  default.
- (agent) Where it would look something up, one line begins `web research is off:`; the run
  then ends with the `research_needed` marker and the window shows "Turn on web research".
- Fails: a question for the phone's dimensions, a web lookup of its size, a size from memory.

Scored by `python -m hotend_agent.evals --components` (case `sample_phone_pla`); the harness's
side is tested without a model in `tests/agent/test_agent_components.py`.

### Variant: an open-ended request, web research on

> A holder for my headphones, to screw under my desk.

No size, no product, no component. The agent looks at examples once (prompt, "Web research",
Examples):

- (agent) One short `web_search` before the first `write_model_code`, for how such holders are
  built, what people complain about and the material and wall thickness; no page is read and no
  model-sharing site is opened (through a search service the core refuses such a `web_fetch`).
- (agent) The answer says in two or three lines what it took from the examples, with the sites,
  and asks at most one real choice with its default; a measurement only the maker has (the
  headband's width) may stand beside it. A number it took is a research note whose
  key starts with "example" and stays unconfirmed.
- (agent) The next message of the project makes no example search. A request with sizes ("A box
  100x60x40") or for a device with a component ("sample phone case using PLA") makes none at all.
- (agent) A size from a page that is not the maker's own, or read only in a search excerpt, is
  called that next to the number.
- Fails: more than one example search, a search for a plain part, a copied design, a line
  `web research is off:` for examples.

Scored by `python -m hotend_agent.evals --research` (case `open_ended`, and
`sources_labelled` of `named_part`). What the harness does around it (the tools, the limits,
the notes, the labels) is tested with a stand-in model and a stand-in search service in
`crates/hotend-core/src/search/example_tests.rs`.

### Pass criteria

The phone's data
1. (engine) `components[0].id` is `sample_phone`; its `size` is `[80.0, 130.0, 10.0]`; every
   entry of `sources` has an https URL.
2. (engine) Every entry of `components[0].unverified` belongs to a feature whose question is
   in `questions` (as `sample_phone_<feature>`). The `reference_fit` result of kind `provenance`
   is `warn`, never `pass`, while that list is not empty.

The fit
3. (engine) The `reference_fit` pocket result has `clearance_proven` true and `intrusions`
   empty. For TPU the `fit_class` is `stretch`, `stretch_strain` is below `stretch_limit` and
   `grip_coverage` is at least 0.5. Its status may be `warn` only because the printer is not
   calibrated or because of the phone's own tolerance; then `questions` holds the request to
   print the ring gauge.
4. (measured) Rays through the solid at mid thickness: the gap between the phone's stated
   width and height and the cavity wall is the same along each side, equals
   `size / 2 * (data.scale - 1) + data.offset` within 0.001 mm, and lies between 0.10 and
   0.30 mm per side as drawn.

The phone's rules, measured against the component's numbers
5. (engine) The `reference_fit` result of kind `component` is `pass`: `collisions` and
   `keep_outs_not_handled` are empty.
6. (engine + measured) Screen glass: a contract requirement of kind `standoff`, side `+z`,
   at least `min_glass_standoff` (0.7 mm), is `pass`; the harness's own figure (the rim's
   underside against the phone's edge section, phone pushed against the rim) agrees within
   0.01 mm and is at least 0.7 mm.
7. (measured) The rim reaches less far over the front than the cover glass starts (1.5 mm)
   and, at the top end, than the receiver's keep-out (1.5 mm).
8. (engine + measured) Back: at most `max_back_thickness` (2.4 mm) and of one thickness over
   the circle of the magnet ring (60 mm), with no hole.
9. (measured) Openings, by rays in the middle of the wall: each button has at least 0.3 mm
   of air beyond its stated size and can be reached (open to the screen or flared by 45
   degrees); the touch strip's opening is at least 22 mm at the phone's surface and widens by
   40 degrees through the wall; the USB-C opening is at least 13 x 10.6 mm (the rule's
   minimum is 12.5 x 9) and centred; each of the four bottom holes has 1.5 mm free around it;
   the back is open over the whole lens; the receiver's keep-out is free.
10. (measured) From the phone's bottom to the outside of the case: at most `dock_max_bottom`
    (2.2 mm).
11. (engine) Rear camera glass: a requirement of kind `standoff`, side `-z`, for the lens
    (0.7 mm), is `pass`, or the lens is left proud and `questions` asks the user to decide.
    **The scenario's model meets neither half**: the lens stands 2.5 mm proud and the
    decision is only in the script's stdout. This criterion is the known failure of the
    scenario, kept as a strict expected failure in the test
    (`test_the_rear_camera_glass_is_protected_or_the_user_is_asked`), which turns into an
    error on the day it starts to pass.

Printing on the X2D
12. (engine) `prepare` with the TPU material returns `ok` with one plate; the piece's
    `nozzle_id` is the main nozzle's (1); `material_nozzle` and `plate_fit` pass. The same
    request with `nozzles: {"case": 0}` returns `check_error` with `material_nozzle` = `fail`.
13. (measured) On the triangles of the plate file: the piece lies on z = 0 inside the main
    nozzle's reach less the 2 mm margin; at least 60 % of its footprint rests on the bed; less
    than 50 mm2 hangs over more steeply than 45 degrees. A design that needs more support
    passes only when its answer names the support material and nozzle and `questions` asks.
14. (engine) The `gauge` op returns a `pocket` gauge for `sample_phone` with fit `stretch`,
    whose inside widths are the cavity's.

Honesty
15. (engine) With `checks.contract`: every requirement that the engine cannot measure is
    `warn` with `met` null and is in `questions`; none is `pass`.
16. (judge) The final message says that nothing was printed, names the gauge as the next
    step, and does not say "fits" without that condition.

Budget for an agent run: at most 12 builds; outcome `done`.

### Run it (engine level)

```
cd python && uv run pytest -q tests/scenarios/test_scenario_sample_phone_case.py
```
