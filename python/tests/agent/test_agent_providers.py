"""The five provider SDKs through the core transport, in process, with canned model answers.

For each provider the real Pydantic AI model class and the real provider SDK run a whole
tool loop. Every request they build is checked against a mirror of the core's egress table
(agent_canned.py), so these tests fail when an SDK update would make the core refuse the
agent's requests. No request leaves the process.
"""

from __future__ import annotations

import asyncio
import base64
import json
import threading
import warnings
from typing import Any

import pytest
from agent_canned import (
    CREDENTIAL_HEADERS,
    PROVIDERS,
    check_egress,
    conversation_length,
    error_reply,
    reply,
    surviving_headers,
)
from agent_fake_core import (
    FakeCore,
    LlmReply,
    finish,
    in_process,
    run_params,
    text_outcome,
)

from hotend_agent.prompt import SYSTEM_PROMPT
from hotend_agent.providers import DEFAULT_MAX_OUTPUT_TOKENS, PLACEHOLDER_KEY
from hotend_agent.runner import BadRunParams, Session

pytestmark = pytest.mark.timeout(120)

PNG = bytes.fromhex("89504e470d0a1a0a") + b"\x00" * 300
PNG_B64 = base64.b64encode(PNG).decode()
CODE = "from build123d import *\nPARAMS = {}\ndef build(p, ctx):\n    return Box(10, 10, 10)\n"


def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
    if name == "render_views":
        return {"ok": True, "content": [
            {"type": "text", "text": "views of version 1: iso"},
            {"type": "image", "path": "views/iso.png", "media_type": "image/png",
             "data_b64": PNG_B64},
        ]}
    return text_outcome("status: ok\nversion: 1 created and now current\nchecks: 0 fail")


async def run(core: FakeCore, channel: Any, text: str, **overrides: Any) -> dict[str, Any]:
    session = Session(channel, run_params(core, text, **overrides))
    return await finish(core, channel, await session.run())


def test_the_egress_rules_come_from_the_shared_fixture_and_cover_every_provider(tmp_path):
    """The rules these tests check requests against are data in fixtures/provider_table.json
    (the core's table, for a sync test on the core's side), not a copy kept in Python. Every
    provider kind the agent can be pointed at has a row, and a row the loader does not fully
    understand is an error, not a silently weaker check."""
    import json

    from agent_canned import PROVIDER_TABLE, load_provider_table

    from hotend_agent.providers import PROVIDER_KINDS

    table = json.loads(PROVIDER_TABLE.read_text(encoding="utf-8"))
    assert [row["name"] for row in table["providers"]] == list(PROVIDER_KINDS)
    assert list(PROVIDERS) == list(PROVIDER_KINDS)
    for row in table["providers"]:
        case = PROVIDERS[row["name"]]
        assert case.base_url == row["sdk_base_url"]
        assert case.pass_headers == frozenset(row["pass_headers"])
        assert [list(field) for field in case.token_fields] == row["token_fields"]
        assert case.require_token_limit is row["require_token_limit"]
        assert len(case.routes) == len(row["routes"])
    # A change in the fixture changes what is enforced here.
    changed = json.loads(json.dumps(table))
    changed["providers"][0]["routes"] = changed["providers"][0]["routes"][1:]
    path = tmp_path / "table.json"
    path.write_text(json.dumps(changed), encoding="utf-8")
    _, cases = load_provider_table(path)
    request = {"method": "POST", "path": "/v1/messages", "headers": {},
               "body": json.dumps({"max_tokens": 10})}
    assert check_egress(PROVIDERS["anthropic"], request) is None
    assert "not an allowed anthropic API call" in check_egress(cases["anthropic"], request)
    # Fields the loader does not know are refused, so the fixture cannot grow a rule that
    # these tests then ignore.
    for tamper in (lambda t: t["providers"][0].update(denied_fields=["x"]),
                   lambda t: t["providers"][0]["routes"][0].update(body="any"),
                   lambda t: t["providers"][2]["routes"][0].update(path={"regex": ".*"}),
                   lambda t: t.update(extra=1),
                   lambda t: t["providers"][1].update(sdk_base_url="https://evil.example/v1")):
        broken = json.loads(json.dumps(table))
        tamper(broken)
        path.write_text(json.dumps(broken), encoding="utf-8")
        with pytest.raises(ValueError, match=r"table\.json"):
            load_provider_table(path)


def test_the_fixture_matches_method_path_and_query_as_the_core_does():
    google, anthropic = PROVIDERS["google"], PROVIDERS["anthropic"]
    body = '{"generationConfig": {"maxOutputTokens": 10}}'
    ok = {"method": "POST", "headers": {}, "body": body,
          "path": "/v1beta/models/gemini-3-pro:streamGenerateContent?alt=sse"}
    assert check_egress(google, ok) is None
    for path in ("/v1beta/models/:generateContent", "/v1beta/models/a/b:generateContent",
                 "/v1beta/models/gemini:countTokens", "/v1beta/models/gemini:generateContent?x=1",
                 "/v1beta/tunedModels/gemini:generateContent"):
        assert check_egress(google, dict(ok, path=path)) is not None, path
    assert check_egress(google, dict(ok, method="GET")) == "method GET is not allowed"
    count = {"method": "POST", "path": "/v1/messages/count_tokens?beta=true", "headers": {},
             "body": '{"max_tokens": 5}'}
    assert check_egress(anthropic, count) is None


@pytest.mark.parametrize("kind", list(PROVIDERS))
def test_a_tool_loop_runs_through_the_provider_sdk_within_the_egress_rules(kind: str) -> None:
    case = PROVIDERS[kind]
    lengths: list[int] = []
    refused: list[str] = []

    def llm(params: dict[str, Any]) -> LlmReply:
        problem = check_egress(case, params)
        if problem is not None:
            refused.append(problem)
            return LlmReply(refuse=("denied", problem))
        lengths.append(conversation_length(case, json.loads(params["body"])))
        turn = len(lengths)
        if turn == 1:
            return reply(case, "I will write it.",
                         ("write_model_code", {"code": CODE, "note": "a cube"}))
        if turn == 2:
            return reply(case, None, ("render_views", {"views": ["iso"]}))
        return reply(case, "All done.")

    async def scenario() -> None:
        async with in_process(tools=tools, llm=llm) as (core, channel):
            # The model asks for its own view here; Hotend's look review (its own views and
            # the `looked at:` line) has tests of its own (test_agent_cache.py).
            result = await run(core, channel, "make a cube", provider=case.provider,
                               look=False)
            assert refused == []
            assert result["stop_reason"] == "done", result
            assert "error" not in result
            assert result["usage"] == {
                "requests": 3, "input_tokens": 300, "output_tokens": 120,
                "cache_read_tokens": 0, "cache_write_tokens": 0, "tool_calls": 2}

            # What the core's gateway was asked to do, in order, with the model's arguments.
            assert core.tool_calls == [
                ("write_model_code", {"code": CODE, "note": "a cube"}),
                ("render_views", {"views": ["iso"]}),
            ]
            # What the UI is told.
            assert core.text() == "I will write it.\n\nAll done."
            kinds = [e["type"] for e in core.events if e["type"] != "text_delta"]
            assert kinds == ["tool_start", "tool_end", "usage",
                             "tool_start", "tool_end", "usage", "usage"]
            start, end = core.events_of("tool_start")[0], core.events_of("tool_end")
            assert start["name"] == "write_model_code" and start["note"] == "a cube"
            assert start["args"] == {"code": CODE, "note": "a cube"}
            assert [(e["name"], e["ok"], e["summary"], e["images"]) for e in end] == [
                ("write_model_code", True, "status: ok", 0),
                ("render_views", True, "views of version 1: iso", 1),
            ]
            assert core.events_of("usage")[-1]["requests"] == 3
            assert core.violations == []

            # What the provider is sent: the prompt, the tools, a token limit, no credential.
            assert lengths == sorted(lengths) and len(set(lengths)) == 3
            for request in core.llm_requests:
                assert not CREDENTIAL_HEADERS & {name.lower() for name in request["headers"]}
                assert PLACEHOLDER_KEY not in json.dumps(request["headers"])
                assert surviving_headers(case, request)["content-type"] == "application/json"
            first = core.llm_requests[0]["body"].decode()
            assert SYSTEM_PROMPT[:200].replace("\n", "\\n") in first
            assert str(DEFAULT_MAX_OUTPUT_TOKENS) in first
            for name in ("write_model_code", "render_views", "propose_print"):
                assert name in first
            # The model saw the image of the view, and the tool result text.
            last = core.llm_requests[2]["body"].decode()
            assert PNG_B64 in last
            assert "views of version 1: iso" in last
            assert "status: ok\\nversion: 1 created" in last

            # The history is JSON the next run reads. The image stays in it as it was sent,
            # so the next message only appends to what the provider has cached.
            history = result["history"]
            stored = json.dumps(history)
            assert json.loads(stored) == history
            assert PNG_B64 in stored and "[image views/iso.png" not in stored
            assert CODE.replace("\n", "\\n") in stored.replace('\\"', '"').replace("\\\\n", "\\n")

            core.result_ready.clear()
            again = await run(core, channel, "thanks", provider=case.provider, history=history,
                              look=False)
            assert again["stop_reason"] == "done", again
            assert again["history"][:len(history)] == history
            assert len(again["history"]) == len(history) + 2
            fourth = core.llm_requests[3]["body"].decode()
            assert "make a cube" in fourth and "thanks" in fourth
            assert PNG_B64 in fourth

    with warnings.catch_warnings():
        warnings.simplefilter("error")
        asyncio.run(scenario())


def test_every_provider_request_would_be_refused_without_a_token_limit() -> None:
    """The mirror of the egress rule is not vacuous: the limit is what makes requests pass."""
    case = PROVIDERS["anthropic"]
    seen: list[dict[str, Any]] = []

    def llm(params: dict[str, Any]) -> LlmReply:
        seen.append(params)
        return reply(case, "Hello.")

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            await run(core, channel, "hi", provider=case.provider,
                      budget={"max_turns": 3, "max_output_tokens": 1_000_000})

    asyncio.run(scenario())
    body = json.loads(seen[0]["body"])
    assert body["max_tokens"] == 32_000, "a limit above the core's ceiling is clamped"
    assert check_egress(case, seen[0]) is None
    without = dict(seen[0], body=json.dumps({k: v for k, v in body.items() if k != "max_tokens"}))
    assert check_egress(case, without) == "the request sets no output token limit"
    assert check_egress(case, dict(seen[0], path="/v1/files")) is not None
    assert check_egress(case, dict(seen[0], path="/v1/messages?beta=true&x=1")) is not None


def test_a_refused_model_request_ends_the_run_with_the_reason_and_is_not_retried() -> None:
    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(refuse=("cap_exceeded", "the run's limit of 100 requests is used up"))

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            earlier = [{"anything": "the core stored before"}]
            session = Session(channel, run_params(core, "hi"))
            session._history_in = earlier
            result = await session.run()
            # An unreadable history is its own error; use a readable one for the refusal.
            assert result["stop_reason"] == "error" and "cannot read" in result["error"]
            assert core.llm_requests == []

            result = await run(core, channel, "hi")
            assert result["stop_reason"] == "error"
            assert result["error"] == ("Hotend did not send the request to the AI provider: "
                                       "the run's limit of 100 requests is used up")
            assert result["history"] == []
            assert len(core.llm_requests) == 1, "the SDK must not retry a refusal"
            assert core.violations == []

    asyncio.run(scenario())


@pytest.mark.parametrize("kind", ["anthropic", "openai", "google", "openrouter"])
def test_a_provider_error_reaches_the_user_with_its_message(kind: str) -> None:
    case = PROVIDERS[kind]

    def llm(params: dict[str, Any]) -> LlmReply:
        return error_reply(case, 400, "model: no such model as the one you named")

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            result = await run(core, channel, "hi", provider=case.provider)
            assert result["stop_reason"] == "error"
            assert result["error"].startswith("The AI provider answered with an error (HTTP 400)")
            assert "no such model as the one you named" in result["error"]
            assert result["history"] == []
            assert len(core.llm_requests) == 1

    asyncio.run(scenario())


def test_cancelling_during_a_model_answer_keeps_what_was_said() -> None:
    case = PROVIDERS["anthropic"]
    started, release = threading.Event(), threading.Event()

    def llm(params: dict[str, Any]) -> LlmReply:
        answer = reply(case, "This sentence will be interrupted before it ends.")
        return LlmReply(chunks=answer.chunks, started=started, release=release)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            session = Session(channel, run_params(core, "make a cube"))
            task = asyncio.create_task(session.run())
            assert await asyncio.to_thread(started.wait, 20)
            session.cancel()
            result = await asyncio.wait_for(task, 5)
            release.set()
            await finish(core, channel, result)
            assert result["stop_reason"] == "cancelled"
            assert "make a cube" in json.dumps(result["history"])
            assert "error" not in result
            assert core.violations == []

    asyncio.run(scenario())


def test_bad_run_params_are_refused_before_anything_is_sent() -> None:
    async def scenario() -> None:
        async with in_process() as (core, channel):
            good = run_params(core, "hi")
            for broken in (
                dict(good, provider={"kind": "skynet", "model": "m", "base_url": "https://x"}),
                dict(good, provider={"kind": "anthropic", "model": "", "base_url": "https://x"}),
                dict(good, history="not a list"),
                dict(good, user_message={"attachments": []}),
                dict(good, budget={"max_turns": 0}),
                dict(good, tools=[{"name": "x"}]),
                dict(good, tools=[good["tools"][0], good["tools"][0]]),
                {k: v for k, v in good.items() if k != "egress_token"},
                {k: v for k, v in good.items() if k != "run_id"},
                "not an object",
            ):
                with pytest.raises(BadRunParams):
                    Session(channel, broken)
            assert core.lines == []

    asyncio.run(scenario())
