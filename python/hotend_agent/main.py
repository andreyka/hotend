"""The agent process: serves one `agent.run` over stdin and stdout, then exits.

Guarantees:
- Standard output carries nothing but JSON-RPC lines. The descriptor is taken over at start
  and whatever any library prints to stdout afterwards goes to stderr instead.
- Logs go to stderr and never contain a message body, a prompt, a tool argument or a result.
- `agent.cancel` ends the run with `stop_reason: "cancelled"`, also when the run does not wind
  down by itself. When stdin closes the process stops at once, without an answer.
- A second `agent.run` and unknown methods are answered with JSON-RPC errors.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
import os
import sys
import traceback
from typing import Any

from .channel import EOF, Channel, ChannelClosed, Incoming

log = logging.getLogger("hotend_agent")

SCRIPT_ENV = "HOTEND_AGENT_SCRIPT"
#: Seconds a cancelled run gets to wind down before it is answered for. The core waits 5.
CANCEL_GRACE = 3.0

INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603

_RUN_DONE = "<run-done>"


def claim_stdout() -> int:
    """Returns a private descriptor for the channel and points fd 1 at stderr, so a stray
    `print` anywhere in the process cannot corrupt the channel."""
    with contextlib.suppress(Exception):
        sys.stdout.flush()
    private = os.dup(1)
    try:
        os.dup2(2, 1)
    except OSError:  # no stderr: discard instead
        sink = os.open(os.devnull, os.O_WRONLY)
        os.dup2(sink, 1)
        os.close(sink)
    sys.stdout = sys.stderr
    return private


async def serve(read_fd: int, write_fd: int, script_source: str | None) -> int:
    """Serves the channel until the run is answered or the core closes it. Returns the exit
    code."""
    channel = Channel(read_fd, write_fd)
    channel.start()
    code, stuck = 0, False
    try:
        code, stuck = await _serve(channel, script_source)
    except ChannelClosed:
        pass
    finally:
        channel.flush()
    if stuck:
        # A run that ignores its cancellation would keep the event loop from closing.
        _hard_exit(code)
    return code


def _hard_exit(code: int) -> None:
    os._exit(code)


async def _serve(channel: Channel, script_source: str | None) -> tuple[int, bool]:
    """Returns the exit code and whether a run is still going on that could not be stopped."""
    # Imported here: the channel answers within milliseconds, the agent stack takes longer.
    from .runner import BadRunParams, Session
    from .scripted import ScriptError, load_script

    session: Session | None = None
    task: asyncio.Task[dict[str, Any]] | None = None
    run_request_id: Any = None
    cancel_timer: asyncio.TimerHandle | None = None
    loop = asyncio.get_running_loop()

    while True:
        incoming = await channel.inbox.get()
        if incoming is EOF:
            log.info("the core closed the channel; stopping")
            if task is not None:
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError, Exception):
                    await task
            return 0, False

        if incoming.method == _RUN_DONE:
            assert task is not None and session is not None
            if cancel_timer is not None:
                cancel_timer.cancel()
            if incoming.params == "grace-expired":
                log.warning("the cancelled run did not stop in time; answering for it")
                task.cancel()
                channel.respond(run_request_id, session.cancelled_result())
                return 0, True
            try:
                result = task.result()
            except ChannelClosed:
                return 0, False
            except Exception as error:  # noqa: BLE001 - any failure must still be answered
                # Where it failed, not what it said: an exception's text can quote a body.
                frames = "; ".join(
                    f"{frame.filename}:{frame.lineno} in {frame.name}"
                    for frame in traceback.extract_tb(error.__traceback__)[-6:]
                )
                log.error("the run failed with %s at %s", type(error).__name__, frames)
                channel.respond_error(
                    run_request_id, INTERNAL_ERROR,
                    f"the agent failed internally ({type(error).__name__})",
                )
                return 1, False
            channel.respond(run_request_id, result)
            return 0, False

        if incoming.method == "agent.run":
            if session is not None:
                channel.respond_error(incoming.id, INVALID_REQUEST, "a run is already going on")
                continue
            try:
                script = load_script(script_source) if script_source else None
                session = Session(channel, incoming.params, script)
            except (BadRunParams, ScriptError) as error:
                log.warning("agent.run was refused: %s", type(error).__name__)
                channel.respond_error(incoming.id, INVALID_PARAMS, str(error))
                return 2, False
            run_request_id = incoming.id
            task = asyncio.create_task(session.run())
            task.add_done_callback(
                lambda _: channel.inbox.put_nowait(Incoming(method=_RUN_DONE))
            )
        elif incoming.method == "agent.cancel":
            channel.respond(incoming.id, {})
            params = incoming.params if isinstance(incoming.params, dict) else {}
            if session is None or params.get("run_id") != session.run_id:
                continue
            log.info("run %s: cancel requested", session.run_id)
            session.cancel()
            if cancel_timer is None:
                cancel_timer = loop.call_later(
                    CANCEL_GRACE, channel.inbox.put_nowait,
                    Incoming(method=_RUN_DONE, params="grace-expired"),
                )
        else:
            channel.respond_error(
                incoming.id, METHOD_NOT_FOUND,
                "unknown method; the agent answers agent.run and agent.cancel",
            )


def main(argv: list[str] | None = None) -> int:
    write_fd = claim_stdout()
    # The banner of the agent library is for people at a terminal.
    os.environ.setdefault("PYDANTIC_AI_NO_BANNER", "1")
    logging.basicConfig(
        stream=sys.stderr, level=logging.WARNING,
        format="hotend-agent %(levelname)s %(name)s: %(message)s",
    )
    log.setLevel(logging.INFO)

    parser = argparse.ArgumentParser(prog="hotend_agent", description=__doc__)
    parser.add_argument(
        "--script", default=os.environ.get(SCRIPT_ENV) or None,
        help="development and demo only: replay this conversation script instead of calling "
             f"a model (a path, or builtin:<name>; default ${SCRIPT_ENV})",
    )
    args = parser.parse_args(argv)
    try:
        return asyncio.run(serve(sys.stdin.fileno(), write_fd, args.script))
    except KeyboardInterrupt:
        return 130
