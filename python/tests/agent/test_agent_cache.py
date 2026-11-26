"""The conversation the provider caches stays append-only, pictures included, and the look
review after a build (docs/WEB_RESEARCH.md section 15).

A scripted multi-message conversation runs through the real Anthropic SDK against the fake
core. Every request body is recorded and cut into the blocks the provider caches, in its
order: the tool definitions, the system blocks, then each content block of each message. The
cached prefix of a request is everything up to and including the last block that carries a
`cache_control` marker. Each request must repeat the previous request's cached prefix byte for
byte (markers aside, as they move) and add to it, so the provider reads it from the cache.
No request leaves the process.
"""

from __future__ import annotations

import asyncio
import base64
import dataclasses
import itertools
import json
from typing import Any

import pytest
from agent_canned import PROVIDERS, check_egress, reply
from agent_fake_core import (
    FIXTURES,
    LlmReply,
    finish,
    in_process,
    run_params,
    sse,
    text_outcome,
)
from pydantic_ai import BinaryContent
from pydantic_ai.messages import (
    ModelRequest,
    ModelResponse,
    NativeToolCallPart,
    NativeToolReturnPart,
    TextPart,
    ThinkingPart,
    ToolCallPart,
    ToolReturnPart,
    UserPromptPart,
)

from hotend_agent import runner
from hotend_agent.prompt import SYSTEM_PROMPT
from hotend_agent.runner import (
    LOOK_NUDGE,
    Session,
    picture_count,
    pictures_to_drop,
    with_pictures_capped,
    without_oversized_service_results,
    without_unkept_web_results,
)
from hotend_agent.scripted import load_script
from hotend_agent.tools import LOOKED_AT, STANDARD_VIEWS, VIEWS_HEADER

pytestmark = pytest.mark.timeout(120)

ANTHROPIC = PROVIDERS["anthropic"]
OFF: dict[str, Any] = json.loads((FIXTURES / "research" / "off_params.json").read_text("utf-8"))
BUILT = "status: ok\nversion: {n} created and now current\nchecks: 0 fail, 0 warn, 17 pass"
BROKEN = "status: model_error\nno version was created\nerror: ValueError at line 3: radius"


def png(seed: int) -> bytes:
    return bytes.fromhex("89504e470d0a1a0a") + bytes([seed % 256]) * 96


def picture_outcome(text: str, names: list[str], seed: int) -> dict[str, Any]:
    outcome = text_outcome(text)
    for offset, name in enumerate(names):
        outcome["content"].append({"type": "image", "path": f"views/{name}.png",
                                   "media_type": "image/png",
                                   "data_b64": base64.b64encode(png(seed + offset)).decode()})
    return outcome


class Gateway:
    """The fake core's tools: builds number their versions, views answer with pictures."""

    def __init__(self) -> None:
        self.version = 0
        self.calls: list[tuple[str, dict[str, Any]]] = []

    def __call__(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        self.calls.append((name, args))
        if name == "write_model_code":
            if args.get("code") == "BROKEN":
                return text_outcome(BROKEN)
            self.version += 1
            return text_outcome(BUILT.format(n=self.version))
        if name == "render_views":
            views = args.get("views") or ["iso"]
            return picture_outcome(f"views of version {self.version}: {', '.join(views)}",
                                   views, seed=len(self.calls) * 7)
        return text_outcome(f"status: ok\n{name} called")


def call(name: str, args: dict[str, Any] | None = None, text: str | None = None) -> LlmReply:
    return reply(ANTHROPIC, text, (name, args or {}))


def say(text: str) -> LlmReply:
    return reply(ANTHROPIC, text)


def play(messages: list[tuple[str, list[LlmReply]]], *, gateway: Gateway | None = None,
         research: dict[str, Any] | None = None, look: bool | None = None,
         model: str = "claude-fable-5-2") -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Runs one project's messages in order, each with the history the run before stored, as
    the core does. Returns every request body sent and every run's result."""
    gateway = gateway or Gateway()
    flat = [answer for _, answers in messages for answer in answers]
    bodies: list[dict[str, Any]] = []

    def llm(params: dict[str, Any]) -> LlmReply:
        problem = check_egress(ANTHROPIC, params)
        if problem is not None:
            return LlmReply(refuse=("denied", problem))
        bodies.append(json.loads(params["body"]))
        return flat[len(bodies) - 1]

    results: list[dict[str, Any]] = []

    async def scenario() -> None:
        history: list[Any] = []
        shape = None
        async with in_process(llm=llm, tools=gateway) as (core, channel):
            for text, _ in messages:
                extra: dict[str, Any] = {"research": dict(research or OFF, shape=shape)}
                if look is not None:
                    extra["look"] = look
                core.result_ready.clear()
                session = Session(channel, run_params(
                    core, text, history=history, provider=dict(ANTHROPIC.provider, model=model),
                    **extra))
                result = await finish(core, channel, await session.run())
                assert result["stop_reason"] == "done", result
                results.append(result)
                history, shape = result["history"], result.get("shape")
            assert core.violations == []

    asyncio.run(scenario())
    assert len(bodies) == len(flat), "every scripted answer is played"
    return bodies, results


# -- the cached prefix ------------------------------------------------------------------------


def _without_markers(value: Any) -> Any:
    if isinstance(value, dict):
        return {k: _without_markers(v) for k, v in value.items() if k != "cache_control"}
    if isinstance(value, list):
        return [_without_markers(v) for v in value]
    return value


def cached_blocks(body: dict[str, Any]) -> list[tuple[str, bool]]:
    """The request as the provider caches it, block by block: each block's bytes without its
    marker, and whether it carried one."""
    blocks: list[tuple[str, Any]] = [("tool", tool) for tool in body.get("tools", [])]
    system = body.get("system") or []
    if isinstance(system, str):
        system = [{"type": "text", "text": system}]
    blocks += [("system", block) for block in system]
    for message in body["messages"]:
        content = message["content"]
        if isinstance(content, str):
            content = [{"type": "text", "text": content}]
        blocks += [(message["role"], block) for block in content]
    return [(json.dumps([role, _without_markers(block)]), "cache_control" in json.dumps(block))
            for role, block in blocks]


def cached_prefix(body: dict[str, Any]) -> list[str]:
    """The blocks up to and including the last one with a cache marker."""
    blocks = cached_blocks(body)
    last = max(index for index, (_, marked) in enumerate(blocks) if marked)
    return [block for block, _ in blocks[:last + 1]]


def prefix_report(bodies: list[dict[str, Any]]) -> list[tuple[int, int, int]]:
    """Per request: blocks of the cached prefix, its bytes, and the bytes of the previous
    request's cached prefix that this request repeats unchanged (its cache read)."""
    rows = []
    previous: list[str] = []
    for body in bodies:
        prefix = cached_prefix(body)
        now = [block for block, _ in cached_blocks(body)]
        same = 0
        while same < len(previous) and same < len(now) and previous[same] == now[same]:
            same += 1
        rows.append((len(prefix), sum(map(len, prefix)), sum(map(len, previous[:same]))))
        previous = prefix
    return rows


def assert_append_only(bodies: list[dict[str, Any]]) -> list[tuple[int, int, int]]:
    rows = prefix_report(bodies)
    for number, (earlier, later) in enumerate(itertools.pairwise(bodies), start=1):
        was = cached_prefix(earlier)
        now = [block for block, _ in cached_blocks(later)]
        assert now[:len(was)] == was, (
            f"request {number + 1} does not repeat the cached prefix of request {number}")
    sizes = [size for _, size, _ in rows]
    assert sizes == sorted(sizes) and len(set(sizes)) == len(sizes), sizes
    for (_, _, read), (_, size, _) in zip(rows[1:], rows, strict=False):
        assert read == size, "each request reads all of the previous cached prefix"
    return rows


def three_messages() -> list[tuple[str, list[LlmReply]]]:
    return [
        ("a holder for the steamer basket", [
            call("list_cards"),
            call("write_model_code", {"code": "BROKEN", "note": "first"}),
            call("write_model_code", {"code": "FIXED", "note": "second"}),
            call("render_views", {"views": ["front"], "section": {"plane": "XZ"}}),
            say("looked at: iso, front, top and a section; the basket sits on the ring."),
        ]),
        ("make it 2 mm taller", [
            call("write_model_code", {"code": "TALLER", "note": "2 mm taller"}),
            say("It is 2 mm taller now."),          # no `looked at:`: asked once
            say("looked at: iso, front and top; still one piece, nothing floats."),
        ]),
        ("thanks", [say("You are welcome.")]),
    ]


def test_the_cached_prefix_only_grows_from_request_to_request_with_pictures(capsys) -> None:
    """Three messages with builds, Hotend's views, a section the model asked for and the
    look-review question: every request repeats the cached prefix of the one before byte for
    byte, so its cache read is all of it, and the prefix grows with every request."""
    gateway = Gateway()
    bodies, results = play(three_messages(), gateway=gateway)
    rows = assert_append_only(bodies)
    with capsys.disabled():
        print("\ncached prefix per request (blocks, bytes, bytes read from the previous):")
        for number, row in enumerate(rows, start=1):
            print(f"  request {number}: {row}")
    # The pictures were sent, stay sent, and the three markers are the ones of today.
    assert all(body["messages"][-1]["content"][-1]["cache_control"] for body in bodies)
    images = [json.dumps(body).count('"type": "image"') for body in bodies]
    assert images == sorted(images) and images[-1] == 3 + 1 + 3
    for body in bodies:
        assert json.dumps(body).count("cache_control") == 3
    stored = json.dumps(results[-1]["history"])
    assert "shown to the model when it was made" not in stored
    # The second message's run was asked once for its `looked at:` line, as a message of
    # its own at the end of the conversation.
    asked = "Reply with one line that begins"
    nudges = [n for n, b in enumerate(bodies, 1) if asked in json.dumps(b["messages"][-1])]
    assert nudges == [8]


def test_a_failed_run_leaves_the_conversation_the_next_run_appends_to() -> None:
    """A run that ends with an error hands back the history it was given, so the next
    message's first request repeats the last cached prefix of the run before it."""
    gateway = Gateway()
    flat: list[LlmReply] = [
        call("write_model_code", {"code": "FIXED"}),
        say("looked at: iso, front and top; one piece."),
        LlmReply(status=400, headers={"content-type": "application/json"},
                 chunks=[(b'{"type":"error","error":{"type":"invalid_request_error",'
                         b'"message":"boom"}}')]),
        say("Here again."),
    ]
    bodies: list[dict[str, Any]] = []

    def llm(params: dict[str, Any]) -> LlmReply:
        bodies.append(json.loads(params["body"]))
        return flat[min(len(bodies), len(flat)) - 1]

    async def scenario() -> list[dict[str, Any]]:
        out = []
        history: list[Any] = []
        async with in_process(llm=llm, tools=gateway) as (core, channel):
            for text in ("a box", "taller", "taller, please"):
                core.result_ready.clear()
                session = Session(channel, run_params(
                    core, text, history=history, research=dict(OFF),
                    provider=dict(ANTHROPIC.provider, model="claude-fable-5-2"),
                    budget={"max_turns": 24}))
                result = await finish(core, channel, await session.run())
                out.append(result)
                if result["stop_reason"] != "error":
                    history = result["history"]
        return out

    results = asyncio.run(scenario())
    assert [r["stop_reason"] for r in results] == ["done", "error", "done"]
    assert results[1]["history"] == results[0]["history"]
    assert "HTTP 400" in results[1]["error"]
    # Request 3 failed; request 4 is the retry with the same history and another message.
    retry, failed, last = bodies[3], bodies[2], bodies[1]
    prefix = cached_prefix(last)
    for body in (failed, retry):
        assert [block for block, _ in cached_blocks(body)][:len(prefix)] == prefix


# -- old pictures go a block at a time ---------------------------------------------------------


def _picture_turn(seed: int, count: int = 1) -> list[Any]:
    return [
        ModelResponse(parts=[ThinkingPart("look", signature="sig"), TextPart("rendering")]),
        ModelRequest(parts=[ToolReturnPart(
            "render_views", [f"views {seed}", *(
                BinaryContent(png(seed * 10 + i), media_type="image/png",
                              identifier=f"views/v{seed}-{i}.png") for i in range(count))],
            f"call-{seed}")]),
    ]


def test_old_pictures_go_a_block_at_a_time_and_a_cut_never_moves() -> None:
    """Up to the cap nothing changes. Past it the oldest pictures are replaced by a note, a
    whole block at once; the same conversation grown by more requests is cut in the same
    place until the next block, so the cached prefix changes once per block and not once
    per request."""
    cap, keep = 6, 2
    messages: list[Any] = [ModelRequest(parts=[UserPromptPart("a box")])]
    cuts = []
    previous: list[Any] = list(messages)
    for seed in range(20):
        messages = previous + _picture_turn(seed)
        processed = with_pictures_capped(messages, cap, keep)
        assert picture_count(processed) <= cap
        unchanged = processed[:len(previous)] == previous
        if not unchanged:
            cuts.append(seed + 1)
        # Doing it again changes nothing: the cut is stable.
        assert with_pictures_capped(processed, cap, keep) == processed
        previous = processed
    # Pictures 7, 11, 15 and 19 each start a new block of cap - keep = 4.
    assert cuts == [7, 11, 15, 19]
    assert [pictures_to_drop(n, cap, keep) for n in (6, 7, 10, 11, 14, 15)] == [
        0, 4, 4, 8, 8, 12]
    notes = [part.content[1] for m in previous if isinstance(m, ModelRequest)
             for part in m.parts if isinstance(part, ToolReturnPart)]
    assert notes[:16] == [f"[image views/v{seed}-0.png: shown to the model when it was made]"
                          for seed in range(16)]
    assert all(isinstance(item, BinaryContent) for item in notes[16:])


def _has_thinking(message: Any) -> bool:
    return any(isinstance(part, ThinkingPart) for part in message.parts)


def test_a_cut_drops_the_thinking_made_over_the_pictures_it_replaced() -> None:
    """Thinking after the first replaced picture goes, from earlier turns and from the tool
    loop in progress alike: a provider that binds its thinking refuses every block after a
    changed result. (Before, the loop in progress kept its thinking, and the provider then
    refused the request.) Thinking before the first replaced picture stays."""
    messages: list[Any] = [ModelRequest(parts=[UserPromptPart("a box")])]
    for seed in range(4):
        messages += _picture_turn(seed)
    messages.append(ModelRequest(parts=[UserPromptPart("make it taller")]))
    loop = len(messages)
    for seed in range(4, 6):
        messages += _picture_turn(seed)
    cut = with_pictures_capped(messages, 5, 1)
    # Four pictures went, all of the earlier turn's: thinking before the first of them
    # stays, thinking after it goes, in the loop in progress too.
    assert picture_count(cut) == 2
    assert _has_thinking(cut[1])
    earlier = [m for m in cut[3:loop] if isinstance(m, ModelResponse)]
    assert len(earlier) == 3 and not any(_has_thinking(m) for m in earlier)
    current = [m for m in cut[loop:] if isinstance(m, ModelResponse)]
    assert len(current) == 2 and not any(_has_thinking(m) for m in current)
    assert [m.parts[0].content for m in current] == ["rendering", "rendering"]

    # A loop that alone holds more than a block: the cut reaches into it, and the thinking
    # after the first replaced picture goes there too.
    messages = [ModelRequest(parts=[UserPromptPart("a box")])]
    for seed in range(4):
        messages += _picture_turn(seed)
    cut = with_pictures_capped(messages, 3, 1)
    assert picture_count(cut) == 2
    responses = [m for m in cut if isinstance(m, ModelResponse)]
    assert [_has_thinking(m) for m in responses] == [True, False, False, False]
    # An older Hotend replaced every picture by a note: those notes count as the pictures
    # they stand for, so the cut is where it would have been.
    old = [dataclasses.replace(m, parts=[dataclasses.replace(
        p, content=[p.content[0], (f"[image {p.content[1].identifier}: shown to the model when "
                                  "it was made]")]) for p in m.parts])
           if isinstance(m, ModelRequest) and isinstance(m.parts[0], ToolReturnPart) and
           m.parts[0].tool_call_id in ("call-0", "call-1") else m for m in messages]
    assert with_pictures_capped(old, 3, 1) is old
    assert picture_count(old) == picture_count(cut) == 2


def test_a_replaced_web_result_drops_every_later_thinking_part() -> None:
    """The web results are held to the same rule as the pictures: every thinking part after
    the first replaced result goes, the response holding it and the loop in progress
    included (before, the loop in progress kept them)."""
    big = "page text " * 500
    pdf = {"content": {"source": {"type": "base64", "data": "JVBERi0="}}}

    def thought(text: str) -> ModelResponse:
        return ModelResponse(parts=[ThinkingPart(text, signature="sig"), TextPart("ok")])

    def turn(n: int) -> list[Any]:
        return [
            ModelResponse(parts=[
                ToolCallPart("web_fetch", {"url": "https://e.x/"}, f"s{n}"),
                NativeToolCallPart("web_fetch", {"url": "https://e.x/a.pdf"}, f"n{n}"),
                NativeToolReturnPart("web_fetch", pdf, f"n{n}"),
                ThinkingPart(f"over the pdf {n}", signature="sig"),
            ]),
            ModelRequest(parts=[ToolReturnPart("web_fetch", big, f"s{n}")]),
            thought(f"over the page {n}"),
        ]

    messages: list[Any] = [ModelRequest(parts=[UserPromptPart("a holder")]), *turn(0),
                           ModelRequest(parts=[UserPromptPart("go on")]), *turn(1)]
    # The PDF of the first turn is the first replaced result: no thinking stays after it,
    # not even in the response that holds it.
    cleaned = without_unkept_web_results(messages, False, None)
    assert _thoughts(cleaned) == []
    # The oversized page of the first turn comes after its PDF: the thinking over the PDF,
    # made before the page, stays; everything after the page goes.
    cleaned = without_oversized_service_results(messages, 100)
    assert _thoughts(cleaned) == ["over the pdf 0"]
    # Every other part is where it was: only thinking went.
    for new, old in zip(cleaned, messages, strict=True):
        if isinstance(new, ModelResponse):
            assert new.parts == [p for p in old.parts if not isinstance(p, ThinkingPart)
                                 or p.content == "over the pdf 0"]


def test_a_response_of_only_thinking_after_an_edit_goes_whole() -> None:
    """Regression: a response that held nothing but thinking was kept as it was after an edited
    result, so a thinking block made over the old result was still sent. Such a response now
    goes whole, for each of the three edits; one before the edit stays."""
    pdf = {"content": {"source": {"type": "base64", "data": "JVBERi0="}}}

    def only_thinking(text: str) -> ModelResponse:
        return ModelResponse(parts=[ThinkingPart(text, signature="sig")])

    before = only_thinking("before")
    after = only_thinking("after")
    prompt = ModelRequest(parts=[UserPromptPart("a holder")])
    edited = {
        "web result": [prompt, before, ModelResponse(parts=[
            NativeToolCallPart("web_fetch", {"url": "https://e.x/a.pdf"}, "n0"),
            NativeToolReturnPart("web_fetch", pdf, "n0")]),
            ModelRequest(parts=[UserPromptPart("go on")]), after],
        "service result": [prompt, before, ModelResponse(parts=[
            ToolCallPart("web_fetch", {"url": "https://e.x/"}, "s0")]),
            ModelRequest(parts=[ToolReturnPart("web_fetch", "page " * 500, "s0")]), after],
    }
    for name, messages in edited.items():
        cleaned = (without_unkept_web_results(messages, False, None) if name == "web result"
                   else without_oversized_service_results(messages, 100))
        assert all(m is not after for m in cleaned), name
        assert len(cleaned) == len(messages) - 1, name
        assert _thoughts(cleaned) == ["before"], name

    messages: list[Any] = [ModelRequest(parts=[UserPromptPart("a box")]), before]
    for seed in range(4):
        messages += _picture_turn(seed)
    messages.append(after)
    cut = with_pictures_capped(messages, 3, 1)
    assert cut[1] is before and all(m is not after for m in cut)
    assert "after" not in _thoughts(cut)


def _thoughts(messages: list[Any]) -> list[str]:
    return [p.content for m in messages if isinstance(m, ModelResponse)
            for p in m.parts if isinstance(p, ThinkingPart)]


def _thinking_then(signature: str, *blocks: dict[str, Any], stop: str = "tool_use") -> LlmReply:
    """A hand-written answer: a thinking block with `signature`, then `blocks` as they stream."""
    events: list[dict[str, Any]] = [
        {"type": "message_start", "message": {
            "id": f"msg_{signature[:6]}", "type": "message", "role": "assistant", "model": "m",
            "content": [], "stop_reason": None, "stop_sequence": None,
            "usage": {"input_tokens": 900, "output_tokens": 1}}},
        {"type": "content_block_start", "index": 0, "content_block": {
            "type": "thinking", "thinking": "", "signature": ""}},
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "signature_delta", "signature": signature}},
        {"type": "content_block_stop", "index": 0},
    ]
    searches = 0
    for index, block in enumerate(blocks, start=1):
        if block["type"] == "tool_use":
            args = json.dumps(block.pop("input"))
            events += [
                {"type": "content_block_start", "index": index,
                 "content_block": {**block, "input": {}}},
                {"type": "content_block_delta", "index": index,
                 "delta": {"type": "input_json_delta", "partial_json": args}},
            ]
        elif block["type"] == "text":
            text = block["text"]
            events += [
                {"type": "content_block_start", "index": index,
                 "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": index,
                 "delta": {"type": "text_delta", "text": text}},
            ]
        else:
            searches += block["type"] == "web_search_tool_result"
            events.append({"type": "content_block_start", "index": index,
                           "content_block": block})
        events.append({"type": "content_block_stop", "index": index})
    events += [
        {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None},
         "usage": {"output_tokens": 30, "server_tool_use": {
             "web_search_requests": searches, "web_fetch_requests": 0}}},
        {"type": "message_stop"},
    ]
    return LlmReply(chunks=[sse(*events, named=True)])


SEARCH = [
    {"type": "server_tool_use", "id": "srvtoolu_01n", "name": "web_search",
     "input": {"query": "brenta 600 basket diameter"}},
    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01n", "content": [
        {"type": "web_search_result", "url": "https://maker.example/brenta",
         "title": "Brenta 600", "encrypted_content": "e"}]},
]


def _nudged_run(searched: bool) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """A message whose run builds a model and does not say what it looked at, so it is
    asked once (the look nudge), then a second message. With `searched` the run made a web
    search before the build."""
    build = {"type": "tool_use", "id": "toolu_01b", "name": "write_model_code",
             "input": {"code": "V1"}}
    first = [*(dict(b) for b in SEARCH)] if searched else []
    messages = [
        ("a holder for the Brenta 600 basket", [
            _thinking_then("c2lnLWJ1aWxk", *first, build),
            _thinking_then("c2lnLWRvbmU=", {"type": "text", "text": "It is built."},
                           stop="end_turn"),
            _thinking_then("c2lnLW51ZGdl", {"type": "text", "text":
                           "looked at: iso, front and top; one piece."}, stop="end_turn"),
        ]),
        ("thanks", [say("You are welcome.")]),
    ]
    return play(messages, research={"search": True, "fetch": True, "cache_ttl": "1h",
                                    "notes": ""})


def _signatures(body: dict[str, Any]) -> list[str]:
    return [block["signature"] for message in body["messages"]
            if isinstance(message["content"], list)
            for block in message["content"] if block.get("type") == "thinking"]


def test_the_look_nudge_after_a_web_call_replays_and_keeps_no_thinking() -> None:
    """Regression: the look nudge starts a new provider turn, for which the core writes the web
    tool entries from what the run has left: after a web call they carry less than the
    entries the run's thinking was made under, and the next message's first request carries
    the full ones again. The nudge replayed all the run's thinking, and the thinking of its
    own turn was stored and replayed later. Now, once the run made a web call, the nudge is
    sent without thinking and none of its turn's thinking is kept."""
    bodies, results = _nudged_run(searched=True)
    asked = "Reply with one line that begins"
    nudge = next(n for n, body in enumerate(bodies) if asked in json.dumps(body["messages"][-1]))
    assert _signatures(bodies[nudge - 1]) == ["c2lnLWJ1aWxk"]
    assert _signatures(bodies[nudge]) == []
    stored = json.dumps(results[0]["history"])
    assert "c2lnLW51ZGdl" not in stored and "c2lnLWJ1aWxk" not in stored
    assert _signatures(bodies[-1]) == []
    # The web result itself stays: only thinking goes.
    assert "srvtoolu_01n" in json.dumps(bodies[nudge])


def test_the_look_nudge_without_a_web_call_changes_nothing() -> None:
    """Without a web call in the run the nudge's entries are the run's own: the nudge is
    appended as before, thinking and all, and its thinking is kept."""
    bodies, results = _nudged_run(searched=False)
    asked = "Reply with one line that begins"
    nudge = next(n for n, body in enumerate(bodies) if asked in json.dumps(body["messages"][-1]))
    assert _signatures(bodies[nudge]) == ["c2lnLWJ1aWxk", "c2lnLWRvbmU="]
    assert "c2lnLW51ZGdl" in json.dumps(results[0]["history"])
    was = cached_prefix(bodies[nudge - 1])
    assert [block for block, _ in cached_blocks(bodies[nudge])][:len(was)] == was


RESEARCH_ON = {"search": True, "fetch": True, "cache_ttl": "1h", "notes": "", "shape": None}
BUILD = {"type": "tool_use", "id": "toolu_01b", "name": "write_model_code",
         "input": {"code": "V1"}}


def _one_run(replies: list[LlmReply], max_turns: int = 24, cancel_on: int | None = None,
             ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """One research run against `replies`: the request bodies and the result. With
    `cancel_on`, the run is cancelled while the answer to that request (1-based) streams."""
    bodies: list[dict[str, Any]] = []
    holder: dict[str, Any] = {}

    def llm(params: dict[str, Any]) -> LlmReply:
        problem = check_egress(ANTHROPIC, params)
        if problem is not None:
            return LlmReply(refuse=("denied", problem))
        bodies.append(json.loads(params["body"]))
        answer = replies[min(len(bodies), len(replies)) - 1]
        if cancel_on == len(bodies):
            holder["session"].cancel()
        return answer

    async def scenario() -> dict[str, Any]:
        async with in_process(llm=llm, tools=Gateway()) as (core, channel):
            session = Session(channel, run_params(
                core, "a holder for the Brenta 600 basket", research=dict(RESEARCH_ON),
                provider=dict(ANTHROPIC.provider, model="claude-fable-5-2"),
                budget={"max_turns": max_turns}))
            holder["session"] = session
            result = await finish(core, channel, await session.run())
            assert core.violations == []
            return result

    return bodies, asyncio.run(scenario())


def _nudge_replies() -> list[LlmReply]:
    """A build after a search, an answer without `looked at:`, then the nudge's answer,
    which calls a tool (so its turn needs one more request)."""
    return [
        _thinking_then("c2lnLWJ1aWxk", *(dict(b) for b in SEARCH), dict(BUILD)),
        _thinking_then("c2lnLWRvbmU=", {"type": "text", "text": "It is built."},
                       stop="end_turn"),
        _thinking_then("c2lnLW51ZGdl", {"type": "tool_use", "id": "toolu_01c",
                                        "name": "list_cards", "input": {}}),
        say("looked at: iso, front and top; one piece."),
    ]


def test_no_thinking_of_a_changed_turn_is_kept_when_the_run_runs_out_of_turns() -> None:
    """Regression: thinking was dropped after the nudge only when the run ended normally. A run
    that runs out of turns in the nudge's turn kept that turn's thinking. Now it keeps none."""
    bodies, result = _one_run(_nudge_replies(), max_turns=3)
    assert result["stop_reason"] == "budget", result
    assert len(bodies) == 3 and _signatures(bodies[2]) == []
    stored = json.dumps(result["history"])
    assert "c2lnLW51ZGdl" not in stored and "c2lnLWJ1aWxk" not in stored
    assert "srvtoolu_01n" in stored


def test_no_thinking_of_a_changed_turn_is_kept_when_the_run_is_cancelled() -> None:
    """The same for a run cancelled in the nudge's turn."""
    _, result = _one_run(_nudge_replies(), cancel_on=3)
    assert result["stop_reason"] == "cancelled", result
    stored = json.dumps(result["history"])
    assert "c2lnLW51ZGdl" not in stored and "c2lnLWJ1aWxk" not in stored
    assert "c2lnLWRvbmU=" not in stored


def test_a_retry_after_an_answer_without_output_after_a_web_call_replays_no_thinking() -> None:
    """The model library answers a reply that held only thinking with feedback in a message
    of its own: a new provider turn, whose entries can carry less after a web call. That
    request goes without thinking, the thinking-only reply goes whole, and the run keeps no
    thinking."""
    replies = [
        _thinking_then("c2lnLWJ1aWxk", *(dict(b) for b in SEARCH), dict(BUILD)),
        _thinking_then("c2lnLWVtcHR5", stop="end_turn"),
        _thinking_then("c2lnLWFmdGVy", {"type": "text", "text":
                       "looked at: iso, front and top; one piece."}, stop="end_turn"),
    ]
    bodies, result = _one_run(replies)
    assert result["stop_reason"] == "done", result
    assert len(bodies) == 3
    assert _signatures(bodies[1]) == ["c2lnLWJ1aWxk"]
    assert _signatures(bodies[2]) == []
    assert "c2lnLWVtcHR5" not in json.dumps(bodies[2])
    stored = json.dumps(result["history"])
    assert not any(sig in stored for sig in ("c2lnLWJ1aWxk", "c2lnLWVtcHR5", "c2lnLWFmdGVy"))


def test_without_any_thinking_drops_a_reply_of_only_thinking() -> None:
    only = ModelResponse(parts=[ThinkingPart("t", signature="sig")])
    mixed = ModelResponse(parts=[ThinkingPart("t", signature="sig"), TextPart("hi")])
    prompt = ModelRequest(parts=[UserPromptPart("a box")])
    cleaned = runner.without_any_thinking([prompt, only, mixed])
    assert len(cleaned) == 2 and cleaned[0] is prompt
    assert cleaned[1].parts == [TextPart("hi")]


def test_a_long_project_is_cut_once_per_block_through_real_runs(monkeypatch) -> None:
    """The same rule inside runs: a conversation with more pictures than the cap is cut
    before a request, the cut is kept in what is stored, and the requests between two cuts
    only grow."""
    monkeypatch.setattr(runner, "MAX_KEPT_PICTURES", 8)
    monkeypatch.setattr(runner, "KEPT_AFTER_DROP", 2)
    messages = []
    for number in range(4):
        messages.append((f"change {number}", [
            call("write_model_code", {"code": f"V{number}"}),
            say(f"looked at: iso, front and top of version {number + 1}."),
        ]))
    bodies, results = play(messages)
    images = [json.dumps(body).count('"type": "image"') for body in bodies]
    # 3 pictures per build: 3, 3, 6, 6, 9 -> cut to 3, ...
    assert max(images) <= 8
    breaks = []
    for number, (earlier, later) in enumerate(itertools.pairwise(bodies), start=1):
        was = cached_prefix(earlier)
        if [block for block, _ in cached_blocks(later)][:len(was)] != was:
            breaks.append(number + 1)
    assert len(breaks) == 1, (images, breaks)
    assert picture_count_of(results[-1]["history"]) <= 8


def picture_count_of(history: list[Any]) -> int:
    return json.dumps(history).count('"kind": "binary"')


# -- the look review ---------------------------------------------------------------------------


def test_the_views_follow_an_ok_build_only_and_are_reported_as_hotends() -> None:
    gateway = Gateway()
    bodies, _ = play([("a box", [
        call("write_model_code", {"code": "BROKEN"}),
        call("write_model_code", {"code": "FIXED"}),
        say("looked at: iso, front and top; the lid sits on the box."),
    ])], gateway=gateway)
    assert [name for name, _ in gateway.calls] == [
        "write_model_code", "write_model_code", "render_views"]
    assert gateway.calls[2][1] == {"views": list(STANDARD_VIEWS)}
    # The views are part of the build's own result, after its text.
    result = bodies[2]["messages"][-1]["content"][0]
    assert result["type"] == "tool_result"
    kinds = [block["type"] for block in result["content"]]
    assert kinds == ["text", "text", "image", "image", "image"]
    assert result["content"][0]["text"].startswith("status: ok")
    assert result["content"][1]["text"] == VIEWS_HEADER
    assert VIEWS_HEADER not in json.dumps(bodies[1]), "not after a failed build"


def test_the_look_question_is_asked_once_and_only_when_needed() -> None:
    # Answered in the same run: not asked.
    bodies, _ = play([("a box", [
        call("write_model_code", {"code": "A"}),
        say("Looked at: the iso view and the top; both parts are there."),
    ])])
    assert len(bodies) == 2 and LOOK_NUDGE not in json.dumps(bodies)
    # Not answered even after the question: asked once, never twice.
    bodies, results = play([("a box", [
        call("write_model_code", {"code": "A"}),
        say("Done."),
        say("Still done."),
    ])])
    assert len(bodies) == 3
    assert json.dumps(bodies[2]).count("Reply with one line that begins") == 1
    assert results[0]["usage"]["requests"] == 3
    # No views, no question: a run that only talks, a scripted run, a run without
    # `render_views`, and `look: false`.
    bodies, _ = play([("hi", [say("Hello.")])])
    assert len(bodies) == 1
    gateway = Gateway()
    bodies, _ = play([("a box", [call("write_model_code", {"code": "A"}), say("Done.")])],
                     gateway=gateway, look=False)
    assert len(bodies) == 2 and [n for n, _ in gateway.calls] == ["write_model_code"]


def test_the_look_question_respects_the_turn_budget() -> None:
    gateway = Gateway()
    bodies: list[dict[str, Any]] = []
    answers = [call("write_model_code", {"code": "A"}), say("Done.")]

    def llm(params: dict[str, Any]) -> LlmReply:
        bodies.append(json.loads(params["body"]))
        return answers[len(bodies) - 1]

    async def scenario() -> dict[str, Any]:
        async with in_process(llm=llm, tools=gateway) as (core, channel):
            session = Session(channel, run_params(core, "a box", budget={"max_turns": 2}))
            return await finish(core, channel, await session.run())

    result = asyncio.run(scenario())
    assert result["stop_reason"] == "done" and len(bodies) == 2


def test_scripted_runs_and_runs_without_the_tool_get_no_views() -> None:
    script = load_script("builtin:cable_box_demo")

    async def scenario() -> None:
        async with in_process() as (core, channel):
            assert Session(channel, run_params(core, "x"), script)._look is False
            assert Session(channel, run_params(core, "x", look=True), script)._look is True
            assert Session(channel, run_params(core, "x"))._look is True
            tools = [spec for spec in run_params(core, "x")["tools"]
                     if spec["name"] != "render_views"]
            session = Session(channel, run_params(core, "x", tools=tools))
            assert session._proxy._can_render is False

    asyncio.run(scenario())


def test_the_prompt_asks_for_what_the_runner_checks() -> None:
    text = " ".join(SYSTEM_PROMPT.split())
    assert f"standard views ({', '.join(STANDARD_VIEWS)})" in text
    assert LOOKED_AT.search(text) and "`looked at:`" in text
    assert LOOKED_AT.search(VIEWS_HEADER) and LOOKED_AT.search(LOOK_NUDGE)
