"""Canned provider answers and the core's egress table, for the agent tests.

`PROVIDERS` holds, per provider kind, what `agent.run` would carry and what the core's egress
(crates/hotend-core/src/egress/providers.rs) allows: the routes, the query strings, the
headers that survive and the body fields that cap the output. `check_egress` applies those
rules to a recorded `llm.request` the way the core does, so a test fails when a provider SDK
starts to build a request the core would refuse.

The rules are not kept here by hand: they are read from `fixtures/provider_table.json`, the
core's provider table as data (the fields named in `TABLE_FIELDS`). That file is meant to be
compared with the Rust table by a test on the core's side, as `fixtures/tool_specs.json` is
with the tool registry; only what the Rust table does not hold (a model name to ask for and
the wire format of the canned answers) is written in this module.

`reply` builds one streamed model answer (text, then at most one tool call) in the wire format
of each provider's streaming API, as documented publicly by the providers.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from agent_fake_core import LlmReply, sse

TOKEN_CEILING = 32_000
CREDENTIAL_HEADERS = {"authorization", "x-api-key", "x-goog-api-key", "api-key", "cookie"}

PROVIDER_TABLE = Path(__file__).with_name("fixtures") / "provider_table.json"
#: The fields of one provider's row in the fixture; nothing else may appear there.
TABLE_FIELDS = frozenset({"name", "origin", "sdk_base_url", "routes", "pass_headers",
                          "token_fields", "require_token_limit"})
ROUTE_FIELDS = frozenset({"method", "path", "queries", "generates"})
#: A model name in a path, as the core matches it (`PathRule::ModelAction`).
MODEL_SEGMENT = r"[A-Za-z0-9._-]+"

# What the core's table does not say: a model to ask for and the wire format of `reply`.
MODELS = {
    "anthropic": ("claude-sonnet-5", "anthropic"),
    "openai": ("gpt-6", "responses"),
    "google": ("gemini-3-pro", "google"),
    "openrouter": ("anthropic/claude-sonnet-5", "chat"),
    "ollama": ("qwen3:14b", "chat"),
}


@dataclass(frozen=True)
class ProviderCase:
    kind: str
    model: str
    base_url: str
    #: (method, path regex, allowed query strings)
    routes: tuple[tuple[str, str, tuple[str, ...]], ...]
    pass_headers: frozenset[str]
    token_fields: tuple[tuple[str, ...], ...]
    require_token_limit: bool = True
    #: The wire format of `reply`.
    wire: str = "chat"

    @property
    def provider(self) -> dict[str, str]:
        return {"kind": self.kind, "model": self.model, "base_url": self.base_url}


def _path_regex(rule: dict[str, Any]) -> str:
    """The regular expression for one route's path rule: `{"exact": path}` or
    `{"model_action": {"prefix", "actions"}}` (the prefix, one model name, one action)."""
    if set(rule) == {"exact"}:
        return re.escape(rule["exact"])
    if set(rule) == {"model_action"} and set(rule["model_action"]) == {"prefix", "actions"}:
        action = rule["model_action"]
        suffixes = "|".join(re.escape(suffix) for suffix in action["actions"])
        return f"{re.escape(action['prefix'])}{MODEL_SEGMENT}({suffixes})"
    raise ValueError(f"unknown path rule in {PROVIDER_TABLE.name}: {rule!r}")


def load_provider_table(path: Path = PROVIDER_TABLE) -> tuple[frozenset[str], dict[str, ProviderCase]]:
    """(headers allowed for every provider, the cases by kind) from the fixture."""
    table = json.loads(path.read_text(encoding="utf-8"))
    if set(table) != {"global_request_headers", "providers"}:
        raise ValueError(f"{path.name} must hold global_request_headers and providers")
    cases = {}
    for row in table["providers"]:
        if set(row) != TABLE_FIELDS:
            raise ValueError(f"{path.name}: a provider row must hold exactly {sorted(TABLE_FIELDS)}")
        if any(set(route) != ROUTE_FIELDS for route in row["routes"]):
            raise ValueError(f"{path.name}: a route must hold exactly {sorted(ROUTE_FIELDS)}")
        if not row["sdk_base_url"].startswith(row["origin"]):
            raise ValueError(f"{path.name}: {row['name']}'s SDK base URL is not on its origin")
        model, wire = MODELS[row["name"]]
        cases[row["name"]] = ProviderCase(
            kind=row["name"], model=model, base_url=row["sdk_base_url"],
            routes=tuple((route["method"], _path_regex(route["path"]), tuple(route["queries"]))
                         for route in row["routes"]),
            pass_headers=frozenset(row["pass_headers"]),
            token_fields=tuple(tuple(field) for field in row["token_fields"]),
            require_token_limit=row["require_token_limit"], wire=wire,
        )
    return frozenset(table["global_request_headers"]), cases


GLOBAL_HEADERS, PROVIDERS = load_provider_table()


def check_egress(case: ProviderCase, request: dict[str, Any]) -> str | None:
    """Why the core would refuse this recorded `llm.request`, or None when it would send it."""
    method = request["method"].upper()
    if method not in {allowed for allowed, _, _ in case.routes}:
        return f"method {request['method']} is not allowed"
    path, _, query = request["path"].partition("?")
    if not any(method == allowed and re.fullmatch(pattern, path)
               and (not query or query in queries)
               for allowed, pattern, queries in case.routes):
        return f"{request['path']} is not an allowed {case.kind} API call"
    try:
        body = json.loads(request["body"])
    except ValueError:
        return "the request body is not JSON"
    if not isinstance(body, dict):
        return "the request body is not a JSON object"
    limits = []
    for field in case.token_fields:
        value: Any = body
        for key in field:
            value = value.get(key) if isinstance(value, dict) else None
        if value is not None:
            if not isinstance(value, int) or isinstance(value, bool):
                return "the output token limit is not a whole number"
            limits.append(value)
    if limits and max(limits) > TOKEN_CEILING:
        return f"the request asks for {max(limits)} output tokens"
    if not limits and case.require_token_limit:
        return "the request sets no output token limit"
    return None


def surviving_headers(case: ProviderCase, request: dict[str, Any]) -> dict[str, str]:
    """The request headers the core would pass on to the provider."""
    allowed = GLOBAL_HEADERS | case.pass_headers
    return {k: v for k, v in request["headers"].items() if k.lower() in allowed}


def conversation_length(case: ProviderCase, body: dict[str, Any]) -> int:
    """How many conversation items a request body carries, to tell the turns apart."""
    if case.wire == "anthropic" or case.wire == "chat":
        return len(body["messages"])
    if case.wire == "responses":
        return len(body["input"])
    return len(body["contents"])


# -- streamed answers ------------------------------------------------------------------------


def _anthropic(text: str | None, tool: tuple[str, dict[str, Any]] | None, serial: int) -> bytes:
    events: list[dict[str, Any]] = [{"type": "message_start", "message": {
        "id": f"msg_{serial}", "type": "message", "role": "assistant", "model": "claude-test",
        "content": [], "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 100, "output_tokens": 1}}}]
    index = 0
    if text:
        events.append({"type": "content_block_start", "index": index,
                       "content_block": {"type": "text", "text": ""}})
        for piece in re.findall(r"\S+\s*", text):
            events.append({"type": "content_block_delta", "index": index,
                           "delta": {"type": "text_delta", "text": piece}})
        events.append({"type": "content_block_stop", "index": index})
        index += 1
    if tool:
        name, args = tool
        arguments = json.dumps(args)
        events.append({"type": "content_block_start", "index": index, "content_block": {
            "type": "tool_use", "id": f"toolu_{serial}", "name": name, "input": {}}})
        for part in (arguments[:7], arguments[7:]):
            events.append({"type": "content_block_delta", "index": index,
                           "delta": {"type": "input_json_delta", "partial_json": part}})
        events.append({"type": "content_block_stop", "index": index})
    events.append({"type": "message_delta", "usage": {"output_tokens": 40},
                   "delta": {"stop_reason": "tool_use" if tool else "end_turn",
                             "stop_sequence": None}})
    events.append({"type": "message_stop"})
    return sse(*events, named=True)


def _chat(text: str | None, tool: tuple[str, dict[str, Any]] | None, serial: int) -> bytes:
    def chunk(delta: dict[str, Any], finish: str | None = None) -> dict[str, Any]:
        return {"id": f"chatcmpl-{serial}", "object": "chat.completion.chunk", "created": 1,
                "model": "test", "choices": [{"index": 0, "delta": delta,
                                              "finish_reason": finish}]}

    events: list[dict[str, Any] | str] = [chunk({"role": "assistant", "content": ""})]
    for piece in re.findall(r"\S+\s*", text or ""):
        events.append(chunk({"content": piece}))
    if tool:
        name, args = tool
        arguments = json.dumps(args)
        events.append(chunk({"tool_calls": [{"index": 0, "id": f"call_{serial}",
                                             "type": "function",
                                             "function": {"name": name, "arguments": ""}}]}))
        for part in (arguments[:7], arguments[7:]):
            events.append(chunk({"tool_calls": [{"index": 0, "function": {"arguments": part}}]}))
    events.append(chunk({}, "tool_calls" if tool else "stop"))
    events.append({"id": f"chatcmpl-{serial}", "object": "chat.completion.chunk", "created": 1,
                   "model": "test", "choices": [],
                   "usage": {"prompt_tokens": 100, "completion_tokens": 40,
                             "total_tokens": 140}})
    events.append("[DONE]")
    return sse(*events)


def _responses(text: str | None, tool: tuple[str, dict[str, Any]] | None, serial: int) -> bytes:
    response: dict[str, Any] = {
        "id": f"resp_{serial}", "object": "response", "created_at": 1, "model": "test",
        "status": "in_progress", "output": [], "parallel_tool_calls": True,
        "tool_choice": "auto", "tools": [],
    }
    events: list[dict[str, Any]] = [{"type": "response.created", "response": response}]
    output: list[dict[str, Any]] = []
    if text:
        item_id = f"msg_{serial}"
        index = len(output)
        events.append({"type": "response.output_item.added", "output_index": index, "item": {
            "id": item_id, "type": "message", "role": "assistant", "status": "in_progress",
            "content": []}})
        events.append({"type": "response.content_part.added", "item_id": item_id,
                       "output_index": index, "content_index": 0,
                       "part": {"type": "output_text", "text": "", "annotations": []}})
        for piece in re.findall(r"\S+\s*", text):
            events.append({"type": "response.output_text.delta", "item_id": item_id,
                           "output_index": index, "content_index": 0, "delta": piece,
                           "logprobs": []})
        done = {"id": item_id, "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": text, "annotations": []}]}
        events.append({"type": "response.output_text.done", "item_id": item_id,
                       "output_index": index, "content_index": 0, "text": text,
                       "logprobs": []})
        events.append({"type": "response.content_part.done", "item_id": item_id,
                       "output_index": index, "content_index": 0,
                       "part": done["content"][0]})
        events.append({"type": "response.output_item.done", "output_index": index,
                       "item": done})
        output.append(done)
    if tool:
        name, args = tool
        arguments = json.dumps(args)
        item_id = f"fc_{serial}"
        index = len(output)
        call = {"id": item_id, "type": "function_call", "call_id": f"call_{serial}",
                "name": name, "arguments": "", "status": "in_progress"}
        events.append({"type": "response.output_item.added", "output_index": index,
                       "item": call})
        for part in (arguments[:7], arguments[7:]):
            events.append({"type": "response.function_call_arguments.delta",
                           "item_id": item_id, "output_index": index, "delta": part})
        events.append({"type": "response.function_call_arguments.done", "item_id": item_id,
                       "output_index": index, "arguments": arguments, "name": name})
        done = dict(call, arguments=arguments, status="completed")
        events.append({"type": "response.output_item.done", "output_index": index,
                       "item": done})
        output.append(done)
    events.append({"type": "response.completed", "response": dict(
        response, status="completed", output=output,
        usage={"input_tokens": 100, "output_tokens": 40, "total_tokens": 140,
               "input_tokens_details": {"cached_tokens": 0},
               "output_tokens_details": {"reasoning_tokens": 0}})})
    for number, event in enumerate(events):
        event["sequence_number"] = number
    return sse(*events, named=True)


def _google(text: str | None, tool: tuple[str, dict[str, Any]] | None, serial: int) -> bytes:
    def event(parts: list[dict[str, Any]], finish: str | None = None) -> dict[str, Any]:
        candidate: dict[str, Any] = {"content": {"role": "model", "parts": parts}, "index": 0}
        if finish:
            candidate["finishReason"] = finish
        return {"candidates": [candidate], "modelVersion": "gemini-test",
                "responseId": f"resp-{serial}",
                "usageMetadata": {"promptTokenCount": 100, "candidatesTokenCount": 40,
                                  "totalTokenCount": 140}}

    events = [event([{"text": piece}]) for piece in re.findall(r"\S+\s*", text or "")]
    last: list[dict[str, Any]] = []
    if tool:
        last = [{"functionCall": {"name": tool[0], "args": tool[1]}}]
    events.append(event(last or [{"text": ""}], "STOP"))
    return sse(*events)


_WIRES = {"anthropic": _anthropic, "chat": _chat, "responses": _responses, "google": _google}
_serial = [0]


def reply(case: ProviderCase, text: str | None = None,
          tool: tuple[str, dict[str, Any]] | None = None) -> LlmReply:
    """One streamed model answer in the provider's wire format, in two chunks."""
    _serial[0] += 1
    data = _WIRES[case.wire](text, tool, _serial[0])
    middle = len(data) // 2
    return LlmReply(chunks=[data[:middle], data[middle:]])


def error_reply(case: ProviderCase, status: int, message: str) -> LlmReply:
    """An upstream error answer, as each provider shapes it."""
    if case.wire == "google":
        body = {"error": {"code": status, "message": message, "status": "INVALID_ARGUMENT"}}
    else:
        body = {"type": "error", "error": {"type": "invalid_request_error", "message": message}}
    return LlmReply(status=status, headers={"content-type": "application/json"},
                    chunks=[json.dumps(body).encode()])
