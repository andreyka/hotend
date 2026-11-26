"""The agent's only way to a model: an HTTP transport that goes through the core.

The agent process has no socket. `CoreTransport` turns every HTTP request a provider SDK makes
into an `llm.request` to the core (docs/PROTOCOL.md section 5) and yields the response as the
core streams it back. The core chooses the host, adds the credential and enforces the caps;
this side never holds a key and never sends a credential header.

What the SDK sees:
- an upstream answer, whatever its status, is a normal streamed response;
- a request the core refuses (not allowed, cap used up, no key stored) is a response with
  status 401 or 403, an `x-should-retry: false` header and a JSON body naming the reason, so
  no SDK retries it; the reason is also kept in `CoreTransport.refusal`;
- a request the core could not carry out (provider unreachable, transfer cut off) is an
  `httpx2.ConnectError` or `httpx2.ReadError`, which SDKs treat as a network failure.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import itertools
import json
import logging
from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import Any

import httpx2

from .channel import Channel, ChannelClosed, RpcError, StreamItem

log = logging.getLogger("hotend_agent.transport")

# Never forwarded: the placeholder credentials SDKs insist on, and headers that describe a
# connection this process does not have. The core applies its own allowlist to the rest.
_DROPPED_HEADERS = frozenset({
    "authorization", "x-api-key", "x-goog-api-key", "api-key", "cookie", "proxy-authorization",
    "host", "content-length", "transfer-encoding", "connection", "accept-encoding", "user-agent",
})

#: Egress reasons that mean "this request will never be allowed": answered as an HTTP status.
REFUSAL_STATUS = {
    "no_credential": 401,
    "not_authorised": 403,
    "denied": 403,
    "invalid_request": 403,
    "cap_exceeded": 403,
}


@dataclass(frozen=True)
class Refusal:
    """Why the core refused a model request."""

    code: str
    message: str


class CoreTransport(httpx2.AsyncBaseTransport):
    """Sends requests as `llm.request` over the channel. One instance per run."""

    def __init__(self, channel: Channel, run_id: str, token: str) -> None:
        self._channel = channel
        self._run_id = run_id
        self._token = token
        self._ids = itertools.count(1)
        #: The last refusal of the core, for the error shown to the user.
        self.refusal: Refusal | None = None
        self.requests = 0

    async def handle_async_request(self, request: httpx2.Request) -> httpx2.Response:
        body = await request.aread()
        stream_id = f"llm-{next(self._ids)}"
        path = request.url.raw_path.decode("ascii")
        headers: dict[str, str] = {}
        for name, value in request.headers.items():
            if name.lower() not in _DROPPED_HEADERS:
                headers[name.lower()] = value
        timeouts = request.extensions.get("timeout") or {}
        read_timeout = timeouts.get("read")
        self.requests += 1
        log.info("llm.request %s %s %s (%d bytes)", stream_id, request.method,
                 path.split("?", 1)[0], len(body))
        try:
            items = self._channel.open_stream("llm.request", {
                "run_id": self._run_id,
                "request_id": stream_id,
                "token": self._token,
                "method": request.method,
                "path": path,
                "headers": headers,
                "body_b64": base64.b64encode(body).decode("ascii"),
            }, stream_id)
        except ChannelClosed as error:
            raise httpx2.ConnectError("the core closed the channel", request=request) from error
        try:
            kind, value = await _next(items, read_timeout, request)
        except BaseException:
            self._channel.close_stream(stream_id)
            raise
        if kind == "start":
            status, response_headers = _start(value)
            log.info("llm.response %s status %d", stream_id, status)
            return httpx2.Response(
                status,
                headers=response_headers,
                stream=_CoreStream(self._channel, stream_id, items, read_timeout, request),
                request=request,
            )
        self._channel.close_stream(stream_id)
        if kind == "error":
            return self._failed(value, request)
        # A result without a response: the core never does this.
        raise httpx2.RemoteProtocolError("the core answered without a response", request=request)

    def _failed(self, error: Exception, request: httpx2.Request) -> httpx2.Response:
        """A request the core answered with an error before any response."""
        if isinstance(error, RpcError):
            code = error.data_code or ""
            log.warning("llm.request failed: %s (%d)", code or "error", error.code)
            status = REFUSAL_STATUS.get(code)
            if status is not None:
                self.refusal = Refusal(code, error.message)
                payload = {"error": {"type": "hotend_egress_refused", "code": code,
                                     "message": error.message}}
                return httpx2.Response(
                    status,
                    headers={"content-type": "application/json", "x-should-retry": "false"},
                    content=json.dumps(payload).encode(),
                    request=request,
                )
            raise httpx2.ConnectError(error.message, request=request) from error
        raise httpx2.ConnectError("the core closed the channel", request=request) from error


async def _next(
    items: asyncio.Queue[StreamItem], read_timeout: float | None, request: httpx2.Request
) -> StreamItem:
    if read_timeout is None:
        return await items.get()
    try:
        return await asyncio.wait_for(items.get(), read_timeout)
    except TimeoutError as error:
        raise httpx2.ReadTimeout("the core sent nothing in time", request=request) from error


def _start(params: Any) -> tuple[int, dict[str, str]]:
    status = params.get("status") if isinstance(params, dict) else None
    headers = params.get("headers") if isinstance(params, dict) else None
    if not isinstance(status, int) or not 100 <= status <= 599:
        raise httpx2.RemoteProtocolError("the core sent a response without a valid status")
    clean = {}
    if isinstance(headers, dict):
        clean = {k: v for k, v in headers.items() if isinstance(k, str) and isinstance(v, str)}
    return status, clean


class _CoreStream(httpx2.AsyncByteStream):
    """The body of one response, as the core's `llm.response_chunk` notifications arrive."""

    def __init__(
        self,
        channel: Channel,
        stream_id: str,
        items: asyncio.Queue[StreamItem],
        read_timeout: float | None,
        request: httpx2.Request,
    ) -> None:
        self._channel = channel
        self._stream_id = stream_id
        self._items = items
        self._read_timeout = read_timeout
        self._request = request

    async def __aiter__(self) -> AsyncIterator[bytes]:
        try:
            while True:
                kind, value = await _next(self._items, self._read_timeout, self._request)
                if kind == "chunk":
                    try:
                        data = base64.b64decode(value, validate=True)
                    except (binascii.Error, TypeError, ValueError) as error:
                        raise httpx2.RemoteProtocolError(
                            "the core sent a chunk that is not base64", request=self._request
                        ) from error
                    if data:
                        yield data
                elif kind == "end":
                    return
                elif kind == "error":
                    message = value.message if isinstance(value, RpcError) else "channel closed"
                    log.warning("llm.response %s was cut off", self._stream_id)
                    raise httpx2.ReadError(message, request=self._request) from value
        finally:
            self._channel.close_stream(self._stream_id)

    async def aclose(self) -> None:
        self._channel.close_stream(self._stream_id)
