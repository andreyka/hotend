"""A scripted stand-in for the agent process, for hotend-core's tests. Standard library only.

It speaks the core <-> agent protocol of docs/PROTOCOL.md section 5 on stdin and stdout. The
user message is the script: a JSON object {"steps": [...]} whose steps run in order. Anything
else is echoed back as one text_delta. Every tool result, model response and error the core
sends back is appended to the returned history, which is how the tests see them.

Steps:
  {"tool": name, "args": {...}}            tool.call, waits for the answer
  {"tool": name, "args": {...}, "wait": false}   tool.call, goes on without the answer
  {"llm": {"path": ..., "body": {...}, "method"?, "headers"?, "token"?}}   llm.request
  {"event": {...}}                         agent.event notification
  {"say": "text"}                          agent.event text_delta
  {"raw": "line"}                          writes the line as is
  {"request": {"method": ..., "params": ...}}   any other request, waits for the answer
  {"sleep": seconds}                       waits; a cancel ends the run unless ignore_cancel
  {"ignore_cancel": true}                  from here on agent.cancel is not obeyed
  {"exit": code}                           exits at once without answering
  {"stop_reason": "budget"}                finishes with that stop reason
  {"params": ["research", "tools"]}        logs those parameters of agent.run (tools by name)
  {"shape": "0123abcd"}                    returns that request shape with the result
  {"keep": {...}}                          appends that object to the returned history, the
                                           way the real agent stores a part of an answer
"""

import base64
import json
import queue
import sys
import threading
import time

inbox: "queue.Queue[dict | None]" = queue.Queue()
state = {"next_id": 100, "ignore_cancel": False, "cancelled": False}


def send(message: dict) -> None:
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def read_stdin() -> None:
    for line in sys.stdin:
        line = line.strip()
        if line:
            inbox.put(json.loads(line))
    inbox.put(None)


def take(timeout: float | None = None) -> dict | None:
    """The next message from the core, or None on timeout. Exits when stdin closes."""
    try:
        message = inbox.get(timeout=timeout)
    except queue.Empty:
        return None
    if message is None:
        sys.exit(0)
    if message.get("method") == "agent.cancel":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {}})
        if not state["ignore_cancel"]:
            state["cancelled"] = True
        return take(timeout)
    return message


def call(method: str, params: dict) -> tuple[dict, list[dict]]:
    """Sends a request and returns (its response, the notifications received meanwhile)."""
    state["next_id"] += 1
    request_id = state["next_id"]
    send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
    notifications = []
    while True:
        message = take()
        if message is None:
            continue
        if message.get("id") == request_id and "method" not in message:
            return message, notifications
        notifications.append(message)


def run(params: dict, run_request_id: int) -> None:
    run_id = params["run_id"]
    text = params["user_message"]["text"]
    history = list(params["history"]) + [{"role": "user", "content": text}]
    log: list[dict] = []
    stop_reason = "done"
    shape = None
    try:
        # The core may add a note of its own after the maker's text (after a failed run);
        # the script is the JSON object at the start (the whole text stays in the history).
        script, _ = json.JSONDecoder().raw_decode(text)
        steps = script["steps"]
    except (ValueError, KeyError, TypeError):
        steps = [{"say": f"echo: {text}"}]

    def event(body: dict) -> None:
        send({"jsonrpc": "2.0", "method": "agent.event",
              "params": {"run_id": run_id, "event": body}})

    for step in steps:
        if state["cancelled"]:
            break
        if "tool" in step and step.get("wait") is False:
            state["next_id"] += 1
            send({"jsonrpc": "2.0", "id": state["next_id"], "method": "tool.call",
                  "params": {"run_id": run_id, "name": step["tool"],
                             "args": step.get("args", {})}})
        elif "tool" in step:
            event({"type": "tool_start", "name": step["tool"]})
            response, _ = call("tool.call", {"run_id": step.get("run_id", run_id),
                                             "name": step["tool"], "args": step.get("args", {})})
            result = response.get("result", {})
            event({"type": "tool_end", "name": step["tool"], "ok": bool(result.get("ok"))})
            log.append({"tool": step["tool"], "response": response})
        elif "llm" in step:
            spec = step["llm"]
            response, notes = call("llm.request", {
                "run_id": run_id, "request_id": f"q{state['next_id']}",
                "token": spec.get("token", params["egress_token"]),
                "method": spec.get("method", "POST"), "path": spec["path"],
                "headers": spec.get("headers", {"content-type": "application/json"}),
                "body_b64": base64.b64encode(json.dumps(spec.get("body", {})).encode()).decode(),
            })
            start = [n["params"] for n in notes if n.get("method") == "llm.response_start"]
            chunks = [base64.b64decode(n["params"]["data_b64"]) for n in notes
                      if n.get("method") == "llm.response_chunk"]
            body = b"".join(chunks).decode("utf-8", "replace")
            log.append({"llm": spec["path"], "response": response, "start": start,
                        "chunks": len(chunks), "body": body})
            if body:
                event({"type": "text_delta", "text": body})
        elif "event" in step:
            event(step["event"])
        elif "say" in step:
            event({"type": "text_delta", "text": step["say"]})
        elif "raw" in step:
            sys.stdout.write(step["raw"] + "\n")
            sys.stdout.flush()
        elif "request" in step:
            response, _ = call(step["request"]["method"], step["request"].get("params", {}))
            log.append({"request": step["request"]["method"], "response": response})
        elif "sleep" in step:
            deadline = time.monotonic() + step["sleep"]
            while time.monotonic() < deadline and not state["cancelled"]:
                take(timeout=0.05)
        elif "ignore_cancel" in step:
            state["ignore_cancel"] = True
        elif "exit" in step:
            sys.stderr.write("fake agent: exiting on request\n")
            sys.stderr.flush()
            sys.exit(step["exit"])
        elif "stop_reason" in step:
            stop_reason = step["stop_reason"]
        elif "params" in step:
            seen = {name: params.get(name) for name in step["params"]}
            if "tools" in seen:
                seen["tools"] = [tool["name"] for tool in params["tools"]]
            log.append({"params": seen})
        elif "shape" in step:
            shape = step["shape"]
        elif "keep" in step:
            history.append(step["keep"])
    if state["cancelled"]:
        stop_reason = "cancelled"
    history.append({"role": "assistant", "log": log})
    result = {"history": history, "stop_reason": stop_reason,
              "usage": {"tool_calls": sum(1 for entry in log if "tool" in entry)}}
    if stop_reason == "error":
        result["error"] = "scripted failure"
    if shape is not None:
        result["shape"] = shape
    send({"jsonrpc": "2.0", "id": run_request_id, "result": result})


def main() -> None:
    threading.Thread(target=read_stdin, daemon=True).start()
    while True:
        message = take()
        if message is not None and message.get("method") == "agent.run":
            run(message["params"], message["id"])
            return


if __name__ == "__main__":
    main()
