from __future__ import annotations

from uuid import uuid4
from a2a.types import Task, TaskStatus, Message, TextPart, Part
from ..model.a2a import A2ACall

import base64
import binascii
import json
from typing import Mapping, TypedDict, cast

from x402.http.utils import (
    decode_payment_response_header,
    encode_payment_required_header,
    encode_payment_response_header,
)
from x402.mcp import MCP_PAYMENT_META_KEY, MCP_PAYMENT_RESPONSE_META_KEY
from x402.mcp.types import MCPToolResult
from x402.mcp.utils import (
    extract_payment_from_meta,
    extract_payment_required_from_result,
    extract_payment_response_from_meta,
)
from x402.schemas import PaymentPayload, PaymentRequired, SettleResponse, parse_payment_required

from ..model import HttpCredential, PaymentChallenge, PaymentCredential, SettledIncomingPayment
from .adapter import (
    HttpResponse,
    McpMetadata,
    McpPaymentRequired,
    MerchantEvidence,
    PaymentProtocol,
)


# These private projections cover only the opaque payload inside the official
# x402.schemas.PaymentPayload envelope. Importing the mechanism modules solely
# for their dataclasses requires x402[evm,svm] and would pull chain RPC/signing
# dependencies into this transport-only SDK boundary.
_Eip3009AuthorizationPayload = TypedDict("_Eip3009AuthorizationPayload", {
    "from": str,
    "to": str,
    "value": str,
    "validAfter": str,
    "validBefore": str,
    "nonce": str,
})


class _Eip3009ExactPayload(TypedDict):
    signature: str
    authorization: _Eip3009AuthorizationPayload


class _SolanaExactPayload(TypedDict):
    transaction: str


class X402Adapter(PaymentProtocol):
    @property
    def name(self) -> str:
        return "x402"

    def credential_header_names(self) -> tuple[str, ...]:
        return ("payment-signature",)

    def merchant_evidence_header_names(self) -> tuple[str, ...]:
        return ("payment-response",)

    def credential(self, credential: PaymentCredential) -> HttpCredential:
        delivery = credential.http
        if (
            credential.protocol != self.name
            or delivery is None
            or delivery.header_name.lower() != "payment-signature"
        ):
            raise ValueError("x402 credential has no PAYMENT-SIGNATURE delivery")
        self._payment_payload(delivery.header_value, "PAYMENT-SIGNATURE")
        return delivery

    def merchant_evidence(self, headers: Mapping[str, str]) -> MerchantEvidence | None:
        value = self._header(headers, "payment-response")
        if not value:
            return None
        self._settlement(value, "PAYMENT-RESPONSE")
        return MerchantEvidence("payment-response", value)

    def presented_credential(self, headers: Mapping[str, str], limit: int) -> object | None:
        encoded = self._header(headers, "payment-signature")
        if not encoded:
            return None
        if len(encoded) > (limit * 4 + 2) // 3 + 4:
            raise ValueError("PAYMENT-SIGNATURE exceeds configured limit")
        payload = self._payment_payload(encoded, "PAYMENT-SIGNATURE")
        return payload.model_dump(by_alias=True, exclude_none=True)

    def challenge(self, challenge: PaymentChallenge) -> HttpResponse:
        required = self._challenge_payment_required(challenge)
        body = required.model_dump_json(by_alias=True, exclude_none=True).encode()
        return HttpResponse(
            {"Payment-Required": encode_payment_required_header(required)},
            "application/json",
            body,
        )

    def receipt(self, payment: SettledIncomingPayment) -> HttpResponse:
        settlement = self._payment_settlement(payment)
        return HttpResponse({"Payment-Response": encode_payment_response_header(settlement)})

    def mcp_request_metadata_key(self) -> str:
        return MCP_PAYMENT_META_KEY

    def mcp_receipt_metadata_key(self) -> str:
        return MCP_PAYMENT_RESPONSE_META_KEY

    def mcp_challenge(self, message: object, limit: int) -> HttpResponse | None:
        encoded = self._bounded_json(message, limit, "MCP x402 message")
        parsed = json.loads(encoded)
        if not isinstance(parsed, dict) or not isinstance(parsed.get("result"), dict):
            return None
        result = parsed["result"]
        if result.get("isError") is not True:
            return None
        content = result.get("content") if isinstance(result.get("content"), list) else []
        structured = result.get("structuredContent")
        structured_required = None
        if isinstance(structured, dict):
            structured_required = extract_payment_required_from_result(
                MCPToolResult([], is_error=True, structured_content=structured)
            )
            if structured_required is None and (
                "x402Version" in structured or "accepts" in structured
            ):
                raise ValueError("MCP x402 structuredContent is not PaymentRequired v2")
        text_required = extract_payment_required_from_result(
            MCPToolResult(content, is_error=True)
        )
        if structured_required is None and text_required is None:
            return None
        if (
            structured_required is not None
            and text_required is not None
            and structured_required.model_dump(by_alias=True, exclude_none=True)
            != text_required.model_dump(by_alias=True, exclude_none=True)
        ):
            raise ValueError("MCP x402 structuredContent conflicts with content text")
        required = structured_required or text_required
        if not isinstance(required, PaymentRequired) or required.x402_version != 2:
            raise ValueError("MCP x402 challenge is not PaymentRequired v2")
        body = required.model_dump_json(by_alias=True, exclude_none=True).encode()
        if len(body) > limit:
            raise ValueError("MCP x402 PaymentRequired exceeds configured limit")
        return HttpResponse(
            {"payment-required": encode_payment_required_header(required)},
            "application/json",
            body,
        )

    def mcp_credential(self, credential: PaymentCredential) -> McpMetadata:
        delivery = self.credential(credential)
        payload = self._payment_payload(delivery.header_value, "PAYMENT-SIGNATURE")
        return McpMetadata(
            MCP_PAYMENT_META_KEY,
            payload.model_dump(by_alias=True, exclude_none=True),
        )

    def presented_mcp_credential(self, meta: object, limit: int) -> object | None:
        if not isinstance(meta, dict):
            raise ValueError("MCP params._meta must be an object")
        if MCP_PAYMENT_META_KEY not in meta:
            return None
        self._bounded_json(meta[MCP_PAYMENT_META_KEY], limit, "MCP x402 payment metadata")
        payload = extract_payment_from_meta({"_meta": meta})
        if not isinstance(payload, PaymentPayload) or payload.x402_version != 2:
            raise ValueError("MCP x402 payment metadata is not PaymentPayload v2")
        self._validate_exact_payload(payload, "MCP x402 payment metadata")
        return payload.model_dump(by_alias=True, exclude_none=True)

    def mcp_payment_required(self, challenge: PaymentChallenge) -> McpPaymentRequired:
        return self._mcp_payment_result(self._challenge_payment_required(challenge))

    def mcp_payment_rejected(self, challenge: PaymentChallenge, message: str) -> McpPaymentRequired:
        required = self._challenge_payment_required(challenge).model_copy(
            update={"error": message}
        )
        return self._mcp_payment_result(required)

    def mcp_receipt(
        self, _challenge: PaymentChallenge, payment: SettledIncomingPayment,
    ) -> McpMetadata:
        settlement = self._payment_settlement(payment)
        return McpMetadata(
            MCP_PAYMENT_RESPONSE_META_KEY,
            settlement.model_dump(by_alias=True, exclude_none=True),
        )

    def mcp_merchant_evidence(self, meta: object, limit: int) -> MerchantEvidence | None:
        if not isinstance(meta, dict):
            raise ValueError("MCP result._meta must be an object")
        if MCP_PAYMENT_RESPONSE_META_KEY not in meta:
            return None
        self._bounded_json(meta[MCP_PAYMENT_RESPONSE_META_KEY], limit, "MCP x402 receipt metadata")
        settlement = extract_payment_response_from_meta(MCPToolResult([], meta=meta))
        if settlement is None or not settlement.success or not settlement.payer:
            raise ValueError("MCP x402 settlement metadata is invalid")
        return MerchantEvidence(
            "payment-response", encode_payment_response_header(settlement)
        )

    def _challenge_payment_required(self, challenge: PaymentChallenge) -> PaymentRequired:
        if challenge.protocol != self.name or not isinstance(challenge.payload, dict):
            raise ValueError("x402 challenge protocol or payload is invalid")
        value = challenge.payload.get("payment_required")
        if not isinstance(value, dict):
            raise ValueError("x402 challenge has no payment_required wire value")
        required = parse_payment_required(value)
        if (
            not isinstance(required, PaymentRequired)
            or required.x402_version != 2
            or not required.accepts
        ):
            raise ValueError("x402 challenge is not PaymentRequired v2")
        return required

    def _payment_payload(self, encoded: str, label: str) -> PaymentPayload:
        try:
            decoded = base64.b64decode(encoded, validate=True)
            if base64.b64encode(decoded).decode("ascii") != encoded:
                raise ValueError("non-canonical base64")
            wire = json.loads(decoded.decode("utf-8"))
            payload = PaymentPayload.model_validate(wire, strict=True)
        except (binascii.Error, TypeError, ValueError) as error:
            raise ValueError(f"{label} is not valid x402") from error
        if wire.get("x402Version") != 2 or payload.x402_version != 2:
            raise ValueError(f"{label} is not PaymentPayload v2")
        # The wire is an unordered JSON object. Validate field names using the
        # official models, without reserializing their defaults or field order.
        for value, model in ((wire, payload), (wire["accepted"], payload.accepted),
                             (wire.get("resource"), payload.resource)):
            if model is not None:
                fields = {field.alias or name for name, field in type(model).model_fields.items()}
                if not set(value).issubset(fields):
                    raise ValueError(f"{label} contains unknown x402 fields")
        self._validate_exact_payload(payload, label)
        return payload

    @staticmethod
    def _validate_exact_payload(payload: PaymentPayload, label: str) -> None:
        if payload.accepted.scheme != "exact":
            raise ValueError(f"{label} uses an unsupported x402 scheme")
        network = payload.accepted.network
        wire = payload.payload
        if network.startswith("eip155:"):
            if set(wire) != {"signature", "authorization"}:
                raise ValueError(f"{label} is not an official EIP-3009 payload")
            authorization = wire.get("authorization")
            if not isinstance(authorization, dict) or set(authorization) != {
                "from", "to", "value", "validAfter", "validBefore", "nonce",
            }:
                raise ValueError(f"{label} is not an official EIP-3009 payload")
            typed = cast(_Eip3009ExactPayload, wire)
            authorization = typed["authorization"]
            if (
                not isinstance(typed["signature"], str)
                or not typed["signature"]
                or not all(isinstance(value, str) and value for value in authorization.values())
            ):
                raise ValueError(f"{label} is not an official EIP-3009 payload")
            extra = payload.accepted.extra
            if (
                not isinstance(extra.get("name"), str)
                or not extra["name"]
                or not isinstance(extra.get("version"), str)
                or not extra["version"]
            ):
                raise ValueError(f"{label} has no EIP-3009 domain metadata")
            return
        if network.startswith("solana:"):
            if set(wire) != {"transaction"}:
                raise ValueError(f"{label} is not an official Solana exact payload")
            typed = cast(_SolanaExactPayload, wire)
            transaction = typed["transaction"]
            try:
                decoded = base64.b64decode(transaction, validate=True)
            except (binascii.Error, TypeError, ValueError) as error:
                raise ValueError(f"{label} has no canonical base64 Solana transaction") from error
            if not decoded or base64.b64encode(decoded).decode() != transaction:
                raise ValueError(f"{label} has no canonical base64 Solana transaction")
            fee_payer = payload.accepted.extra.get("feePayer")
            if not isinstance(fee_payer, str) or not fee_payer:
                raise ValueError(f"{label} has no Solana facilitator fee payer")
            return
        raise ValueError(f"{label} uses an unsupported x402 network")

    def _payment_settlement(self, payment: SettledIncomingPayment) -> SettleResponse:
        receipt = payment.receipt
        if (
            receipt.protocol != self.name
            or not receipt.tx_hash
            or not receipt.payer
            or not receipt.asset.network
        ):
            raise ValueError("confirmed x402 receipt omitted transaction, payer, or network")
        return SettleResponse(
            success=True,
            payer=receipt.payer,
            transaction=receipt.tx_hash,
            network=receipt.asset.network,
        )

    def _settlement(self, encoded: str, label: str) -> SettleResponse:
        try:
            settlement = decode_payment_response_header(encoded)
        except (TypeError, ValueError) as error:
            raise ValueError(f"{label} is not a valid x402 settlement") from error
        if (
            not settlement.success
            or not settlement.payer
            or not settlement.transaction
            or not settlement.network
        ):
            raise ValueError(f"{label} is not a successful x402 settlement")
        return settlement

    def _mcp_payment_result(self, required: PaymentRequired) -> McpPaymentRequired:
        value = required.model_dump(by_alias=True, exclude_none=True)
        text = required.model_dump_json(by_alias=True, exclude_none=True)
        return McpPaymentRequired(result={
            "content": [{"type": "text", "text": text}],
            "structuredContent": value,
            "isError": True,
        })

    @staticmethod
    def _bounded_json(value: object, limit: int, label: str) -> bytes:
        try:
            encoded = json.dumps(value, separators=(",", ":")).encode()
        except (TypeError, ValueError) as error:
            raise ValueError(f"{label} is invalid") from error
        if not encoded or len(encoded) > limit:
            raise ValueError(f"{label} is empty or oversized")
        return encoded

    @staticmethod
    def _header(headers: Mapping[str, str], wanted: str) -> str:
        return next((value for name, value in headers.items() if name.lower() == wanted), "")


    def a2a_extension(self) -> str:
        return "https://github.com/google-agentic-commerce/a2a-x402/blob/main/spec/v0.2"

    def a2a_challenge(self, message: dict, limit: int) -> HttpResponse | None:
        result = message.get("result")
        if not isinstance(result, dict):
            return None
        meta = result.get("status", {}).get("message", {}).get("metadata", {})
        required = meta.get("x402.payment.required")
        if required is None:
            if meta.get("x402.payment.status") == "payment-required":
                raise ValueError("embedded A2A payment mandates are unsupported")
            return None
        task = A2ACall.task(message)
        if task.status.state != "input-required" or meta.get("x402.payment.status") != "payment-required":
            raise ValueError("A2A payment challenge state is invalid")
        # Reuse the upstream v2 parser through the existing protocol projection.
        return self.mcp_challenge({"result": {"isError": True, "structuredContent": required}}, limit)

    def a2a_apply_credential(self, message: dict, credential: PaymentCredential) -> None:
        meta = A2ACall.metadata(message)
        if any(key.startswith("x402.payment.") for key in meta):
            raise ValueError("A2A request already contains payment metadata")
        meta.update({"x402.payment.status": "payment-submitted", "x402.payment.payload": self.mcp_credential(credential).value})

    def a2a_presented_credential(self, message: dict, limit: int) -> object | None:
        meta = A2ACall.metadata(message)
        keys = {key for key in meta if key.startswith("x402.payment.")}
        if not keys:
            return None
        if keys != {"x402.payment.status", "x402.payment.payload"} or meta["x402.payment.status"] != "payment-submitted":
            raise ValueError("invalid A2A payment credential metadata")
        return self.presented_mcp_credential({self.mcp_request_metadata_key(): meta["x402.payment.payload"]}, limit)

    def a2a_strip_credential(self, message: dict) -> None:
        meta = A2ACall.metadata(message)
        meta.pop("x402.payment.status", None)
        meta.pop("x402.payment.payload", None)

    def a2a_payment_required(self, challenge: PaymentChallenge, task: Task, rejected: bool) -> McpPaymentRequired:
        required = self._challenge_payment_required(challenge).model_dump(by_alias=True, exclude_none=True)
        meta = {"x402.payment.status": "payment-required", "x402.payment.required": required}
        if rejected:
            meta["x402.payment.error"] = "PAYMENT_INVALID"
        result = task.model_copy(deep=True)
        result.status = TaskStatus(state="input-required", message=self._a2a_message(task, "Payment required", meta))
        return McpPaymentRequired(result=result.model_dump(by_alias=True, exclude_none=True))

    def a2a_evidence(self, message: dict, limit: int) -> MerchantEvidence | None:
        result = message.get("result")
        if not isinstance(result, dict):
            return None
        meta = result.get("status", {}).get("message", {}).get("metadata", {})
        if "x402.payment.receipts" not in meta:
            return None
        A2ACall.task(message)
        receipts = meta["x402.payment.receipts"]
        if not isinstance(receipts, list) or len(receipts) != 1 or meta.get("x402.payment.status") != "payment-completed":
            raise ValueError("A2A requires exactly one completed payment receipt")
        return self.mcp_merchant_evidence({self.mcp_receipt_metadata_key(): receipts[0]}, limit)

    def a2a_inject_receipt(self, message: dict, challenge: PaymentChallenge, payment: SettledIncomingPayment) -> None:
        task = A2ACall.task(message)
        status_message = task.status.message or self._a2a_message(task, "Payment completed", {})
        meta = dict(status_message.metadata or {})
        if any(key.startswith("x402.payment.") for key in meta):
            raise ValueError("handler attempted to supply payment metadata")
        meta.update({"x402.payment.status": "payment-completed", "x402.payment.receipts": [self.mcp_receipt(challenge, payment).value]})
        status_message.metadata = meta
        message["result"]["status"]["message"] = status_message.model_dump(by_alias=True, exclude_none=True)

    @staticmethod
    def _a2a_message(task: Task, text: str, meta: dict) -> Message:
        return Message(message_id=str(uuid4()), role="agent", task_id=task.id, context_id=task.context_id,
                       parts=[Part(root=TextPart(text=text))], metadata=meta)
