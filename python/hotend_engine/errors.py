"""Error types the worker turns into result.json statuses."""

from __future__ import annotations


class BadRequest(Exception):
    """The request, or a file it names, is unusable. The worker exits with code 2."""


class ModelError(Exception):
    """The model script raised or broke a rule of docs/PROTOCOL.md section 2.

    Carries what the agent needs to fix the script: an error type, a message, the line in the
    model script and a traceback trimmed to the script's own frames.
    """

    def __init__(
        self, type_: str, message: str, line: int | None = None, traceback: str = ""
    ) -> None:
        super().__init__(message)
        self.type = type_
        self.message = message
        self.line = line
        self.traceback = traceback

    def to_json(self) -> dict:
        return {
            "type": self.type,
            "message": self.message,
            "line": self.line,
            "traceback": self.traceback,
        }
