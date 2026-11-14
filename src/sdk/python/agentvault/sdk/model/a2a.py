from __future__ import annotations

import json
import math
from dataclasses import dataclass

from a2a.types import MessageSendParams, Task


@dataclass
class A2ACall:
    """Original JSON-RPC carrier composed with the official A2A models."""

    envelope: dict[str, object]

    @classmethod
    def parse(cls, body: bytes, limit: int) -> A2ACall | None:
        if len(body) > limit:
            raise ValueError("A2A request exceeds configured limit")
        try:
            message = json.loads(body)
        except (ValueError, UnicodeError):
            return None
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            return None
        if message.get("method") in ("message/stream", "SendStreamingMessage", "SendMessage"):
            raise ValueError("paid A2A supports non-streaming v0.3 message/send")
        if message.get("method") != "message/send":
            return None
        value = message.get("id")
        if isinstance(value, bool) or not isinstance(value, (str, int, float)) or (
            isinstance(value, float) and not math.isfinite(value)
        ):
            raise ValueError("paid A2A requires a JSON-RPC request id")
        call = cls(message)
        params = call.params()
        if not params.message.message_id or params.message.role != "user" or not params.message.parts:
            raise ValueError("invalid A2A user message")
        return call

    def params(self) -> MessageSendParams:
        return MessageSendParams.model_validate(self.envelope.get("params"))

    def bind_task(self, task: Task) -> None:
        message = self.params().message
        if not task.id or not task.context_id or (
            message.task_id and message.task_id != task.id
        ) or (message.context_id and message.context_id != task.context_id):
            raise ValueError("A2A task/context does not match the request")
        self.envelope["params"]["message"].update(taskId=task.id, contextId=task.context_id)

    def body(self) -> bytes:
        return json.dumps(self.envelope, separators=(",", ":"), sort_keys=True, allow_nan=False).encode()

    @staticmethod
    def metadata(message: dict, *, root: bool = False) -> dict:
        owner = message if root else message["params"]["message"]
        value = owner.setdefault("_meta" if root else "metadata", {})
        if not isinstance(value, dict):
            raise ValueError("A2A payment metadata must be an object")
        return value

    @staticmethod
    def task(message: dict) -> Task:
        task = Task.model_validate(message.get("result"))
        if not task.id or not task.context_id:
            raise ValueError("A2A task/context is required")
        return task

    @staticmethod
    def response(message: object, request_id: object) -> dict:
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0" or (
            type(message.get("id")) is not type(request_id) or message.get("id") != request_id
        ) or (("result" in message) == ("error" in message)):
            raise ValueError("A2A JSON-RPC response does not match the request")
        return message
