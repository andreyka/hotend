"""Builds the two cable box conversations that ship with the agent, and replays what the
core's gateway answered to them.

`hotend_agent/scripts/cable_box_demo.json` is the short conversation: a first version with a
planted mistake, the fix, the views, a summary that ends with the first question for the
maker. `cable_box_print_demo.json` goes on from there over three more messages of the maker:
the question is answered and the contract follows, a second question, a slice whose plate
summary is acted on, the suggestion to print the fit gauge first, and a print job proposed
for review. Both must stay in step with the example model `tests/models/cable_box.py`: the
fixed version is that file, the first version is the same file with one planted mistake, and
the later versions are that file with the edits listed below. test_agent_demo.py fails when a
shipped file differs from what this module builds; regenerate them with

    cd python && uv run python tests/agent/agent_demo_script.py

`fixtures/cable_box_print_demo_results.json` holds what the real gateway answered to every
tool call of the long conversation (real engine, Hotend's stand-in slicer). `Replay` plays
those answers back in order, so the agent tests need neither the core nor the engine;
test_agent_demo.py compares them with the real engine. Record them again, after a change to
the scripts, the model or the gateway's wording, with a built `hotend`:

    cd python && uv run python tests/agent/agent_demo_script.py --record
"""

from __future__ import annotations

import base64
import json
import sqlite3
import sys
import tempfile
import threading
from pathlib import Path
from typing import Any

PYTHON_DIR = Path(__file__).resolve().parents[2]
MODEL = PYTHON_DIR / "tests" / "models" / "cable_box.py"
SCRIPTS = PYTHON_DIR / "hotend_agent" / "scripts"
SHIPPED = SCRIPTS / "cable_box_demo.json"
SHIPPED_PRINT = SCRIPTS / "cable_box_print_demo.json"
RESULTS = Path(__file__).resolve().parent / "fixtures" / "cable_box_print_demo_results.json"
SNAPSHOTS = Path(__file__).resolve().parent / "fixtures" / "cable_box_print_demo_eval.json"

# The planted mistake: a wall the snap groove cuts too thin, with a range that allows it.
GOOD_WALL = '"wall": {"default": 2.0, "min": 1.6, "max": 3.0,'
THIN_WALL = '"wall": {"default": 1.0, "min": 0.8, "max": 3.0,'

# The maker measured the power strip: the box is sized around it and the contract says so.
SIZED = (
    (("The declared ranges passed the range sampler at both material extremes; what fails outside\n"
     "them is noted next to each range (tests/fit/test_fit_cable_box.py checks both).\n"),
     "Sized for the maker's power strip, measured 180 x 50 x 35 mm with the plugs in.\n"),
    (('    "width": {"default": 120.0, "min": 60.0, "max": 200.0, "unit": "mm",\n'
     '              "label": "Inside width", "provenance": "guessed"},\n'),
     ("    # The strip is 180 mm long; 2 mm of room at each end.\n"
     '    "width": {"default": 184.0, "min": 60.0, "max": 200.0, "unit": "mm",\n'
     '              "label": "Inside width", "provenance": "measured"},\n')),
    (('    "depth": {"default": 60.0, "min": 30.0, "max": 80.0, "unit": "mm",\n'
     '              "label": "Inside depth", "provenance": "guessed"},\n'),
     ("    # The strip is 50 mm wide; 2 mm of room on each side.\n"
     '    "depth": {"default": 54.0, "min": 30.0, "max": 80.0, "unit": "mm",\n'
     '              "label": "Inside depth", "provenance": "measured"},\n')),
    (('    "height": {"default": 40.0, "min": 26.0, "max": 80.0, "unit": "mm",\n'
     '               "label": "Height", "provenance": "style"},\n'),
     ("    # The strip is 35 mm high: a 2 mm floor, the strip, 1 mm of air, then the lid's\n"
     "    # rails, which hang 8 mm into the box.\n"
     '    "height": {"default": 46.0, "min": 26.0, "max": 80.0, "unit": "mm",\n'
     '               "label": "Height", "provenance": "measured"},\n')),
    ('         "size": [120.0, 60.0, 36.0], "provenance": "guessed",\n',
     '         "size": [180.0, 50.0, 35.0], "provenance": "measured",\n'),
)
# The maker measured the thickest cable and confirmed the slot.
CABLE = (
    ('    "cable": {"default": 12.0, "unit": "mm", "label": "Cable slot", "provenance": "guessed"},\n',
     ("    # The thickest cable measures 9 mm; the maker confirmed a 12 mm slot.\n"
     '    "cable": {"default": 12.0, "unit": "mm", "label": "Cable slot", "provenance": "measured"},\n')),
    ('         "value": 12.0, "provenance": "guessed", "params": ["cable"]},\n',
     ('         "value": 12.0, "provenance": "measured", "params": ["cable"],\n'
     '         "note": "the thickest cable measures 9 mm"},\n')),
)
# The plate summary said the flat top of the slot needs support: it becomes a roof.
ROOF = (
    (("    # One slot through both end walls, low enough to stay clear of the lid's rails.\n"
     "    box -= Pos(0, 0, FLOOR + 2.0) * Box(outer_x + 2.0, p.cable, p.cable, align=bottom)\n"),
     ("    # One slot through both end walls, low enough to stay clear of the lid's rails. Its top\n"
     "    # is a roof steeper than 45 degrees, so the box prints without support.\n"
     "    half = p.cable / 2\n"
     "    shoulder = p.cable - 1.2 * half\n"
     "    outline = Polygon((-half, 0), (half, 0), (half, shoulder), (0, p.cable),\n"
     "                      (-half, shoulder), align=None)\n"
     "    slot = extrude(Plane.YZ * outline, amount=outer_x / 2 + 1.0, both=True)\n"
     "    box -= Pos(0, 0, FLOOR + 2.0) * slot\n")),
)

#: Where a 9 mm cable's centre is when it lies in the slot of the sized box: in the middle of
#: the end wall (x), on the slot's centre line (y), 4.5 mm above the slot's bottom (z).
CABLE_QUERY = {"kind": "distance", "a": {"point": [93.0, 0.0, 8.5]}, "b": {"part": "box"}}
PROFILE = "demo-0.20mm"

USER_MESSAGES = (
    ("A box that hides a power strip on my desk, with a lid that snaps on. Show me a first "
    "version before I measure anything."),
    "It is 180 mm long, 50 mm wide and 35 mm high with the plugs in.",
    ("The thickest cable is 9 mm, so a 12 mm slot is fine. Please slice it so I can see how "
    "long it prints."),
    "I will print the gauge first. Queue the box for my review anyway.",
)


def _edit(text: str, edits: tuple[tuple[str, str], ...]) -> str:
    for old, new in edits:
        assert text.count(old) == 1, f"cable_box.py no longer has exactly one of: {old!r}"
        text = text.replace(old, new)
    return text


def versions() -> tuple[str, str]:
    """(the script with the planted mistake, the fixed script)."""
    good = MODEL.read_text(encoding="utf-8")
    assert good.count(GOOD_WALL) == 1, "cable_box.py no longer has the expected wall line"
    return good.replace(GOOD_WALL, THIN_WALL), good


def print_versions() -> tuple[str, str, str]:
    """The later versions of the long conversation: sized for the measured power strip, with
    the cable slot confirmed, and with the roof over the slot."""
    sized = _edit(versions()[1], SIZED)
    cable = _edit(sized, CABLE)
    return sized, cable, _edit(cable, ROOF)


def _design_turns() -> list[dict[str, Any]]:
    """Turns 1 to 5, the same in both scripts."""
    thin, good = versions()
    return [
        {
            "say": (
                "Here is what I will make.\n\n"
                "- A box that hides a power strip, open at the top, with a slot in both "
                "end walls for the cables.\n"
                "- A lid that snaps on and is locked with one M3 screw in a corner.\n\n"
                "Numbers and where they come from:\n"
                "- Inside 120 x 60 mm, 40 mm high: guessed. I do not know your power "
                "strip yet.\n"
                "- Cable slot 12 mm: guessed.\n"
                "- Wall thickness: style.\n"
                "- M3 screw: standard.\n"
                "- Every clearance of the snap lid and the screw: from your printer's "
                "calibration.\n\n"
                "You asked to see it first, so I will start with these sizes and make "
                "them sliders. First I am checking which fit features are available."
            ),
            "calls": [{"tool": "list_cards", "args": {}}],
        },
        {
            "expect": ["snap_lid", "screw_boss"],
            "say": (
                "The lid will use the snap-lid feature and the screw the screw-boss "
                "feature, so both halves of each joint come from one definition. "
                "Writing the first version."
            ),
            "calls": [{"tool": "write_model_code", "args": {
                "code": thin,
                "note": "First version: box, snap lid and screw boss, 1 mm walls",
            }}],
        },
        {
            "expect": ["status: check_error", "FAIL min_wall [box]"],
            "say": (
                "It builds, but the min_wall check failed: behind the groove that the "
                "lid snaps into, the wall is thinner than the 0.8 mm your printer needs. "
                "A 1 mm wall is too thin for this lid. I am making the wall 2 mm and "
                "limiting its slider to 1.6 to 3 mm."
            ),
            "calls": [{"tool": "write_model_code", "args": {
                "code": good,
                "note": "Wall 2 mm (slider 1.6 to 3 mm) so the snap groove leaves 0.8 mm",
            }}],
        },
        {
            "expect": ["status: ok", "checks: 0 fail"],
            "say": "All error checks pass now. Looking at the result, outside and cut open.",
            "calls": [
                {"tool": "render_views", "args": {"views": ["iso"]}},
                {"tool": "render_views", "args": {"views": ["front"],
                                                  "section": {"plane": "XZ"}}},
            ],
        },
        {
            "expect": ["views of version"],
            "say": (
                "Your cable box is ready to look at: a box with 120 x 60 mm of room "
                "inside, 40 mm high, with 2 mm walls, a cable slot at both ends and a lid "
                "that snaps on and takes one M3 screw.\n\n"
                "Checked: both parts are valid solids, fit the printer, have no wall "
                "under 0.8 mm and do not collide, and every clearance of the snap lid and "
                "the screw is drawn as your printer's calibration asks. One warning: the "
                "flat top of each cable slot hangs over and would need a little support. "
                "I will deal with that when we come to printing.\n\n"
                "Not settled: the inside size and the cable slot are my guesses. The check "
                "confirms that a 120 x 60 x 36 mm block goes in as built (the screw sits "
                "in a post outside the inside), but that only proves my guess, not your "
                "power strip. Your printer's fits have not been measured either.\n\n"
                "So I have three questions for you, one at a time. The first: how long, "
                "how wide and how high is your power strip with the plugs in? A ruler is "
                "enough. The cable slot and the lid's snap fit come after that, before "
                "the first print."
            ),
        },
    ]


def build() -> dict:
    """The short conversation: one message of the maker."""
    return {
        "protocol": 0,
        "name": "cable_box_demo",
        "description": (
            "Offline demo: a desk cable box with a snap lid and one screw. The first version "
            "has a wall that is too thin; the agent reads the failed check, fixes it, looks "
            "at the views and ends with what is checked, what is guessed and its first "
            "question for the maker."
        ),
        "user_messages": [USER_MESSAGES[0]],
        "turns": _design_turns(),
    }


def build_print() -> dict:
    """The long conversation: four messages of the maker, up to a print job proposed for
    review. Its first five turns are the short conversation."""
    sized, cable, roof = print_versions()
    measure_and_slice = [
        {"tool": "measure", "args": {"queries": [CABLE_QUERY]}},
        {"tool": "slice", "args": {"profile": PROFILE}},
    ]
    turns = _design_turns() + [
        # -- the maker answers the first question --------------------------------------------
        {
            "expect": ["180 mm long", "50 mm wide", "35 mm high"],
            "say": (
                "Thank you. I am sizing the box around a 180 x 50 x 35 mm power strip: "
                "184 x 54 mm inside, so the strip has 2 mm of room all round, and 46 mm "
                "high, so it stays below the lid's rails, which hang 8 mm "
                "into the box. These three sizes and the contract now carry your "
                "measurement instead of my guess."
            ),
            "calls": [{"tool": "write_model_code", "args": {
                "code": sized,
                "note": "Sized for the measured power strip: 180 x 50 x 35 mm",
            }}],
        },
        {
            "expect": ["status: ok", "checks: 0 fail", "questions for the user (2"],
            "say": (
                "It builds, and the contract check now confirms that a 180 x 50 x 35 mm "
                "block goes into the box. Two questions are left. The next one: how thick "
                "is the thickest cable that has to pass through the slot? Measure it at "
                "its widest point, with a ruler or calipers. The slot is 12 mm wide and "
                "12 mm high now, which is my guess."
            ),
        },
        # -- the maker answers the second question and asks for a slice ----------------------
        {
            "expect": ["9 mm", "slice"],
            "say": (
                "Good: a 9 mm cable has room in a 12 mm slot, so the slot stays as it is. "
                "I am marking it as measured and noting your 9 mm in the contract."
            ),
            "calls": [{"tool": "write_model_code", "args": {
                "code": cable,
                "note": "Cable slot confirmed: thickest cable 9 mm, slot 12 mm",
            }}],
        },
        {
            "expect": ["status: ok", "checks: 0 fail", "questions for the user (1",
                       "NOT CHECKABLE contract [box]"],
            "say": (
                "One question is left, about the lid's snap fit; I will come to it after "
                "the slice. The contract check cannot measure a slot (it reports that "
                "requirement as not checkable, which proves nothing), so I am measuring it "
                "myself: how far the nearest wall of the slot is from the centre of a "
                "9 mm cable lying in it. Then I am slicing with the 0.20 mm demo profile."
            ),
            "calls": measure_and_slice,
        },
        {
            "expect": ['"distance":4.5', "status: ok", "needs support",
                       "plate check warn overhangs"],
            "say": (
                "The cable passes: the nearest wall is 4.5 mm from its centre, which is "
                "its radius, so it rests on the bottom of the slot and nothing else "
                "touches it.\n\n"
                "The plate summary shows one thing to fix before printing: the box "
                "prints as modelled, and the flat top of each cable slot would need "
                "support. I am giving the slot a pointed top, like a roof, steeper than "
                "45 degrees, so it prints without support. The slot stays 12 mm wide and "
                "12 mm high at its peak."
            ),
            "calls": [{"tool": "write_model_code", "args": {
                "code": roof,
                "note": "Roof-shaped top on the cable slot, so the box needs no support",
            }}],
        },
        {
            "expect": ["status: ok", "checks: 0 fail, 2 warn"],
            "say": (
                "The overhang warning is gone. The slot changed shape, so I am measuring "
                "it again, and slicing again."
            ),
            "calls": measure_and_slice,
        },
        {
            "expect": ['"distance":4.5', "nothing needs support", "every piece fits a plate",
                       "plate check warn fit_orientation", "warning fake_slicer"],
            "say": (
                "The 9 mm cable still passes under the roof, and nothing on the plate "
                "needs support now: the box prints as modelled and the lid is turned "
                "upside down, flat on the bed. Both fit on one plate. The slice says "
                "about 1 hour 24 minutes and 43 g of filament, but it comes from "
                "Hotend's stand-in slicer with an untuned demo profile: take it as a "
                "rough estimate, and slice again with your real slicer before you print "
                "anything.\n\n"
                "One thing I cannot design away: printed flat, the lid's snap rails "
                "stand upright, across the layers, which is their weaker direction. Push "
                "the lid on gently.\n\n"
                "That leaves my last question, and it is about your printer, not the "
                "box. Nobody has measured how a snap fit comes out on it, so its fit "
                "numbers are still guesses: on a printer at the tight end the lid could "
                "bind, at the loose end it would not hold. Before the whole box, I "
                "suggest printing the fit gauge: four small pieces, a short piece of the "
                "snap joint and the screw boss with its plate. You export it from "
                "Hotend (`hotend gauge` on the command line) and record what you "
                "measure with calipers (`hotend calibrate`); then I rebuild the box so "
                "that its clearances follow your printer. Would you like to print the "
                "gauge first, or shall I queue the box for your review as it is?"
            ),
        },
        # -- the maker decides ----------------------------------------------------------------
        {
            "expect": ["gauge first", "Queue"],
            "say": (
                "Good. I am putting the box into your review queue, with a note that the "
                "gauge comes first."
            ),
            "calls": [{"tool": "propose_print", "args": {"note": (
                "Cable box for a 180 x 50 x 35 mm power strip, PLA, 0.20 mm demo profile. "
                "Sliced by the stand-in slicer: review only, slice again before printing. "
                "Print and measure the fit gauge first."
            )}}],
        },
        {
            "expect": ["pending print job"],
            "say": (
                "The box is queued as a print job that waits for your review. I have not "
                "started a print and I cannot: only you can confirm a job, in Hotend's "
                "own confirmation.\n\n"
                "Where things stand:\n"
                "- Made: a cable box for your 180 x 50 x 35 mm power strip, 188 x 62 mm "
                "outside and 46 mm high (48.4 mm with the lid on), with a cable slot at "
                "both ends and a lid that snaps on and takes one M3 screw.\n"
                "- Checked: both parts are valid solids, fit the printer, have no wall "
                "under 0.8 mm and do not collide; a block the size of your strip goes "
                "into the box; a 9 mm cable passes the slot; nothing on the plate needs "
                "support.\n"
                "- Still open: the snap fit on your printer. Print the fit gauge, record "
                "what you measure and tell me. I will rebuild the box with those "
                "numbers; it must then be sliced again, with your real slicer, before "
                "anything is printed."
            ),
        },
    ]
    return {
        "protocol": 0,
        "name": "cable_box_print_demo",
        "description": (
            "Offline demo, long form: the cable box of cable_box_demo (turns 1 to 5 are "
            "identical), then the maker answers the agent's questions one at a time. The "
            "agent keeps the contract true, slices, removes an overhang the plate summary "
            "reports, suggests printing the fit gauge first and, when the maker asks, "
            "proposes a print job for review. `user_messages` is the maker's side."
        ),
        "user_messages": list(USER_MESSAGES),
        "turns": turns,
    }


def script_calls(script: dict) -> list[tuple[str, dict[str, Any]]]:
    return [(call["tool"], call["args"]) for turn in script["turns"]
            for call in turn.get("calls", [])]


# -- what the gateway answered -------------------------------------------------------------------

PNG = bytes.fromhex("89504e470d0a1a0a") + b"\x00" * 300


def recorded() -> list[dict[str, Any]]:
    """The gateway's answers to the long conversation's tool calls, in order: `tool`, `text`
    and `images` (the names of the images that came with it)."""
    return json.loads(RESULTS.read_text(encoding="utf-8"))["results"]


def result_text(index: int) -> str:
    return recorded()[index]["text"]


class Replay:
    """A stand-in gateway that answers the conversation's tool calls with the recorded
    answers, in order. A call that is not the next one of the conversation is refused, as a
    gateway refuses what it does not know."""

    def __init__(self, script: dict | None = None) -> None:
        self._expected = script_calls(script or build_print())
        self._results = recorded()
        assert [tool for tool, _ in self._expected] == [
            r["tool"] for r in self._results[:len(self._expected)]], (
            "the recorded results are not those of this script; record them again")
        self._next = 0
        self._lock = threading.Lock()

    def __call__(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        with self._lock:
            index = self._next
            if index >= len(self._expected) or (name, args) != self._expected[index]:
                return {"ok": False, "error": {
                    "code": "unknown_tool", "message": f"{name} is not call {index + 1}"}}
            self._next += 1
        result = self._results[index]
        content: list[dict[str, Any]] = [{"type": "text", "text": result["text"]}]
        for path in result["images"]:
            content.append({"type": "image", "path": path, "media_type": "image/png",
                            "data_b64": base64.b64encode(PNG).decode()})
        return {"ok": True, "content": content}

    @property
    def played(self) -> int:
        return self._next


# -- recording, with a built `hotend` -----------------------------------------------------------


def _tool_returns(data_dir: Path) -> list[dict[str, Any]]:
    """The tool results in the conversation the core stored (Pydantic AI's message list)."""
    with sqlite3.connect(data_dir / "hotend.db") as store:
        (history,) = store.execute("SELECT history_json FROM messages").fetchone()
    results = []
    for message in json.loads(history):
        for part in message.get("parts", []):
            if part.get("part_kind") != "tool-return":
                continue
            content = part["content"] if isinstance(part["content"], list) else [part["content"]]
            # An image is stored either as a placeholder line or as the binary part itself
            # (a dict with its identifier, the path the gateway gave it).
            images = [item["identifier"] if isinstance(item, dict)
                      else item.split(" ", 1)[1].split(":", 1)[0] for item in content
                      if isinstance(item, dict) or item.startswith("[image ")]
            text = "\n".join(item for item in content
                             if isinstance(item, str) and not item.startswith("[image "))
            results.append({"tool": part["tool_name"], "text": text, "images": images})
    return results


def record() -> None:
    """Plays the long conversation through the real `hotend` (core, gateway, sandboxed
    engine, stand-in slicer) and stores what the gateway answered and what the evaluation
    harness saw."""
    from dataclasses import asdict

    from hotend_agent import evals

    binary = evals.find_hotend()
    assert binary is not None, "build the hotend command first: cargo build --workspace"
    with tempfile.TemporaryDirectory(prefix="hotend-record-") as folder:
        outcome = evals.run_scripted(binary, Path(folder), str(SHIPPED_PRINT), print)
        assert [run.exit_code for run in outcome.runs] == [0] * len(USER_MESSAGES), outcome.runs
        results = _tool_returns(Path(folder) / "cable_box_print_demo")
    calls = script_calls(build_print())
    assert [r["tool"] for r in results] == [tool for tool, _ in calls], results
    RESULTS.write_text(json.dumps({
        "about": "What the core's gateway answered to the tool calls of "
                 "cable_box_print_demo.json, in order: real engine, Hotend's stand-in "
                 "slicer. Recorded by tests/agent/agent_demo_script.py --record.",
        "results": results,
    }, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    SNAPSHOTS.write_text(json.dumps({
        "about": "What hotend_agent.evals saw of the same run: the runs, and the project "
                 "after the first message and at the end. Recorded with the results.",
        "runs": [asdict(run) for run in outcome.runs],
        "first": asdict(outcome.first),
        "final": asdict(outcome.final),
    }, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {RESULTS} and {SNAPSHOTS}")
    for name, verdict in outcome.verdicts.items():
        print(f"  {verdict.passed!s:5} {name}: {verdict.why}")


if __name__ == "__main__":
    for path, script in ((SHIPPED, build()), (SHIPPED_PRINT, build_print())):
        path.write_text(json.dumps(script, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"wrote {path}")
    if "--record" in sys.argv[1:]:
        record()
