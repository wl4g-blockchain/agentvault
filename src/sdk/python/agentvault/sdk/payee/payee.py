from __future__ import annotations

import logging
import urllib.parse

from ..config import PayeeConfig
from ..model import (
    CreateChallengeRequest, PayeeOperation, PaymentChallenge, PaymentCredential, PaymentPrice,
    PaymentDirection, PaymentReceipt, PaymentVerification, ProtectionOutcome, ResolvePriceRequest,
    SettlePaymentRequest, SettledIncomingPayment, VerifyPaymentRequest,
)
from ..transport import VaultTransport
from ..protocol import (
    McpMetadata,
    McpPaymentRequired,
    PaymentProtocol,
    PaymentProtocolFactory,
)

_LOG = logging.getLogger("agentvault.sdk")


class PaymentRejectedError(PermissionError):
    def __init__(self, challenge: PaymentChallenge) -> None:
        super().__init__("AgentVault rejected the presented payment")
        self.challenge = challenge


class PayeeMiddleware:
    """Transport-neutral inbound challenge, verification, and settlement flow."""

    def __init__(self, transport: VaultTransport, config: PayeeConfig) -> None:
        if transport is None:
            raise ValueError("AgentVault transport is not configured")
        self._transport = transport
        self.config = config
        self._protocols = PaymentProtocolFactory().create(config.protocols)

    def fast(self, app: "ASGIApp", routes: list["FastPayeeRoute"]) -> "FastPayeeMiddleware":
        from .fast import FastPayeeMiddleware

        return FastPayeeMiddleware(
            app, middleware=self, routes=routes, max_body_bytes=self.config.max_body_bytes)

    def a2a(self) -> "A2APayeeMiddleware":
        from .a2a import A2APayeeMiddleware
        return A2APayeeMiddleware(self)

    def standard(self) -> "StandardPayeeMiddleware":
        from .standard import StandardPayeeMiddleware

        return StandardPayeeMiddleware(self)

    def protocol(self, name: str) -> PaymentProtocol:
        return self._protocols.get(name)

    def credential_header_names(self) -> tuple[str, ...]:
        return self._protocols.credential_header_names()

    def merchant_evidence_header_names(self) -> tuple[str, ...]:
        return self._protocols.merchant_evidence_header_names()

    def presented_credential(
        self, protocol: str, headers: dict[str, str],
    ) -> object | None:
        self._protocols.get(protocol)
        selected, payload = self._protocols.presented_credential(
            headers, self.config.max_body_bytes
        )
        if selected is not None and selected != protocol:
            raise ValueError(
                "payment credential protocol does not match the protected route"
            )
        return payload

    def presented_mcp_credential(
        self, protocol: str, meta: object,
    ) -> object | None:
        selected, payload = self._protocols.presented_mcp_credential(
            meta, self.config.max_body_bytes
        )
        if selected is not None and selected != protocol:
            raise ValueError(
                "MCP payment credential protocol does not match the protected route"
            )
        return payload

    def mcp_request_metadata_keys(self) -> tuple[str, ...]:
        return self._protocols.mcp_request_metadata_keys()

    def mcp_root_request_metadata_keys(self) -> tuple[str, ...]:
        return self._protocols.mcp_root_request_metadata_keys()

    def mcp_receipt_metadata_keys(self) -> tuple[str, ...]:
        return self._protocols.mcp_receipt_metadata_keys()

    def mcp_payment_required(
        self, challenge: PaymentChallenge, rejected: bool = False,
    ) -> McpPaymentRequired:
        adapter = self._protocols.get(challenge.protocol)
        if rejected:
            return adapter.mcp_payment_rejected(
                challenge, "AgentVault rejected the presented payment"
            )
        return adapter.mcp_payment_required(challenge)

    def mcp_receipt(
        self, challenge: PaymentChallenge, payment: SettledIncomingPayment,
    ) -> McpMetadata:
        return self._protocols.get(payment.receipt.protocol).mcp_receipt(
            challenge, payment
        )

    def mcp_merchant_evidence(self, meta: object) -> tuple[str | None, object | None]:
        return self._protocols.mcp_merchant_evidence(
            meta, self.config.max_body_bytes
        )

    def merchant_evidence(
        self, headers: dict[str, str],
    ) -> tuple[str | None, object | None]:
        return self._protocols.merchant_evidence(headers)

    def challenge(self, operation: PayeeOperation) -> PaymentChallenge:
        challenge = self._create_challenge(CreateChallengeRequest(
            operation.challenge_context(), operation.price_id, operation.resource,
            operation.origin, operation.protocol, operation.request_hash))
        PayeeMiddleware._validate_challenge_binding(operation, challenge)
        return challenge

    def protect(self, operation: PayeeOperation) -> ProtectionOutcome:
        PayeeMiddleware._event("agentvault.sdk.payee.intercepted", run_id=operation.run_id,
               scenario_id=operation.scenario_id)
        challenge = self.challenge(operation)
        PayeeMiddleware._event("agentvault.sdk.payee.challenge_created", run_id=operation.run_id,
               challenge_hash=challenge.challenge_hash)
        if operation.credential_payload is None:
            return ProtectionOutcome.payment_required(challenge)
        credential = PaymentCredential(
            operation.protocol, challenge.challenge_hash, "untrusted", operation.credential_payload)
        verification = self._verify_payment(VerifyPaymentRequest(
            operation.challenge_context(), challenge.challenge_hash, operation.request_hash, credential))
        if not verification.valid:
            raise PaymentRejectedError(challenge)
        if verification.verification_ref is None:
            raise RuntimeError("valid AgentVault verification omitted settlement identity")
        PayeeMiddleware._validate_verification_binding(operation, challenge, verification)
        PayeeMiddleware._event("agentvault.sdk.payee.verified", run_id=operation.run_id,
               intent_id=verification.intent.id)
        payment = self._settle_payment(SettlePaymentRequest(
            operation.settlement_context(), verification.intent.id, verification.verification_ref))
        PayeeMiddleware._validate_settlement_binding(operation, verification, payment)
        PayeeMiddleware._event("agentvault.sdk.payee.settled", run_id=operation.run_id,
               intent_id=payment.intent.id)
        return ProtectionOutcome.authorized(payment, challenge)

    def resolve_price(self, request: ResolvePriceRequest) -> PaymentPrice:
        value = self._transport.request("POST", "/api/v1/prices/resolve", request.wire())
        return PaymentPrice.from_wire(value)

    def _create_challenge(self, request: CreateChallengeRequest) -> PaymentChallenge:
        value = self._transport.request("POST", "/api/v1/payee/challenges", request.wire())
        return PaymentChallenge.from_wire(value)

    def _verify_payment(self, request: VerifyPaymentRequest) -> PaymentVerification:
        value = self._transport.request("POST", "/api/v1/payee/verify", request.wire())
        return PaymentVerification.from_wire(value)

    def _settle_payment(self, request: SettlePaymentRequest) -> SettledIncomingPayment:
        value = self._transport.request("POST", "/api/v1/payee/settle", request.wire())
        return SettledIncomingPayment.from_wire(value)

    def get_receipt(self, scenario_id: str, intent_id: str) -> PaymentReceipt:
        if not scenario_id.strip() or not intent_id.strip():
            raise ValueError("scenario_id and intent_id cannot be blank")
        query = urllib.parse.urlencode({"scenario_id": scenario_id})
        intent = urllib.parse.quote(intent_id, safe="")
        value = self._transport.request("GET", f"/api/v1/payments/{intent}/receipt?{query}")
        return PaymentReceipt.from_wire(value)


    @staticmethod
    def _validate_challenge_binding(operation: PayeeOperation, challenge: PaymentChallenge) -> None:
        if (
            challenge.scenario_id != operation.scenario_id
            or challenge.price_id != operation.price_id
            or challenge.resource != operation.resource
            or challenge.protocol != operation.protocol
            or not challenge.asset.network
            or not challenge.asset.asset
            or not challenge.amount
            or not challenge.recipient
            or not challenge.nonce
            or not challenge.challenge_hash
        ):
            raise RuntimeError("AgentVault challenge is not bound to the protected operation")


    @staticmethod
    def _validate_verification_binding(
        operation: PayeeOperation, challenge: PaymentChallenge, verification: PaymentVerification,
    ) -> None:
        intent = verification.intent
        if (
            intent.direction is not PaymentDirection.INBOUND
            or intent.scenario_id != operation.scenario_id
            or intent.price_id != operation.price_id
            or intent.resource != operation.resource
            or intent.protocol != operation.protocol
            or intent.challenge_hash != challenge.challenge_hash
            or intent.request_hash != operation.request_hash
            or intent.asset != challenge.asset
            or intent.amount != challenge.amount
            or intent.recipient != challenge.recipient
        ):
            raise RuntimeError("AgentVault verification is not bound to the protected operation")


    @staticmethod
    def _validate_settlement_binding(
        operation: PayeeOperation,
        verification: PaymentVerification,
        payment: SettledIncomingPayment,
    ) -> None:
        intent = payment.intent
        verified = verification.intent
        receipt = payment.receipt
        if (
            intent.id != verified.id
            or intent.direction is not PaymentDirection.INBOUND
            or intent.scenario_id != verified.scenario_id
            or intent.price_id != verified.price_id
            or intent.resource != verified.resource
            or intent.protocol != verified.protocol
            or intent.challenge_hash != verified.challenge_hash
            or intent.request_hash != verified.request_hash
            or intent.asset != verified.asset
            or intent.amount != verified.amount
            or intent.recipient != verified.recipient
            or intent.status.value != "settled"
            or receipt.intent_id != intent.id
            or receipt.scenario_id != operation.scenario_id
            or receipt.direction is not PaymentDirection.INBOUND
            or receipt.protocol != operation.protocol
            or receipt.asset != intent.asset
            or receipt.recipient != intent.recipient
            or receipt.requested_amount != intent.amount
            or receipt.settled_amount != intent.settled_amount
            or receipt.status.value != "settled"
        ):
            raise RuntimeError("invalid AgentVault settlement response")


    @staticmethod
    def _event(name: str, **fields: object) -> None:
        rendered = " ".join(
            f"{key.replace('_', '.')}={value}" for key, value in sorted(fields.items())
        )
        message = f"{name} {rendered}" if rendered else name
        _LOG.info(message, extra={"event_name": name, **fields})
