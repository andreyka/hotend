"""One agent run: `agent.run` params in, the result object out (docs/PROTOCOL.md section 5).

Guarantees:
- The model is reached only through `CoreTransport`, tools only through `ToolProxy`.
- `agent.event` notifications are sent for text and thinking deltas, tool start and end, and
  usage; none is larger than the core accepts.
- The run stops at `budget.max_turns` model requests (`stop_reason: "budget"`), on `cancel()`
  (`"cancelled"`), or with an error text fit for the user (`"error"`). It never raises.
- The returned history is Pydantic AI's own message serialisation, a JSON array this module
  can read back. It is the conversation as it was sent, pictures included, so the next
  message only appends to it and the provider's prompt cache holds; only the oldest pictures
  of a long conversation are replaced by a note, a block at a time (`with_pictures_capped`).
  On `"error"` the history is the one that was passed in, so a failed run leaves nothing
  half-done behind.
- Look review: after a `write_model_code` whose status is `ok` the model is shown Hotend's
  standard views with the result (`ToolProxy`), and its answer must say what it checked in a
  line beginning `looked at:`. A run that ends without one is asked once more for it.
- Web research (docs/WEB_RESEARCH.md) is used only when the core's `research` parameter says
  so, and only by declaring the provider's two web tools as markers: the searches run at the
  provider, the core writes the real tool entries and counts what was used. What the web
  tools return is replayed unchanged and never interpreted here. A run that stops while a
  web call is still unanswered leaves that last message out of the history, so the next
  message is not refused for it.
- Research the run does not have: when an answer of a run without web research has a line
  beginning `web research is off:` (prompt, step 1), the run ends with a `text_delta` event
  without text that carries `research_needed: {"lookup", "state", "source"}` (the core's
  research state of the run and the chosen source, null when it did not say), for the
  window's action next to that answer (docs/PROTOCOL.md section 5.2). The maker's message
  of such a run carries a note that says why research is off.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import logging
import re
from typing import Any
from urllib.parse import urlsplit

from pydantic import ValidationError
from pydantic_ai import Agent, BinaryContent, CancellationToken, capture_run_messages
from pydantic_ai.capabilities import ProcessHistory
from pydantic_ai.exceptions import (
    AgentRunError,
    ModelAPIError,
    ModelHTTPError,
    RunCancelled,
    UsageLimitExceeded,
    UserError,
)
from pydantic_ai.messages import (
    ModelMessage,
    ModelMessagesTypeAdapter,
    ModelRequest,
    ModelResponse,
    NativeToolCallPart,
    NativeToolReturnPart,
    PartDeltaEvent,
    PartEndEvent,
    PartStartEvent,
    RetryFeedbackPart,
    TextPart,
    TextPartDelta,
    ThinkingPart,
    ThinkingPartDelta,
    ToolReturnPart,
)
from pydantic_ai.usage import RunUsage, UsageLimits

from .channel import Channel, ChannelClosed
from .prompt import (
    PROMPT_VERSION,
    RESEARCH_PROMPT,
    SERVICE_RESEARCH_PROMPT,
    SYSTEM_PROMPT,
)
from .providers import (
    CACHE_TTLS,
    PROVIDER_KINDS,
    RESEARCH_KINDS,
    UnknownProvider,
    build_model,
    cache_settings,
    http_client,
    model_settings,
    research_capabilities,
    research_settings,
)
from .scripted import Script, ScriptError, scripted_model
from .tools import LOOKED_AT, VIEWS_HEADER, BadToolSpecs, ToolProxy
from .transport import CoreTransport

log = logging.getLogger("hotend_agent.run")

DEFAULT_MAX_TURNS = 24
MAX_TURNS = 100
# An agent.event must stay under 64 kB as a JSON line; text is cut into pieces well below.
_EVENT_TEXT_CHARS = 8000
_MAX_ERROR_CHARS = 600
# The provider's web tools, by the names their calls carry.
_WEB_TOOLS = ("web_search", "web_fetch")
# The research notes the core hands over are a few lines per number; more is not taken.
_MAX_NOTES_CHARS = 16_000
_ERROR_CODE_RE = re.compile(r"[a-z_]{1,40}\Z")
# The research states of a project as the core names them (`ResearchState`), with the
# reason a run without search is given for each.
RESEARCH_STATES = {
    "on": "",
    "off": "web research is switched off for this project",
    "needs_consent": ("the maker has not agreed to web research yet; Hotend shows them a "
                      "button to turn it on"),
    "budget_used": "this project has used its web research allowance",
    "stopped": "this project has used its web research allowance",
    "unsupported_provider": "the chosen AI provider has no web research in Hotend",
    "provider_refused": "the AI provider refused web search for this account",
    "needs_key": "the chosen search service needs an API key in Hotend's settings",
}
RESEARCH_SOURCES = ("built_in", "keenable", "tavily", "off")
# Search services the core can call for a run (docs/WEB_RESEARCH.md, section 16). Their
# `web_search` and `web_fetch` come with the gateway's tools; nothing is declared here.
SEARCH_SERVICES = ("keenable", "tavily")
# Sent once, as a message of its own, when a run showed the standard views and its answer did
# not say what was checked in them (look review).
LOOK_NUDGE = (
    "[Hotend: you were shown the standard views of a version in this message, and your answer "
    "does not say what you checked in them. Reply with one line that begins \"looked at:\" "
    "and names what you compared with the contract. Fix the model first only if the views "
    "show a problem.]")
# The line an answer begins when it would look a fact up and the run has no web research
# (prompt, step 1). The run then sends one `text_delta` event without text and with
# `research_needed: {"lookup", "state", "source"}`, so that the window can offer the right
# thing next to that message (agreeing, the settings, or nothing) without reading the text.
RESEARCH_OFF = re.compile(r"^[^\w\n]*web research is off:[ \t]*(.*)$",
                          re.IGNORECASE | re.MULTILINE)
_MAX_LOOKUP_CHARS = 200


def research_wanted(messages: list[ModelMessage]) -> str | None:
    """What the final answer among `messages` (the newest response with text) says it would
    look up on the web, from its line `web research is off: ...`; None when that answer has
    no such line. An earlier answer's line is not taken: the final one is what the maker
    reads next to the action."""
    for message in reversed(messages):
        if not isinstance(message, ModelResponse):
            continue
        texts = [part.content for part in message.parts
                 if isinstance(part, TextPart) and part.content.strip()]
        if not texts:
            continue
        found = RESEARCH_OFF.search("\n".join(texts))
        if found is None:
            return None
        return found.group(1).strip().strip("*_`").strip()[:_MAX_LOOKUP_CHARS]
    return None


class BadRunParams(ValueError):
    """`agent.run` params that cannot be used; answered as JSON-RPC error -32602."""


def _string(params: dict[str, Any], key: str) -> str:
    value = params.get(key)
    if not isinstance(value, str) or not value:
        raise BadRunParams(f"`{key}` must be a non-empty string")
    return value


def _usage(usage: RunUsage, tool_calls: int, web_fetches: int = 0) -> dict[str, int]:
    counts = {
        "requests": usage.requests,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_write_tokens": usage.cache_write_tokens,
        "tool_calls": tool_calls,
    }
    # Web research, when there was any: the searches the provider reported and the fetches
    # this run saw the model make. The core keeps its own counts for the caps and the cost;
    # these are shown next to the token counts.
    searches = (usage.details or {}).get("web_search_requests")
    if searches:
        counts["web_searches"] = int(searches)
    if web_fetches:
        counts["web_fetches"] = web_fetches
    return counts


class Research:
    """What the core's `research` parameter allows this run (docs/WEB_RESEARCH.md)."""

    def off_note(self) -> str:
        """The note on the maker's message of a run without search that says why, so that
        the assistant's line `web research is off: ...` is true. Empty when the run searches
        or the core did not say why."""
        if self.search or self.state in (None, "on"):
            return ""
        if self.state == "off" and self.source == "off":
            why = ("no web research source is chosen in Hotend's settings; Hotend shows the "
                   "maker a button to the settings")
        else:
            why = RESEARCH_STATES[self.state]
        return f"[Hotend: web research is not available in this run: {why}.]"

    def __init__(self, raw: Any, provider_kind: str | None) -> None:
        raw = raw if isinstance(raw, dict) else {}
        supported = provider_kind in RESEARCH_KINDS
        # Research through a search service: the core makes the calls, so it works with
        # every provider; the gateway lists the two tools. A scripted run has none.
        service = raw.get("service")
        self.service: str | None = service if service in SEARCH_SERVICES else None
        if self.service is not None:
            self.search = provider_kind is not None and raw.get("search") is True
        else:
            self.search = supported and raw.get("search") is True
        self.fetch = self.search and raw.get("fetch") is True
        ttl = raw.get("cache_ttl")
        self.cache_ttl: str = ttl if ttl in CACHE_TTLS else "5m"
        # The prompt cache does not depend on search being on: web results stay in the
        # conversation after research stopped. On whenever the core names a lifetime.
        self.cache = supported and (self.search or ttl in CACHE_TTLS)
        # Whether a fetched PDF may stay in the conversation, and the largest web result
        # that may; the core refuses to send anything else again.
        self.keep_pdf = raw.get("keep_pdf") is True
        size = raw.get("max_result_bytes")
        self.max_result_bytes: int | None = (
            size if isinstance(size, int) and not isinstance(size, bool) and size > 0 else None)
        notes = raw.get("notes")
        self.notes: str = notes[:_MAX_NOTES_CHARS] if isinstance(notes, str) else ""
        # Why a run without search has none, as the core decided it: the project's research
        # state and the source chosen in the settings. None when the core did not say (a
        # scripted run, an older core).
        state, source = raw.get("state"), raw.get("source")
        self.state: str | None = state if state in RESEARCH_STATES else None
        self.source: str | None = source if source in RESEARCH_SOURCES else None
        # The shape of the requests the stored history was made with; None when the core
        # did not say (then nothing is known about it and nothing is changed).
        shape = raw.get("shape")
        self.stored_shape: str | None = shape if isinstance(shape, str) and shape else None


def request_shape(instructions: str, tools: Any, research: Research, model: str) -> str:
    """A short hash of everything in front of the conversation in a request: the
    instructions, the tools and what research adds. A provider that binds its thinking
    blocks to the request rejects them when any of it changed."""
    material = json.dumps(
        [instructions, tools, research.search, research.fetch, research.cache_ttl, model],
        sort_keys=True, default=str,
    )
    return hashlib.sha256(material.encode("utf-8")).hexdigest()[:16]


def without_thinking(messages: list[ModelMessage]) -> list[ModelMessage]:
    """The same messages without the model's thinking parts. A response that holds nothing
    else is kept as it is."""
    cleaned: list[ModelMessage] = []
    for message in messages:
        if isinstance(message, ModelResponse):
            parts = [part for part in message.parts if not isinstance(part, ThinkingPart)]
            if parts and len(parts) != len(message.parts):
                message = dataclasses.replace(message, parts=parts)
        cleaned.append(message)
    return cleaned


def _without_thinking_parts(message: ModelResponse) -> ModelResponse | None:
    """`message` without its thinking parts: the same object when that removes nothing, and
    `None` for a response that held only thinking, which then goes whole (the provider joins
    the requests on either side of it)."""
    parts = [part for part in message.parts if not isinstance(part, ThinkingPart)]
    if len(parts) == len(message.parts):
        return message
    return dataclasses.replace(message, parts=parts) if parts else None


def without_any_thinking(messages: list[ModelMessage]) -> list[ModelMessage]:
    """The messages without any thinking part; a response that held only thinking goes
    whole (`_without_thinking_parts`)."""
    cleaned: list[ModelMessage] = []
    for message in messages:
        if isinstance(message, ModelResponse):
            kept = _without_thinking_parts(message)
            if kept is None:
                continue
            message = kept
        cleaned.append(message)
    return cleaned


# How the functions below edit a conversation (docs/WEB_RESEARCH.md, section 6). A provider
# that binds its thinking (Anthropic: Fable 5.1 and 5.2, Opus 5.5, Sonnet 5.5, Haiku 5.5)
# records in each thinking block everything before it, and refuses a replayed block once any
# of that changed ("bound to a different conversation"; Fable 5.2 on every account). So the
# conversation is only ever appended to, and when a result has to be replaced (a picture
# cut, a web result that may not stay), every thinking part after the first replaced result
# goes: in earlier turns and in the tool loop in progress alike, the response that holds the
# result included. Removing a trailing run of thinking blocks is what the provider accepts;
# a block kept after a changed result makes it refuse the request (before, only the model
# library's single retry with `drop_block` rescued that). Thinking before the first replaced
# result stays as it was made, and a conversation that needs no change is returned as the
# same object, so what was sent before is sent again byte for byte.


# What stands in for a web result that is not kept: the provider's own error blocks, the way
# it reports a page it could not use.
_NOT_KEPT = {
    "web_fetch": ("web_fetch_tool_result_error", "unsupported_content_type", "unavailable"),
    "web_search": ("web_search_tool_result_error", "unavailable", "unavailable"),
}


def _is_pdf(content: Any) -> bool:
    try:
        return content["content"]["source"]["type"] == "base64"
    except (KeyError, TypeError):
        return False


def without_unkept_web_results(messages: list[ModelMessage], keep_pdf: bool,
                               max_bytes: int | None) -> list[ModelMessage]:
    """The messages with every web result the core does not let stay replaced by an error
    block: a fetched PDF (unless the project keeps them) and a result larger than
    `max_bytes`. The provider does not cap a PDF, and either would be paid for again with
    every later request.

    The call stays, so the model sees what it asked for. Every thinking part after the first
    replaced result is dropped, the tool loop in progress included: they were made over the
    result as it was, and the provider rejects them once it changed (see above). Messages
    that need no change are returned as they are.
    """
    cleaned: list[ModelMessage] = []
    changed = False
    for message in messages:
        if isinstance(message, ModelResponse):
            parts: list[Any] = []
            for part in message.parts:
                if isinstance(part, NativeToolReturnPart) and part.tool_name in _NOT_KEPT:
                    kind, pdf_code, size_code = _NOT_KEPT[part.tool_name]
                    code = None
                    if not keep_pdf and _is_pdf(part.content):
                        code = pdf_code
                    elif max_bytes is not None and len(
                            json.dumps(part.content, default=str)) > max_bytes:
                        code = size_code
                    if code is not None:
                        part = dataclasses.replace(
                            part, content={"type": kind, "error_code": code})
                        changed = True
                elif changed and isinstance(part, ThinkingPart):
                    continue
                parts.append(part)
            if changed and not parts:
                continue  # it held only thinking made after the change
            if changed and (len(parts) != len(message.parts) or any(
                    new is not old for new, old in zip(parts, message.parts, strict=False))):
                message = dataclasses.replace(message, parts=parts)
        cleaned.append(message)
    return cleaned if changed else messages


# What stands in for a search service's web result that is larger than the conversation
# keeps (docs/WEB_RESEARCH.md, section 16).
SERVICE_RESULT_NOT_KEPT = ("[web result left out of the conversation: it is larger than this "
                           "project keeps; search or read the page again if it is needed]")


def _size(content: Any) -> int:
    if isinstance(content, str):
        return len(content.encode("utf-8"))
    return len(json.dumps(content, default=str).encode("utf-8"))


def without_oversized_service_results(messages: list[ModelMessage],
                                      max_bytes: int | None) -> list[ModelMessage]:
    """The messages with every search-service web result (the gateway's `web_search` and
    `web_fetch` tool results) larger than `max_bytes` replaced by a short note: the same
    rule `without_unkept_web_results` applies to the provider's own web results. The core
    frames and sizes a service result to fit, so in practice this replaces nothing; it holds
    when the project's limit was lowered since, or for a result from elsewhere.

    The call stays. Every thinking part after the first replaced result is dropped, the tool
    loop in progress included, as for the provider's results. Messages that need no change
    are returned as they are.
    """
    if max_bytes is None:
        return messages
    cleaned: list[ModelMessage] = []
    changed = False
    for message in messages:
        if isinstance(message, ModelRequest):
            parts = [
                dataclasses.replace(part, content=SERVICE_RESULT_NOT_KEPT)
                if isinstance(part, ToolReturnPart) and part.tool_name in _WEB_TOOLS
                and _size(part.content) > max_bytes else part
                for part in message.parts
            ]
            if any(new is not old for new, old in zip(parts, message.parts, strict=True)):
                message = dataclasses.replace(message, parts=parts)
                changed = True
        elif changed and isinstance(message, ModelResponse):
            kept = _without_thinking_parts(message)
            if kept is None:
                continue
            message = kept
        cleaned.append(message)
    return cleaned if changed else messages


def without_unanswered_web_call(messages: list[ModelMessage]) -> list[ModelMessage]:
    """The messages without a last response whose web call has no result yet.

    The provider refuses a conversation that goes on after such a response, so a run that
    was stopped in the middle of a search must not hand it to the next one.
    """
    if not messages or not isinstance(messages[-1], ModelResponse):
        return messages
    parts = messages[-1].parts
    answered = {part.tool_call_id for part in parts if isinstance(part, NativeToolReturnPart)}
    waiting = any(isinstance(part, NativeToolCallPart) and part.tool_call_id not in answered
                  for part in parts)
    return messages[:-1] if waiting else messages


# Pictures in the conversation (docs/WEB_RESEARCH.md, section 15). A picture a tool showed the model stays
# in the conversation exactly as it was sent, also in what is stored, so the conversation is
# only ever appended to and the provider's prompt cache holds from one message to the next.
# Only when a conversation holds more than `MAX_KEPT_PICTURES` (the provider takes at most 100
# per request, and every one is sent again with every request) are the oldest replaced by a
# note, all of a block of `MAX_KEPT_PICTURES - KEPT_AFTER_DROP` at once: the conversation is
# then written to the cache again from the first replaced picture on, once per block and not
# once per message. Which pictures go depends only on their place in the conversation, so the
# same conversation is always cut in the same place.
MAX_KEPT_PICTURES = 60
KEPT_AFTER_DROP = 24
_PICTURE_NOTE = "[image {}: shown to the model when it was made]"
_PICTURE_NOTE_RE = re.compile(r"\[image [^\n]*: shown to the model when it was made\]\Z")


def _is_picture_slot(item: Any) -> bool:
    """A picture, or the note that stands for one that was taken out."""
    return isinstance(item, BinaryContent) and item.is_image or (
        isinstance(item, str) and _PICTURE_NOTE_RE.match(item) is not None)


def pictures_to_drop(total: int, cap: int | None = None, keep: int | None = None) -> int:
    """How many of the oldest of `total` pictures a conversation no longer keeps: none up to
    `cap`, then whole blocks of `cap - keep`."""
    cap = MAX_KEPT_PICTURES if cap is None else cap
    keep = KEPT_AFTER_DROP if keep is None else keep
    step = max(cap - keep, 1)
    return 0 if total <= cap else ((total - cap - 1) // step + 1) * step


def picture_count(messages: list[ModelMessage]) -> int:
    """The pictures a conversation still holds."""
    return sum(isinstance(item, BinaryContent)
               for message in messages if isinstance(message, ModelRequest)
               for part in message.parts if isinstance(part, ToolReturnPart)
               for item in (part.content if isinstance(part.content, list | tuple)
                            else [part.content]))


def with_pictures_capped(messages: list[ModelMessage], cap: int | None = None,
                         keep: int | None = None) -> list[ModelMessage]:
    """The messages with the oldest pictures replaced by a note naming them, as far as
    `pictures_to_drop` says; messages that need no change are returned as they are.

    Notes left by an earlier cut (or by an older Hotend, which replaced every picture) count
    as the pictures they stand for, so a cut once made is never moved. Every thinking part
    after the first picture replaced now is dropped, the tool loop in progress included:
    they were made over the picture, and a provider that binds its thinking to the
    conversation rejects them once it changed (see the note above `_NOT_KEPT`).
    """
    slots: list[tuple[int, int, int | None]] = []
    for m, message in enumerate(messages):
        if not isinstance(message, ModelRequest):
            continue
        for p, part in enumerate(message.parts):
            if not isinstance(part, ToolReturnPart):
                continue
            if isinstance(part.content, list | tuple):
                slots.extend((m, p, i) for i, item in enumerate(part.content)
                             if _is_picture_slot(item))
            elif _is_picture_slot(part.content):
                slots.append((m, p, None))
    drop = pictures_to_drop(len(slots), cap, keep)
    targets: dict[tuple[int, int], set[int | None]] = {}
    for m, p, i in slots[:drop]:
        content = messages[m].parts[p].content  # type: ignore[union-attr]
        item = content if i is None else content[i]
        if isinstance(item, BinaryContent):
            targets.setdefault((m, p), set()).add(i)
    if not targets:
        return messages
    first = min(m for m, _ in targets)

    def note(item: Any) -> Any:
        return _PICTURE_NOTE.format(item.identifier) if isinstance(item, BinaryContent) else item

    cleaned: list[ModelMessage] = []
    for m, message in enumerate(messages):
        if isinstance(message, ModelRequest) and any(key[0] == m for key in targets):
            parts = list(message.parts)
            for (tm, p), items in targets.items():
                if tm != m:
                    continue
                part = parts[p]
                assert isinstance(part, ToolReturnPart)
                if None in items:
                    content: Any = note(part.content)
                else:
                    content = [note(item) if i in items else item
                               for i, item in enumerate(part.content)]
                parts[p] = dataclasses.replace(part, content=content)
            message = dataclasses.replace(message, parts=parts)
        elif first < m and isinstance(message, ModelResponse):
            kept = _without_thinking_parts(message)
            if kept is None:
                continue
            message = kept
        cleaned.append(message)
    return cleaned


def dump_history(messages: list[ModelMessage]) -> list[Any]:
    """The conversation as it is stored: as it was sent, pictures included, cut as
    `with_pictures_capped` says."""
    return ModelMessagesTypeAdapter.dump_python(with_pictures_capped(messages), mode="json")


class Session:
    """The state of one run. Build it from the `agent.run` params, then await `run()`."""

    def __init__(self, channel: Channel, params: Any, script: Script | None = None) -> None:
        if not isinstance(params, dict):
            raise BadRunParams("params must be an object")
        self._channel = channel
        self.run_id = _string(params, "run_id")
        self._history_in = params.get("history")
        if not isinstance(self._history_in, list):
            raise BadRunParams("`history` must be an array")
        message = params.get("user_message")
        if not isinstance(message, dict) or not isinstance(message.get("text"), str):
            raise BadRunParams("`user_message.text` must be a string")
        self._user_text: str = message["text"]
        if message.get("attachments"):
            log.warning("attachments are not supported yet and were ignored")
        budget = params.get("budget") if isinstance(params.get("budget"), dict) else {}
        turns = budget.get("max_turns", DEFAULT_MAX_TURNS)
        if not isinstance(turns, int) or isinstance(turns, bool) or turns < 1:
            raise BadRunParams("`budget.max_turns` must be a whole number of at least 1")
        self._max_turns = min(turns, MAX_TURNS)
        self._settings = model_settings(budget.get("max_output_tokens"))
        provider_kind = (params.get("provider") or {}).get("kind") if isinstance(
            params.get("provider"), dict) else None
        # A scripted run talks to no provider and has no web research.
        self._research = Research(params.get("research"),
                                  None if script is not None else provider_kind)
        self._instructions = SYSTEM_PROMPT
        # Before every request: the oldest pictures of a long conversation are cut as
        # `with_pictures_capped` says, and with research a web result that may not stay is
        # taken out. What this returns is also what the run keeps.
        self._capabilities: list[Any] = [ProcessHistory(self._kept_history)]
        if self._research.cache:
            self._settings = cache_settings(self._settings, self._research.cache_ttl)
        if self._research.search and self._research.service is not None:
            # The gateway's `web_search` and `web_fetch` are ordinary tools whose results
            # the core already framed and sized: no provider marker. They stay in the
            # history under the same rule as the provider's own web results.
            self._instructions = SYSTEM_PROMPT + SERVICE_RESEARCH_PROMPT
        elif self._research.search:
            self._instructions = SYSTEM_PROMPT + RESEARCH_PROMPT
            self._capabilities = [
                *research_capabilities(self._research.fetch),
                *self._capabilities,
            ]
            self._settings = research_settings(self._settings, self._research.cache_ttl)
        # The notes go into the maker's own message and nowhere else.
        if self._research.notes:
            self._user_text = f"{self._user_text}\n\n{self._research.notes}"
        # So does why a run has no search: it differs between messages, so it is not part
        # of the instructions (a cached prefix).
        if off_note := self._research.off_note():
            self._user_text = f"{self._user_text}\n\n{off_note}"
        self._tool_specs = params.get("tools")

        self._cancel = CancellationToken()
        self._text_started = False
        self._web_started: set[str] = set()
        self._web_ended: set[str] = set()
        self._web_fetches = 0
        # Set when a turn started within this run may carry other web tool entries than the
        # run's thinking was made under; from then on no thinking is sent or kept.
        self._entries_changed = False
        self._last_usage: dict[str, int] | None = None
        # Look review: on for a model, off for a scripted run (a recorded conversation makes
        # its own `render_views` calls); `look` in the params says otherwise.
        look = params.get("look")
        self._look = look if isinstance(look, bool) else script is None
        self._proxy = ToolProxy(channel, self.run_id, self._emit, look=self._look)
        try:
            self._tools = self._proxy.tools(params.get("tools"))
        except BadToolSpecs as error:
            raise BadRunParams(str(error)) from error

        self._transport: CoreTransport | None = None
        if script is not None:
            log.warning("scripted mode: playing `%s`; no model provider is used", script.name)
            self._model: Any = scripted_model(script)
            self._model_label = f"scripted:{script.name}"
        else:
            provider = params.get("provider")
            if not isinstance(provider, dict):
                raise BadRunParams("`provider` must be an object")
            kind = _string(provider, "kind")
            if kind not in PROVIDER_KINDS:
                raise BadRunParams(f"unknown provider kind; expected one of {PROVIDER_KINDS}")
            self._transport = CoreTransport(channel, self.run_id, _string(params, "egress_token"))
            try:
                self._model = build_model(
                    kind, _string(provider, "model"), _string(provider, "base_url"),
                    http_client(self._transport),
                )
            except (UnknownProvider, UserError, ValueError, ImportError) as error:
                raise BadRunParams(f"cannot set up the {kind} provider: {error}") from error
            self._model_label = f"{kind}:{provider['model']}"

    def cancel(self) -> None:
        """Stops the run; `run()` then returns with `stop_reason: "cancelled"`."""
        self._cancel.cancel()

    def cancelled_result(self) -> dict[str, Any]:
        """The answer when the run could not wind down by itself after a cancel."""
        return {"history": self._history_in, "stop_reason": "cancelled", "usage": {}}

    # -- events ----------------------------------------------------------------------------

    def _emit(self, event: dict[str, Any]) -> None:
        self._channel.notify("agent.event", {"run_id": self.run_id, "event": event})

    def _emit_text(self, kind: str, text: str) -> None:
        for start in range(0, len(text), _EVENT_TEXT_CHARS):
            self._emit({"type": kind, "text": text[start:start + _EVENT_TEXT_CHARS]})

    def _usage_now(self, usage: RunUsage) -> dict[str, int]:
        return _usage(usage, self._proxy.calls, self._web_fetches)

    def _emit_usage(self, usage: RunUsage) -> None:
        current = self._usage_now(usage)
        if current != self._last_usage and usage.requests:
            self._last_usage = current
            self._emit({"type": "usage", **current})

    async def _on_events(self, ctx: Any, stream: Any) -> None:
        async for event in stream:
            if isinstance(event, PartStartEvent):
                part = event.part
                if isinstance(part, TextPart):
                    # The core joins the deltas into the transcript: keep the texts of
                    # separate model turns apart.
                    if self._text_started and part.content:
                        self._emit_text("text_delta", "\n\n")
                    if part.content:
                        self._text_started = True
                        self._emit_text("text_delta", part.content)
                elif isinstance(part, ThinkingPart) and part.content:
                    self._emit_text("thinking", part.content)
                elif isinstance(part, NativeToolReturnPart):
                    # A result arrives whole, with its start.
                    self._on_web_part(part)
            elif isinstance(event, PartEndEvent):
                # A call is complete (its query or address is known) when it ends.
                self._on_web_part(event.part)
            elif isinstance(event, PartDeltaEvent):
                delta = event.delta
                if isinstance(delta, TextPartDelta) and delta.content_delta:
                    if not self._text_started:
                        self._text_started = True
                    self._emit_text("text_delta", delta.content_delta)
                elif isinstance(delta, ThinkingPartDelta) and delta.content_delta:
                    self._emit_text("thinking", delta.content_delta)
        self._emit_usage(ctx.usage)

    def _on_web_part(self, part: Any) -> None:
        """Reports a call of one of the provider's web tools, and its result, with the
        events a gateway tool call has. Only the query, the page's host and a count or an
        error code are passed on: nothing a page wrote. Each call and each result is
        reported once, also when the provider's answer comes in several pieces."""
        if isinstance(part, NativeToolCallPart) and part.tool_name in _WEB_TOOLS:
            if part.tool_call_id in self._web_started:
                return
            self._web_started.add(part.tool_call_id)
            if part.tool_name == "web_fetch":
                self._web_fetches += 1
            try:
                args = part.args_as_dict()
            except (ValueError, TypeError):
                args = {}
            if part.tool_name == "web_search":
                note = str(args.get("query") or "")[:200]
            else:
                try:
                    note = urlsplit(str(args.get("url") or "")).hostname or ""
                except ValueError:
                    note = ""
            self._emit({"type": "tool_start", "name": part.tool_name,
                        "tool_call_id": part.tool_call_id, "note": note})
        elif isinstance(part, NativeToolReturnPart) and part.tool_name in _WEB_TOOLS:
            if part.tool_call_id in self._web_ended:
                return
            self._web_ended.add(part.tool_call_id)
            content = part.content
            error = content.get("error_code") if isinstance(content, dict) else None
            if isinstance(error, str):
                ok = False
                summary = error if _ERROR_CODE_RE.match(error) else "error"
            elif isinstance(content, list):
                ok = True
                summary = "1 result" if len(content) == 1 else f"{len(content)} results"
            else:
                ok, summary = True, "page read"
            self._emit({"type": "tool_end", "name": part.tool_name,
                        "tool_call_id": part.tool_call_id, "ok": ok, "summary": summary,
                        "images": 0})

    # -- the run ---------------------------------------------------------------------------

    def _error(self, text: str, usage: RunUsage | None = None) -> dict[str, Any]:
        log.warning("run %s ended with an error", self.run_id)
        return {
            "history": self._history_in,
            "stop_reason": "error",
            "usage": self._usage_now(usage or RunUsage()),
            "error": text[:_MAX_ERROR_CHARS],
        }

    def _model_error(self, error: ModelAPIError) -> str:
        refusal = self._transport.refusal if self._transport else None
        if refusal is not None:
            return f"Hotend did not send the request to the AI provider: {refusal.message}"
        if isinstance(error, ModelHTTPError):
            detail = ""
            body = error.body
            inner = body.get("error") if isinstance(body, dict) else None
            message = (inner if isinstance(inner, dict) else body if isinstance(body, dict)
                       else {}).get("message")
            if isinstance(message, str) and message:
                detail = f": {message[:300]}"
            return f"The AI provider answered with an error (HTTP {error.status_code}){detail}"
        return ("The AI provider could not be reached or the answer was cut off. "
                "Check the connection and try again.")

    def _kept_web_results(self, messages: list[ModelMessage]) -> list[ModelMessage]:
        messages = without_unkept_web_results(
            messages, self._research.keep_pdf, self._research.max_result_bytes)
        return without_oversized_service_results(messages, self._research.max_result_bytes)

    def missing_look_line(self, messages: list[ModelMessage]) -> bool:
        """Whether this run showed the standard views and no answer after the last of them
        says what was looked at."""
        if not self._proxy.views_shown:
            return False
        last = max((m for m, message in enumerate(messages)
                    if isinstance(message, ModelRequest) and any(
                        isinstance(part, ToolReturnPart) and isinstance(part.content, list)
                        and VIEWS_HEADER in part.content for part in message.parts)),
                   default=None)
        if last is None:
            return False
        return not any(isinstance(part, TextPart) and LOOKED_AT.search(part.content)
                       for message in messages[last:] if isinstance(message, ModelResponse)
                       for part in message.parts)

    def _entries_may_change(self) -> bool:
        """Whether a new provider turn started now may carry other web tool entries than the
        requests the run's thinking was made under.

        The core writes the provider's web tool entries for a new turn from what the run has
        left (docs/WEB_RESEARCH.md, section 3): once the run made a web call, a turn started
        within the run carries less, or leaves a used-up tool out. Two things start such a
        turn: the look nudge, and the model library's own retry after an answer that held
        no usable output (only thinking, or nothing), which it sends as feedback in a
        message of its own (`RetryFeedbackPart`; every other retry answers a tool call as
        its result and goes on with the turn). A provider that binds its thinking to the
        tool list refuses thinking made under other entries, and the next message's first
        request again carries the entries the run began with. So from such a turn on the run
        sends and keeps no thinking at all (`_entries_changed`): removing every thinking
        block is what the provider accepts. Without a web call this run, nothing differs and
        nothing goes.
        """
        return (self._research.search and self._research.service is None
                and bool(self._web_started))

    def _kept_history(self, messages: list[ModelMessage]) -> list[ModelMessage]:
        messages = with_pictures_capped(messages)
        if self._research.search:
            messages = self._kept_web_results(messages)
        if (not self._entries_changed and messages and isinstance(messages[-1], ModelRequest)
                and any(isinstance(part, RetryFeedbackPart) for part in messages[-1].parts)
                and self._entries_may_change()):
            log.info("run %s: a retry starts a new turn after a web call; thinking is left out "
                     "from here on", self.run_id)
            self._entries_changed = True
        if self._entries_changed:
            messages = without_any_thinking(messages)
        return messages

    async def run(self) -> dict[str, Any]:
        """Runs the agent loop to its end and returns the `agent.run` result."""
        try:
            history = ModelMessagesTypeAdapter.validate_python(self._history_in)
        except ValidationError:
            return self._error(
                "This project's conversation was saved in a form this version of Hotend "
                "cannot read, so it cannot be continued. Start a new project for this design."
            )
        shape = request_shape(self._instructions, self._tool_specs, self._research,
                              self._model_label)
        if history and self._research.stored_shape not in (None, shape):
            # The instructions or the tools changed since this conversation was stored
            # (research switched, a new prompt): thinking parts made under the old ones
            # would be rejected, so the conversation goes on without them.
            history = without_thinking(history)
        log.info("run %s: model %s, %d tools, %d history messages, max %d turns, prompt %s, "
                 "web research %s",
                 self.run_id, self._model_label, len(self._tools), len(history),
                 self._max_turns, PROMPT_VERSION,
                 "off" if not self._research.search
                 else "search and fetch" if self._research.fetch else "search")
        agent: Agent[None, str] = Agent(
            self._model,
            instructions=self._instructions,
            tools=self._tools,
            model_settings=self._settings,
            capabilities=self._capabilities,
            retries=2,
        )
        stop_reason = "done"
        usage = RunUsage()
        with capture_run_messages() as captured:
            try:
                result = await agent.run(
                    self._user_text,
                    message_history=history,
                    usage_limits=UsageLimits(request_limit=self._max_turns),
                    cancellation_token=self._cancel,
                    event_stream_handler=self._on_events,
                )
                messages = result.all_messages()
                usage = result.usage
                if self.missing_look_line(messages) and usage.requests < self._max_turns:
                    # Asked once, appended as a new message: nothing sent before changes,
                    # except thinking when the web tool entries may differ (see
                    # `_entries_may_change`; `_kept_history` then leaves it out).
                    log.info("run %s: the answer does not say what was looked at; asking once",
                             self.run_id)
                    self._entries_changed = self._entries_changed or self._entries_may_change()
                    result = await agent.run(
                        LOOK_NUDGE,
                        message_history=messages,
                        usage_limits=UsageLimits(request_limit=self._max_turns),
                        usage=usage,
                        cancellation_token=self._cancel,
                        event_stream_handler=self._on_events,
                    )
                    messages = result.all_messages()
                    usage = result.usage
            except RunCancelled as error:
                stop_reason = "cancelled"
                messages = error.all_messages() or history
                usage = error.usage
            except UsageLimitExceeded:
                stop_reason = "budget"
                messages = list(captured) or history
            except ChannelClosed:
                raise
            except ModelAPIError as error:
                return self._error(self._model_error(error))
            except ScriptError as error:
                return self._error(str(error))
            except (AgentRunError, UserError) as error:
                return self._error(f"The AI model's answer could not be used: {error}")
        if self._last_usage is not None and stop_reason != "done":
            usage_out = self._last_usage
        else:
            usage_out = self._usage_now(usage)
        log.info("run %s finished: %s after %d model requests and %d tool calls",
                 self.run_id, stop_reason, usage_out["requests"], usage_out["tool_calls"])
        messages = list(messages)
        if self._entries_changed:
            # Thinking made in a turn with other entries than the next message's first
            # request is not kept, however the run ended (cancelled, out of turns, done).
            messages = without_any_thinking(messages)
        if stop_reason != "done":
            messages = without_unanswered_web_call(messages)
        if self._research.search:
            # The last answer may hold one too, with no request after it.
            messages = self._kept_web_results(messages)
        else:
            # The answers of this run only: an older one was offered in its own run.
            lookup = research_wanted(messages[len(history):])
            if lookup is not None:
                self._emit({"type": "text_delta", "text": "", "research_needed": {
                    "lookup": lookup, "state": self._research.state,
                    "source": self._research.source}})
        return {
            "history": dump_history(messages),
            "stop_reason": stop_reason,
            "usage": usage_out,
            "shape": shape,
        }
