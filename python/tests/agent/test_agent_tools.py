"""The tool proxies, the history and the main loop, in process."""

from __future__ import annotations

import asyncio
import base64
import json
from typing import Any

import pytest
from agent_fake_core import TOOL_SPECS, finish, in_process, run_params
from pydantic_ai import BinaryContent
from pydantic_ai.messages import (
    ModelMessagesTypeAdapter,
    ModelRequest,
    ToolReturnPart,
    UserPromptPart,
)

from hotend_agent import main as agent_main
from hotend_agent.runner import Session, dump_history
from hotend_agent.tools import ToolProxy, summary_line, to_model_content

pytestmark = pytest.mark.timeout(60)

PNG = bytes.fromhex("89504e470d0a1a0a") + b"\x00" * 64
PNG_B64 = base64.b64encode(PNG).decode()


def image(path: str = "views/iso.png", **changes: Any) -> dict[str, Any]:
    return {"type": "image", "path": path, "media_type": "image/png", "data_b64": PNG_B64,
            **changes}


def test_text_and_images_reach_the_model_in_the_cores_order() -> None:
    ok, content, images = to_model_content({"ok": True, "content": [
        {"type": "text", "text": "views of version 3: iso, top"},
        image("views/iso.png"),
        {"type": "text", "text": "second"}, {"type": "text", "text": "third"},
        image("views/top.png"),
    ]})
    assert ok and images == 2
    assert content[0] == "views of version 3: iso, top"
    assert content[2] == "second\nthird"
    for item, name in ((content[1], "views/iso.png"), (content[3], "views/top.png")):
        assert isinstance(item, BinaryContent)
        assert (item.data, item.media_type, item.identifier) == (PNG, "image/png", name)


def test_a_single_text_stays_a_plain_string() -> None:
    assert to_model_content({"ok": True, "content": [{"type": "text", "text": "status: ok"}]}) == (
        True, "status: ok", 0)
    assert to_model_content({"ok": True, "content": []})[1] == "ok (the tool returned no content)"


def test_a_refusal_becomes_a_text_the_model_can_act_on() -> None:
    ok, content, images = to_model_content({"ok": False, "error": {
        "code": "invalid_arguments", "message": "/views/0: 'side' is not one of the views"}})
    assert (ok, images) == (False, 0)
    assert content == ("error (invalid_arguments): /views/0: 'side' is not one of the views\n"
                       "Hotend refused this call; nothing was changed.")


@pytest.mark.parametrize("outcome", [
    None, "text", [], {}, {"ok": "yes"}, {"ok": False}, {"ok": False, "error": "broken"},
    {"ok": True}, {"ok": True, "content": "text"}, {"ok": True, "content": [None, 3, {}]},
])
def test_a_malformed_outcome_never_raises(outcome: Any) -> None:
    ok, content, images = to_model_content(outcome)
    assert isinstance(content, str) and content and images == 0
    assert ok is (isinstance(outcome, dict) and outcome.get("ok") is True)


@pytest.mark.parametrize("broken", [
    image(data_b64="not base64!"), image(data_b64=""), image(media_type="application/pdf"),
    image(media_type=None), {"type": "image"},
])
def test_an_unreadable_image_is_named_instead_of_passed_on(broken: dict[str, Any]) -> None:
    ok, content, images = to_model_content({"ok": True, "content": [
        {"type": "text", "text": "views"}, broken]})
    assert ok and images == 0
    assert all(isinstance(item, str) for item in content)
    assert "could not be read" in content[-1]


def test_the_proxies_take_the_cores_tool_list_unchanged() -> None:
    proxy = ToolProxy(channel=None, run_id="run-1", emit=lambda event: None)  # type: ignore[arg-type]
    tools = proxy.tools(TOOL_SPECS)
    assert [tool.name for tool in tools] == [spec["name"] for spec in TOOL_SPECS]
    for tool, spec in zip(tools, TOOL_SPECS, strict=True):
        definition = tool.tool_def
        assert definition.description == spec["description"]
        assert definition.parameters_json_schema == spec["input_schema"]
        assert definition.sequential is True, "tools change the current version: one at a time"


def test_the_registry_is_the_fourteen_tools_of_this_milestone() -> None:
    """The fixture is written from the core's registry (its test `real_agent` fails when the
    two differ). The proxies need no knowledge of a tool: a new one only has to be listed."""
    assert [spec["name"] for spec in TOOL_SPECS] == [
        "write_model_code", "set_parameter", "sweep_parameters", "run_checks", "render_views",
        "measure", "read_docs", "list_cards", "list_components", "list_print_profiles",
        "print_plan", "slice", "propose_print", "snapshot_model"]
    for spec in TOOL_SPECS:
        assert set(spec) == {"name", "description", "input_schema"}
        assert spec["input_schema"]["type"] == "object"
        assert spec["input_schema"]["additionalProperties"] is False
    described = {spec["name"]: spec["description"] for spec in TOOL_SPECS}
    assert "cannot start a print" in described["propose_print"]
    # The snapshot tool names the current version and nothing else; it has no path.
    assert "changes no version" in described["snapshot_model"]
    snapshot = next(spec for spec in TOOL_SPECS if spec["name"] == "snapshot_model")
    assert set(snapshot["input_schema"]["properties"]) == {"name", "note"}
    assert not [name for name in described if "confirm" in name or "start" in name]
    # The component list takes nothing and changes nothing.
    assert described["list_components"].endswith("Read-only.")
    components = next(spec for spec in TOOL_SPECS if spec["name"] == "list_components")
    assert components["input_schema"]["properties"] == {}


def test_the_new_result_texts_reach_the_model_whole() -> None:
    """A build result with its questions, a plate summary and a print proposal are plain
    texts of the gateway: nothing is cut or reworded on the way to the model, and the event
    for the interface carries the status line."""
    from agent_demo_script import recorded

    seen = set()
    for result in recorded():
        outcome = {"ok": True, "content": [{"type": "text", "text": result["text"]}]}
        assert to_model_content(outcome) == (True, result["text"], 0)
        seen.add((result["tool"], summary_line(result["text"])))
    assert ("slice", "status: ok (version 5, profile demo-0.20mm, plate 1 of 1)") in seen
    assert ("write_model_code", "status: check_error") in seen
    assert any(tool == "measure" and summary.startswith('1. {"distance":4.5,')
               for tool, summary in seen)
    proposed = next(summary for tool, summary in seen if tool == "propose_print")
    assert proposed.startswith("pending print job ") and proposed.endswith("for version 5.")
    assert max(len(result["text"]) for result in recorded()) > 4000, "long texts too"


def test_the_stored_history_keeps_images_as_they_were_sent() -> None:
    """Pictures stay in the stored conversation, so the next message only appends to what
    the provider cached (the oldest of a long conversation go a block at a time:
    test_agent_cache.py)."""
    messages = [
        ModelRequest(parts=[UserPromptPart("a box")]),
        ModelRequest(parts=[
            ToolReturnPart("render_views", ["views of version 1: iso",
                                            BinaryContent(PNG, media_type="image/png",
                                                          identifier="views/iso.png")], "c1"),
            ToolReturnPart("measure", "1. {\"kind\": \"volume\"}", "c2"),
        ]),
    ]
    stored = dump_history(messages)
    text = json.dumps(stored)
    assert PNG_B64 in text
    assert "shown to the model when it was made" not in text
    assert "views of version 1: iso" in text and '\\"kind\\": \\"volume\\"' in text
    # The stored form reads back into the same objects, and stores again the same.
    back = ModelMessagesTypeAdapter.validate_python(stored)
    picture = back[1].parts[0].content[1]
    assert isinstance(picture, BinaryContent)
    assert (picture.data, picture.media_type, picture.identifier) == (
        PNG, "image/png", "views/iso.png")
    assert dump_history(back) == stored


class CountingChannel:
    """Stands in for the core's gateway as far as counting goes: the model's own
    `render_views` calls have a cap of 20 per run, and a refusal counts towards the run's
    20; the look review's calls (`auto: true`) count against neither."""

    def __init__(self) -> None:
        self.params: list[dict[str, Any]] = []
        self.own_views = 0
        self.refusals = 0

    async def request(self, method: str, params: dict[str, Any]) -> Any:
        assert method == "tool.call"
        self.params.append(params)
        name = params["name"]
        if name == "render_views":
            if not params.get("auto"):
                self.own_views += 1
                if self.own_views > 20:
                    self.refusals += 1
                    return {"ok": False, "error": {"code": "rate_limited", "message": "cap"}}
            return {"ok": True, "content": [{"type": "text", "text": "views of version 1"},
                                            image("views/iso.png"), image("views/front.png"),
                                            image("views/top.png")]}
        return {"ok": True, "content": [{"type": "text", "text": "status: ok\nversion 2"}]}


def test_every_build_of_a_long_run_gets_the_views_without_using_the_models_allowance() -> None:
    """More builds than the model's 20 `render_views` (both build tools): every one is
    followed by the standard views, each asked for with `auto: true`, and nothing of the
    model's allowance or refusals is used up."""
    async def scenario() -> None:
        channel = CountingChannel()
        proxy = ToolProxy(channel, "run-1", lambda event: None, look=True)  # type: ignore[arg-type]
        proxy.tools(TOOL_SPECS)
        for index in range(30):
            if index % 2:
                result = await proxy.call("set_parameter", {"name": "width", "value": index},
                                          f"call-{index}")
            else:
                result = await proxy.call("write_model_code", {"code": "x"}, f"call-{index}")
            assert isinstance(result, list), result
            assert sum(isinstance(part, BinaryContent) for part in result) == 3, index
        assert proxy.views_shown == 30
        views = [p for p in channel.params if p["name"] == "render_views"]
        assert len(views) == 30 and all(p.get("auto") is True for p in views)
        assert channel.own_views == 0 and channel.refusals == 0
        # A model call is never marked.
        await proxy.call("render_views", {"views": ["top"]}, "own")
        assert "auto" not in channel.params[-1]

    asyncio.run(scenario())


def test_a_tool_request_the_core_rejects_is_told_to_the_model() -> None:
    async def scenario() -> None:
        async with in_process() as (_, channel):
            events: list[dict[str, Any]] = []
            proxy = ToolProxy(channel, "not-the-run", events.append)
            content = await proxy.call("measure", {"queries": []}, "call-1")
            assert content == "error: the tool call failed: invalid params"
            assert [e["type"] for e in events] == ["tool_start", "tool_end"]
            assert events[1]["ok"] is False and events[1]["tool_call_id"] == "call-1"

    asyncio.run(scenario())


def test_a_run_that_ignores_its_cancel_is_answered_for(monkeypatch: pytest.MonkeyPatch) -> None:
    exits: list[int] = []

    async def stubborn(self: Session) -> dict[str, Any]:
        while True:  # swallows every cancellation, like a run stuck in foreign code
            try:
                await asyncio.sleep(3600)
            except asyncio.CancelledError:
                if exits:
                    raise

    monkeypatch.setattr(Session, "run", stubborn)
    monkeypatch.setattr(agent_main, "CANCEL_GRACE", 0.3)
    monkeypatch.setattr(agent_main, "_hard_exit", exits.append)

    async def scenario() -> None:
        async with in_process() as (core, channel):
            earlier = [{"kind": "request", "parts": []}]
            serving = asyncio.create_task(agent_main._serve(channel, None))
            core.send({"jsonrpc": "2.0", "id": 1, "method": "agent.run",
                       "params": run_params(core, "hello", history=earlier)})
            await asyncio.sleep(0.2)
            core.cancel()
            code, stuck = await asyncio.wait_for(serving, 5)
            assert (code, stuck) == (0, True)
            assert await asyncio.to_thread(core.result_ready.wait, 5)
            assert core.responses[1]["result"] == {
                "history": earlier, "stop_reason": "cancelled", "usage": {}}
            assert core.responses[2]["result"] == {}
            exits.append(0)  # lets the stub end when the loop closes

    asyncio.run(scenario())


def test_a_run_that_fails_internally_is_answered_without_its_text(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    async def broken(self: Session) -> dict[str, Any]:
        raise RuntimeError("the user wrote: a private design brief")

    monkeypatch.setattr(Session, "run", broken)

    async def scenario() -> None:
        async with in_process() as (core, channel):
            serving = asyncio.create_task(agent_main._serve(channel, None))
            core.send({"jsonrpc": "2.0", "id": 1, "method": "agent.run",
                       "params": run_params(core, "hello")})
            assert await asyncio.wait_for(serving, 5) == (1, False)
            assert await asyncio.to_thread(core.result_ready.wait, 5)
            error = core.responses[1]["error"]
            assert error["code"] == -32603
            assert error["message"] == "the agent failed internally (RuntimeError)"
            assert core.violations == []

    with caplog.at_level("INFO"):
        asyncio.run(scenario())
    assert "the run failed with RuntimeError" in caplog.text
    assert "private design brief" not in caplog.text


def test_finish_helper_orders_the_result_after_the_events() -> None:
    async def scenario() -> None:
        async with in_process() as (core, channel):
            for number in range(200):
                channel.notify("agent.event", {"run_id": core.run_id, "event": {
                    "type": "text_delta", "text": str(number)}})
            await finish(core, channel, {"history": [], "stop_reason": "done", "usage": {}})
            assert core.text() == "".join(str(n) for n in range(200))
            assert core.violations == []

    asyncio.run(scenario())
