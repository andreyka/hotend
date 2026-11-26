"""Request validation (docs/PROTOCOL.md section 3.2)."""

from __future__ import annotations

import pytest
from conftest import X2D_CONTEXT, build_request
from pydantic import ValidationError

from hotend_engine.context import MissingFit, make_context, resolve_inside
from hotend_engine.errors import BadRequest
from hotend_engine.request import (
    BuildRequest,
    CalibrateRequest,
    GaugeRequest,
    PrepareRequest,
    ViewsRequest,
    parse_request,
)


def test_protocol_example_parses():
    request = parse_request(build_request(params={"width": 90.0}))
    assert isinstance(request, BuildRequest)
    main = request.context.printer.main_nozzle
    assert main.id == 1 and main.role == "main" and main.diameter == 0.4
    assert main.reach.size == (256.0, 256.0, 260.0)
    assert request.context.printer.nozzles[1].reach.size == (235.5, 256.0, 256.0)
    assert request.checks.min_wall == 0.8
    assert request.limits.max_triangles == 2_000_000


def test_only_op_is_required():
    request = parse_request({"op": "build"})
    assert request.model_path == "model.py"
    assert request.outputs.step and request.outputs.brep and request.outputs.mesh
    assert request.outputs.views == () and request.checks.enabled
    assert request.context.printer is None


def test_context_and_limits_keep_unknown_fields_but_the_rest_is_strict():
    context = {**X2D_CONTEXT, "bed": {"texture": "pei"}}
    context["printer"] = {**context["printer"], "name": "Workshop X2D"}
    request = parse_request(build_request(context=context, limits={"memory_mb": 2048}))
    assert request.context.printer.name == "Workshop X2D"
    assert request.context.bed == {"texture": "pei"}
    for bad in (
        build_request(extra_field=1),
        build_request(outputs={"gltf": True}),
        build_request(checks={"min_wal": 1.0}),
    ):
        with pytest.raises(ValidationError):
            parse_request(bad)


@pytest.mark.parametrize(
    "bad",
    [
        {"op": "build", "protocol": 7},
        {"op": "views", "views": []},
        {"op": "views", "views": ["upside-down"]},
        {"op": "views", "views": ["iso"], "section": {"plane": "AB"}},
        {"op": "export", "formats": ["obj"]},
        {"op": "measure", "queries": []},
        {"op": "docs", "query": ""},
        {"op": "docs", "query": "x", "limit": 0},
        {"op": "build", "checks": {"min_wall": 0}},
        {"op": "build", "context": {"printer": {"id": "p", "nozzles": [
            {"id": 1, "diameter": 0.4, "reach": {"x": [10, 5], "y": [0, 1], "z": [0, 1]}}]}}},
        {"op": "build", "context": {"printer": {"id": "p", "nozzles": [{"id": 1, "diameter": 0}]}}},
    ],
)
def test_malformed_requests_are_rejected(bad):
    with pytest.raises(ValueError, match=r".+"):
        parse_request(bad)


def test_view_size_limits():
    assert isinstance(parse_request({"op": "views", "views": ["iso"], "size": [64, 2048]}),
                      ViewsRequest)
    for size in ([32, 100], [4096, 100]):
        with pytest.raises(ValueError, match="view size"):
            parse_request({"op": "views", "views": ["iso"], "size": size})
        with pytest.raises(ValueError, match="view size"):
            parse_request({"op": "build", "outputs": {"view_size": size}})


def test_paths_must_stay_inside_the_job_folder(tmp_path):
    assert resolve_inside(tmp_path, "sub/model.py", "model_path") == tmp_path / "sub/model.py"
    for bad in ("../x.py", "sub/../../x.py", str(tmp_path / "model.py")):
        with pytest.raises(BadRequest, match="model_path"):
            resolve_inside(tmp_path, bad, "model_path")
    (tmp_path / "link").symlink_to(tmp_path.parent)
    with pytest.raises(BadRequest, match="outside the job folder"):
        resolve_inside(tmp_path, "link/x.py", "model_path")


def test_ctx_fit_fails_clearly_without_hotend_fit(tmp_path, monkeypatch):
    monkeypatch.setattr("hotend_engine.context.importlib.util.find_spec", lambda name: None)
    ctx = make_context(parse_request(build_request()).context, tmp_path)
    assert isinstance(ctx.fit, MissingFit)
    assert ctx.declared_fits == []
    assert ctx.printer.id == "bambu-x2d" and ctx.material.family == "PLA"
    with pytest.raises(RuntimeError, match=r"ctx\.fit\.clearance needs the hotend_fit package"):
        ctx.fit.clearance("slide", 5.0)


def test_ctx_fit_uses_the_hotend_fit_resolver_when_present(tmp_path, monkeypatch):
    import importlib.machinery
    import sys
    import types

    seen = {}

    class Resolver:
        def __init__(self) -> None:
            self.declared_fits = []

        def clearance(self, fit_class, nominal):
            return 0.2

    def resolver_for(context):
        seen.update(context)
        return Resolver()

    package = types.ModuleType("hotend_fit")
    package.__spec__ = importlib.machinery.ModuleSpec("hotend_fit", None, is_package=True)
    package.__path__ = []
    fit = types.ModuleType("hotend_fit.fit")
    fit.resolver_for = resolver_for
    monkeypatch.setitem(sys.modules, "hotend_fit", package)
    monkeypatch.setitem(sys.modules, "hotend_fit.fit", fit)

    (tmp_path / "calibration.json").write_text("{}", encoding="utf-8")
    request = parse_request(build_request(
        context={**X2D_CONTEXT, "calibration_path": "calibration.json"}))
    ctx = make_context(request.context, tmp_path)
    assert ctx.fit.clearance("slide", 5.0) == 0.2
    # the resolver gets the request's context as a dict, with an absolute calibration path
    assert seen["calibration_path"] == str(tmp_path / "calibration.json")
    assert seen["printer"]["nozzles"][0]["reach"]["x"] == [0.0, 256.0]
    assert seen["material"]["family"] == "PLA"
    ctx.fit.declared_fits.append({"between": ["box", "lid"]})
    assert ctx.declared_fits == [{"between": ["box", "lid"]}]
    # a fresh resolver per build: declared fits never leak between builds
    assert make_context(request.context, tmp_path).declared_fits == []
    with pytest.raises(BadRequest, match="calibration_path"):
        make_context(parse_request(build_request(
            context={"calibration_path": "../calibration.json"})).context, tmp_path)


# --- milestone 2: the options of a build that are off unless asked, and the plate ops -----------


def test_the_new_check_options_are_off_unless_asked():
    plain = parse_request({"op": "build"}).checks
    assert plain.orientation == "as_modelled"
    assert plain.contract is False and plain.undeclared_joints is False
    asked = parse_request({"op": "build", "checks": {
        "orientation": "prepared", "contract": True, "undeclared_joints": True}}).checks
    assert asked.orientation == "prepared" and asked.contract and asked.undeclared_joints
    with pytest.raises(ValidationError):
        parse_request({"op": "build", "checks": {"orientation": "flat"}})


def test_the_plate_ops_parse_with_their_defaults():
    prepare = parse_request({"op": "prepare"})
    assert isinstance(prepare, PrepareRequest)
    assert prepare.parts is None and prepare.copies == {} and prepare.nozzles == {}
    assert (prepare.gap, prepare.margin, prepare.threemf) == (5.0, 2.0, True)
    assert prepare.views == ("iso", "top") and prepare.view_size == (768, 768)
    full = parse_request({
        "op": "prepare", "context": X2D_CONTEXT, "parts": ["lid"], "copies": {"lid": 12},
        "nozzles": {"lid": 0}, "orientations": {"lid": {"up": "-z"}}, "gap": 3, "margin": 0,
        "threemf": False, "views": ["top"], "view_size": [320, 240]})
    assert full.copies == {"lid": 12} and full.nozzles == {"lid": 0} and full.margin == 0

    gauge = parse_request({"op": "gauge"})
    assert isinstance(gauge, GaugeRequest) and gauge.cards is None and gauge.nozzle_id is None
    named = parse_request({"op": "gauge", "nozzle_id": 0, "cards": [
        {"card": "peg_hole", "args": {"diameter": 6.0, "length": 6.0, "fit": "slide"}}]})
    assert named.cards[0].card == "peg_hole" and named.cards[0].name is None

    calibrate = parse_request({"op": "calibrate", "measured_at": "2026-11-17T09:00:00Z",
                               "readings": [{"feature": "hole", "drawn": 6.4, "measured": 6.2}]})
    assert isinstance(calibrate, CalibrateRequest) and calibrate.nozzle_id is None
    assert calibrate.readings[0].feature == "hole"


@pytest.mark.parametrize(
    "bad",
    [
        {"op": "prepare", "copies": {"lid": 0}},
        {"op": "prepare", "copies": {"lid": 65}},
        {"op": "prepare", "gap": -1},
        {"op": "prepare", "views": ["sideways"]},
        {"op": "prepare", "view_size": [16, 16]},
        {"op": "prepare", "plate": 2},
        {"op": "gauge", "cards": []},
        {"op": "gauge", "cards": [{"card": ""}]},
        {"op": "gauge", "cards": [{"card": "peg_hole", "at": [0, 0, 0]}]},
        {"op": "calibrate", "readings": [{"feature": "hole", "drawn": 6.4, "measured": 6.2}]},
        {"op": "calibrate", "measured_at": "2026-11-17T09:00:00Z", "readings": []},
        {"op": "calibrate", "measured_at": "2026-11-17T09:00:00Z",
         "readings": [{"feature": "slot", "drawn": 6.4, "measured": 6.2}]},
        {"op": "calibrate", "measured_at": "2026-11-17T09:00:00Z",
         "readings": [{"feature": "hole", "drawn": 6.4, "measured": 0}]},
    ],
)
def test_malformed_plate_requests_are_rejected(bad):
    with pytest.raises(ValueError, match=r".+"):
        parse_request(bad)
