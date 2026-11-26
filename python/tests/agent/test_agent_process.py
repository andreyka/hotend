"""The agent as the core starts it: `python -I -m hotend_agent`, JSON-RPC on stdin and stdout.

`FakeCore` plays the core. Runs use the scripted mode (no provider) unless a test is about a
provider, in which case the fake core answers `llm.request` with a canned model stream.
"""

from __future__ import annotations

import base64
import json
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from agent_canned import PROVIDERS, check_egress, reply
from agent_demo_script import Replay, build
from agent_fake_core import (
    PYTHON_DIR,
    AgentProcess,
    LlmReply,
    refusal,
    sandbox_launcher,
    text_outcome,
)

from hotend_agent.scripted import END_OF_SCRIPT

pytestmark = pytest.mark.timeout(120)

PNG = bytes.fromhex("89504e470d0a1a0a") + b"\x00" * 300
VIEW = {"ok": True, "content": [
    {"type": "text", "text": "views of version 2: iso"},
    {"type": "image", "path": "views/iso.png", "media_type": "image/png",
     "data_b64": base64.b64encode(PNG).decode()},
]}
MODEL_ERROR = (
    "status: model_error\nno version was created; the project still has no version\n"
    "error: ValueError at line 9: radius too big\ntraceback:\n  File \"model.py\", line 9"
)
BUILT = "status: ok\nversion: 1 created and now current\nchecks: 0 fail, 1 warn, 16 pass, 2 skipped"

# A conversation in which the model is refused once, gets a script error once, and corrects
# both: each turn only plays when the previous tool result reached the model.
CORRECTING = {
    "protocol": 0,
    "name": "correcting",
    "turns": [
        {"say": "Trying the slider first.",
         "calls": [{"tool": "set_parameter", "args": {"name": "width", "value": 100}}]},
        {"expect": ["error (no_current_version)", "nothing was changed"],
         "say": "There is no model yet. Writing one.",
         "calls": [{"tool": "write_model_code", "args": {"code": "BROKEN", "note": "first"}}]},
        {"expect": ["status: model_error", "ValueError at line 9: radius too big"],
         "say": "The fillet radius was too big. Fixed.",
         "calls": [{"tool": "write_model_code", "args": {"code": "FIXED", "note": "second"}}]},
        {"expect": ["status: ok", "version: 1 created"],
         "calls": [{"tool": "render_views", "args": {"views": ["iso"]}}]},
        {"expect": ["views of version 2: iso", "[image views/iso.png]"],
         "say": "Done: the part builds and passes its checks."},
    ],
}


def correcting_tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
    if name == "set_parameter":
        return refusal("no_current_version", "the project has no model yet")
    if name == "write_model_code":
        return text_outcome(MODEL_ERROR if args["code"] == "BROKEN" else BUILT)
    return VIEW


def write_script(folder: Path, script: dict[str, Any]) -> str:
    path = folder / "script.json"
    path.write_text(json.dumps(script), encoding="utf-8")
    return str(path)


def result_of(agent: AgentProcess, timeout: float = 60) -> dict[str, Any]:
    message = agent.core.result(timeout)
    assert "result" in message, message
    assert set(message["result"]) <= {"history", "stop_reason", "usage", "error", "shape"}
    assert isinstance(message["result"]["history"], list)
    return message["result"]


def test_a_tool_error_is_fed_back_to_the_model_and_corrected(tmp_path: Path) -> None:
    with AgentProcess(script=write_script(tmp_path, CORRECTING), tools=correcting_tools) as agent:
        agent.core.run("A bracket, please.")
        result = result_of(agent)
        assert agent.wait() == 0
        core = agent.core

    assert result["stop_reason"] == "done", result
    assert "error" not in result
    # The gateway saw the refused call, the broken script, the fixed one, then the views.
    assert core.tool_calls == [
        ("set_parameter", {"name": "width", "value": 100}),
        ("write_model_code", {"code": "BROKEN", "note": "first"}),
        ("write_model_code", {"code": "FIXED", "note": "second"}),
        ("render_views", {"views": ["iso"]}),
    ]
    ends = core.events_of("tool_end")
    assert [(e["name"], e["ok"], e["summary"]) for e in ends] == [
        ("set_parameter", False, "error (no_current_version): the project has no model yet"),
        ("write_model_code", True, "status: model_error"),
        ("write_model_code", True, "status: ok"),
        ("render_views", True, "views of version 2: iso"),
    ]
    assert [e["tool_call_id"] for e in core.events_of("tool_start")] == [
        e["tool_call_id"] for e in ends]
    assert core.text() == (
        "Trying the slider first.\n\nThere is no model yet. Writing one.\n\n"
        "The fillet radius was too big. Fixed.\n\nDone: the part builds and passes its checks."
    )
    usage = core.events_of("usage")[-1]
    assert usage["requests"] == 5 and usage["tool_calls"] == 4
    assert result["usage"] == {k: v for k, v in usage.items() if k != "type"}
    # The history holds what the model was told, the picture included as it was sent (the
    # next message then only appends to what the provider cached).
    stored = json.dumps(result["history"])
    assert "error (no_current_version)" in stored and "radius too big" in stored
    assert base64.b64encode(PNG).decode() in stored
    assert "[image views/iso.png" not in stored
    assert core.violations == []


def test_a_script_that_no_longer_matches_ends_the_run_with_an_error(tmp_path: Path) -> None:
    def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
        return text_outcome("status: ok")  # not the refusal the script expects

    with AgentProcess(script=write_script(tmp_path, CORRECTING), tools=tools) as agent:
        agent.core.run("A bracket, please.")
        result = result_of(agent)
        assert agent.wait() == 0
    assert result["stop_reason"] == "error"
    assert "turn 2" in result["error"] and "no_current_version" in result["error"]
    assert result["history"] == [], "an error leaves the stored history as it was"
    assert len(agent.core.tool_calls) == 1


def test_the_turn_budget_stops_the_run_and_keeps_the_work(tmp_path: Path) -> None:
    script = write_script(tmp_path, CORRECTING)
    with AgentProcess(script=script, tools=correcting_tools) as agent:
        agent.core.run("A bracket, please.", max_turns=3)
        result = result_of(agent)
        assert agent.wait() == 0
        core = agent.core
    assert result["stop_reason"] == "budget", result
    assert "error" not in result
    assert result["usage"]["requests"] == 3
    assert [name for name, _ in core.tool_calls] == [
        "set_parameter", "write_model_code", "write_model_code"]
    history = result["history"]
    assert "FIXED" in json.dumps(history)
    assert core.violations == []

    # The next message picks the conversation up where the budget cut it.
    with AgentProcess(script=script, tools=correcting_tools) as agent:
        agent.core.run("Go on.", history=history)
        result = result_of(agent)
        assert agent.wait() == 0
    assert result["stop_reason"] == "done", result
    assert agent.core.tool_calls == [("render_views", {"views": ["iso"]})]
    assert agent.core.text().endswith("Done: the part builds and passes its checks.")
    assert result["history"][:len(history)] == history


def test_cancel_stops_a_run_that_waits_for_a_tool(tmp_path: Path) -> None:
    waiting, release = threading.Event(), threading.Event()

    def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
        waiting.set()
        release.wait(30)
        return text_outcome("status: ok")

    with AgentProcess(script=write_script(tmp_path, CORRECTING), tools=tools) as agent:
        agent.core.run("A bracket, please.")
        assert waiting.wait(20)
        began = time.monotonic()
        agent.core.cancel()
        result = result_of(agent, timeout=10)
        took = time.monotonic() - began
        assert agent.wait() == 0
        release.set()
        core = agent.core
    assert result["stop_reason"] == "cancelled", result
    assert took < 2.5, f"cancel took {took:.1f} s; the core kills the agent after 5"
    assert core.responses[2] == {"jsonrpc": "2.0", "id": 2, "result": {}}
    assert "A bracket, please." in json.dumps(result["history"])
    assert len(core.tool_calls) == 1
    assert core.violations == []


def test_cancel_stops_a_run_in_the_middle_of_a_model_answer() -> None:
    case = PROVIDERS["anthropic"]
    started, release = threading.Event(), threading.Event()

    def llm(params: dict[str, Any]) -> LlmReply:
        answer = reply(case, "This sentence will never be finished.")
        return LlmReply(chunks=answer.chunks, started=started, release=release)

    with AgentProcess(llm=llm) as agent:
        agent.core.run("A bracket, please.", provider=case.provider)
        assert started.wait(30)
        agent.core.cancel()
        result = result_of(agent, timeout=10)
        assert agent.wait() == 0
        release.set()
    assert result["stop_reason"] == "cancelled", result
    assert agent.core.violations == []


def test_a_cancel_for_another_run_is_acknowledged_and_ignored(tmp_path: Path) -> None:
    with AgentProcess(script=write_script(tmp_path, CORRECTING), tools=correcting_tools) as agent:
        agent.core.send({"jsonrpc": "2.0", "id": 2, "method": "agent.cancel",
                         "params": {"run_id": "some-other-run"}})
        agent.core.run("A bracket, please.")
        result = result_of(agent)
        assert agent.wait() == 0
    assert result["stop_reason"] == "done"
    assert agent.core.responses[2]["result"] == {}


def test_the_agent_exits_when_stdin_closes_before_a_run() -> None:
    with AgentProcess() as agent:
        agent.close_stdin()
        assert agent.wait(10) == 0
    assert agent.core.lines == []


def test_the_agent_exits_when_stdin_closes_during_a_run(tmp_path: Path) -> None:
    waiting, release = threading.Event(), threading.Event()

    def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
        waiting.set()
        release.wait(30)
        return text_outcome("status: ok")

    with AgentProcess(script=write_script(tmp_path, CORRECTING), tools=tools) as agent:
        agent.core.run("A bracket, please.")
        assert waiting.wait(20)
        began = time.monotonic()
        agent.close_stdin()
        assert agent.wait(10) == 0
        assert time.monotonic() - began < 3
        release.set()
    assert 1 not in agent.core.responses, "nobody is left to answer"
    assert agent.core.violations == []


def test_protocol_errors_are_answered_and_do_not_end_the_process(tmp_path: Path) -> None:
    with AgentProcess(script=write_script(tmp_path, CORRECTING), tools=correcting_tools) as agent:
        core = agent.core
        core.send_raw(b"this is not json\n\n[1, 2, 3]\n")
        core.send({"jsonrpc": "2.0", "method": "some.notification", "params": {}})
        core.send({"jsonrpc": "2.0", "id": 77, "result": "an answer to nothing"})
        core.send({"jsonrpc": "2.0", "id": 2, "method": "fs.read", "params": {"path": "/etc"}})
        core.run("A bracket, please.")
        result = result_of(agent)
        assert agent.wait() == 0
    assert result["stop_reason"] == "done"
    assert core.responses[2]["error"]["code"] == -32601
    assert core.violations == []
    assert "not JSON" in agent.stderr


def test_a_second_run_in_the_same_process_is_refused(tmp_path: Path) -> None:
    waiting, release = threading.Event(), threading.Event()

    def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
        waiting.set()
        release.wait(30)
        return refusal("no_current_version", "the project has no model yet")

    script = dict(CORRECTING, turns=CORRECTING["turns"][:1] + [{"say": "Stopping here."}])
    with AgentProcess(script=write_script(tmp_path, script), tools=tools) as agent:
        core = agent.core
        core.run("A bracket, please.")
        assert waiting.wait(20)
        core.send({"jsonrpc": "2.0", "id": 2, "method": "agent.run", "params": {}})
        deadline = time.monotonic() + 10
        while 2 not in core.responses and time.monotonic() < deadline:
            time.sleep(0.01)
        release.set()
        result = result_of(agent)
        assert agent.wait() == 0
    assert core.responses[2]["error"]["code"] == -32600
    assert result["stop_reason"] == "done"


@pytest.mark.parametrize("change", [
    {"provider": {"kind": "skynet", "model": "m", "base_url": "https://example.invalid"}},
    {"history": {"not": "a list"}},
    {"tools": [{"name": "no schema"}]},
    {"budget": {"max_turns": "many"}},
])
def test_unusable_run_params_are_a_json_rpc_error(change: dict[str, Any]) -> None:
    with AgentProcess() as agent:
        agent.core.run("hello", **change)
        message = agent.core.result(30)
        assert agent.wait() == 2
    assert message["error"]["code"] == -32602
    assert agent.core.tool_calls == [] and agent.core.llm_requests == []
    assert agent.core.violations == []


def test_a_missing_script_is_reported_not_guessed(tmp_path: Path) -> None:
    for source in (str(tmp_path / "missing.json"), "builtin:no_such_script", "builtin:../x"):
        with AgentProcess(script=source) as agent:
            agent.core.run("hello")
            message = agent.core.result(30)
            assert agent.wait() == 2
        assert message["error"]["code"] == -32602, message
        assert "script" in message["error"]["message"]


def test_the_script_can_come_from_the_environment() -> None:
    with AgentProcess(script="builtin:cable_box_demo", script_env=True) as agent:
        agent.core.run("A cable box.", max_turns=1)
        result = result_of(agent)
        assert agent.wait() == 0
    assert result["stop_reason"] == "budget"
    assert agent.core.tool_calls == [("list_cards", {})]


def test_after_the_last_turn_the_scripted_model_says_so(tmp_path: Path) -> None:
    script = {"protocol": 0, "name": "short", "turns": [{"say": "Hello."}]}
    with AgentProcess(script=write_script(tmp_path, script)) as agent:
        agent.core.run("One.")
        first = result_of(agent)
        assert agent.wait() == 0
    assert agent.core.text() == "Hello."
    with AgentProcess(script=write_script(tmp_path, script)) as agent:
        agent.core.run("Two.", history=first["history"])
        second = result_of(agent)
        assert agent.wait() == 0
    assert agent.core.text() == END_OF_SCRIPT
    assert second["history"][:len(first["history"])] == first["history"]


def test_stdout_carries_only_json_rpc_and_events_stay_under_the_cores_limit(
    tmp_path: Path,
) -> None:
    long_text = "y" * 150_000 + " the end"    # 150 kB of text in one piece of a model turn
    big_code = "# padding\n" * 15_000         # 150 kB of script in one tool call
    odd = "café   日本 \U0001f600 \x00 \ud83d tab\t"
    script = {"protocol": 0, "name": "big", "turns": [
        {"say": long_text, "calls": [
            {"tool": "write_model_code", "args": {"code": big_code, "note": "n" * 2000}}]},
        {"say": odd},
    ]}

    def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
        return text_outcome("status: ok\n" + "line\n" * 50_000 + odd)

    path = tmp_path / "script.json"
    path.write_text(json.dumps(script), encoding="utf-8")
    with AgentProcess(script=str(path), tools=tools) as agent:
        agent.core.run("Go.")
        result = result_of(agent)
        assert agent.wait() == 0
        core = agent.core

    assert result["stop_reason"] == "done", result
    assert core.violations == []
    assert len(core.lines) > 20
    for line in core.lines:
        message = json.loads(line.decode("utf-8"))        # strict UTF-8, one object per line
        assert message["jsonrpc"] == "2.0"
        if message.get("method") == "agent.event":
            assert len(line) < 64 * 1024
    assert core.text().startswith(long_text[:1000])
    assert core.text().split("\n\n")[0] == long_text
    assert core.tool_calls[0][1]["code"] == big_code, "the arguments reach the core whole"
    start = core.events_of("tool_start")[0]
    assert start["args_omitted"] is True and "args" not in start
    assert len(start["note"]) == 500
    assert len(core.events_of("tool_end")[0]["summary"]) <= 300


def test_logs_go_to_stderr_and_hold_no_message_bodies(tmp_path: Path) -> None:
    def tools(name: str, args: dict[str, Any]) -> dict[str, Any]:
        if name == "set_parameter":
            return refusal("no_current_version", "SECRET-REFUSAL the project has no model yet")
        if name == "write_model_code":
            return text_outcome((MODEL_ERROR if args["code"] == "BROKEN" else BUILT)
                                + "\nSECRET-RESULT")
        return VIEW

    script = json.loads(json.dumps(CORRECTING).replace("first", "SECRET-NOTE"))
    with AgentProcess(script=write_script(tmp_path, script), tools=tools) as agent:
        agent.core.run("SECRET-PROMPT A bracket, please.")
        result = result_of(agent)
        assert agent.wait() == 0
    assert result["stop_reason"] == "done"
    assert "SECRET-RESULT" in json.dumps(result["history"])
    log = agent.stderr
    assert "run run-1 finished: done after 5 model requests and 4 tool calls" in log
    assert "tool.call write_model_code ok=True" in log
    assert "scripted mode" in log
    assert "SECRET" not in log
    assert "Fixed" not in log and "BROKEN" not in log


def test_a_stray_print_cannot_reach_stdout() -> None:
    code = (
        "import os, sys\n"
        "from hotend_agent.main import claim_stdout\n"
        "fd = claim_stdout()\n"
        "print('a library prints a banner')\n"
        "sys.stdout.write('and writes to sys.stdout\\n')\n"
        "sys.__stdout__.write('even to the original object\\n')\n"
        "sys.__stdout__.flush()\n"
        "os.write(1, b'or straight to descriptor 1\\n')\n"
        "os.write(fd, b'{\"jsonrpc\":\"2.0\"}\\n')\n"
    )
    done = subprocess.run([sys.executable, "-I", "-c", code], capture_output=True, timeout=30,
                          cwd=PYTHON_DIR, check=True)
    assert done.stdout == b'{"jsonrpc":"2.0"}\n'
    for text in (b"banner", b"sys.stdout", b"original object", b"descriptor 1"):
        assert text in done.stderr


def test_a_real_provider_run_goes_through_llm_request_only() -> None:
    case = PROVIDERS["anthropic"]
    turns: list[int] = []
    refused: list[str] = []

    def llm(params: dict[str, Any]) -> LlmReply:
        problem = check_egress(case, params)
        if problem:
            refused.append(problem)
            return LlmReply(refuse=("denied", problem))
        turns.append(len(json.loads(params["body"])["messages"]))
        if len(turns) == 1:
            return reply(case, "Writing the model.",
                         ("write_model_code", {"code": "FIXED", "note": "a bracket"}))
        if len(turns) == 2:
            return reply(case, "It builds and passes.")
        return reply(case, "looked at: iso, front and top; one piece, upright.")

    with AgentProcess(llm=llm, tools=correcting_tools) as agent:
        agent.core.run("A bracket, please.", provider=case.provider)
        result = result_of(agent)
        assert agent.wait() == 0
        core = agent.core
    assert refused == []
    assert result["stop_reason"] == "done", (result, agent.stderr)
    # Look review: the build is followed by Hotend's own views, and the answer that did not
    # say what was looked at is asked for it once, as a message of its own.
    assert turns == [1, 3, 5]
    assert core.tool_calls == [("write_model_code", {"code": "FIXED", "note": "a bracket"}),
                               ("render_views", {"views": ["iso", "front", "top"]})]
    assert [e.get("auto", False) for e in core.events_of("tool_end")] == [False, True]
    assert core.text() == ("Writing the model.\n\nIt builds and passes.\n\n"
                           "looked at: iso, front and top; one piece, upright.")
    assert result["usage"]["requests"] == 3 and result["usage"]["input_tokens"] == 300
    assert core.violations == []
    assert "llm.request llm-1 POST /v1/messages" in agent.stderr
    assert "Writing the model" not in agent.stderr
    assert "asking once" in agent.stderr

    # The stored history continues in a new process.
    with AgentProcess(llm=lambda params: reply(case, "You are welcome.")) as agent:
        agent.core.run("Thanks.", provider=case.provider, history=result["history"])
        again = result_of(agent)
        assert agent.wait() == 0
    assert again["stop_reason"] == "done"
    body = json.loads(agent.core.llm_requests[0]["body"])
    assert len(body["messages"]) == 7
    assert "A bracket, please." in json.dumps(body["messages"][0])
    assert "Reply with one line that begins" in json.dumps(body["messages"][4])


# -- inside the real sandbox ----------------------------------------------------------------

needs_sandbox = pytest.mark.skipif(
    sandbox_launcher() is None,
    reason="the hotend-sandbox launcher is not built (cargo build -p hotend-sandbox)",
)


@needs_sandbox
def test_the_scripted_agent_runs_inside_the_agent_sandbox(tmp_path: Path) -> None:
    job = tmp_path / "job"
    job.mkdir()
    with AgentProcess(script="builtin:cable_box_demo", sandbox=job, tools=Replay(build())) as agent:
        agent.core.run("A cable box.", max_turns=2)
        result = result_of(agent)
        code = agent.wait()
    assert code == 0, agent.stderr
    assert result["stop_reason"] == "budget"
    assert [name for name, _ in agent.core.tool_calls] == ["list_cards", "write_model_code"]
    assert agent.core.violations == []


@needs_sandbox
@pytest.mark.parametrize("kind", list(PROVIDERS))
def test_every_provider_sdk_works_inside_the_agent_sandbox(kind: str, tmp_path: Path) -> None:
    """No network, no files beyond the runtime: the SDKs must still load and run a turn."""
    case = PROVIDERS[kind]
    refused: list[str] = []

    def llm(params: dict[str, Any]) -> LlmReply:
        problem = check_egress(case, params)
        if problem:
            refused.append(problem)
            return LlmReply(refuse=("denied", problem))
        return reply(case, "Hello from inside.")

    job = tmp_path / "job"
    job.mkdir()
    with AgentProcess(llm=llm, sandbox=job) as agent:
        agent.core.run("Hello.", provider=case.provider)
        result = result_of(agent)
        code = agent.wait()
    assert code == 0, agent.stderr
    assert refused == []
    assert result["stop_reason"] == "done", (result, agent.stderr)
    assert agent.core.text() == "Hello from inside."
    assert agent.core.violations == []
