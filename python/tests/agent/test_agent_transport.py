"""The core transport against a fake core: what a provider SDK's HTTP client sees.

Every test drives a plain `httpx2.AsyncClient` whose transport is `CoreTransport`, connected
by pipes to `FakeCore`, which answers `llm.request` the way the core's egress does.
"""

from __future__ import annotations

import asyncio
import json
import threading
from typing import Any

import httpx2
import pytest
from agent_fake_core import TOKEN, FakeCore, LlmReply, in_process

from hotend_agent.channel import MAX_IN_FLIGHT
from hotend_agent.transport import CoreTransport

pytestmark = pytest.mark.timeout(60)

BASE = "https://api.anthropic.com"


def client_for(core: FakeCore, channel: Any, **kwargs: Any) -> tuple[httpx2.AsyncClient, CoreTransport]:
    transport = CoreTransport(channel, core.run_id, TOKEN)
    return httpx2.AsyncClient(transport=transport, base_url=BASE, **kwargs), transport


def test_a_response_is_streamed_as_the_core_sends_it() -> None:
    started, release = threading.Event(), threading.Event()
    body = [b"data: one\n\n", b"data: two\n\n", bytes(range(256)) * 200]  # the last is 51 kB

    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(status=200, headers={"content-type": "text/event-stream",
                                             "request-id": "req_1"},
                        chunks=body, started=started, release=release)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, _ = client_for(core, channel)
            async with client.stream("POST", "/v1/messages", json={"max_tokens": 5}) as response:
                # Status and headers are there before any of the body was sent.
                assert started.is_set() and not release.is_set()
                assert response.status_code == 200
                assert response.headers["content-type"] == "text/event-stream"
                assert response.headers["request-id"] == "req_1"
                release.set()
                pieces = [piece async for piece in response.aiter_raw()]
            assert b"".join(pieces) == b"".join(body)
            # The fake core cuts chunks at 16 kB like the real one; they arrive one by one.
            assert pieces[:2] == body[:2] and len(pieces) == 2 + 4
            assert core.violations == []

    asyncio.run(scenario())


def test_the_request_carries_the_run_token_path_and_body_but_no_credentials() -> None:
    payload = bytes(range(256)) * 40

    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(chunks=[b"ok"])

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, transport = client_for(core, channel)
            first = await client.post(
                "/v1/messages?beta=true", content=payload,
                headers={"x-api-key": "sk-secret-1", "authorization": "Bearer sk-secret-2",
                         "x-goog-api-key": "sk-secret-3", "cookie": "session=sk-secret-4",
                         "anthropic-version": "2026-10-21",
                         "content-type": "application/octet-stream"},
            )
            second = await client.get("/v1/models")
            assert first.content == b"ok" and second.content == b"ok"
            one, two = core.llm_requests
            assert one["run_id"] == core.run_id and one["token"] == TOKEN
            assert one["method"] == "POST" and one["path"] == "/v1/messages?beta=true"
            assert one["body"] == payload
            assert one["headers"]["anthropic-version"] == "2026-10-21"
            assert one["headers"]["content-type"] == "application/octet-stream"
            sent = json.dumps(core.llm_requests, default=repr)
            assert "sk-secret" not in sent
            assert not {"authorization", "x-api-key", "x-goog-api-key", "cookie", "host",
                        "content-length", "accept-encoding"} & set(one["headers"])
            assert two["method"] == "GET" and two["path"] == "/v1/models" and two["body"] == b""
            assert one["request_id"] != two["request_id"]
            assert transport.requests == 2 and transport.refusal is None
            assert core.violations == []

    asyncio.run(scenario())


def test_an_upstream_error_status_is_a_normal_response() -> None:
    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(status=429, headers={"content-type": "application/json",
                                             "retry-after": "7"},
                        chunks=[b'{"error": {"message": "slow down"}}'])

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, transport = client_for(core, channel)
            response = await client.post("/v1/messages", json={})
            assert response.status_code == 429
            assert response.headers["retry-after"] == "7"
            assert response.json() == {"error": {"message": "slow down"}}
            assert transport.refusal is None

    asyncio.run(scenario())


@pytest.mark.parametrize(("code", "status"), [
    ("denied", 403), ("cap_exceeded", 403), ("not_authorised", 403), ("invalid_request", 403),
    ("no_credential", 401),
])
def test_a_refusal_of_the_core_is_a_response_no_sdk_retries(code: str, status: int) -> None:
    message = f"the request was refused: {code}"

    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(refuse=(code, message))

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, transport = client_for(core, channel)
            response = await client.post("/v1/messages", json={})
            assert response.status_code == status
            assert response.headers["x-should-retry"] == "false"
            assert response.json()["error"] == {
                "type": "hotend_egress_refused", "code": code, "message": message}
            assert transport.refusal is not None
            assert (transport.refusal.code, transport.refusal.message) == (code, message)

    asyncio.run(scenario())


def test_a_wrong_token_is_refused_by_the_core() -> None:
    async def scenario() -> None:
        async with in_process() as (core, channel):
            transport = CoreTransport(channel, core.run_id, "not-the-token")
            client = httpx2.AsyncClient(transport=transport, base_url=BASE)
            response = await client.post("/v1/messages", json={})
            assert response.status_code == 403
            assert transport.refusal is not None and transport.refusal.code == "not_authorised"

    asyncio.run(scenario())


@pytest.mark.parametrize("code", ["upstream_error", "setup_error", "secret_store_error"])
def test_a_request_the_core_could_not_send_is_a_connection_error(code: str) -> None:
    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(refuse=(code, "the provider request failed: could not connect"))

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, transport = client_for(core, channel)
            with pytest.raises(httpx2.ConnectError, match="could not connect"):
                await client.post("/v1/messages", json={})
            assert transport.refusal is None

    asyncio.run(scenario())


def test_a_response_cut_off_midway_is_a_read_error() -> None:
    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(chunks=[b"data: partial\n\n"], cut_off=True)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, _ = client_for(core, channel)
            got = []
            with pytest.raises(httpx2.ReadError, match="cut off"):
                async with client.stream("POST", "/v1/messages", json={}) as response:
                    assert response.status_code == 200
                    async for piece in response.aiter_raw():
                        got.append(piece)
            assert got == [b"data: partial\n\n"]
            assert channel._streams == {}

    asyncio.run(scenario())


def test_cancelling_a_reader_drops_the_rest_of_the_stream() -> None:
    started, release = threading.Event(), threading.Event()
    replies = [
        LlmReply(chunks=[b"never read"] * 50, started=started, release=release),
        LlmReply(chunks=[b"second answer"]),
    ]

    def llm(params: dict[str, Any]) -> LlmReply:
        return replies.pop(0)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, _ = client_for(core, channel)

            async def read_all() -> bytes:
                response = await client.post("/v1/messages", json={})
                return response.content

            reader = asyncio.create_task(read_all())
            assert await asyncio.to_thread(started.wait, 10)
            reader.cancel()
            with pytest.raises(asyncio.CancelledError):
                await reader
            assert channel._streams == {}, "the stream is no longer delivered"
            release.set()  # the core now sends 50 chunks and a result nobody waits for
            response = await client.post("/v1/messages", json={})
            assert response.content == b"second answer"
            await asyncio.sleep(0.2)
            assert channel._streams == {} and channel._pending == {}
            assert core.violations == []

    asyncio.run(scenario())


def test_a_silent_core_is_a_read_timeout() -> None:
    started, release = threading.Event(), threading.Event()

    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(chunks=[b"late"], started=started, release=release)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, _ = client_for(core, channel, timeout=httpx2.Timeout(0.3))
            try:
                with pytest.raises(httpx2.ReadTimeout):
                    await client.post("/v1/messages", json={})
                assert channel._streams == {}
            finally:
                release.set()

    asyncio.run(scenario())


def test_requests_in_flight_together_get_their_own_answers() -> None:
    def llm(params: dict[str, Any]) -> LlmReply:
        tag = json.loads(params["body"])["tag"]
        return LlmReply(chunks=[f"answer {tag} ".encode()] * 30)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, _ = client_for(core, channel)
            count = MAX_IN_FLIGHT + 4
            responses = await asyncio.gather(*(
                client.post("/v1/messages", json={"tag": n}) for n in range(count)
            ))
            for n, response in enumerate(responses):
                assert response.content == f"answer {n} ".encode() * 30
            assert len({r["request_id"] for r in core.llm_requests}) == count
            assert core.violations == []

    asyncio.run(scenario())


def test_the_core_going_away_ends_a_waiting_request() -> None:
    started, release = threading.Event(), threading.Event()

    def llm(params: dict[str, Any]) -> LlmReply:
        return LlmReply(chunks=[b"x"], started=started, release=release)

    async def scenario() -> None:
        async with in_process(llm=llm) as (core, channel):
            client, _ = client_for(core, channel)
            async with client.stream("POST", "/v1/messages", json={}) as response:
                assert await asyncio.to_thread(started.wait, 10)
                channel._on_eof()  # what the reader thread does when the core closes its end
                with pytest.raises(httpx2.ReadError):
                    async for _ in response.aiter_raw():
                        pass
            with pytest.raises(httpx2.ConnectError):
                await client.post("/v1/messages", json={})
            release.set()

    asyncio.run(scenario())
