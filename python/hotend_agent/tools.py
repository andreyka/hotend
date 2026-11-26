"""Tools as thin proxies to the core's tool gateway (docs/PROTOCOL.md section 5.1).

The agent process executes nothing. Every tool the core lists in `agent.run` becomes a
Pydantic AI tool whose body sends `tool.call` and hands the core's answer to the model: text
as text, images as binary content the model can see. The core validates the arguments against
its own schema, so nothing is validated here, and the schema reaches the model unchanged.

A refused call (`ok: false`) and a failed request come back to the model as a short error
text, so it can adapt; they never end the run.
"""

from __future__ import annotations

import base64
import binascii
import json
import logging
import re
from collections.abc import Callable
from typing import Any

from pydantic_ai import BinaryContent, RunContext, Tool

from .channel import Channel, RpcError

log = logging.getLogger("hotend_agent.tools")

IMAGE_TYPES = frozenset({"image/png", "image/jpeg", "image/webp", "image/gif"})

# An agent.event over 64 kB is a protocol violation; arguments (a whole model script) are only
# shown when they are small.
_MAX_EVENT_ARGS = 4000
_MAX_SUMMARY = 300

ToolResult = str | list[str | BinaryContent]
Emit = Callable[[dict[str, Any]], None]


def _as_list(content: ToolResult) -> list[str | BinaryContent]:
    return [content] if isinstance(content, str) else list(content)


class BadToolSpecs(ValueError):
    """`agent.run` carried a tool list this agent cannot use."""


def summary_line(text: str) -> str:
    """The first line of a result text: the gateway puts the status there."""
    first = text.strip().split("\n", 1)[0] if text.strip() else ""
    return first[:_MAX_SUMMARY]


def to_model_content(outcome: Any) -> tuple[bool, ToolResult, int]:
    """Turns the core's `tool.call` result into (ok, what the model gets, number of images)."""
    if not isinstance(outcome, dict):
        return False, "error: the tool returned nothing usable", 0
    if outcome.get("ok") is not True:
        error = outcome.get("error")
        code = error.get("code") if isinstance(error, dict) else None
        message = error.get("message") if isinstance(error, dict) else None
        return False, (
            f"error ({code or 'failed'}): {message or 'the call was refused'}\n"
            "Hotend refused this call; nothing was changed."
        ), 0
    parts: list[str | BinaryContent] = []
    images = 0
    items = outcome.get("content")
    for item in items if isinstance(items, list) else []:
        if not isinstance(item, dict):
            continue
        if item.get("type") == "text" and isinstance(item.get("text"), str):
            if parts and isinstance(parts[-1], str):
                parts[-1] = f"{parts[-1]}\n{item['text']}"
            else:
                parts.append(item["text"])
        elif item.get("type") == "image":
            name = item.get("path") if isinstance(item.get("path"), str) else "image"
            media_type = item.get("media_type")
            try:
                data = base64.b64decode(item.get("data_b64") or "", validate=True)
            except (binascii.Error, TypeError, ValueError):
                data = b""
            if data and media_type in IMAGE_TYPES:
                parts.append(BinaryContent(data=data, media_type=media_type, identifier=name))
                images += 1
            else:
                parts.append(f"[{name}: the image could not be read]")
    if not parts:
        return True, "ok (the tool returned no content)", 0
    if len(parts) == 1 and isinstance(parts[0], str):
        return True, parts[0], 0
    return True, parts, images


#: Look review: the views Hotend renders after every build (`write_model_code` or
#: `set_parameter`) whose status is `ok`, and the text that comes with them. The text never changes, so it costs the prompt cache
#: nothing; `LOOKED_AT` is what the model's answer must then contain.
STANDARD_VIEWS = ("iso", "front", "top")
VIEWS_HEADER = (
    "Hotend's standard views of this version (iso, front, top) follow. Compare them with the "
    "contract: every part there, in place and the right way up, nothing missing, floating or "
    "merged. Then write one line that begins \"looked at:\" and says what you checked.")
LOOKED_AT = re.compile(r"looked at:", re.IGNORECASE)
_BUILD_TOOLS = frozenset({"write_model_code", "set_parameter"})
_STATUS_OK = re.compile(r"status: ok\b")


class ToolProxy:
    """Sends tool calls of one run to the core and reports them as `agent.event`s.

    With `look`, a build (`write_model_code` or `set_parameter`) whose status is `ok` is
    followed by one `render_views` call of Hotend's own for `STANDARD_VIEWS`, and its pictures
    are added to the end of that tool's result: new content at the end of the conversation, so
    nothing sent before changes. The call carries `auto: true` in its `tool.call` params: the
    core then renders its own views of the new version (smaller ones) and counts the call
    against none of the model's caps, and never as a refusal. It is reported with `auto: true`.
    """

    def __init__(self, channel: Channel, run_id: str, emit: Emit, look: bool = False) -> None:
        self._channel = channel
        self._run_id = run_id
        self._emit = emit
        self._look = look
        self._can_render = False
        self.calls = 0
        #: How many times this run showed the standard views.
        self.views_shown = 0

    async def call(self, name: str, args: dict[str, Any], tool_call_id: str | None) -> ToolResult:
        content = await self._call(name, args, tool_call_id)
        if not (self._look and self._can_render and name in _BUILD_TOOLS):
            return content
        text = content if isinstance(content, str) else next(
            (part for part in content if isinstance(part, str)), "")
        if not _STATUS_OK.match(summary_line(text)):
            return content
        views = await self._call("render_views", {"views": list(STANDARD_VIEWS)},
                                 f"{tool_call_id}-views" if tool_call_id else None, auto=True)
        pictures = [part for part in views if isinstance(part, BinaryContent)] if isinstance(
            views, list) else []
        if not pictures:
            note = views if isinstance(views, str) else "no pictures came back"
            failed = f"Hotend could not render the standard views: {summary_line(note)}"
            return [*_as_list(content), failed]
        self.views_shown += 1
        return [*_as_list(content), VIEWS_HEADER, *pictures]

    async def _call(self, name: str, args: dict[str, Any], tool_call_id: str | None,
                    auto: bool = False) -> ToolResult:
        self.calls += 1
        start: dict[str, Any] = {"type": "tool_start", "name": name, "tool_call_id": tool_call_id}
        if auto:
            start["auto"] = True
        if len(json.dumps(args, ensure_ascii=False)) <= _MAX_EVENT_ARGS:
            start["args"] = args
        else:
            start["args_omitted"] = True
        if isinstance(args.get("note"), str):
            start["note"] = args["note"][:500]
        self._emit(start)
        params: dict[str, Any] = {"run_id": self._run_id, "name": name, "args": args}
        if auto:
            params["auto"] = True
        try:
            outcome = await self._channel.request("tool.call", params)
        except RpcError as error:
            log.warning("tool.call %s failed with JSON-RPC error %d", name, error.code)
            ok, content, images = False, f"error: the tool call failed: {error.message}", 0
        else:
            ok, content, images = to_model_content(outcome)
        text = content if isinstance(content, str) else next(
            (part for part in content if isinstance(part, str)), "")
        log.info("tool.call %s ok=%s images=%d", name, ok, images)
        end: dict[str, Any] = {"type": "tool_end", "name": name, "tool_call_id": tool_call_id,
                               "ok": ok, "summary": summary_line(text), "images": images}
        if auto:
            end["auto"] = True
        self._emit(end)
        return content

    def tools(self, specs: Any) -> list[Tool[None]]:
        """One Pydantic AI tool per spec of `agent.run`'s `tools`."""
        if not isinstance(specs, list):
            raise BadToolSpecs("`tools` must be a list")
        tools: list[Tool[None]] = []
        seen: set[str] = set()
        for spec in specs:
            name = spec.get("name") if isinstance(spec, dict) else None
            schema = spec.get("input_schema") if isinstance(spec, dict) else None
            description = spec.get("description") if isinstance(spec, dict) else None
            if not isinstance(name, str) or not name or name in seen:
                raise BadToolSpecs("every tool needs a unique `name`")
            if not isinstance(schema, dict) or not isinstance(description, str | None):
                raise BadToolSpecs(f"tool `{name}` needs a `description` and an `input_schema`")
            seen.add(name)
            self._can_render = self._can_render or name == "render_views"
            tools.append(Tool.from_schema(
                self._function(name), name=name, description=description, json_schema=schema,
                takes_ctx=True,
                # Tools change the project's current version: one at a time, in order.
                sequential=True,
            ))
        return tools

    def _function(self, name: str) -> Callable[..., Any]:
        async def proxy(ctx: RunContext[None], **args: Any) -> ToolResult:
            return await self.call(name, args, ctx.tool_call_id)

        proxy.__name__ = name
        return proxy
