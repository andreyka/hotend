"""The three feature cards on real geometry: both halves from one definition, the measured
clearance equal to the resolver's at the default calibration and at both extremes, gauges,
ports, and the ways a fit can be broken."""

from __future__ import annotations

import inspect

import pytest
from build123d import Align, Box, Compound, Cylinder, Pos, Rot
from fit_helpers import EXTREMES, check_env, context, fit_results, resolver, write_table

from hotend_engine.checks import run_checks
from hotend_fit import cards, fit
from hotend_fit.cards import CardError, standards
from hotend_fit.verify import FIT_TOLERANCE_MM, measure_pairs

BOTTOM = (Align.CENTER, Align.CENTER, Align.MIN)
HANG = (Align.CENTER, Align.CENTER, Align.MAX)
SOLID_CHECKS = ("valid_solid", "manifold", "single_solid_per_part", "fits_build_volume",
                "parts_do_not_intersect")


def snap_box(r, opening=(80.0, 60.0), wall=2.0, height=30.0):
    width, depth = opening
    rim = Pos(0, 0, height)
    box = Box(width + 2 * wall, depth + 2 * wall, height, align=BOTTOM) - Pos(0, 0, wall) * Box(
        width, depth, height, align=BOTTOM
    )
    lid = rim * Box(width + 2 * wall, depth + 2 * wall, 2.0, align=BOTTOM)
    card = cards.snap_lid(opening=opening, wall=wall, ctx=r)
    return {"box": card.apply_a(box, at=rim), "lid": card.apply_b(lid, at=rim)}, card


def snap_box_turned(r):
    return snap_box(r, opening=(40.0, 70.0))


def pegged(r, fit_class="slide", apart=0.0, through=False):
    """A base with a hole and a cover with a peg; `apart` lifts the cover off the base."""
    base = Box(30, 30, 6.0 if through else 10.0, align=HANG)
    cover = Pos(0, 0, apart) * Box(30, 30, 3, align=BOTTOM)
    card = cards.peg_hole(diameter=5.0, length=6.0, fit=fit_class, ctx=r, through=through)
    return {"base": card.apply_a(base), "cover": card.apply_b(cover, at=Pos(0, 0, apart))}, card


def bossed(r, screw="M3", kind="self_tap"):
    top = Pos(0, 0, 10)
    base = Box(30, 30, 2, align=BOTTOM)
    cover = top * Box(30, 30, 2.4, align=BOTTOM)
    card = cards.screw_boss(screw=screw, kind=kind, height=8.0, plate=2.4, ctx=r)
    return {"base": card.apply_a(base, at=top), "cover": card.apply_b(cover, at=top)}, card


CASES = {
    "snap_lid": snap_box,
    "snap_lid_turned": snap_box_turned,
    "peg_hole_slide": pegged,
    # A press fit is drawn with interference for a loose printer, so it is shown apart.
    "peg_hole_press": lambda r: pegged(r, "press", apart=20.0),
    "peg_hole_through": lambda r: pegged(r, "snug", through=True),
    "screw_boss_self_tap": bossed,
    "screw_boss_tapping_screw": lambda r: bossed(r, "ST2.9"),
    "screw_boss_heat_set": lambda r: bossed(r, "M3", "heat_set"),
}
PAIR_COUNT = {
    "snap_lid": 10, "snap_lid_turned": 10, "peg_hole_slide": 2, "peg_hole_press": 2,
    "peg_hole_through": 1, "screw_boss_self_tap": 2, "screw_boss_tapping_screw": 2,
    "screw_boss_heat_set": 3,
}


def independent_gap(extreme: str, clearance: fit.Clearance, **extra) -> float:
    """The same request put to a fresh resolver: what the solids must show."""
    again = resolver(extreme, **extra).clearance(
        clearance.fit_class, clearance.nominal, axis=clearance.axis,
        printed_hole=clearance.printed_hole, printed_peg=clearance.printed_peg,
        datasheet_gap=clearance.target if clearance.target_source == "datasheet" else None,
    )
    return again.gap


@pytest.mark.parametrize("extreme", EXTREMES)
@pytest.mark.parametrize("case", CASES)
def test_measured_clearance_equals_the_resolvers(case, extreme):
    r = resolver(extreme)
    parts, card = CASES[case](r)
    assert r.declared_fits == card.checks() and len(card.checks()) == PAIR_COUNT[case]
    measurements = measure_pairs(parts, r.declared_fits, r)
    for m in measurements:
        assert m.ok, m.message
        assert m.measured == pytest.approx(independent_gap(extreme, m.pair.clearance), abs=1e-3)
        assert m.pair.clearance.extreme == extreme
    # The engine's check says the same, one result per pair, and nothing is left unverified.
    results = fit_results(parts, r)
    assert [res.status for res in results] == ["pass"] * PAIR_COUNT[case]
    assert all(res.id == "fit_pairs" and res.severity == "error" for res in results)
    assert results[0].data["expected"] == pytest.approx(results[0].data["measured"], abs=1e-3)
    assert {id(c) for c in r.issued} == {id(p.clearance) for p in r.declared_fits}
    # The card is also checked as a joint between its two parts, and that passes too.
    (joint,) = fit_results(parts, r, "joint")
    assert joint.status == "pass", joint.message
    assert joint.data["instance"] == card.name and sorted(joint.data["parts"]) == sorted(parts)
    assert fit_results(parts, r, "unverified") == []


@pytest.mark.parametrize("case", CASES)
def test_the_halves_are_valid_single_solids_that_do_not_overlap(case):
    r = resolver()
    parts, _ = CASES[case](r)
    results = run_checks(check_env(parts, r, mesh=False, only=SOLID_CHECKS))
    assert [(res.id, res.part, res.status) for res in results if res.status != "pass"] == []


@pytest.mark.parametrize("case", CASES)
def test_measured_clearance_follows_a_calibration_table(case, tmp_path):
    path = write_table(tmp_path)
    r = fit.resolver_for(context(calibration_path=str(path)))
    parts, _ = CASES[case](r)
    guessed = resolver()
    CASES[case](guessed)
    for m, default in zip(measure_pairs(parts, r.declared_fits, r), guessed.declared_fits,
                          strict=True):
        assert m.ok, m.message
        # The offsets are measured; the class's target is still a guess, a datasheet's is not.
        clearance = m.pair.clearance
        assert clearance.offsets_provenance == "calibrated"
        datasheet = clearance.target_source == "datasheet"
        assert clearance.target_provenance == ("standard" if datasheet else "guessed")
        assert clearance.provenance == ("calibrated" if datasheet else "guessed")
        assert ("target guessed" in m.message) is not datasheet
        assert m.measured == pytest.approx(
            independent_gap("nominal", m.pair.clearance, calibration_path=str(path)), abs=1e-3)
        if m.pair.clearance.axis == "xy":  # the table's offsets differ from the defaults
            assert abs(m.measured - default.expected) > 0.01


def test_the_material_extremes_move_the_solids_not_only_the_numbers():
    gaps = {}
    for extreme in EXTREMES:
        r = resolver(extreme)
        parts, _ = snap_box(r)
        gaps[extreme] = {m.pair.name: m.measured for m in measure_pairs(parts, r.declared_fits, r)}
    assert gaps["tight"]["wall_plus"] > gaps["nominal"]["wall_plus"] > gaps["loose"]["wall_plus"]
    assert gaps["loose"]["snap_plus"] < gaps["nominal"]["snap_plus"] < 0
    assert gaps["tight"]["wall_minus"] == pytest.approx(gaps["tight"]["wall_plus"], abs=1e-6)


def test_a_press_fit_for_a_loose_printer_is_measured_as_interference():
    r = resolver("loose")
    parts, card = pegged(r, "press", apart=20.0)
    radial = measure_pairs(parts, card.checks(), r)[0]
    assert radial.ok and radial.measured == pytest.approx(-0.10, abs=1e-3)
    assert card.peg_radius > card.hole_radius


# --- gauges ----------------------------------------------------------------------------


@pytest.mark.parametrize("extreme", EXTREMES)
@pytest.mark.parametrize("case", CASES)
def test_the_gauge_is_a_small_valid_print_with_the_products_clearances(case, extreme):
    r = resolver(extreme)
    _, card = CASES[case](r)
    issued, declared = len(r.issued), len(r.declared_fits)
    gauge = card.gauge_set()
    assert (len(r.issued), len(r.declared_fits)) == (issued, declared)  # nothing new resolved
    assert sorted(gauge.parts) == sorted([f"gauge_{card.half_a}", f"gauge_{card.half_b}"])
    results = run_checks(check_env(gauge.parts, r, mesh=False, only=SOLID_CHECKS))
    assert [(res.id, res.part, res.status) for res in results if res.status != "pass"] == []
    assert len(results) == 9
    for part in gauge.parts.values():
        assert part.bounding_box().min.Z == pytest.approx(0.0, abs=1e-6)  # sits on the bed
    assert max(gauge.size) < 80.0 and gauge.size[2] < 15.0
    # The gauge's own pairs are the product's clearance records, measured on the gauge.
    measurements = measure_pairs(gauge.parts, gauge.fits, r)
    assert len(measurements) == PAIR_COUNT[case]
    for m, product in zip(measurements, card.checks(), strict=True):
        assert m.ok, m.message
        assert m.pair.clearance is product.clearance
        assert m.measured == pytest.approx(product.expected, abs=FIT_TOLERANCE_MM)
    compound = card.gauge()
    assert isinstance(compound, Compound) and len(compound.solids()) == 2 and compound.is_valid


# --- ports and the card interface ----------------------------------------------------


def test_cards_take_no_clearance_arguments():
    banned = ("clearance", "gap", "tolerance", "offset", "interference", "play", "allowance")
    listing = cards.list_cards()
    assert [entry["name"] for entry in listing] == ["snap_lid", "peg_hole", "screw_boss"]
    for entry in listing:
        card = cards.CARDS[entry["name"]]
        names = [n for n in inspect.signature(card.__init__).parameters if n != "self"]
        assert "ctx" in names and not [n for n in names if any(word in n for word in banned)]
        assert entry["summary"].startswith(entry["name"]) and "_shared" not in entry["signature"]
        assert entry["ports"] == [card.half_a, card.half_b]
    assert cards.snap_lid is cards.SnapLid and cards.peg_hole is cards.PegHole
    with pytest.raises(fit.FitError, match="pass ctx"):
        cards.peg_hole(diameter=5.0, length=6.0, ctx=None)
    with pytest.raises(TypeError):
        cards.peg_hole(diameter=5.0, length=6.0, ctx=resolver(), clearance=0.2)


@pytest.mark.parametrize("case", ["snap_lid", "peg_hole_slide", "screw_boss_self_tap"])
def test_ports_have_a_location_a_direction_and_a_keep_out(case):
    _, card = CASES[case](resolver())
    assert list(card.ports) == [card.half_a, card.half_b]
    for name, port in card.ports.items():
        assert port.name == name and port.note
        assert sum(c * c for c in port.direction) == pytest.approx(1.0)
        assert tuple(port.location.position) is not None
    assert any(port.keep_out is not None for port in card.ports.values())
    assert card.ports is card.ports  # built once


def test_the_snap_lids_keep_out_holds_the_rails_and_a_filled_keep_out_collides():
    r = resolver()
    parts, card = snap_box(r)
    rim = Pos(0, 0, 30.0)
    keep_out = rim * card.ports["box"].keep_out
    assert keep_out.volume > 0
    plate = rim * Box(84, 64, 2.0, align=BOTTOM)
    rails = parts["lid"] - plate
    outside = rails - keep_out
    # Only the beads, which sit in the wall's grooves, reach outside the keep-out.
    assert sum(s.volume for s in outside.solids()) < 0.02 * sum(s.volume for s in rails.solids())
    # A box that fills the keep-out with a rib is caught as an overlap of the two parts.
    rib = rim * Pos(0, 27.0, 0) * Box(20, 6, 8, align=HANG)
    blocked = {"box": parts["box"] + rib, "lid": parts["lid"]}
    clash = run_checks(check_env(blocked, r, mesh=False, only=("parts_do_not_intersect",)))
    assert clash[0].status == "fail"
    # ... and by the joint check, which names the room the rib stands in.
    (joint,) = fit_results(blocked, r, "joint")
    assert joint.status == "fail" and "must leave free" in joint.message
    assert joint.data["keep_out"]["box"] > 100


def test_at_accepts_a_location_a_plane_or_a_position():
    from build123d import Plane

    for at in (Pos(0, 0, 10), Plane.XY.offset(10), (0, 0, 10), [0.0, 0.0, 10.0]):
        r = resolver()
        base = Box(30, 30, 10, align=BOTTOM)
        cover = Pos(0, 0, 10) * Box(30, 30, 3, align=BOTTOM)
        card = cards.peg_hole(diameter=5.0, length=6.0, ctx=r)
        parts = {"base": card.apply_a(base, at=at), "cover": card.apply_b(cover, at=at)}
        assert all(m.ok for m in measure_pairs(parts, card.checks(), r))
    with pytest.raises(CardError, match="'at' must be"):
        cards.peg_hole(diameter=5.0, length=6.0, ctx=resolver()).apply_a(base, at="top")


def test_a_card_works_at_any_orientation():
    r = resolver()
    frame = Pos(15, 4, 7) * Rot(0, 90, 0)  # the hole is drilled along -X of the model
    base = Pos(5, 4, 7) * Box(20, 30, 30)
    cover = Pos(16.5, 4, 7) * Box(3, 30, 30)
    card = cards.peg_hole(diameter=5.0, length=6.0, fit="press", ctx=r)
    parts = {"base": card.apply_a(base, at=frame), "cover": card.apply_b(cover, at=frame)}
    measurements = measure_pairs(parts, card.checks(), r)
    assert [m.ok for m in measurements] == [True, True], [m.message for m in measurements]
    assert measurements[0].parts == ("base", "cover")


def test_named_cards_and_default_names():
    r = resolver()
    first = cards.peg_hole(diameter=5.0, length=6.0, ctx=r)
    second = cards.peg_hole(diameter=4.0, length=6.0, ctx=r, name="dowel")
    boss = cards.screw_boss(ctx=r)
    assert (first.name, second.name, boss.name) == ("peg_hole_1", "dowel", "screw_boss_1")
    assert r.cards == [first, second, boss]
    assert [p.label for p in second.checks()] == ["dowel.radial", "dowel.relief"]


# --- broken fits -----------------------------------------------------------------------


def test_a_hand_widened_hole_fails_with_the_numbers():
    r = resolver()
    parts, card = pegged(r)
    parts["base"] = parts["base"] - Pos(0, 0, -6) * Cylinder(card.hole_radius + 0.1, 6, align=BOTTOM)
    radial, relief = fit_results(parts, r)
    assert radial.status == "fail" and relief.status == "pass"
    assert radial.data["measured"] == pytest.approx(card.radial.gap + 0.1, abs=1e-3)
    assert radial.data["difference"] == pytest.approx(0.1, abs=1e-3)
    assert radial.data["expected"] == pytest.approx(card.radial.gap)
    assert "0.365 mm" in radial.message and "0.265 mm" in radial.message
    assert "0.100 mm from where the card put it" in radial.message


def test_a_hand_made_half_with_a_typed_clearance_fails():
    """The hole comes from the card; the peg is modelled by hand with its own gap."""
    r = resolver()
    card = cards.peg_hole(diameter=5.0, length=6.0, ctx=r)
    base = card.apply_a(Box(30, 30, 10, align=HANG))
    cover = Box(30, 30, 3, align=BOTTOM) + Cylinder(2.5 - 0.2, 6, align=HANG)
    results = fit_results({"base": base, "cover": cover}, r)
    assert [res.status for res in results] == ["fail", "fail"]
    assert "never applied" in results[0].message and results[0].data["measured"] is None


def test_a_clearance_from_another_build_fails_even_when_the_solids_match():
    r = resolver()
    parts, card = pegged(r)
    fresh = resolver()  # the next build: same numbers, but none of them issued by it
    fresh.declared_fits.extend(card.checks())
    results = fit_results(parts, fresh)
    assert [res.status for res in results] == ["fail", "fail"]
    assert "did not come from this build's fit resolver" in results[0].message


def test_a_removed_or_moved_mating_face_fails():
    r = resolver()
    parts, card = snap_box(r)
    # Later modelling cuts a window through the wall the lid's plain rail bears on.
    window = Pos(40, 0, 30) * Box(10, 20, 12, align=HANG)
    cut = {"box": parts["box"] - window, "lid": parts["lid"]}
    by_pair = {res.data["pair"]: res for res in fit_results(cut, r)}
    assert by_pair["locate_plus"].status == "fail"
    assert "removed or covered" in by_pair["locate_plus"].message
    assert by_pair["locate_minus"].status == "pass" and by_pair["wall_plus"].status == "pass"
    # Moving a part after the card was applied is reported, with the clearance as it is now.
    shifted = {"box": parts["box"], "lid": Pos(0.3, 0, 0) * parts["lid"]}
    by_pair = {res.data["pair"]: res for res in fit_results(shifted, r)}
    assert by_pair["locate_plus"].status == "fail"
    assert by_pair["locate_plus"].data["measured"] == pytest.approx(card.c_locate.gap - 0.3, abs=1e-3)
    assert by_pair["locate_minus"].data["measured"] == pytest.approx(card.c_locate.gap + 0.3, abs=1e-3)
    assert "0.300 mm from where the card put it" in by_pair["locate_plus"].message


def test_a_clearance_taken_without_a_card_is_reported_not_passed():
    r = resolver()
    r.clearance("slide", 5.0)
    (alone,) = fit_results({"a": Box(10, 10, 10)}, r, "unverified")
    assert alone.status == "warn" and "without a feature card" in alone.message
    assert alone.data["unverified"][0]["fit_class"] == "slide"
    parts, _ = pegged(r)
    assert [res.status for res in fit_results(parts, r)] == ["pass", "pass"]
    (warning,) = fit_results(parts, r, "unverified")
    assert warning.status == "warn"
    assert fit_results({"a": Box(10, 10, 10)}, resolver())[0].message == "Skipped: no declared fits."


# --- cards that cannot be built say why ------------------------------------------------


def test_a_card_is_one_joint():
    r = resolver()
    card = cards.peg_hole(diameter=5.0, length=6.0, ctx=r)
    base = card.apply_a(Box(30, 30, 10, align=HANG))
    with pytest.raises(CardError, match="already applied"):
        card.apply_a(base)
    with pytest.raises(CardError, match="applied to a solid part"):
        cards.peg_hole(diameter=5.0, length=6.0, ctx=r).apply_a("base")


def test_a_half_that_does_not_touch_its_host_is_refused():
    r = resolver()
    card = cards.screw_boss(ctx=r)
    with pytest.raises(CardError, match="does not touch the part"):
        card.apply_a(Box(30, 30, 2, align=BOTTOM), at=Pos(0, 0, 40))


@pytest.mark.parametrize(("make", "needle"), [
    (lambda r: cards.peg_hole(diameter=5.0, length=6.0, fit="snap", ctx=r), "fit must be one of"),
    (lambda r: cards.peg_hole(diameter=0.5, length=6.0, ctx=r), "at least 1"),
    (lambda r: cards.peg_hole(diameter=8.0, length=2.0, ctx=r), "too short"),
    (lambda r: cards.snap_lid(opening=(6.0, 40.0), wall=2.0, ctx=r), "too small for the lid's rails"),
    (lambda r: cards.snap_lid(opening=(80.0, 60.0), wall=0.5, ctx=r), "wall of at least"),
    (lambda r: cards.snap_lid(opening=(80.0, 60.0), wall=2.0, depth=2.0, ctx=r), "use at least"),
    (lambda r: cards.snap_lid(opening=80.0, wall=2.0, ctx=r), "opening must be"),
    (lambda r: cards.screw_boss("M77", ctx=r), "unknown screw"),
    (lambda r: cards.screw_boss("M3", "glue", ctx=r), "kind must be one of"),
    (lambda r: cards.screw_boss("M3", height=3.0, ctx=r), "too little thread"),
    (lambda r: cards.screw_boss("ST2.9", "heat_set", ctx=r), "machine screw"),
    (lambda r: cards.screw_boss("M3", "heat_set", height=2.0, ctx=r), "make the boss taller"),
    (lambda r: cards.screw_boss("M3", "heat_set", insert="M3-0.5-99", ctx=r), "unknown CNCKitchen insert"),
    (lambda r: cards.screw_boss("M3", "heat_set", maker="Acme", ctx=r), "unknown insert maker"),
])
def test_impossible_cards_raise_a_card_error_that_names_the_fix(make, needle):
    r = resolver()
    with pytest.raises(CardError, match=needle):
        make(r)
    assert r.declared_fits == [] and r.cards == []


@pytest.mark.parametrize(("wall", "depth", "strain"), [
    (2.0, 5.0, "4.1 %"), (2.0, 3.0, "15.0 %"), (4.0, 4.0, "8.8 %"),
])
def test_a_snap_rail_that_would_break_on_closing_is_refused(wall, depth, strain):
    """The rail bends by the bead's undercut when the lid is pushed on. A short rail takes
    that as a strain PLA does not survive; the card must say so, not draw it."""
    r = resolver()
    with pytest.raises(CardError) as refused:
        cards.snap_lid(opening=(80.0, 60.0), wall=wall, depth=depth, ctx=r)
    message = str(refused.value)
    assert f"would be bent by {strain}" in message and "2 % PLA takes" in message
    assert "use a depth of at least" in message
    assert r.declared_fits == [] and r.cards == []
    # The depth the message names is enough.
    needed = float(message.rsplit("at least ", 1)[1].split(" mm")[0])
    card = cards.snap_lid(opening=(80.0, 60.0), wall=wall, depth=needed, ctx=resolver())
    assert card.insertion_strain <= card.strain_limit == 0.02


def test_the_snap_rail_is_capped_and_the_strain_limit_follows_the_material():
    # The rail no longer grows with the wall beyond 2 mm, so a thick wall does not make the
    # default 8 mm rail too stiff to bend (it was 2.1 % at a 3 mm wall).
    thick = cards.snap_lid(opening=(80.0, 60.0), wall=3.0, ctx=resolver())
    assert thick.rail == 2.0 and thick.insertion_strain < 0.02
    thicker = cards.snap_lid(opening=(80.0, 60.0), wall=4.0, ctx=resolver())
    assert thicker.rail == 2.0 and thicker.insertion_strain == thick.insertion_strain
    # PETG takes twice the strain: a depth refused in PLA is accepted there ...
    petg = context()
    petg["material"] = {"id": "petg-hf", "family": "PETG"}
    card = cards.snap_lid(opening=(80.0, 60.0), wall=2.0, depth=5.5, ctx=fit.resolver_for(petg))
    assert card.strain_limit == 0.04 and 0.02 < card.insertion_strain <= 0.04
    with pytest.raises(CardError, match="2 % PLA takes"):
        cards.snap_lid(opening=(80.0, 60.0), wall=2.0, depth=5.5, ctx=resolver())
    # ... and a material nobody knows gets the lowest limit.
    unknown = context()
    unknown["material"] = {"id": "mystery"}
    with pytest.raises(CardError, match="2 % an unknown material takes"):
        cards.snap_lid(opening=(80.0, 60.0), wall=2.0, depth=5.5, ctx=fit.resolver_for(unknown))


# --- standard parts --------------------------------------------------------------------


def test_screws_and_inserts_come_from_bd_warehouse():
    from bd_warehouse.fastener import HeatSetNut, PanHeadScrew

    m3 = standards.screw("M3")
    real = PanHeadScrew("M3-0.5", 10, "iso1580")
    assert m3.diameter == real.thread_diameter == 3.0
    assert m3.head_diameter == pytest.approx(real.head_diameter, abs=1e-4)
    assert not m3.thread_forming and "bd_warehouse" in m3.source
    assert {"M2", "M3", "M4", "M5", "ST2.9"} <= set(standards.screw_designations())
    tapping = standards.screw("ST2.9")
    assert tapping.thread_forming and tapping.diameter == 2.9 and "ISO 1478" in tapping.source

    insert = standards.heat_set_insert("M3", max_length=7.0)
    assert insert.size == "M3-0.5-5.7" and insert.maker == "CNCKitchen"
    nut = HeatSetNut(insert.size, "CNCKitchen")
    assert insert.outer_diameter == nut.nut_diameter == 4.6 and insert.length == 5.7
    assert insert.hole_diameter == pytest.approx(4.0894, abs=1e-4)  # drill #20
    assert insert.hole_diameter < insert.outer_diameter
    assert standards.heat_set_insert("M3", max_length=4.5).size == "M3-0.5-4"
    assert [s.length for s in standards.heat_set_inserts("M3")] == sorted(
        s.length for s in standards.heat_set_inserts("M3"))


def test_the_heat_set_hole_is_the_makers_hole_compensated_for_the_printer():
    r = resolver()
    _, card = bossed(r, "M3", "heat_set")
    spec = card.insert
    assert card.grip.target_source == "datasheet"
    assert card.grip.target == pytest.approx((spec.hole_diameter - spec.outer_diameter) / 2)
    # Printed, the hole comes out at the maker's size: drawn = maker's + this printer's pull-in.
    pull_in = r.calibration.hole_offset.value + r.calibration.xy_shrink.value * spec.outer_diameter / 2
    assert 2 * card.hole_radius == pytest.approx(spec.hole_diameter + 2 * pull_in)
    assert card.hole_depth > spec.length and card.boss_diameter == pytest.approx(4.6 + 2 * 1.6)
    self_tap = bossed(resolver())[1]
    assert 2 * self_tap.hole_radius == pytest.approx(0.8 * 3.0 + 2 * (0.08 + 0.003 * 1.5))


def test_the_holes_mouth_is_bevelled_so_the_peg_finds_its_way_in():
    """A press fit aims for a few hundredths of interference. The hole's mouth is where
    the peg enters, and when that face lies on the print bed the squashed first layers
    narrow it; a 45 degree bevel of at most 0.4 mm keeps it open. The fit itself is the
    straight part below the bevel and is measured as before."""
    from hotend_fit.cards.peg_hole import MAX_MOUTH_BEVEL_MM

    r = resolver()
    solids, card = pegged(r, "press", apart=20.0, through=True)
    assert card.mouth_bevel == pytest.approx(MAX_MOUTH_BEVEL_MM)
    assert card.mouth_bevel < card.tip_chamfer + 1e-9  # the peg's tip still leads
    base = solids["base"]

    def radius_at(depth: float) -> float:
        section = base.intersect(Pos(0, 0, -depth) * Box(40, 40, 1e-3))
        solid = sum(s.volume for s in section.solids())
        return ((900.0 * 1e-3 - solid) / 1e-3 / 3.141592653589793) ** 0.5

    assert radius_at(0.0005) == pytest.approx(card.hole_radius + card.mouth_bevel, abs=0.002)
    assert radius_at(card.mouth_bevel / 2) == pytest.approx(
        card.hole_radius + card.mouth_bevel / 2, abs=0.002)
    assert radius_at(card.mouth_bevel + 0.2) == pytest.approx(card.hole_radius, abs=0.001)
    assert radius_at(card.length - 0.2) == pytest.approx(card.hole_radius, abs=0.001)
    # A thin peg gets a bevel in proportion, never one that eats the hole.
    small = cards.peg_hole(diameter=2.0, length=3.0, fit="slide", ctx=resolver())
    assert small.mouth_bevel == pytest.approx(0.16)
    # The joint still verifies: the clearance is measured on the straight part.
    assert {m.ok for m in measure_pairs(solids, r.declared_fits, r)} == {True}
