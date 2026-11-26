Demo settings profiles, written for Hotend (no vendor profile text is copied here).

They exist so the stand-in slicer and the tests have something to load. They are generic and
untuned: a result sliced with one carries the warning `demo_profile`, and they are never
handed to a real Bambu Studio. A real Bambu Studio is given the system profiles of its own
installation, flattened (`src/system.rs`, `profiles/system-profiles.toml`).

The Bambu Studio filament files carry a nozzle temperature and one bed temperature per plate
type, so that the stand-in can state both the way a real sliced file does. The numbers are
round placeholders, not tuned values.

Layout, the same as an installed profile folder:
`<slicer>/<machine|process|filament>/<id>.<json|ini>`.
