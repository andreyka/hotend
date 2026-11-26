"""Hotend's agent process: the AI design loop, sandboxed and without any reach of its own.

`python -I -m hotend_agent` speaks JSON-RPC 2.0, one message per line, on stdin and stdout
with the Rust core (docs/PROTOCOL.md section 5). The process holds no socket and no key:

- `channel`: the line channel; the only thing that touches stdin and stdout.
- `transport`: every model request becomes an `llm.request` to the core.
- `providers`: Pydantic AI models for anthropic, openai, google, openrouter and ollama, all
  sending through that transport.
- `tools`: every tool is a proxy for `tool.call`; the core's gateway executes it.
- `prompt`: the system prompt and its version.
- `runner`: one run: events, budget, cancel, history.
- `scripted`: a model that replays a written conversation, for tests and the offline demo.
- `main`: the process: one `agent.run`, `agent.cancel`, shutdown when stdin closes.
"""

PROTOCOL_VERSION = 0

__all__ = ["PROTOCOL_VERSION"]
