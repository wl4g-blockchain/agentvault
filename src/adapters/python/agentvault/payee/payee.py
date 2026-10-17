from __future__ import annotations

import urllib.parse

from ..config import PayeeConfig
from ..model import (
    CreateChallengeRequest, PayeeOperation, PaymentChallenge, PaymentCredential, PaymentOffer,
    PaymentDirection, PaymentReceipt, PaymentVerification, ProtectionOutcome, ResolveOfferRequest,
    SettlePaymentRequest, SettledIncomingPayment, VerifyPaymentRequest,
)
from ..transport import VaultTransport


class PayeeMiddleware:
    """Transport-neutral inbound challenge, verification, and settlement flow."""

    def __init__(self, transport: VaultTransport, config: PayeeConfig) -> None:
        if transport is None:
            raise ValueError("AgentVault transport is not configured")
        self._transport = transport
        self.config = config

    def fast(self, app: "ASGIApp", routes: list["FastPayeeRoute"]) -> "FastPayeeMiddleware":
        from .fast import FastPayeeMiddleware

        return FastPayeeMiddleware(
            app, middleware=self, routes=routes, max_body_bytes=self.config.max_body_bytes)

    def standard(self) -> "StandardPayeeMiddleware":
        from .standard import StandardPayeeMiddleware

        return StandardPayeeMiddleware(self)

    def challenge(self, operation: PayeeOperation) -> PaymentChallenge:
        challenge = self._create_challenge(CreateChallengeRequest(
            operation.challenge_context(), operation.offer_id, operation.resource,
            operation.origin, operation.protocol, operation.request_hash))
        _validate_challenge_binding(operation, challenge)
        return challenge

    def protect(self, operation: PayeeOperation) -> ProtectionOutcome:
        challenge = self.challenge(operation)
        if operation.credential_payload is None:
            return ProtectionOutcome.payment_required(challenge)
        credential = PaymentCredential(
            operation.protocol, challenge.challenge_hash, "untrusted", operation.credential_payload)
        verification = self._verify_payment(VerifyPaymentRequest(
            operation.challenge_context(), challenge.challenge_hash, operation.request_hash, credential))
        if not verification.valid:
            raise PermissionError("AgentVault rejected the presented payment")
        if verification.verification_ref is None:
            raise RuntimeError("valid AgentVault verification omitted settlement identity")
        _validate_verification_binding(operation, challenge, verification)
        payment = self._settle_payment(SettlePaymentRequest(
            operation.settlement_context(), verification.intent.id, verification.verification_ref))
        _validate_settlement_binding(operation, verification, payment)
        return ProtectionOutcome.authorized(payment)

    def resolve_offer(self, request: ResolveOfferRequest) -> PaymentOffer:
        value = self._transport.request("POST", "/api/v1/offers/resolve", request.wire())
        return PaymentOffer.from_wire(value)

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


def _validate_challenge_binding(operation: PayeeOperation, challenge: PaymentChallenge) -> None:
    if (
        challenge.scenario_id != operation.scenario_id
        or challenge.offer_id != operation.offer_id
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


def _validate_verification_binding(
    operation: PayeeOperation, challenge: PaymentChallenge, verification: PaymentVerification,
) -> None:
    intent = verification.intent
    if (
        intent.direction is not PaymentDirection.INBOUND
        or intent.scenario_id != operation.scenario_id
        or intent.offer_id != operation.offer_id
        or intent.resource != operation.resource
        or intent.protocol != operation.protocol
        or intent.challenge_hash != challenge.challenge_hash
        or intent.request_hash != operation.request_hash
        or intent.asset != challenge.asset
        or intent.amount != challenge.amount
        or intent.recipient != challenge.recipient
    ):
        raise RuntimeError("AgentVault verification is not bound to the protected operation")


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
        or intent.offer_id != verified.offer_id
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
