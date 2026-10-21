from __future__ import annotations

import json
from typing import Mapping

from x402.http.utils import (
    decode_payment_response_header,
    decode_payment_signature_header,
    encode_payment_required_header,
    encode_payment_response_header,
    encode_payment_signature_header,
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
        payload = self._payment_payload(delivery.header_value, "PAYMENT-SIGNATURE")
        if encode_payment_signature_header(payload) != delivery.header_value:
            raise ValueError("PAYMENT-SIGNATURE is not canonical x402 v2")
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
        if encode_payment_signature_header(payload) != encoded:
            raise ValueError("PAYMENT-SIGNATURE is non-canonical or oversized")
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
            payload = decode_payment_signature_header(encoded)
        except (TypeError, ValueError) as error:
            raise ValueError(f"{label} is not valid x402") from error
        if not isinstance(payload, PaymentPayload) or payload.x402_version != 2:
            raise ValueError(f"{label} is not PaymentPayload v2")
        return payload

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
