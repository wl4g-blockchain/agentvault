from __future__ import annotations

from dataclasses import dataclass
from uuid import uuid4

from a2a.server.tasks import TaskStore
from a2a.types import AgentCard, AgentExtension, Task, TaskStatus, Message, Part, DataPart

from ..model.a2a import A2ACall
from ..model.payee import PayeeOperation


@dataclass
class A2APaymentCall:
    call: A2ACall
    task: Task
    store: TaskStore
    credential: object | None
    context: object | None = None

    @property
    def id(self):
        return self.call.envelope["id"]

    @property
    def canonical_body(self) -> bytes:
        return self.call.body()


class A2APayeeMiddleware:
    """TaskStore integration around the same PayeeMiddleware settlement flow."""

    _SCOPE = ("agentvault.payment.run_id", "agentvault.payment.scenario_id", "agentvault.payment.offer_id")

    def __init__(self, core) -> None:
        self.core = core

    def agent_card(self, card: AgentCard) -> AgentCard:
        extensions = list(card.capabilities.extensions or [])
        for value in self.core._protocols.a2a_extensions().split(","):
            value = value.strip()
            if value and not any(extension.uri == value for extension in extensions):
                extensions.append(AgentExtension(uri=value, required=True))
        card.capabilities.extensions = extensions
        return card

    async def prepare(self, body: bytes, operation: PayeeOperation, store: TaskStore, headers, context=None) -> A2APaymentCall | None:
        call = A2ACall.parse(body, self.core.config.max_body_bytes)
        if call is None:
            return None
        if not operation.run_id or store is None:
            raise ValueError("paid A2A requires x-run-id and the application TaskStore")
        adapter = self.core.protocol(operation.protocol)
        if adapter.a2a_extension() and adapter.a2a_extension() not in {
            value.strip() for value in headers.get("x-a2a-extensions", "").split(",")
        }:
            raise ValueError("required A2A payment extension is not activated")
        selected, credential = self.core._protocols.a2a_credential(call.envelope, self.core.config.max_body_bytes)
        if selected and selected != operation.protocol:
            raise ValueError("A2A credential protocol mismatch")
        self.core._protocols.a2a_strip_credentials(call.envelope)
        params = call.params()
        task_id = params.message.task_id or operation.run_id
        task = await store.get(task_id, context)
        if task is None:
            if credential is not None or params.message.task_id:
                raise ValueError("A2A task must exist before credential delivery or continuation")
            task = Task(id=task_id, context_id=params.message.context_id or str(uuid4()),
                        status=TaskStatus(state="submitted"), history=[params.message])
        task = task.model_copy(deep=True)
        metadata = dict(task.metadata or {})
        for key, value in zip(self._SCOPE, (operation.run_id, operation.scenario_id, operation.offer_id)):
            if (key in metadata and metadata[key] != value) or (key not in metadata and credential is not None):
                raise ValueError("A2A task is not bound to this payment scope")
            metadata[key] = value
        task.metadata = metadata
        call.bind_task(task)
        await store.save(task, context)
        return A2APaymentCall(call, task, store, credential, context)

    async def challenge(self, call: A2APaymentCall, challenge, rejected):
        adapter = self.core.protocol(challenge.protocol)
        wire = adapter.a2a_payment_required(challenge, call.task, rejected)
        if wire.result is not None:
            await self._persist(call, Task.model_validate(wire.result))
        return wire

    async def receipt(self, call: A2APaymentCall, message: dict, challenge, payment) -> dict:
        A2ACall.response(message, call.id)
        name, evidence = self.core._protocols.a2a_evidence(message, self.core.config.max_body_bytes)
        if evidence is not None:
            raise ValueError(f"A2A handler attempted to supply {name} payment evidence")
        if "error" in message:
            # Preserve a business failure in a failed Task while returning the confirmed payment receipt.
            task = call.task.model_copy(deep=True)
            task.status = TaskStatus(state="failed", message=Message(
                message_id=str(uuid4()), role="agent", task_id=task.id, context_id=task.context_id,
                parts=[Part(root=DataPart(data={"jsonrpc_error": message.pop("error")}))]))
            message["result"] = task.model_dump(by_alias=True, exclude_none=True)
        task = A2ACall.task(message)
        call.call.bind_task(task)
        self.core.protocol(payment.receipt.protocol).a2a_inject_receipt(message, challenge, payment)
        await self._persist(call, A2ACall.task(message))
        return message

    async def _persist(self, call: A2APaymentCall, task: Task) -> None:
        if task.id != call.task.id or task.context_id != call.task.context_id:
            raise ValueError("A2A response changed task/context")
        metadata = dict(task.metadata or {})
        for key in self._SCOPE:
            if key in metadata and metadata[key] != call.task.metadata[key]:
                raise ValueError("A2A handler changed payment scope")
            metadata[key] = call.task.metadata[key]
        task.metadata = metadata
        await call.store.save(task, call.context)
