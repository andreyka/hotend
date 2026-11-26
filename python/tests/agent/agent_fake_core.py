"""A stand-in for the Rust core's end of the agent channel (docs/PROTOCOL.md section 5).

`FakeCore` speaks the protocol over two file descriptors, from threads, the way the supervisor
does: it answers `tool.call` with a handler's outcome and `llm.request` with
`llm.response_start`, `llm.response_chunk` notifications and a result. It records everything
the agent writes, byte for byte, so tests can prove that the channel carries only JSON-RPC.

`AgentProcess` starts the real agent (`python -I -m hotend_agent`) with a `FakeCore` on its
stdin and stdout, optionally inside the real `hotend-sandbox` agent profile. `in_process`
connects a `FakeCore` to a `hotend_agent.channel.Channel` in the test's own event loop.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import json
import os
import subprocess
import sys
import threading
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Self

PYTHON_DIR = Path(__file__).resolve().parents[2]
REPO = PYTHON_DIR.parent
FIXTURES = Path(__file__).resolve().parent / "fixtures"
TOOL_SPECS: list[dict[str, Any]] = json.loads((FIXTURES / "tool_specs.json").read_text("utf-8"))

TOKEN = "ab" * 32
CHUNK = 16 * 1024
EVENT_TYPES = {"text_delta", "tool_start", "tool_end", "thinking", "usage"}
MAX_EVENT_BYTES = 64 * 1024

ToolHandler = Callable[[str, dict[str, Any]], dict[str, Any]]
LlmHandler = Callable[[dict[str, Any]], "LlmReply"]


def text_outcome(text: str) -> dict[str, Any]:
    return {"ok": True, "content": [{"type": "text", "text": text}]}


def refusal(code: str, message: str) -> dict[str, Any]:
    return {"ok": False, "error": {"code": code, "message": message}}


@dataclass
class LlmReply:
    """What the fake egress does with one `llm.request`."""

    status: int = 200
    headers: dict[str, str] = field(default_factory=lambda: {"content-type": "text/event-stream"})
    chunks: list[bytes] = field(default_factory=list)
    #: A JSON-RPC error instead of a response: (data.code, message).
    refuse: tuple[str, str] | None = None
    #: A JSON-RPC error after the chunks: the transfer broke.
    cut_off: bool = False
    #: Set between the start and the first chunk; the reply then waits for `release`.
    started: threading.Event | None = None
    release: threading.Event | None = None


def sse(*events: dict[str, Any] | str, named: bool = False) -> bytes:
    """Server-sent events: one `data:` line per event, with an `event:` line when `named`."""
    out = []
    for event in events:
        if isinstance(event, str):
            out.append(f"data: {event}\n\n")
        elif named:
            out.append(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n")
        else:
            out.append(f"data: {json.dumps(event)}\n\n")
    return "".join(out).encode()


def _default_tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
    return text_outcome(f"status: ok\n{name} called")


def _default_llm(params: dict[str, Any]) -> LlmReply:
    return LlmReply(refuse=("denied", "this test expects no model request"))


class FakeCore:
    """The core's side of the channel on two descriptors. Start it with `start()`."""

    def __init__(
        self,
        read_fd: int,
        write_fd: int,
        *,
        tools: ToolHandler = _default_tools,
        llm: LlmHandler = _default_llm,
    ) -> None:
        self._read_fd = read_fd
        self._write_fd = write_fd
        self._tools = tools
        self._llm = llm
        self._write_lock = threading.Lock()
        self.run_id = "run-1"
        #: Every line the agent wrote, without its newline.
        self.lines: list[bytes] = []
        #: Why a line was not acceptable to the real supervisor.
        self.violations: list[str] = []
        self.events: list[dict[str, Any]] = []
        self.tool_calls: list[tuple[str, dict[str, Any]]] = []
        #: Whether each of `tool_calls` carried `auto: true` (the look review's views).
        self.auto_flags: list[bool] = []
        self.llm_requests: list[dict[str, Any]] = []
        self.responses: dict[Any, dict[str, Any]] = {}
        self.result_ready = threading.Event()
        self.closed = threading.Event()
        self._reader = threading.Thread(target=self._read, daemon=True)

    def start(self) -> None:
        self._reader.start()

    # -- sending ---------------------------------------------------------------------------

    def send(self, message: dict[str, Any]) -> None:
        data = (json.dumps(message) + "\n").encode()
        with self._write_lock:
            view = memoryview(data)
            while view:
                view = view[os.write(self._write_fd, view):]

    def send_raw(self, data: bytes) -> None:
        with self._write_lock:
            os.write(self._write_fd, data)

    def run(
        self,
        text: str,
        *,
        history: list[Any] | None = None,
        provider: dict[str, str] | None = None,
        max_turns: int = 24,
        tools: list[dict[str, Any]] | None = None,
        **overrides: Any,
    ) -> None:
        params = {
            "run_id": self.run_id,
            "project_id": "project-1",
            "provider": provider or {"kind": "anthropic", "model": "claude-test",
                                     "base_url": "https://api.anthropic.com"},
            "egress_token": TOKEN,
            "history": history or [],
            "user_message": {"text": text, "attachments": []},
            "tools": TOOL_SPECS if tools is None else tools,
            "budget": {"max_turns": max_turns},
        }
        params.update(overrides)
        self.send({"jsonrpc": "2.0", "id": 1, "method": "agent.run", "params": params})

    def cancel(self) -> None:
        self.send({"jsonrpc": "2.0", "id": 2, "method": "agent.cancel",
                   "params": {"run_id": self.run_id}})

    def result(self, timeout: float = 60) -> dict[str, Any]:
        """The response to `agent.run` (the whole JSON-RPC message)."""
        assert self.result_ready.wait(timeout), (
            f"no answer to agent.run within {timeout} s; events so far: {self.events[-5:]}"
        )
        return self.responses[1]

    # -- receiving -------------------------------------------------------------------------

    def _read(self) -> None:
        buffer = bytearray()
        while True:
            try:
                data = os.read(self._read_fd, 1 << 16)
            except OSError:
                data = b""
            if not data:
                break
            buffer += data
            while (end := buffer.find(b"\n")) >= 0:
                line = bytes(buffer[:end])
                del buffer[:end + 1]
                self.lines.append(line)
                self._on_line(line)
        if buffer:
            self.lines.append(bytes(buffer))
            self.violations.append("output ended without a newline")
        self.closed.set()

    def _on_line(self, line: bytes) -> None:
        try:
            message = json.loads(line)
        except ValueError:
            self.violations.append(f"not JSON: {line[:80]!r}")
            return
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            self.violations.append(f"not JSON-RPC 2.0: {line[:80]!r}")
            return
        method = message.get("method")
        if method is None:
            if message.get("id") not in (1, 2) or ("result" in message) == ("error" in message):
                self.violations.append(f"a response the core did not expect: {line[:80]!r}")
                return
            self.responses[message["id"]] = message
            if message["id"] == 1:
                self.result_ready.set()
        elif "id" not in message:
            params = message.get("params")
            event = params.get("event") if isinstance(params, dict) else None
            if (method != "agent.event" or set(params) != {"run_id", "event"}
                    or params["run_id"] != self.run_id or not isinstance(event, dict)
                    or event.get("type") not in EVENT_TYPES or len(line) > MAX_EVENT_BYTES):
                self.violations.append(f"a malformed notification: {line[:120]!r}")
                return
            self.events.append(event)
        elif method == "tool.call":
            threading.Thread(target=self._tool_call, args=(message,), daemon=True).start()
        elif method == "llm.request":
            threading.Thread(target=self._llm_request, args=(message,), daemon=True).start()
        else:
            self.violations.append(f"unknown method {method!r}")
            self.send({"jsonrpc": "2.0", "id": message["id"],
                       "error": {"code": -32601, "message": "unknown method"}})

    def _tool_call(self, message: dict[str, Any]) -> None:
        params = message.get("params")
        # `auto: true` marks the look review's own views (render_views only).
        keys = {"run_id", "name", "args"}
        if (not isinstance(params, dict)
                or set(params) not in (keys, keys | {"auto"})
                or params.get("auto", True) is not True
                or ("auto" in params and params.get("name") != "render_views")
                or params["run_id"] != self.run_id):
            self.violations.append("a tool.call with invalid params")
            self.send({"jsonrpc": "2.0", "id": message["id"],
                       "error": {"code": -32602, "message": "invalid params"}})
            return
        self.tool_calls.append((params["name"], params["args"]))
        self.auto_flags.append(params.get("auto", False))
        outcome = self._tools(params["name"], params["args"])
        self._try_send({"jsonrpc": "2.0", "id": message["id"], "result": outcome})

    def _llm_request(self, message: dict[str, Any]) -> None:
        params = message.get("params")
        expected = {"run_id", "request_id", "token", "method", "path", "headers", "body_b64"}
        if (not isinstance(params, dict) or set(params) != expected
                or params["run_id"] != self.run_id
                or not all(isinstance(v, str) for v in params["headers"].values())):
            self.violations.append("an llm.request with invalid params")
            self.send({"jsonrpc": "2.0", "id": message["id"],
                       "error": {"code": -32602, "message": "invalid params"}})
            return
        params = dict(params, body=base64.b64decode(params["body_b64"]))
        self.llm_requests.append(params)
        if params["token"] != TOKEN:
            reply = LlmReply(refuse=("not_authorised", "the request does not belong to an open run"))
        else:
            reply = self._llm(params)

        def error(code: str, text: str) -> None:
            self._try_send({"jsonrpc": "2.0", "id": message["id"], "error": {
                "code": -32000, "message": text, "data": {"code": code}}})

        if reply.refuse is not None:
            error(*reply.refuse)
            return
        request_id = params["request_id"]
        self._try_send({"jsonrpc": "2.0", "method": "llm.response_start", "params": {
            "request_id": request_id, "status": reply.status, "headers": reply.headers}})
        if reply.started is not None:
            reply.started.set()
        if reply.release is not None:
            reply.release.wait(30)
        for chunk in reply.chunks:
            for start in range(0, len(chunk), CHUNK):
                self._try_send({"jsonrpc": "2.0", "method": "llm.response_chunk", "params": {
                    "request_id": request_id,
                    "data_b64": base64.b64encode(chunk[start:start + CHUNK]).decode()}})
        if reply.cut_off:
            error("upstream_error", "the provider request failed: the response was cut off")
        else:
            self._try_send({"jsonrpc": "2.0", "id": message["id"], "result": {"ok": True}})

    def _try_send(self, message: dict[str, Any]) -> None:
        try:
            self.send(message)
        except OSError:
            pass  # the agent is gone

    # -- what tests read -------------------------------------------------------------------

    def text(self) -> str:
        """The transcript the core would store: all text deltas joined."""
        return "".join(e["text"] for e in self.events if e["type"] == "text_delta")

    def events_of(self, kind: str) -> list[dict[str, Any]]:
        return [e for e in self.events if e["type"] == kind]


def sandbox_launcher() -> Path | None:
    """The `hotend-sandbox` launcher, when it has been built."""
    override = os.environ.get("HOTEND_SANDBOX_BIN")
    candidates = [Path(override)] if override else [
        REPO / "target" / "debug" / "hotend-sandbox",
        REPO / "target" / "release" / "hotend-sandbox",
    ]
    return next((path for path in candidates if path.is_file()), None)


class AgentProcess:
    """The real agent process with a `FakeCore` on its stdin and stdout.

    Use as a context manager; `core` is the fake core. `script` starts the agent in scripted
    mode, through `--script` or, with `script_env=True`, through `HOTEND_AGENT_SCRIPT`.
    `sandbox` is a job folder: the agent then runs under `hotend-sandbox --profile agent`.
    """

    def __init__(
        self,
        *,
        script: str | None = None,
        script_env: bool = False,
        tools: ToolHandler = _default_tools,
        llm: LlmHandler = _default_llm,
        sandbox: Path | None = None,
        extra_ro: tuple[Path, ...] = (),
    ) -> None:
        command = [sys.executable, "-I", "-m", "hotend_agent"]
        # Nothing of the test's environment: the core starts the agent with an empty one.
        env = {"PATH": os.environ.get("PATH", ""), "LANG": "C.UTF-8"}
        if script is not None and script_env:
            env["HOTEND_AGENT_SCRIPT"] = script
        elif script is not None:
            command += ["--script", script]
        if sandbox is not None:
            launcher = sandbox_launcher()
            assert launcher is not None
            prefix = [str(launcher), "run", "--profile", "agent", "--job", str(sandbox)]
            for path in (Path(sys.prefix), Path(sys.base_prefix), PYTHON_DIR, *extra_ro):
                prefix += ["--ro", str(path.resolve())]
            if script is not None and script_env:
                prefix += ["--env", "HOTEND_AGENT_SCRIPT"]
            command = [*prefix, "--", *command]
        self.process = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=env, cwd=sandbox or PYTHON_DIR,
        )
        assert self.process.stdin and self.process.stdout and self.process.stderr
        self.core = FakeCore(
            self.process.stdout.fileno(), self.process.stdin.fileno(), tools=tools, llm=llm
        )
        self._stderr: list[bytes] = []
        self._stderr_reader = threading.Thread(
            target=lambda: self._stderr.append(self.process.stderr.read()), daemon=True
        )

    def __enter__(self) -> Self:
        self.core.start()
        self._stderr_reader.start()
        return self

    def __exit__(self, *_: object) -> None:
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(10)
        self._stderr_reader.join(5)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            try:
                pipe.close()
            except OSError:
                pass

    def close_stdin(self) -> None:
        self.process.stdin.close()

    def wait(self, timeout: float = 20) -> int:
        """Waits for the process to exit and for its output to be read to the end."""
        code = self.process.wait(timeout)
        assert self.core.closed.wait(10)
        self._stderr_reader.join(10)
        return code

    @property
    def stderr(self) -> str:
        return b"".join(self._stderr).decode("utf-8", "replace")


def run_params(core: FakeCore, text: str, **overrides: Any) -> dict[str, Any]:
    """`agent.run` params as the core would send them."""
    params = {
        "run_id": core.run_id,
        "project_id": "project-1",
        "provider": {"kind": "anthropic", "model": "claude-test",
                     "base_url": "https://api.anthropic.com"},
        "egress_token": TOKEN,
        "history": [],
        "user_message": {"text": text, "attachments": []},
        "tools": TOOL_SPECS,
        "budget": {"max_turns": 24},
    }
    params.update(overrides)
    return params


@contextlib.asynccontextmanager
async def in_process(
    *, tools: ToolHandler = _default_tools, llm: LlmHandler = _default_llm
) -> AsyncIterator[tuple[FakeCore, Any]]:
    """A `FakeCore` and the agent's `Channel`, connected by two pipes, in this event loop."""
    from hotend_agent.channel import Channel

    agent_in, core_out = os.pipe()
    core_in, agent_out = os.pipe()
    core = FakeCore(core_in, core_out, tools=tools, llm=llm)
    core.start()
    channel = Channel(agent_in, agent_out)
    channel.start()
    try:
        yield core, channel
    finally:
        channel.flush()
        os.close(agent_out)
        os.close(core_out)
        await asyncio.to_thread(core.closed.wait, 5)
        await asyncio.sleep(0.05)  # lets the channel's reader see the end of its input
        os.close(core_in)
        os.close(agent_in)


async def finish(core: FakeCore, channel: Any, result: dict[str, Any]) -> dict[str, Any]:
    """Answers `agent.run` the way the process does and waits until the core has read
    everything the agent wrote before it."""
    channel.respond(1, result)
    assert await asyncio.to_thread(core.result_ready.wait, 10)
    return result
