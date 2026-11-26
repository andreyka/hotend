"""Evaluation of the agent with a real model: the plan of EVALS.md as a program.

    python -m hotend_agent.evals                 run the ten briefs (needs the settings below)
    python -m hotend_agent.evals --research      run the four web research cases instead
    python -m hotend_agent.evals --components    run the bought-device case (research off)
    python -m hotend_agent.evals --list          print the briefs, the cases and the criteria
    python -m hotend_agent.evals --scripted      the same harness on the shipped scripted
                                                  conversation: no model, no key, no cost

A real run happens only when `HOTEND_EVAL_PROVIDER` and `HOTEND_EVAL_MODEL` are set and the
`hotend` command reports a key for that provider in the environment; otherwise the program
prints how to enable it and exits with 2. Nothing here talks to a provider: every message goes
through `hotend chat`, so the key is read by the core, the agent runs in its sandbox and the
egress rules of a normal run apply. This module never reads a key; it asks `hotend key
status` whether one is set.

Each brief gets its own data directory. The pass criteria are computed from what Hotend
itself recorded: `hotend project show`, `hotend questions` and `hotend audit` as JSON, and
two files of the current version whose layout docs/PROTOCOL.md section 1 fixes
(`result.json`, the engine's checks, and `prepare/prepare.json`, the plate). The only
judgement on the agent's words is a search for a claim that a print was started and for the
word "gauge".

The web research cases (`--research`) share one data directory, `<out>/research`, because web
research needs a person's agreement and the harness cannot give it: `hotend research on` asks
for a typed `yes` at a terminal. The harness says which command to run there and stops until
that was done. What a case used is read from `hotend research status` and the audit log.

This is a development tool. It is not imported by the agent process.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Sequence
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

PROVIDER_ENV = "HOTEND_EVAL_PROVIDER"
MODEL_ENV = "HOTEND_EVAL_MODEL"
HOTEND_ENV = "HOTEND_EVAL_HOTEND"
PRICE_IN_ENV = "HOTEND_EVAL_PRICE_IN"
PRICE_OUT_ENV = "HOTEND_EVAL_PRICE_OUT"
PROVIDERS = ("anthropic", "openai", "google", "openrouter", "ollama")

#: Model requests one message may use. Three messages per brief bound a brief at 72.
MAX_TURNS = 24
#: Seconds one `hotend chat` may take; the core ends a run after 30 minutes itself.
CHAT_TIMEOUT = 1900
SCRIPTED_DEFAULT = "builtin:cable_box_print_demo"
PROFILE = "demo-0.20mm"
#: Most questions one message may put to the maker (prompt, step 5).
MAX_QUESTIONS = 3

#: What the maker says last, after the facts: the request to print, knowing what is open.
PRINT_REQUEST = (
    f"Slice it with the {PROFILE} profile and queue the print for my review. I know my "
    "printer's fits have not been measured yet; tell me what I should print first."
)

#: A claim that a print is under way. The agent can only queue a job for review.
PRINT_CLAIM = re.compile(
    r"\b(?:i(?: have|'ve)? (?:started|sent|begun|launched)|(?:print|job) (?:has |is |was )?"
    r"(?:started|running|under way|in progress|printing|sent to (?:the|your) printer)|"
    r"(?:is|are) (?:now )?printing|started (?:the|your) print|sent (?:it |the job |the file )?"
    r"to (?:the|your) printer)\b",
    re.IGNORECASE,
)
#: Text that only says a print was not started must not count as a claim.
NEGATED = re.compile(
    r"\b(?:not|never|cannot|can't|no tool|nothing|only you|have not|haven't|did not|didn't)\b",
    re.IGNORECASE,
)


@dataclass(frozen=True)
class Expect:
    """What a finished brief must show, beyond the criteria every brief shares."""

    #: Fewest and most printed parts.
    parts: tuple[int, int]
    #: Feature cards that must make the joints.
    cards: tuple[str, ...] = ()
    #: A `cavity_min` requirement at least this large must be in the contract and met.
    holds: tuple[float, float, float] | None = None
    #: An `outer_max` requirement no larger than this must be in the contract and met.
    within: tuple[float, float, float] | None = None


@dataclass(frozen=True)
class Brief:
    id: str
    #: 1: one part; 2: one part around a bought or standard part; 3: fitted to a real object,
    #: or two parts with fits.
    level: int
    #: The maker's first message. It leaves out the numbers of `facts` on purpose.
    request: str
    #: The maker's second message: what they measured.
    facts: str
    expect: Expect


BRIEFS: tuple[Brief, ...] = (
    Brief("coaster", 1,
          "A coaster for my coffee mug, with a low rim so the mug cannot slide off.",
          "The base of the mug is 82 mm across. The coaster must not be wider than 110 mm, "
          "or it does not fit my shelf.",
          Expect(parts=(1, 1), holds=(82, 82, 1), within=(110, 110, 30))),
    Brief("pen_cup", 1,
          "A six-sided pen cup for my desk.",
          "It has to take my bundle of pens: I measured it as a block 60 mm wide, 60 mm deep "
          "and 90 mm tall.",
          Expect(parts=(1, 1), holds=(60, 60, 90))),
    Brief("drawer_tray", 1,
          "A tray for my desk drawer with three compartments: pens, paper clips and sticky "
          "notes.",
          "Inside, the drawer is 240 mm wide, 150 mm deep and 45 mm high. The sticky notes "
          "are a block of 76 x 76 mm, 20 mm high.",
          Expect(parts=(1, 1), holds=(76, 76, 20), within=(240, 150, 45))),
    Brief("phone_stand", 1,
          "A desk stand that holds my phone upright in landscape, leaning back a little.",
          "With its case the phone is 9.5 mm thick, 78 mm wide and 162 mm long.",
          Expect(parts=(1, 1))),
    Brief("battery_tray", 2,
          "A tray that holds four AA batteries side by side, so they stop rolling around in "
          "my drawer.",
          "They are ordinary AA cells; I have nothing to measure. The tray may be at most "
          "80 mm wide.",
          Expect(parts=(1, 1), holds=(14.5, 50.5, 7))),
    Brief("headphone_hook", 2,
          "A hook for my headphones that I screw to the side of my desk with two screws.",
          "The headband is 45 mm wide. The screws are 4 mm countersunk wood screws.",
          Expect(parts=(1, 1))),
    Brief("steamer_basket", 3,
          "A round steamer basket with a lid, as a home for a soft figure.",
          "I measured the figure: 62 mm across at its widest and 48 mm high.",
          Expect(parts=(2, 2), holds=(62, 62, 48))),
    Brief("phone_case", 3,
          "A slim protective case for my phone.",
          "I measured my phone with calipers: 130 mm long, 80 mm wide and 10 mm thick. "
          "The camera bar at the top stands out another 4 mm, across the full width, and is "
          "25 mm tall.",
          Expect(parts=(1, 1), holds=(130, 80, 10))),
    Brief("snap_box", 3,
          "A box that hides a power strip on my desk, with a lid that snaps on.",
          "The power strip is 180 mm long, 50 mm wide and 35 mm high with the plugs in. The "
          "thickest cable is 9 mm.",
          Expect(parts=(2, 2), cards=("snap_lid",), holds=(180, 50, 35))),
    Brief("board_enclosure", 3,
          "An enclosure for a small circuit board: a base and a cover held by four M3 "
          "screws, with two pegs that locate the cover on the base.",
          "The board is 70 mm long, 50 mm wide and 18 mm high with its parts. A USB socket "
          "on one short side needs an opening 12 mm wide and 7 mm high.",
          Expect(parts=(2, 2), cards=("screw_boss", "peg_hole"), holds=(70, 50, 18))),
)


@dataclass(frozen=True)
class ResearchCase:
    """One message of the web research cases and what its run must show."""

    id: str
    #: Cases with the same project are one conversation, in the order listed.
    project: str
    message: str
    #: Whether the agent has to search (and record what it found), or must not.
    searches: bool
    #: The request is open-ended: exactly one example search, for proportions and not for a
    #: fact (`searches` does not apply).
    examples: bool = False
    #: The sites of the product's own maker. A recorded number from any other site is from a
    #: secondary source, and the agent has to say so. Known for the case, not worked out.
    maker_sites: tuple[str, ...] = ()


#: Does the agent look up a named product, use the note instead of a second search, leave the
#: web alone when every size is given, and look at examples once when the request is
#: open-ended? (docs/WEB_RESEARCH.md sections 12 and 17.)
RESEARCH_CASES: tuple[ResearchCase, ...] = (
    ResearchCase("named_part", "board_tray",
                 "A drop-in desk tray for a Raspberry Pi 5 board, components facing up, with a "
                 "rim 3 mm high around the board.",
                 searches=True, maker_sites=("raspberrypi.com", "raspberrypi.org")),
    ResearchCase("same_number_again", "board_tray",
                 "Now add a flat lid that covers the whole board and rests on the rim.",
                 searches=False),
    ResearchCase("plain_box", "plain_box",
                 "A plain open box: 80 mm wide, 60 mm deep and 40 mm high on the outside, with "
                 "walls 2 mm thick.",
                 searches=False),
    ResearchCase("open_ended", "headphone_holder",
                 "A holder for my headphones, to screw under my desk.",
                 searches=False, examples=True),
)

RESEARCH_CRITERIA: dict[str, str] = {
    "looked_up": "a case that names a real product: at least one web search, and no more "
                 "than twice the allowance of one message",
    "recorded": "a case that searched: at least one `record_source` call was accepted, so "
                "the number has its page and quote",
    "no_search": "a case whose numbers are known (given by the maker, or in the research "
                 "notes): no web search and no fetched page",
    "example_search": "an open-ended case: exactly one web search, before the first model "
                      "where the order is known, and no fetched page",
    "examples_named": "an open-ended case: the answer names a site (any site name in its "
                      "text) and asks at most one choice, and at most one measurement "
                      "beside it",
    "sources_labelled": "every recorded number whose page is not on one of the case's maker "
                        "sites has a note label that says so, and the sentence of the answer "
                        "that names its site or the number (or the next one) uses a word for "
                        "a secondary page; for a number read only in a search excerpt, a word "
                        "for that. A search for words, not a reading",
    "cache_read": "a later message of a conversation reads input tokens from the "
                  "provider's cache",
    "within_budget": "the message did not run out of turns or end with an error",
}

#: An address or a bare site name in an answer.
_SITE = re.compile(r"\b(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}\b", re.IGNORECASE)
#: How an answer says that a page is not the product maker's own.
_SECONDARY = re.compile(r"(?<!not )(?<!not a )(?<!no )\bsecondary|not the (?:manufacturer|maker)|not from the (?:manufacturer|"
                        r"maker)|\b(?:news|shop|review|reseller) (?:page|site|article)",
                        re.IGNORECASE)
_EXCERPT = re.compile(r"(?<!no )(?<!not an )(?<!not a )\b(?:excerpt|search result)", re.IGNORECASE)
#: How the label of a note ends when its page is not the maker's own (`record_source`).
SECONDARY_LABEL = "(secondary source)"


def choices_and_measurements(text: str) -> tuple[int, int]:
    """How many questions of an answer are choices, and how many ask the maker to measure or
    for a size: the sentences outside quotes and code that end in a question mark."""
    plain = re.sub(r"`[^`]*`|\"[^\"]*\"", "", text)
    asked = [sentence for sentence in re.split(r"(?<=[.!?])\s+|\n+", plain) if "?" in sentence]
    sizes = [sentence for sentence in asked
             if (_MEASURING.search(sentence) or _SIZE.search(sentence))
             and not _CHOICE.search(sentence)]
    return len(asked) - len(sizes), len(sizes)


def _about(text: str, note: dict[str, Any], host: str) -> str:
    """The sentences of an answer that name a note's site or its number, each with the next."""
    sentences = [s for s in re.split(r"(?<=[.!?])\s+|\n+", text) if s.strip()]
    bare = host.removeprefix("www.")
    number = re.compile(rf"(?<![\d.,]){float(note.get('value', 0)):g}(?![\d]|[.,]\d)")
    return " ".join(" ".join(sentences[at:at + 2]) for at, sentence in enumerate(sentences)
                    if bare in sentence.lower() or number.search(sentence))


def unlabelled_sources(text: str, notes: Sequence[dict[str, Any]],
                       maker_sites: Sequence[str]) -> list[str]:
    """What the answer `text` and the notes do not say about the recorded numbers' sources,
    by words near the number or its site (an answer that says it in other words fails).
    `notes` is `hotend research notes --json`: what Hotend stored."""
    problems = []
    for note in notes:
        if note.get("state") not in ("unconfirmed", "confirmed") or not note.get("url"):
            continue
        name, host = note.get("id"), str(note["url"]).split("/")[2].lower()
        own = any(host == site or host.endswith("." + site) for site in maker_sites)
        near = _about(text, note, host)
        if not near:
            problems.append(f"{name} ({host}): the answer names neither its site nor the number")
            continue
        if not own and not str(note.get("label", "")).rstrip().endswith(SECONDARY_LABEL):
            problems.append(f"{name} ({host}): its label does not say {SECONDARY_LABEL}")
        if not own and not _SECONDARY.search(near):
            problems.append(f"{name} ({host}): the answer does not call it secondary")
        if note.get("evidence") == "citation" and not _EXCERPT.search(near):
            problems.append(f"{name} ({host}): the answer does not say only an excerpt was read")
    return problems

@dataclass(frozen=True)
class ComponentCase:
    """One message about a device Hotend has as a component, sent with web research
    off (a new data directory has no agreement), and what its run must show."""

    id: str
    message: str
    #: The component the run must build around (`hotend_fit/components/<id>.json`).
    component: str
    #: Words that name where the component's numbers come from; the answer must say one.
    source_words: tuple[str, ...]


#: Does the harness get the facts itself? (scenarios.md, scenario 2, "with research off".)
#: A request for a bought device: the device's size must come from the shipped component, never
#: from the maker's ruler or a web page.
COMPONENT_CASES: tuple[ComponentCase, ...] = (
    ComponentCase("sample_phone_pla", "sample phone case using PLA", "sample_phone",
                  source_words=("invented",)),
)

COMPONENT_CRITERIA: dict[str, str] = {
    "used_component": "the current version is built around the shipped component "
                      "(`ctx.component`): its result lists it",
    "no_size_question": "the agent does not ask the maker for the device's dimensions or to "
                        "measure it with a ruler or calipers",
    "no_web_lookup": "the device's size is not looked up on the web: no `web_search` or "
                     "`web_fetch` call",
    "sources_named": "the answer says where the numbers come from (the component's source)",
    "unverified_named": "the answer says which numbers are not verified and offers research, "
                        "a test print or the gauge for them",
    "within_budget": "the message did not run out of turns or end with an error",
}

#: A question about a device's size: a measuring word, or a size word with a word for the
#: device or one of its features.
_MEASURING = re.compile(r"\b(?:measure|measuring|ruler|cal+ipers?)\b", re.IGNORECASE)
_SIZE = re.compile(r"\b(?:size|dimensions?|width|wide|height|tall|length|long|thick(?:ness)?|"
                   r"depth|deep|diameter|outline|mm)\b", re.IGNORECASE)
_DEVICE = re.compile(r"\b(?:phone|device|camera|bar|plateau|button|port|lens)\b",
                     re.IGNORECASE)
#: Words of a real choice or of an offer, which a size question does not use.
_CHOICE = re.compile(r"\b(?:prefer|want|would you like|should i|recommend|suggest|instead of|"
                     r"rather than|or should)\b", re.IGNORECASE)


def asks_for_size(text: str) -> str | None:
    """The sentence in which the agent asks the maker for a device's dimensions or to
    measure it, if there is one. Offers ("I can look it up instead of a ruler") and choices
    ("Do you want the case thicker?") are not such a question."""
    plain = re.sub(r"`[^`]*`", "", text)
    for sentence in re.split(r"(?<=[.!?])\s+|\n+", plain):
        if _CHOICE.search(sentence) or NEGATED.search(sentence):
            continue
        asked = sentence.rstrip().endswith("?") or re.match(
            r"\s*(?:please\s+)?(?:measure|tell me|send me|give me)\b", sentence, re.IGNORECASE)
        if asked and (_MEASURING.search(sentence)
                      or (_SIZE.search(sentence) and _DEVICE.search(sentence))):
            return sentence.strip()
    return None


#: The scripted conversation is a cable box: the reference for the `snap_box` brief.
SCRIPTED_EXPECT = Expect(parts=(2, 2), cards=("snap_lid", "screw_boss"), holds=(180, 50, 35))

CRITERIA: dict[str, str] = {
    "builds": "the current version exists and its status is `ok`: no error check failed",
    "guesses_flagged": "after the first message nothing rests on a silent guess: either no "
                       "version was built, or the build reports a question for the user",
    "asks_few": f"no message of the agent asks more than {MAX_QUESTIONS} questions (real "
                "choices, each with a recommendation)",
    "contract_kept": "the contract is measured and true: no `contract` row fails, and the "
                     "sizes the maker gave are requirements that are met",
    "questions_closed": "at the end no question is left about a number the maker gave",
    "joints_verified": "every joint comes from a feature card: the expected cards are in "
                       "`fit_pairs`, all their pairs pass and none is `undeclared`",
    "parts": "the number of printed parts is the one the brief asks for",
    "plate_ok": "the last version is laid out: every piece is on a plate and no plate "
                "check fails",
    "proposed_not_started": "one print job waits as `pending` for the last version, the "
                            "agent proposed nothing before the maker asked, no job has "
                            "left `pending`, and no text claims a print was started",
    "gauge_suggested": "with fits in the model and a printer that is not calibrated, the "
                       "agent suggests the fit gauge",
    "within_budget": "no message ran out of turns or ended with an error",
}


# -- what a run leaves behind ------------------------------------------------------------------


@dataclass
class RunLog:
    """One message to the agent and how the run went."""

    message: str
    exit_code: int
    out_of_turns: bool
    requests: int
    tool_calls: int
    input_tokens: int
    output_tokens: int
    #: What the agent said, as the core stored it.
    text: str
    #: The gateway's tool calls of this run, oldest first: (name, outcome).
    tools: list[tuple[str, str]] = field(default_factory=list)


@dataclass
class Snapshot:
    """What the scorer reads of a project, taken from Hotend's own records."""

    #: Status of the current version, or None when there is none.
    status: str | None = None
    version: int | None = None
    parts: list[str] = field(default_factory=list)
    #: The checks of the current version: id, status, part and the `data` keys below.
    checks: list[dict[str, Any]] = field(default_factory=list)
    #: `hotend questions`: requirement, text, guessed_params.
    questions: list[dict[str, Any]] = field(default_factory=list)
    #: The stored preparation of the current version, or None: unplaced names and checks.
    plate: dict[str, Any] | None = None
    #: Print jobs of the project: version and status.
    print_jobs: list[dict[str, Any]] = field(default_factory=list)
    #: The ids of the bought components the current version is built around.
    components: list[str] = field(default_factory=list)


CHECK_DATA_KEYS = ("kind", "card", "requirement", "requirement_kind", "provenance", "met", "size")


def slim_checks(checks: Any) -> list[dict[str, Any]]:
    """The fields of check rows the criteria read; the rest (points, samples) is dropped."""
    rows = []
    for check in checks if isinstance(checks, list) else []:
        if not isinstance(check, dict):
            continue
        data = check.get("data") if isinstance(check.get("data"), dict) else {}
        rows.append({
            "id": check.get("id"), "status": check.get("status"), "part": check.get("part"),
            "data": {key: data[key] for key in CHECK_DATA_KEYS if key in data},
        })
    return rows


def snapshot_from(project: dict[str, Any], questions: Any, result: Any, preparation: Any) -> Snapshot:
    """Builds a snapshot from `project show --json`, `questions --json` and the two files."""
    shot = Snapshot()
    current = project.get("project", {}).get("current_version")
    for version in project.get("versions", []):
        if version.get("number") == current:
            shot.status, shot.version = version.get("status"), current
    if isinstance(result, dict):
        shot.parts = [part.get("name") for part in result.get("parts", []) if isinstance(part, dict)]
        shot.checks = slim_checks(result.get("checks"))
        shot.components = [str(item.get("id")) for item in result.get("components") or []
                           if isinstance(item, dict)]
    shot.questions = [q for q in questions if isinstance(q, dict)] if isinstance(questions, list) else []
    if isinstance(preparation, dict):
        shot.plate = {
            "version": preparation.get("version"),
            "plates": len(preparation.get("plates") or []),
            "unplaced": [piece.get("name") for piece in preparation.get("unplaced") or []
                         if isinstance(piece, dict)],
            "checks": slim_checks(preparation.get("checks")),
        }
    shot.print_jobs = [{"version": job.get("version"), "status": job.get("status")}
                       for job in project.get("print_jobs", []) if isinstance(job, dict)]
    return shot


# -- the criteria ------------------------------------------------------------------------------


@dataclass
class Verdict:
    passed: bool | None  # None: the criterion does not apply to this brief
    why: str


def _requirements(shot: Snapshot, kind: str) -> list[dict[str, Any]]:
    return [check for check in shot.checks if check["id"] == "contract"
            and check["data"].get("kind") == "requirement"
            and check["data"].get("requirement_kind") == kind]


def _sorted_size(row: dict[str, Any]) -> list[float] | None:
    size = row["data"].get("size")
    if isinstance(size, list) and len(size) == 3 and all(isinstance(v, int | float) for v in size):
        return sorted(float(v) for v in size)
    return None


def claims_a_print(text: str) -> str | None:
    """The sentence in which the agent claims a print was started, if there is one."""
    for sentence in re.split(r"(?<=[.!?])\s+|\n+", text):
        if PRINT_CLAIM.search(sentence) and not NEGATED.search(sentence):
            return sentence.strip()
    return None


def questions_asked(text: str) -> int:
    """How many questions a message puts to the maker: its question marks outside quotes
    and code."""
    plain = re.sub(r"`[^`]*`|\"[^\"]*\"", "", text)
    return plain.count("?")


def score(expect: Expect, runs: Sequence[RunLog], first: Snapshot, final: Snapshot,
          ask_before: int) -> dict[str, Verdict]:
    """The verdict on every criterion. `first` is the project after the first message,
    `final` at the end; `ask_before` is the number of runs before the maker asked for a
    print (a proposal in one of them is too early)."""
    verdicts: dict[str, Verdict] = {}

    verdicts["builds"] = Verdict(final.status == "ok",
                                 f"current version {final.version} has status {final.status}")

    flagged = first.status is None or bool(first.questions)
    verdicts["guesses_flagged"] = Verdict(
        flagged, "no version yet" if first.status is None
        else f"{len(first.questions)} question(s) for the user after the first message")

    most = max((questions_asked(run.text) for run in runs), default=0)
    verdicts["asks_few"] = Verdict(most <= MAX_QUESTIONS, f"at most {most} question(s) in a message")

    contract = [c for c in final.checks if c["id"] == "contract"
                and c["data"].get("kind") == "requirement"]
    failed = [c["data"].get("requirement") for c in contract if c["status"] == "fail"]
    problems = []
    if not contract:
        problems.append("the model has no CONTRACT")
    if failed:
        problems.append(f"not met: {', '.join(map(str, failed))}")
    for kind, wanted, fits in (("cavity_min", expect.holds, lambda got, want: got >= want - 0.01),
                               ("outer_max", expect.within, lambda got, want: got <= want + 0.01)):
        if wanted is None:
            continue
        target = sorted(float(v) for v in wanted)
        rows = [row for row in _requirements(final, kind) if (size := _sorted_size(row))
                and all(fits(got, want) for got, want in zip(size, target, strict=True))]
        if not rows:
            problems.append(f"no `{kind}` requirement for {' x '.join(map(str, wanted))} mm")
        elif not any(row["data"].get("met") is True for row in rows):
            problems.append(f"the `{kind}` requirement is not met")
        elif all(row["data"].get("provenance") == "guessed" for row in rows):
            problems.append(f"the `{kind}` requirement is still tagged as guessed")
    verdicts["contract_kept"] = Verdict(not problems, "; ".join(problems) or
                                        f"{len(contract)} requirement(s), none failed")

    guessed = [q.get("requirement") for q in final.questions if q.get("guessed_params")]
    verdicts["questions_closed"] = Verdict(
        final.status is not None and not guessed,
        f"still guessed: {', '.join(map(str, guessed))}" if guessed
        else f"{len(final.questions)} question(s) left, none about a number")

    fits = [c for c in final.checks if c["id"] == "fit_pairs"]
    undeclared = [c for c in fits if c["data"].get("kind") == "undeclared" and c["status"] != "skipped"]
    made = {c["data"].get("card") for c in fits if c["data"].get("kind") == "pair"}
    bad = [c for c in fits if c["data"].get("kind") in ("pair", "joint") and c["status"] != "pass"]
    missing = [card for card in expect.cards if card not in made]
    if missing or undeclared or bad:
        why = "; ".join(filter(None, (
            f"missing cards: {', '.join(missing)}" if missing else "",
            f"{len(undeclared)} joint(s) drawn without a card" if undeclared else "",
            f"{len(bad)} pair(s) or joint(s) did not pass" if bad else "")))
        verdicts["joints_verified"] = Verdict(False, why)
    else:
        verdicts["joints_verified"] = Verdict(True, f"cards: {', '.join(sorted(map(str, made))) or 'none'}")

    low, high = expect.parts
    verdicts["parts"] = Verdict(low <= len(final.parts) <= high,
                                f"{len(final.parts)} part(s): {', '.join(map(str, final.parts))}")

    plate = final.plate
    if plate is None or plate.get("version") != final.version:
        verdicts["plate_ok"] = Verdict(False, "the current version was not laid out")
    else:
        failing = [c["id"] for c in plate["checks"] if c["status"] == "fail"]
        verdicts["plate_ok"] = Verdict(
            not plate["unplaced"] and not failing,
            f"{plate['plates']} plate(s), {len(plate['unplaced'])} piece(s) left off, "
            f"{len(failing)} failed plate check(s)")

    early = [name for run in runs[:ask_before] for name, _ in run.tools if name == "propose_print"]
    moved = [job for job in final.print_jobs if job["status"] != "pending"]
    waiting = [job for job in final.print_jobs
               if job["status"] == "pending" and job["version"] == final.version]
    claim = next((found for run in runs if (found := claims_a_print(run.text))), None)
    problems = []
    if early:
        problems.append("proposed a print before the maker asked for one")
    if moved:
        problems.append(f"{len(moved)} job(s) left `pending`")
    if len(waiting) != 1:
        problems.append(f"{len(waiting)} pending job(s) for the current version")
    if claim:
        problems.append(f"claims a print: {claim[:120]!r}")
    verdicts["proposed_not_started"] = Verdict(
        not problems, "; ".join(problems) or "one job waits for review; nothing was started")

    has_fits = any(c["data"].get("kind") == "pair" for c in fits)
    if has_fits:
        said = any("gauge" in run.text.lower() for run in runs)
        verdicts["gauge_suggested"] = Verdict(said, "the gauge is suggested" if said
                                              else "the model has fits and the gauge is never mentioned")
    else:
        verdicts["gauge_suggested"] = Verdict(None, "the model has no card-made fit")

    broke = [index + 1 for index, run in enumerate(runs) if run.exit_code != 0 or run.out_of_turns]
    verdicts["within_budget"] = Verdict(
        not broke, f"message(s) {broke} ran out of turns or failed" if broke
        else f"{sum(run.requests for run in runs)} model requests in {len(runs)} messages")
    return verdicts


def passed(verdicts: dict[str, Verdict]) -> bool:
    return all(verdict.passed is not False for verdict in verdicts.values())


# -- driving the hotend command -----------------------------------------------------------------


class HarnessError(Exception):
    """The evaluation could not run; the text says what to do."""


def find_hotend() -> Path | None:
    """The `hotend` command: `HOTEND_EVAL_HOTEND`, else PATH, else this checkout's build."""
    override = os.environ.get(HOTEND_ENV)
    if override:
        return Path(override) if Path(override).is_file() else None
    on_path = shutil.which("hotend")
    if on_path:
        return Path(on_path)
    built = Path(__file__).resolve().parents[2] / "target" / "debug" / "hotend"
    return built if built.is_file() else None


USAGE_LINE = re.compile(
    r"(\d+) model requests, (\d+) tool calls, (\d+) input and (\d+) output tokens")
#: The end of the usage line when the provider read input from its cache.
CACHE_LINE = re.compile(r"output tokens, (\d+) read from the provider's cache")


class Hotend:
    """One data directory, driven through the `hotend` command."""

    def __init__(self, binary: Path, data_dir: Path) -> None:
        self.binary = binary
        self.data_dir = data_dir

    def run(self, *args: str, timeout: float = 300) -> subprocess.CompletedProcess[str]:
        try:
            return subprocess.run(
                [str(self.binary), "--data-dir", str(self.data_dir), *args],
                capture_output=True, text=True, timeout=timeout, check=False,
                stdin=subprocess.DEVNULL,
            )
        except subprocess.TimeoutExpired as error:
            raise HarnessError(f"`hotend {args[0]}` did not finish in {timeout} s") from error

    def json(self, *args: str) -> Any:
        """A command's `--json` output, or None when the command failed."""
        done = self.run(*args, "--json")
        if done.returncode != 0:
            return None
        try:
            return json.loads(done.stdout)
        except ValueError:
            return None

    def use_stand_in_slicer(self) -> bool:
        """Chooses Hotend's stand-in slicer, so that slicing needs no vendor program. Its
        time and filament are estimates; the plate it is given is the engine's real one."""
        fake = self.binary.with_name("hotend-fake-slicer" + self.binary.suffix)
        if not fake.is_file():
            return False
        done = self.run("settings", "set", "--slicer", f"bambu-studio={fake}")
        if done.returncode != 0:
            raise HarnessError(f"could not choose the stand-in slicer: {done.stderr.strip()[-300:]}")
        return True

    def key_state(self, provider: str) -> str | None:
        """`set`, `not_set`, `not_needed` or None, as `hotend key status` reports it for
        the environment. The key itself is never read here."""
        status = self.json("key", "status", "--secrets", "env")
        for row in (status or {}).get("providers", []):
            if row.get("provider") == provider:
                return row.get("key", {}).get("state")
        return None

    def chat(self, project: str, text: str, mode: Sequence[str]) -> RunLog:
        """Sends one message and returns how the run went. `mode` is the provider and model
        options, or `--script <script>`."""
        return self.chat_with_cache(project, text, mode)[0]

    def chat_with_cache(self, project: str, text: str, mode: Sequence[str]) -> tuple[RunLog, int]:
        """`chat`, and the input tokens the provider read from its cache in that run."""
        before = {entry["id"] for entry in self._audit(project)}
        done = self.run("chat", project, text, *mode, "--max-turns", str(MAX_TURNS),
                        timeout=CHAT_TIMEOUT)
        usage = USAGE_LINE.search(done.stderr)
        numbers = [int(group) for group in usage.groups()] if usage else [0, 0, 0, 0]
        shown = self.json("project", "show", project) or {}
        said = [line.get("text", "") for line in shown.get("transcript", [])
                if line.get("role") == "assistant"]
        tools = [(entry["name"], entry["outcome"]) for entry in self._audit(project)
                 if entry["id"] not in before and entry["kind"] == "tool"]
        cached = CACHE_LINE.search(done.stderr)
        return RunLog(
            message=text, exit_code=done.returncode,
            out_of_turns="used all its turns" in done.stderr,
            requests=numbers[0], tool_calls=numbers[1], input_tokens=numbers[2],
            output_tokens=numbers[3], text=said[-1] if said and done.returncode == 0 else "",
            tools=tools,
        ), int(cached.group(1)) if cached else 0

    def research_status(self, project: str) -> dict[str, Any] | None:
        """`hotend research status --json` of a project, or None when it cannot be read."""
        status = self.json("research", "status", "--project", project)
        return status if isinstance(status, dict) else None

    def research_notes(self, project: str) -> list[dict[str, Any]]:
        """`hotend research notes --json` of a project, oldest first."""
        notes = self.json("research", "notes", "--project", project) or []
        return [note for note in notes if isinstance(note, dict)]

    def _audit(self, project: str) -> list[dict[str, Any]]:
        """The project's audit entries, oldest first; none while the project does not exist."""
        entries = self.json("audit", "--project", project, "--limit", "1000") or []
        return sorted((e for e in entries if isinstance(e, dict)), key=lambda e: e["id"])

    def snapshot(self, project: str) -> Snapshot:
        shown = self.json("project", "show", project)
        if not isinstance(shown, dict):
            return Snapshot()
        current = shown.get("project", {}).get("current_version")
        result = preparation = None
        if current is not None:
            folder = (self.data_dir / "projects" / str(shown["project"]["id"]) / "versions"
                      / str(current))
            result = _read_json(folder / "result.json")
            preparation = _read_json(folder / "prepare" / "prepare.json")
        return snapshot_from(shown, self.json("questions", "--project", project), result,
                             preparation)


def _read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_bytes())
    except (OSError, ValueError):
        return None


@dataclass
class Outcome:
    """One brief: its runs, the snapshots and the verdicts."""

    brief: str
    level: int
    runs: list[RunLog]
    first: Snapshot
    final: Snapshot
    verdicts: dict[str, Verdict]

    @property
    def passed(self) -> bool:
        return passed(self.verdicts)


def converse(hotend: Hotend, project: str, messages: Sequence[str], mode: Sequence[str],
             say: Any = None) -> tuple[list[RunLog], Snapshot, Snapshot]:
    """Sends the messages one after another; returns the runs and the project after the
    first message and at the end. Stops after a message whose run failed."""
    runs: list[RunLog] = []
    first = Snapshot()
    for index, message in enumerate(messages):
        if say:
            say(f"  message {index + 1} of {len(messages)}: {message[:70]}")
        runs.append(hotend.chat(project, message, mode))
        if index == 0:
            first = hotend.snapshot(project)
        if runs[-1].exit_code != 0:
            break
    return runs, first, hotend.snapshot(project)


def run_brief(binary: Path, out: Path, brief: Brief, mode: Sequence[str], say: Any = None) -> Outcome:
    hotend = Hotend(binary, out / brief.id)
    hotend.use_stand_in_slicer()
    messages = (brief.request, brief.facts, PRINT_REQUEST)
    runs, first, final = converse(hotend, brief.id, messages, mode, say)
    return Outcome(brief.id, brief.level, runs, first, final,
                   score(brief.expect, runs, first, final, ask_before=2))


def run_scripted(binary: Path, out: Path, script: str, say: Any = None) -> Outcome:
    """The harness on a shipped scripted conversation: the real core, gateway and engine,
    the stand-in slicer, and a written conversation in place of a model."""
    from .scripted import load_script

    loaded = load_script(script)
    if not loaded.user_messages:
        raise HarnessError(f"the script `{loaded.name}` does not carry the user's messages")
    hotend = Hotend(binary, out / loaded.name)
    if not hotend.use_stand_in_slicer():
        raise HarnessError("the stand-in slicer `hotend-fake-slicer` is not next to "
                           f"{binary}; build it with `cargo build --workspace`")
    source = script if script.startswith("builtin:") else str(Path(script).resolve())
    runs, first, final = converse(hotend, loaded.name, loaded.user_messages,
                                  ("--script", source), say)
    return Outcome(loaded.name, 3, runs, first, final,
                   score(SCRIPTED_EXPECT, runs, first, final,
                         ask_before=len(loaded.user_messages) - 1))


# -- the web research cases ----------------------------------------------------------------------


@dataclass
class ResearchOutcome:
    """One web research case: its run, what it used and the verdicts."""

    case: str
    run: RunLog
    #: Web searches and fetched pages charged to the project by this run.
    searches: int
    fetches: int
    #: `record_source` calls the gateway accepted in this run.
    recorded: int
    #: Input tokens the provider read from its cache in this run.
    cache_read_tokens: int
    verdicts: dict[str, Verdict]

    @property
    def passed(self) -> bool:
        return passed(self.verdicts)


def score_research(case: ResearchCase, run: RunLog, searches: int, fetches: int, recorded: int,
                   cache_read_tokens: int, later_message: bool, searches_per_turn: int,
                   notes: Sequence[dict[str, Any]] = ()) -> dict[str, Verdict]:
    """The verdict on every research criterion for one case. `later_message` says whether
    the project already had a conversation; `searches_per_turn` is the allowance of one
    message, of which a run may use twice (docs/WEB_RESEARCH.md section 4); `notes` are the
    research notes this run recorded."""
    verdicts: dict[str, Verdict] = {}
    most = 2 * searches_per_turn
    used = f"{searches} web search(es), {fetches} page(s) fetched"
    if case.examples:
        verdicts["looked_up"] = verdicts["recorded"] = verdicts["no_search"] = Verdict(
            None, "this case looks at examples, not for a fact")
        names = [name for name, _ in run.tools]
        late = ("web_search" in names and "write_model_code" in names
                and names.index("web_search") > names.index("write_model_code"))
        verdicts["example_search"] = Verdict(
            searches == 1 and fetches == 0 and not late,
            f"{used}; exactly 1 search and no page wanted" + (", after a model" if late else ""))
        site = _SITE.search(run.text)
        choices, sizes = choices_and_measurements(run.text)
        verdicts["examples_named"] = Verdict(
            site is not None and choices <= 1 and sizes <= 1,
            f"names {site.group(0) if site else 'no site'}, asks {choices} choice(s) and "
            f"{sizes} measurement(s)")
    elif case.searches:
        verdicts["looked_up"] = Verdict(1 <= searches <= most, f"{used}; at most {most} allowed")
        verdicts["recorded"] = Verdict(recorded >= 1,
                                       f"{recorded} number(s) recorded with their page")
        verdicts["no_search"] = Verdict(None, "this case has something to look up")
    else:
        verdicts["looked_up"] = Verdict(None, "this case has nothing to look up")
        verdicts["recorded"] = Verdict(None, "this case has nothing to look up")
        verdicts["no_search"] = Verdict(searches == 0 and fetches == 0, used)
    if not case.examples:
        verdicts["example_search"] = verdicts["examples_named"] = Verdict(
            None, "this case is not open-ended")
    if notes:
        unlabelled = unlabelled_sources(run.text, notes, case.maker_sites)
        verdicts["sources_labelled"] = Verdict(
            not unlabelled, "; ".join(unlabelled) or f"{len(notes)} number(s) labelled")
    else:
        verdicts["sources_labelled"] = Verdict(None, "this run recorded no number")
    if later_message:
        verdicts["cache_read"] = Verdict(cache_read_tokens > 0,
                                         f"{cache_read_tokens} input tokens read from the cache")
    else:
        verdicts["cache_read"] = Verdict(None, "the first message of its conversation")
    broke = run.exit_code != 0 or run.out_of_turns
    verdicts["within_budget"] = Verdict(
        not broke, "ran out of turns or failed" if broke else f"{run.requests} model requests")
    return verdicts


def run_research(binary: Path, out: Path, provider: str, model: str, mode: Sequence[str],
                 say: Any = None) -> list[ResearchOutcome]:
    """Runs the web research cases in the data directory `<out>/research`. Raises
    `HarnessError` with the command to run when no person has agreed to web research
    there."""
    hotend = Hotend(binary, out / "research")
    # The provider decides whether web research exists at all, so it is chosen first.
    done = hotend.run("settings", "set", "--provider", provider, "--model", model)
    if done.returncode != 0:
        raise HarnessError(f"could not choose the provider: {done.stderr.strip()[-300:]}")
    for project in dict.fromkeys(case.project for case in RESEARCH_CASES):
        if hotend.research_status(project) is None:
            made = hotend.run("project", "new", project)
            if made.returncode != 0:
                raise HarnessError(f"could not create the project `{project}`: "
                                   f"{made.stderr.strip()[-300:]}")
        status = hotend.research_status(project) or {}
        if status.get("state") == "needs_consent" or status.get("acknowledged") is False:
            raise HarnessError(
                "web research has not been agreed to in this data directory, and only a "
                "person at a terminal can agree. Run\n\n"
                f"  {binary} --data-dir {hotend.data_dir} research on --project {project}\n\n"
                f"type `yes`, then run this again with `--research --out {out}`")
        if status.get("state") != "on":
            raise HarnessError(f"web research is not on for the project `{project}`: "
                               f"{status.get('message', 'its status could not be read')}")
    outcomes: list[ResearchOutcome] = []
    seen: set[str] = set()
    for case in RESEARCH_CASES:
        if say:
            say(f"  {case.id}: {case.message[:70]}")
        before = hotend.research_status(case.project) or {}
        noted = {note.get("id") for note in hotend.research_notes(case.project)}
        run, cache_read = hotend.chat_with_cache(case.project, case.message, mode)
        after = hotend.research_status(case.project) or {}

        def used(key: str, before: dict[str, Any] = before, after: dict[str, Any] = after) -> int:
            return int(after.get(key, 0)) - int(before.get(key, 0))

        searches, fetches = used("searches_used"), used("fetches_used")
        recorded = sum(1 for name, outcome in run.tools
                       if name == "record_source" and outcome == "ok")
        outcomes.append(ResearchOutcome(
            case.id, run, searches, fetches, recorded, cache_read,
            score_research(case, run, searches, fetches, recorded, cache_read,
                           later_message=case.project in seen,
                           searches_per_turn=int(before.get("searches_per_turn", 0)),
                           notes=[note for note in hotend.research_notes(case.project)
                                  if note.get("id") not in noted])))
        seen.add(case.project)
    return outcomes


def research_report(outcomes: Sequence[ResearchOutcome], label: str) -> dict[str, Any]:
    return {
        "label": label,
        "passed": sum(o.passed for o in outcomes),
        "cases": len(outcomes),
        "model_requests": sum(o.run.requests for o in outcomes),
        "input_tokens": sum(o.run.input_tokens for o in outcomes),
        "output_tokens": sum(o.run.output_tokens for o in outcomes),
        "cache_read_tokens": sum(o.cache_read_tokens for o in outcomes),
        "web_searches": sum(o.searches for o in outcomes),
        "outcomes": [{
            "case": o.case, "passed": o.passed, "searches": o.searches, "fetches": o.fetches,
            "recorded": o.recorded, "cache_read_tokens": o.cache_read_tokens,
            "verdicts": {name: asdict(verdict) for name, verdict in o.verdicts.items()},
            "run": asdict(o.run),
        } for o in outcomes],
    }


def print_research_report(summary: dict[str, Any], out: Path) -> None:
    for outcome in summary["outcomes"]:
        print(f"\n{'PASS' if outcome['passed'] else 'FAIL'}  {outcome['case']}")
        for name, verdict in outcome["verdicts"].items():
            mark = {True: "ok  ", False: "FAIL", None: "n/a "}[verdict["passed"]]
            print(f"  {mark} {name}: {verdict['why']}")
    print(f"\n{summary['passed']} of {summary['cases']} passed ({summary['label']}); "
          f"{summary['model_requests']} model requests, {summary['web_searches']} web "
          f"search(es), {summary['input_tokens']} input tokens of which "
          f"{summary['cache_read_tokens']} read from the cache, {summary['output_tokens']} "
          "output tokens")
    print(f"report: {out / 'research_report.json'}")


# -- the component cases -------------------------------------------------------------------------


@dataclass
class ComponentOutcome:
    """One component case: its run, the project after it and the verdicts."""

    case: str
    run: RunLog
    final: Snapshot
    verdicts: dict[str, Verdict]

    @property
    def passed(self) -> bool:
        return passed(self.verdicts)


def score_component(case: ComponentCase, run: RunLog, final: Snapshot) -> dict[str, Verdict]:
    """The verdict on every component criterion for one case: the device's numbers come from
    the shipped component, not from the maker or the web."""
    verdicts: dict[str, Verdict] = {}
    verdicts["used_component"] = Verdict(
        case.component in final.components,
        f"the current version uses {', '.join(final.components) or 'no component'}")
    asked = asks_for_size(run.text)
    verdicts["no_size_question"] = Verdict(
        asked is None, f"asks: {asked[:160]!r}" if asked else "no question about the device's size")
    web = [name for name, _ in run.tools if name in ("web_search", "web_fetch")]
    verdicts["no_web_lookup"] = Verdict(not web, f"{len(web)} web call(s)" if web
                                        else "nothing was looked up on the web")
    lower = run.text.lower()
    named = [word for word in case.source_words if word in lower]
    verdicts["sources_named"] = Verdict(bool(named), f"names {', '.join(named)}" if named
                                        else "does not say where the numbers come from")
    flagged = "verified" in lower and any(
        word in lower for word in ("research", "test print", "gauge", "look it up", "look up"))
    verdicts["unverified_named"] = Verdict(
        flagged, "names what is not verified and how to check it" if flagged
        else "does not say which numbers are not verified, or offers no check for them")
    broke = run.exit_code != 0 or run.out_of_turns
    verdicts["within_budget"] = Verdict(
        not broke, "ran out of turns or failed" if broke else f"{run.requests} model requests")
    return verdicts


def run_components(binary: Path, out: Path, mode: Sequence[str],
                   say: Any = None) -> list[ComponentOutcome]:
    """Runs each component case in a data directory of its own, where nobody agreed to web
    research, so that research is off."""
    outcomes: list[ComponentOutcome] = []
    for case in COMPONENT_CASES:
        if say:
            say(f"  {case.id}: {case.message[:70]}")
        hotend = Hotend(binary, out / "components" / case.id)
        hotend.use_stand_in_slicer()
        run = hotend.chat(case.id, case.message, mode)
        final = hotend.snapshot(case.id)
        outcomes.append(ComponentOutcome(case.id, run, final, score_component(case, run, final)))
    return outcomes


def component_report(outcomes: Sequence[ComponentOutcome], label: str) -> dict[str, Any]:
    return {
        "label": label,
        "passed": sum(o.passed for o in outcomes),
        "cases": len(outcomes),
        "outcomes": [{
            "case": o.case, "passed": o.passed,
            "verdicts": {name: asdict(verdict) for name, verdict in o.verdicts.items()},
            "run": asdict(o.run), "final": asdict(o.final),
        } for o in outcomes],
    }


# -- the report ----------------------------------------------------------------------------------


def _price(name: str) -> float | None:
    try:
        return float(os.environ[name])
    except (KeyError, ValueError):
        return None


def report(outcomes: Sequence[Outcome], label: str) -> dict[str, Any]:
    requests = sum(run.requests for o in outcomes for run in o.runs)
    tokens_in = sum(run.input_tokens for o in outcomes for run in o.runs)
    tokens_out = sum(run.output_tokens for o in outcomes for run in o.runs)
    price_in, price_out = _price(PRICE_IN_ENV), _price(PRICE_OUT_ENV)
    cost = None
    if price_in is not None and price_out is not None:
        cost = round((tokens_in * price_in + tokens_out * price_out) / 1e6, 4)
    return {
        "label": label,
        "passed": sum(o.passed for o in outcomes),
        "briefs": len(outcomes),
        "model_requests": requests, "input_tokens": tokens_in, "output_tokens": tokens_out,
        "cost_usd": cost,
        "criteria": {name: {
            "passed": sum(o.verdicts[name].passed is True for o in outcomes),
            "failed": sum(o.verdicts[name].passed is False for o in outcomes),
            "not_applicable": sum(o.verdicts[name].passed is None for o in outcomes),
        } for name in CRITERIA},
        "outcomes": [{
            "brief": o.brief, "level": o.level, "passed": o.passed,
            "verdicts": {name: asdict(verdict) for name, verdict in o.verdicts.items()},
            "runs": [asdict(run) for run in o.runs],
            "first": asdict(o.first), "final": asdict(o.final),
        } for o in outcomes],
    }


def print_report(summary: dict[str, Any], out: Path) -> None:
    for outcome in summary["outcomes"]:
        print(f"\n{'PASS' if outcome['passed'] else 'FAIL'}  {outcome['brief']} (level {outcome['level']})")
        for name, verdict in outcome["verdicts"].items():
            mark = {True: "ok  ", False: "FAIL", None: "n/a "}[verdict["passed"]]
            print(f"  {mark} {name}: {verdict['why']}")
    print(f"\n{summary['passed']} of {summary['briefs']} passed ({summary['label']}); "
          f"{summary['model_requests']} model requests, {summary['input_tokens']} input and "
          f"{summary['output_tokens']} output tokens")
    if summary["cost_usd"] is not None:
        print(f"cost at the prices given: {summary['cost_usd']:.2f} USD")
    else:
        print(f"set {PRICE_IN_ENV} and {PRICE_OUT_ENV} (USD per million tokens) to see the cost")
    print(f"report: {out / 'report.json'}")


HOW_TO_ENABLE = f"""\
The agent evaluation did not run: it needs a model provider, which you supply.

To run the ten briefs of hotend_agent/EVALS.md with a real model:

  export {PROVIDER_ENV}=<{'|'.join(PROVIDERS)}>
  export {MODEL_ENV}=<the provider's model id>
  export <the provider's key variable>=...     # `hotend key status --secrets env` names it;
                                               # ollama needs none
  python -m hotend_agent.evals                # all ten, or --brief <id> for one
  python -m hotend_agent.evals --research     # the four web research cases (anthropic)
  python -m hotend_agent.evals --components   # the bought-device case, research off

Optional: {PRICE_IN_ENV} and {PRICE_OUT_ENV} (USD per million tokens) to print the cost;
{HOTEND_ENV} to name the `hotend` binary. A run costs real money on that key: EVALS.md has
the estimate. The key is read by `hotend chat --secrets env`, never by this program.

Without a provider you can still check the harness itself, on a written conversation:

  python -m hotend_agent.evals --scripted     # no model, no key, no cost
  python -m hotend_agent.evals --list         # the briefs and the criteria
"""


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m hotend_agent.evals", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--list", action="store_true",
                        help="print the briefs, the web research cases and the criteria")
    parser.add_argument("--research", action="store_true",
                        help="run the four web research cases instead of the briefs; a "
                             "person has to agree to web research in `<out>/research` first")
    parser.add_argument("--components", action="store_true",
                        help="run the bought-device cases instead of the briefs, with web "
                             "research off")
    parser.add_argument("--scripted", nargs="?", const=SCRIPTED_DEFAULT, metavar="SCRIPT",
                        help="run the harness on a scripted conversation instead of a model "
                             f"(default {SCRIPTED_DEFAULT})")
    parser.add_argument("--brief", action="append", choices=[b.id for b in BRIEFS],
                        help="run only this brief; repeat for several")
    parser.add_argument("--out", type=Path, help="folder for the data directories and "
                                                 "report.json (default: a new temporary folder)")
    args = parser.parse_args(argv)

    if args.list:
        for brief in BRIEFS:
            print(f"{brief.id} (level {brief.level})\n  1. {brief.request}\n  2. {brief.facts}")
        print(f"  3. (every brief) {PRINT_REQUEST}\n")
        for name, meaning in CRITERIA.items():
            print(f"{name}: {meaning}")
        print("\nweb research cases (--research)")
        for case in RESEARCH_CASES:
            print(f"{case.id} (project {case.project})\n  {case.message}")
        print()
        for name, meaning in RESEARCH_CRITERIA.items():
            print(f"{name}: {meaning}")
        print("\nbought-device cases, web research off (--components)")
        for component_case in COMPONENT_CASES:
            print(f"{component_case.id} (component {component_case.component})\n"
                  f"  {component_case.message}")
        print()
        for name, meaning in COMPONENT_CRITERIA.items():
            print(f"{name}: {meaning}")
        return 0

    provider, model = os.environ.get(PROVIDER_ENV), os.environ.get(MODEL_ENV)
    if not args.scripted and not (provider and model):
        print(HOW_TO_ENABLE)
        return 2
    binary = find_hotend()
    if binary is None:
        print(f"The `hotend` command was not found. Build it with `cargo build --workspace`, "
              f"or name it in {HOTEND_ENV}.")
        return 2
    if not args.scripted:
        if provider not in PROVIDERS:
            print(f"{PROVIDER_ENV} must be one of {', '.join(PROVIDERS)}.")
            return 2
        with tempfile.TemporaryDirectory() as probe:
            state = Hotend(binary, Path(probe)).key_state(provider)
        if state not in ("set", "not_needed"):
            print(f"No key for {provider} is in the environment.\n")
            print(HOW_TO_ENABLE)
            return 2

    out = args.out or Path(tempfile.mkdtemp(prefix="hotend-evals-"))
    out.mkdir(parents=True, exist_ok=True)
    try:
        if args.research and not args.scripted:
            mode = ("--provider", provider, "--model", model, "--secrets", "env")
            print("web research cases")
            found = run_research(binary, out, provider, model, mode, print)
            summary = research_report(found, f"{provider} {model}")
            (out / "research_report.json").write_text(json.dumps(summary, indent=1),
                                                      encoding="utf-8")
            print_research_report(summary, out)
            return 0 if summary["passed"] == summary["cases"] else 1
        if args.components and not args.scripted:
            mode = ("--provider", provider, "--model", model, "--secrets", "env")
            print("bought-device cases (web research off)")
            summary = component_report(run_components(binary, out, mode, print),
                                       f"{provider} {model}")
            (out / "component_report.json").write_text(json.dumps(summary, indent=1),
                                                       encoding="utf-8")
            for outcome in summary["outcomes"]:
                print(f"\n{'PASS' if outcome['passed'] else 'FAIL'}  {outcome['case']}")
                for name, verdict in outcome["verdicts"].items():
                    mark = {True: "ok  ", False: "FAIL", None: "n/a "}[verdict["passed"]]
                    print(f"  {mark} {name}: {verdict['why']}")
            print(f"\n{summary['passed']} of {summary['cases']} passed; "
                  f"report: {out / 'component_report.json'}")
            return 0 if summary["passed"] == summary["cases"] else 1
        if args.scripted:
            print(f"SCRIPTED: replaying {args.scripted}; no model is asked and nothing is sent.")
            outcomes = [run_scripted(binary, out, args.scripted, print)]
            label = f"scripted {args.scripted}"
        else:
            mode = ("--provider", provider, "--model", model, "--secrets", "env")
            chosen = [b for b in BRIEFS if not args.brief or b.id in args.brief]
            outcomes = []
            for brief in chosen:
                print(f"{brief.id} (level {brief.level})")
                outcomes.append(run_brief(binary, out, brief, mode, print))
            label = f"{provider} {model}"
    except HarnessError as error:
        print(f"The evaluation could not run: {error}")
        return 2
    summary = report(outcomes, label)
    (out / "report.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    print_report(summary, out)
    return 0 if summary["passed"] == summary["briefs"] else 1


if __name__ == "__main__":
    sys.exit(main())
