"""Scripted mode: a model that replays a written conversation, for tests and the offline demo.

With a script the agent runs its whole loop (history, tools through the core, events, budget,
cancel) without any provider: no `llm.request` is sent. The core passes a script only in
development and demo mode (`--script <path>` or `HOTEND_AGENT_SCRIPT`).

A script is JSON:

    {"protocol": 0, "name": "...", "description": "...",
     "user_messages": ["...", ...],
     "turns": [{"expect": ["text", ...], "say": "...",
                "calls": [{"tool": "write_model_code", "args": {...}}]}]}

Each model request plays the next turn; which one is next is the number of model responses
already in the conversation, so a script continues across runs of the same project. A turn
says its text, then makes its tool calls; a turn without calls ends the run. `expect` lists
texts that must all appear in what the model was just sent (the results of the previous turn's
tool calls, or the user's message); a miss ends the run with an error instead of playing on
with a conversation that no longer matches. After the last turn the model only says that the
script is over.

`user_messages` is optional and is for whoever drives the script (the demo, a test, the
evaluation self-check): the user's side of the conversation, one message per run, in order.
The agent never sends them; it only checks that there is one for each turn that ends a run.
"""

from __future__ import annotations

import json
import re
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

from pydantic import BaseModel, ConfigDict, Field, model_validator
from pydantic_ai.messages import (
    BinaryContent,
    ModelMessage,
    ModelRequest,
    ModelResponse,
    TextPart,
    ToolCallPart,
    ToolReturnPart,
    UserPromptPart,
)
from pydantic_ai.models.function import (
    AgentInfo,
    DeltaToolCall,
    DeltaToolCalls,
    FunctionModel,
)

SCRIPT_PROTOCOL = 0
MAX_SCRIPT_BYTES = 4 * 1024 * 1024
BUILTIN_PREFIX = "builtin:"
SCRIPTS_DIR = Path(__file__).parent / "scripts"
END_OF_SCRIPT = "(The scripted demo has no more turns.)"


class ScriptError(Exception):
    """The script cannot be loaded, or the conversation no longer matches it."""


class Call(BaseModel):
    model_config = ConfigDict(extra="forbid")
    tool: str = Field(min_length=1)
    args: dict[str, Any] = Field(default_factory=dict)


class Turn(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expect: list[str] = Field(default_factory=list)
    say: str = ""
    calls: list[Call] = Field(default_factory=list)


class Script(BaseModel):
    model_config = ConfigDict(extra="forbid")
    protocol: int
    name: str
    description: str = ""
    #: The user's side, one message per run; empty when the script does not say.
    user_messages: list[str] = Field(default_factory=list)
    turns: list[Turn] = Field(min_length=1)

    @model_validator(mode="after")
    def _one_message_per_run(self) -> Script:
        runs = sum(not turn.calls for turn in self.turns)
        if self.user_messages and len(self.user_messages) != runs:
            raise ValueError(
                f"`user_messages` has {len(self.user_messages)} messages, but {runs} turns "
                "end a run (a turn without calls); there must be one message for each"
            )
        return self


def load_script(source: str) -> Script:
    """Loads a script from a path, or a shipped one from `builtin:<name>`."""
    if source.startswith(BUILTIN_PREFIX):
        name = source[len(BUILTIN_PREFIX):]
        if not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", name):
            raise ScriptError(f"`{source}` is not the name of a shipped script")
        path = SCRIPTS_DIR / f"{name}.json"
    else:
        path = Path(source)
    try:
        if path.stat().st_size > MAX_SCRIPT_BYTES:
            raise ScriptError(f"the script {path.name} is larger than {MAX_SCRIPT_BYTES} bytes")
        script = Script.model_validate(json.loads(path.read_bytes()))
    except OSError as error:
        raise ScriptError(f"cannot read the script {path.name}: {error.strerror}") from error
    except ValueError as error:
        raise ScriptError(f"the script {path.name} is not valid: {error}") from error
    if script.protocol != SCRIPT_PROTOCOL:
        raise ScriptError(f"the script {path.name} has protocol {script.protocol}, "
                          f"this agent reads {SCRIPT_PROTOCOL}")
    return script


def _text_of(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, BinaryContent):
        return f"[image {content.identifier}]"
    if isinstance(content, list | tuple):
        return "\n".join(_text_of(item) for item in content)
    return json.dumps(content, default=str)


def last_input_text(messages: list[ModelMessage]) -> str:
    """The text of the newest request: tool results and user text, as the model would read it."""
    for message in reversed(messages):
        if isinstance(message, ModelRequest):
            return "\n".join(
                _text_of(part.content)
                for part in message.parts
                if isinstance(part, ToolReturnPart | UserPromptPart)
            )
    return ""


def _next_turn(script: Script, messages: list[ModelMessage]) -> tuple[int, Turn | None]:
    index = sum(isinstance(message, ModelResponse) for message in messages)
    if index >= len(script.turns):
        return index, None
    turn = script.turns[index]
    seen = last_input_text(messages)
    for needle in turn.expect:
        if needle not in seen:
            raise ScriptError(
                f"script `{script.name}`, turn {index + 1}: expected the model's input to "
                f"contain {needle!r}; it starts with {seen[:400]!r}"
            )
    return index, turn


def _call_id(script: Script, turn: int, call: int) -> str:
    return f"{script.name}-{turn + 1}-{call + 1}"


def scripted_model(script: Script) -> FunctionModel:
    """A Pydantic AI model that plays `script`, streaming its text word by word."""

    def respond(messages: list[ModelMessage], info: AgentInfo) -> ModelResponse:
        index, turn = _next_turn(script, messages)
        if turn is None:
            return ModelResponse(parts=[TextPart(END_OF_SCRIPT)])
        parts: list[TextPart | ToolCallPart] = []
        if turn.say:
            parts.append(TextPart(turn.say))
        for number, call in enumerate(turn.calls):
            parts.append(ToolCallPart(call.tool, call.args, _call_id(script, index, number)))
        return ModelResponse(parts=parts or [TextPart("")])

    async def stream(
        messages: list[ModelMessage], info: AgentInfo
    ) -> AsyncIterator[str | DeltaToolCalls]:
        index, turn = _next_turn(script, messages)
        if turn is None:
            yield END_OF_SCRIPT
            return
        for piece in re.findall(r"\S+\s*|\s+", turn.say):
            yield piece
        for number, call in enumerate(turn.calls):
            yield {number: DeltaToolCall(
                name=call.tool, json_args=json.dumps(call.args),
                tool_call_id=_call_id(script, index, number),
            )}
        if not turn.say and not turn.calls:
            yield ""

    return FunctionModel(respond, stream_function=stream, model_name=f"scripted:{script.name}")
