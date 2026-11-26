# Open defect: intermittent geometry-kernel errors

Open, cause unknown, not reproduced. Two errors came out of OCP (the OpenCASCADE binding, with
build123d 0.13.0) in the Python test suite, once each, on Ubuntu under WSL2 (x86-64). Both tests
passed on the next run without a change.

1. `tests/fit/test_fit_edge_profile.py::test_the_makers_tolerance_of_the_thing_widens_the_printed_spread`,
   through `verify_pocket` and `solid_from_mesh` (`hotend_fit/reference.py`, 2392 vertices,
   4780 triangles):

   ```text
   wire = BRepBuilderAPI_MakePolygon(points[a], points[b], points[c], True).Wire()
   OCP.OCP.Standard.Standard_TypeMismatch: TopoDS::Edge
   ```

2. The steamer holder scenario with a local reference file: the proof of a cradle pocket raised
   `TopoDS_LockedShape: BRep_Builder::SameParameter`. There is no traceback; the worker's exit
   code was 0 and its stderr was empty.

Once, the test process itself aborted (signal 6) in a combined run beside a cargo test build. The
failing test is not known, and neither is whether it is the same defect.

## What was tried

- The first test, alone, in fresh processes: 1456 runs, 0 failures.
- `tests/fit` whole, 36 passes, and the scenario test, 240 passes: 0 failures.
- 0 failures in 240 runs rules out, at 95 %, only a failure rate above about 1.2 %; 0 in 1492
  (the first test: 1456 and 36), only one above about 0.2 %.

## What is known and what is not

- The same inputs passed in every repetition, so the inputs alone do not cause it.
- The geometry kernel runs in parallel: build123d asks OpenCascade to run its booleans in parallel
  (`SetRunParallel(True)`, `shape_core.py` and `utils.py` of 0.13.0), and
  `hotend_engine/__init__.py` says it runs a thread per CPU. A race inside OpenCascade is a
  candidate and has not been tested. (The engine's own thread pool only renders views, from
  meshes.) A fault of the machine is not ruled out either.
- What a person would see: a pocket whose proof raises is reported as *not* proven (a failed
  check). That is an error on the safe side. Whether a wrong number can come out without an
  exception is unknown.

## If it happens again

- A test run with `HOTEND_GEOMETRY_TRACE=1` writes the end of the traceback of a pocket's proof to
  stderr, and the steamer scenario test shows that stderr when it fails. In the application
  nothing is written: its worker starts from a cleared environment. A pass or fail of any other
  engine test does not show it.
- Nothing is retried, skipped or loosened. Keep the log, and do not rerun first.
