"""Bought devices as components: the data validates, every number has provenance,
tolerance and a source, the body and the features build, and unverified numbers become
questions. Also the material facts the reference fits depend on."""

from __future__ import annotations

import copy
import json
import re

import numpy as np
import pytest
from fit_helpers import context

from hotend_fit import fit
from hotend_fit.components import (
    CONFIDENCES,
    DATA_DIR,
    PROVENANCES,
    SHIPPED,
    Component,
    ComponentError,
    catalogue_entry,
    list_components,
    load_component,
    use_component,
)
from hotend_fit.materials import (
    FLEXIBLE_FAMILIES,
    is_flexible,
    list_materials,
    material_facts,
)

TPU = {"id": "tpu-95a", "family": "TPU"}
QUESTION_KEYS = {"requirement", "text", "guessed_params"}
REQUIREMENT = re.compile(r"[a-z][a-z0-9_]{0,31}\Z")


def raw_phone() -> dict:
    return json.loads((DATA_DIR / "sample_phone.json").read_text(encoding="utf-8"))


def test_what_ships_is_listed_and_a_local_file_beside_it_is_marked():
    """A person may put a component file of their own into the folder: it is listed, loads
    and is called local. Only the invented sample ships."""
    assert SHIPPED == ("sample_phone",)
    assert set(SHIPPED) <= set(list_components())
    assert catalogue_entry(load_component("sample_phone"))["local"] is False


def test_a_local_component_file_is_listed_loaded_and_called_local(tmp_path, monkeypatch):
    from hotend_fit import components
    from hotend_fit.components import catalogue

    raw = raw_phone()
    (tmp_path / "sample_phone.json").write_text(json.dumps(raw), encoding="utf-8")
    raw["id"], raw["name"] = "my_phone", "My phone"
    (tmp_path / "my_phone.json").write_text(json.dumps(raw), encoding="utf-8")
    monkeypatch.setattr(components, "DATA_DIR", tmp_path)
    assert list_components() == ["my_phone", "sample_phone"]
    ctx = fit.resolver_for(context(material=TPU))
    assert use_component(ctx, "my_phone").reference.size == pytest.approx((80.0, 130.0, 10.0))
    entry = catalogue_entry(load_component("my_phone"))
    assert entry["local"] is True
    assert "- my_phone: My phone, phone (a file of this installation, not shipped" in (
        catalogue._describe("my_phone"))
    assert "not shipped" not in catalogue._describe("sample_phone")
    assert "my_phone, sample_phone (2)" in catalogue._intro()


@pytest.mark.parametrize("component_id", list_components())
def test_every_component_validates_and_every_number_is_sourced(component_id):
    component = load_component(component_id)
    assert component.id == component_id
    numbers = component.numbers()
    assert len(numbers) > 20
    for datum in numbers:
        assert datum.url.startswith("https://"), datum.path
        assert datum.tolerance >= 0, datum.path
        assert datum.provenance in PROVENANCES and datum.confidence in CONFIDENCES, datum.path
        # A number read off an estimate is never passed off as the maker's.
        if component.sources[datum.source]["kind"] == "estimate":
            assert datum.provenance == "guessed" and not datum.proven, datum.path
    for feature in component.features.values():
        if not feature.proven:
            assert feature.question.strip(), feature.id
    assert all(re.fullmatch(r"\d{4}-\d{2}-\d{2}", s["retrieved"]) for s in component.sources.values())


def test_the_sample_phone_is_shipped_and_says_that_it_is_invented():
    assert "sample_phone" in list_components()
    phone = load_component("sample_phone")
    assert "invented" in phone.name and "not a real product" in phone.frame
    assert all("nvented" in source["title"] for source in phone.sources.values())
    # Nobody measured it: nothing claims to be a measurement.
    assert {datum.provenance for datum in phone.numbers()} == {"standard", "guessed"}
    assert phone.size == (80.0, 130.0, 10.0)
    assert [phone.envelope[n].tolerance for n in ("width", "height", "depth")] == [0.25, 0.40, 0.10]
    assert all(phone.envelope[n].proven for n in ("width", "height", "depth", "corner_profile"))
    assert phone.envelope["width"].url == "https://example.com/hotend/sample-phone"
    assert phone.provenance == "standard"
    assert phone.rule("max_back_thickness") == 2.4
    assert phone.rule("min_glass_standoff") == 0.7
    kinds = {feature.kind for feature in phone.features.values()}
    assert {"button", "port", "camera", "magnet"} <= kinds
    side = phone.features["side_button"]
    assert side.face == "+x" and side.center == (40.0, 10.0, 5.0) and side.size == (0.6, 14.0, 3.0)
    assert phone.features["usb_c"].face == "-y"
    assert phone.features["magnet_ring"].value("ring_outer_diameter") == 52.0
    # The lead-in that plays a size scaled off a drawing is tagged as a guess and unused.
    assert phone.envelope["corner_lead_in"].provenance == "guessed"
    with pytest.raises(ComponentError, match="has no feature"):
        phone.feature("headphone_jack")
    with pytest.raises(ComponentError, match="no component"):
        load_component("phone_unknown")
    with pytest.raises(ComponentError, match="not a component id"):
        load_component("../sample_phone")


def test_the_body_outline_passes_through_the_stated_corner_points():
    phone = load_component("sample_phone")
    outline = np.array(phone.outline())
    width, height, _ = phone.size
    assert outline.min(axis=0) == pytest.approx((-width / 2, -height / 2))
    assert outline.max(axis=0) == pytest.approx((width / 2, height / 2))
    start, end = outline, np.roll(outline, -1, axis=0)
    edge = end - start
    for u, v in phone.envelope["corner_profile"].value:
        for sx, sy in ((1, 1), (-1, 1), (1, -1), (-1, -1)):
            point = np.array([sx * (width / 2 - u), sy * (height / 2 - v)])
            along = np.clip(((point - start) * edge).sum(axis=1) / (edge * edge).sum(axis=1), 0, 1)
            distance = np.linalg.norm(point - (start + along[:, None] * edge), axis=1).min()
            assert distance <= 0.05, (u, v, distance)
    # Not a rectangle and not a circular arc of the corner's extent: the area lies between.
    area = 0.5 * abs(float(np.dot(outline[:, 0], end[:, 1]) - np.dot(end[:, 0], outline[:, 1])))
    assert width * height - 4 * (1 - np.pi / 4) * 12.0 ** 2 < area < width * height


def test_the_body_is_a_reference_and_every_feature_builds():
    ctx = fit.resolver_for(context(material=TPU))
    phone = use_component(ctx, "sample_phone")
    assert use_component(ctx, "sample_phone") is phone
    body = phone.reference
    assert body.size == pytest.approx(phone.size) and body.closed
    assert body.provenance == "standard" and body.kind == "rigid"
    assert ctx.references["sample_phone"] is body
    pocket = body.pocket("stretch", ctx=ctx)
    assert pocket.envelope.is_valid
    assert pocket.envelope_tolerance == pytest.approx(0.05)
    for feature in phone.features.values():
        room = feature.keep_out(reach=6.0)
        assert room.is_valid and room.volume > 0, feature.id
        opening = feature.opening(ctx, "loose", reach=4.0)
        assert opening.is_valid and opening.volume > 0, feature.id
    # Proud features have a solid of their own; patches on the surface have none.
    assert phone.features["side_button"].solid.volume == pytest.approx(0.6 * 14.0 * 3.0, rel=1e-6)
    assert phone.features["camera_plateau"].solid.bounding_box().min.Z == pytest.approx(-2.5)
    assert phone.features["usb_c"].solid is None
    usb = phone.features["usb_c"].keep_out(reach=20.0, inward=0.0).bounding_box()
    assert (usb.size.X, usb.size.Z) == pytest.approx((13.0, 10.6), abs=0.02)
    assert (usb.min.Y, usb.max.Y) == pytest.approx((-65.0 - 16.0, -65.0))
    # An opening is the feature's outline plus the resolver's clearance, on every side.
    gap = fit.resolver_for(context(material=TPU)).clearance("loose", 14.0, printed_peg=False).gap
    box = phone.features["side_button"].opening(ctx, "loose", reach=3.0, inward=0.0).bounding_box()
    assert box.size.Y == pytest.approx(14.0 + 2 * gap, abs=0.01)
    assert box.size.Z == pytest.approx(3.0 + 2 * gap, abs=0.01)
    assert (box.min.X, box.max.X) == pytest.approx((40.0, 40.0 + 0.6 + 3.0))
    # Those clearances are accounted for: nothing is "taken without a card".
    assert ctx.declarations()["loose"] == []
    with pytest.raises(ComponentError, match="fit must be one of"):
        phone.features["usb_c"].opening(ctx, "stretch")


def test_unverified_numbers_become_questions_only_for_what_the_model_relies_on():
    ctx = fit.resolver_for(context(material=TPU))
    phone = use_component(ctx, "sample_phone")
    # The body alone is stated, not estimated: nothing to ask.
    assert phone.questions() == [] and phone.report()["proven"] is True
    phone.feature("side_button")
    assert phone.questions() == [] and phone.unverified() == []
    phone.feature("camera_plateau")
    phone.feature("volume_up")
    phone.feature("volume_down")
    asked = phone.questions()
    # The two volume buttons share one question.
    assert [q["requirement"] for q in asked] == ["sample_phone_volume_up", "sample_phone_camera_plateau"]
    for question in asked:
        assert set(question) == QUESTION_KEYS and REQUIREMENT.match(question["requirement"])
        assert question["text"].startswith("Sample phone (invented): ")
    report = phone.report()
    assert report["proven"] is False
    paths = {entry["path"] for entry in report["unverified"]}
    assert "features.camera_plateau.body.size.x" in paths
    assert all(entry["source"].startswith("https://") for entry in report["unverified"])
    assert ctx.declarations()["components"] == [
        {"id": "sample_phone", "features": ["camera_plateau", "side_button", "volume_down", "volume_up"]}]


def _broken(change) -> dict:
    raw = copy.deepcopy(raw_phone())
    change(raw)
    return raw


def _feature(raw: dict, feature_id: str) -> dict:
    return next(f for f in raw["features"] if f["id"] == feature_id)


@pytest.mark.parametrize(("change", "message"), [
    (lambda r: r["defaults"].pop("source"), "no source is given"),
    (lambda r: r["envelope"]["width"].pop("tolerance"), "no tolerance is given"),
    (lambda r: r["envelope"]["corner_lead_in"].update(provenance="standard"),
     "an estimate cannot back a standard number"),
    (lambda r: r["envelope"]["width"].update(source="a_forum_post"), "is not listed in sources"),
    (lambda r: _feature(r, "usb_c").pop("question"), "needs a question"),
    (lambda r: _feature(r, "touch_strip")["keep_out"][0]["size"].__setitem__(1, None),
     "only the size along the face's normal may be null"),
    (lambda r: r["sources"]["sample"].update(url="http://example.com"), "an https url"),
    (lambda r: r.update(id="phone_other"), "must be the file's name"),
    (lambda r: r["features"].append(copy.deepcopy(r["features"][0])), "used twice"),
    (lambda r: _feature(r, "side_button")["body"].update(shape="blob"), "shape must be one of"),
    (lambda r: r["envelope"]["depth"].update(value="10.0"), "not a number"),
    (lambda r: r.update(extra=1), "exactly the keys"),
])
def test_component_data_that_breaks_the_schema_is_refused(change, message):
    with pytest.raises(ComponentError, match=message):
        Component(_broken(change), "sample_phone")


def test_flexible_materials_are_known_and_everything_about_them_is_a_guess():
    tpu = material_facts(TPU)
    assert tpu.flexible and tpu.shore == "95A" and tpu.nozzle == "direct_drive"
    assert tpu.wall_mm == (1.2, 2.5) and tpu.max_overhang_deg == 45.0
    assert tpu.stretch_strain == pytest.approx(0.08) and tpu.provenance == "guessed"
    assert all(url.startswith("https://") for url in tpu.sources)
    assert material_facts({"id": "tpu-85a"}).flexible  # recognised by its id alone
    assert FLEXIBLE_FAMILIES == ("TPU",)
    for rigid in ({"id": "pla-basic", "family": "PLA"}, {"id": "petg-hf", "family": "PETG"},
                  {"id": "mystery", "family": "PA-CF"}):
        facts = material_facts(rigid)
        assert not facts.flexible and facts.stretch_strain is None and facts.nozzle == "any"
        assert not is_flexible(rigid)
    assert {entry["family"] for entry in list_materials()} >= {"PLA", "PETG", "TPU"}
    assert fit.resolver_for(context(material=TPU)).material.flexible
