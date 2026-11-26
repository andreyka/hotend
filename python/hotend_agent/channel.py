"""The JSON-RPC 2.0 line channel to the core (docs/PROTOCOL.md section 5).

Guarantees:
- Only complete JSON-RPC lines are written to the channel's output, one message per line, in
  the order they were sent. Writing never blocks the event loop: a writer thread owns the
  descriptor.
- Reading is a thread on the raw descriptor, so the loop never blocks on the core either and
  no buffered standard stream is held at interpreter shutdown.
- Requests to the core (`tool.call`, `llm.request`) are matched to their responses by id. The
  notifications that carry a streamed model response are delivered, in order, to the queue of
  the request they belong to, followed by the request's own result.
- Nothing here logs a message body.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import queue
import threading
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

log = logging.getLogger("hotend_agent.channel")

#: Requests to the core in flight at once. The core refuses more than eight.
MAX_IN_FLIGHT = 4

_READ_SIZE = 1 << 16


class ChannelClosed(Exception):
    """The core closed its end; nothing more can be sent or received."""


class RpcError(Exception):
    """The core answered a request with a JSON-RPC error."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data

    @property
    def data_code(self) -> str | None:
        """`error.data.code`: the egress module's reason, when there is one."""
        if isinstance(self.data, dict) and isinstance(self.data.get("code"), str):
            return self.data["code"]
        return None


@dataclass(frozen=True)
class Incoming:
    """A request from the core (`agent.run`, `agent.cancel`), or the end of its input."""

    method: str
    id: Any = None
    params: Any = None


#: Put on the inbox when the core closes the agent's input.
EOF = Incoming(method="<eof>")

# What a stream queue carries: ("start", {status, headers}), ("chunk", bytes),
# ("end", None) or ("error", RpcError | ChannelClosed).
StreamItem = tuple[str, Any]


def encode_line(message: dict[str, Any]) -> bytes:
    """One message as one line of UTF-8 JSON. Characters that cannot be encoded (lone
    surrogates from a model's output) are replaced, so the line is always valid for the core."""
    text = json.dumps(message, ensure_ascii=False, separators=(",", ":"))
    return text.encode("utf-8", "replace") + b"\n"


class Channel:
    """The agent's end of the channel. Create and use it inside a running event loop."""

    def __init__(self, read_fd: int, write_fd: int) -> None:
        self._read_fd = read_fd
        self._write_fd = write_fd
        self._loop = asyncio.get_running_loop()
        self._outbox: queue.SimpleQueue[bytes | None] = queue.SimpleQueue()
        self._pending: dict[int, Callable[[dict[str, Any] | None], None]] = {}
        self._streams: dict[str, asyncio.Queue[StreamItem]] = {}
        self._next_id = 1000
        self._closed = False
        self._slots = asyncio.Semaphore(MAX_IN_FLIGHT)
        self.inbox: asyncio.Queue[Incoming] = asyncio.Queue()
        self._reader = threading.Thread(target=self._read_loop, name="core-reader", daemon=True)
        self._writer = threading.Thread(target=self._write_loop, name="core-writer", daemon=True)

    def start(self) -> None:
        self._reader.start()
        self._writer.start()

    # -- writing ---------------------------------------------------------------------------

    def _write_loop(self) -> None:
        while True:
            line = self._outbox.get()
            if line is None:
                return
            view = memoryview(line)
            try:
                while view:
                    written = os.write(self._write_fd, view)
                    view = view[written:]
            except OSError:
                # The core is gone. Keep draining so senders never notice a full queue.
                self._loop_call(self._on_eof)
                while self._outbox.get() is not None:
                    pass
                return

    def _send(self, message: dict[str, Any]) -> None:
        if self._closed:
            raise ChannelClosed
        self._outbox.put(encode_line(message))

    def notify(self, method: str, params: dict[str, Any]) -> None:
        """Sends a notification. Dropped silently when the core is gone."""
        try:
            self._send({"jsonrpc": "2.0", "method": method, "params": params})
        except ChannelClosed:
            pass

    def respond(self, request_id: Any, result: Any) -> None:
        self._send({"jsonrpc": "2.0", "id": request_id, "result": result})

    def respond_error(self, request_id: Any, code: int, message: str) -> None:
        error = {"code": code, "message": message[:2000]}
        self._send({"jsonrpc": "2.0", "id": request_id, "error": error})

    def flush(self, timeout: float = 5.0) -> None:
        """Stops the writer after everything queued so far is written. Blocks; call it last."""
        self._closed = True
        self._outbox.put(None)
        self._writer.join(timeout)

    # -- requests to the core --------------------------------------------------------------

    def _register(self, resolve: Callable[[dict[str, Any] | None], None]) -> int:
        self._next_id += 1
        self._pending[self._next_id] = resolve
        return self._next_id

    async def request(self, method: str, params: dict[str, Any]) -> Any:
        """Sends a request and returns its result. Raises RpcError or ChannelClosed."""
        async with self._slots:
            done: asyncio.Future[dict[str, Any] | None] = self._loop.create_future()

            def resolve(message: dict[str, Any] | None) -> None:
                if not done.done():
                    done.set_result(message)

            request_id = self._register(resolve)
            try:
                self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
                message = await done
            finally:
                self._pending.pop(request_id, None)
        if message is None:
            raise ChannelClosed
        if "error" in message:
            raise _rpc_error(message["error"])
        return message.get("result")

    def open_stream(
        self, method: str, params: dict[str, Any], stream_id: str
    ) -> asyncio.Queue[StreamItem]:
        """Sends a request whose answer is streamed: notifications that carry `request_id ==
        stream_id` arrive on the returned queue, then ("end", None) or ("error", exception)
        for the request's own result. Call `close_stream` when the queue is no longer read."""
        items: asyncio.Queue[StreamItem] = asyncio.Queue()

        def resolve(message: dict[str, Any] | None) -> None:
            if message is None:
                items.put_nowait(("error", ChannelClosed()))
            elif "error" in message:
                items.put_nowait(("error", _rpc_error(message["error"])))
            else:
                items.put_nowait(("end", None))

        request_id = self._register(resolve)
        self._streams[stream_id] = items
        try:
            self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        except ChannelClosed:
            self._pending.pop(request_id, None)
            self._streams.pop(stream_id, None)
            raise
        return items

    def close_stream(self, stream_id: str) -> None:
        """Stops delivering a stream. What the core still sends for it is dropped."""
        self._streams.pop(stream_id, None)

    # -- reading ---------------------------------------------------------------------------

    def _loop_call(self, callback: Callable[..., None], *args: Any) -> None:
        try:
            self._loop.call_soon_threadsafe(callback, *args)
        except RuntimeError:
            pass  # the loop is closed: the process is exiting

    def _read_loop(self) -> None:
        buffer = bytearray()
        while True:
            try:
                data = os.read(self._read_fd, _READ_SIZE)
            except OSError:
                data = b""
            if not data:
                break
            buffer += data
            start = 0
            while (end := buffer.find(b"\n", start)) >= 0:
                line = bytes(buffer[start:end])
                start = end + 1
                if line.strip():
                    self._loop_call(self._on_line, line)
            del buffer[:start]
        self._loop_call(self._on_eof)

    def _on_eof(self) -> None:
        if self._closed:
            return
        self._closed = True
        for resolve in list(self._pending.values()):
            resolve(None)
        self._pending.clear()
        self.inbox.put_nowait(EOF)

    def _on_line(self, line: bytes) -> None:
        try:
            message = json.loads(line)
        except ValueError:
            log.warning("dropped a line from the core that is not JSON (%d bytes)", len(line))
            return
        if not isinstance(message, dict):
            log.warning("dropped a line from the core that is not a JSON object")
            return
        method = message.get("method")
        if isinstance(method, str):
            self._on_call(method, message)
            return
        resolve = self._pending.pop(message.get("id"), None) if _hashable(message) else None
        if resolve is None:
            log.warning("dropped a response from the core with an unknown id")
            return
        resolve(message)

    def _on_call(self, method: str, message: dict[str, Any]) -> None:
        params = message.get("params")
        if method in ("llm.response_start", "llm.response_chunk"):
            stream_id = params.get("request_id") if isinstance(params, dict) else None
            items = self._streams.get(stream_id) if isinstance(stream_id, str) else None
            if items is None:
                return  # the request was abandoned
            if method == "llm.response_start":
                items.put_nowait(("start", params))
            else:
                items.put_nowait(("chunk", params.get("data_b64", "")))
        elif "id" in message:
            self.inbox.put_nowait(Incoming(method=method, id=message["id"], params=params))
        else:
            log.warning("ignored an unknown notification from the core")


def _hashable(message: dict[str, Any]) -> bool:
    return isinstance(message.get("id"), (int, str))


def _rpc_error(error: Any) -> RpcError:
    if not isinstance(error, dict):
        return RpcError(-32603, "the core returned a malformed error")
    code = error.get("code")
    message = error.get("message")
    return RpcError(
        code if isinstance(code, int) else -32603,
        message if isinstance(message, str) else "unknown error",
        error.get("data"),
    )
