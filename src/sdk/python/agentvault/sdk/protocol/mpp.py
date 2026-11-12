from __future__ import annotations

from ..model.a2a import A2ACall

import base64
import json
from dataclasses import dataclass, replace
from datetime import datetime, timezone
from typing import Mapping

from mpp import Challenge, Credential, Receipt
from mpp.extensions.mcp import (
    MCPChallenge,
    MCPCredential,
    MCPReceipt,
    PaymentRequiredError,
    PaymentVerificationError,
)
from mpp.extensions.mcp.constants import (
    CODE_PAYMENT_REQUIRED,
    CODE_PAYMENT_VERIFICATION_FAILED,
    META_CREDENTIAL,
    META_RECEIPT,
)

from ..model import HttpCredential, PaymentChallenge, PaymentCredential, SettledIncomingPayment
from .adapter import (
    HttpResponse,
    McpMetadata,
    McpPaymentRequired,
    MerchantEvidence,
    PaymentProtocol,
)

_MCP_PAYMENT_REQUIRED_META_KEY = "org.paymentauth/payment-required"


@dataclass(frozen=True)
class _McpChallengeProjection:
    """Composes native MCP fields with the header and exact opaque wire binding."""

    native: MCPChallenge
    header: str | None = None
    opaque_raw: str | None = None

    @classmethod
    def parse(cls, value: object) -> "_McpChallengeProjection":
        if not isinstance(value, dict):
            raise ValueError("MPP MCP challenge must be an object")
        if not isinstance(value.get("request"), dict):
            raise ValueError("MPP MCP request must be an expanded object")
        opaque = value.get("opaque")
        native_value = dict(value)
        if opaque is not None:
            decoded = cls.decode_object(opaque, "opaque")
            if not all(isinstance(item, str) for item in decoded.values()):
                raise ValueError("MPP opaque must encode string metadata")
            native_value["opaque"] = decoded
        try:
            native = MCPChallenge.from_dict(native_value)
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError("MPP MCP challenge is invalid") from error
        header = value.get("header")
        if header is not None and (
            not isinstance(header, str) or header.lower() != "payment-authorization"
        ):
            raise ValueError("MPP MCP challenge selected an unsupported credential header")
        return cls(native, header, opaque)

    def wire(self) -> dict[str, object]:
        value: dict[str, object] = self.native.to_dict()
        if self.opaque_raw is not None:
            value["opaque"] = self.opaque_raw
        if self.header is not None:
            value["header"] = self.header
        return value

    def core(self) -> Challenge:
        return replace(self.native.to_core(), header=self.header)

    def www_authenticate(self) -> str:
        core = replace(self.core(), opaque=None)
        header = core.to_www_authenticate(core.realm)
        if self.opaque_raw is not None:
            header += f', opaque="{self.opaque_raw}"'
        return header

    @staticmethod
    def decode_object(value: object, field: str) -> dict:
        if not isinstance(value, str):
            raise ValueError(f"MPP {field} must be a base64url string")
        try:
            raw = base64.b64decode(value + "=" * (-len(value) % 4), altchars=b"-_", validate=True)
            decoded = json.loads(raw)
        except (UnicodeDecodeError, ValueError) as error:
            raise ValueError(f"MPP {field} is not base64url JSON") from error
        if not isinstance(decoded, dict):
            raise ValueError(f"MPP {field} must encode a JSON object")
        return decoded


class MppAdapter(PaymentProtocol):
    @property
    def name(self) -> str:
        return "mpp"

    def credential_header_names(self) -> tuple[str, ...]:
        return ("payment-authorization", "authorization")

    def merchant_evidence_header_names(self) -> tuple[str, ...]:
        return ("payment-receipt",)

    def credential(self, credential: PaymentCredential) -> HttpCredential:
        delivery = credential.http
        if (
            credential.protocol != self.name
            or delivery is None
            or delivery.header_name.lower() not in {"authorization", "payment-authorization"}
        ):
            raise ValueError("MPP credential has no allowed Payment authorization delivery")
        parsed = self._credential(delivery.header_value, "MPP credential")
        self._require_tempo_charge(parsed.challenge.method, parsed.challenge.intent)
        self._require_delivery_header(parsed.challenge, delivery.header_name)
        return delivery

    def merchant_evidence(self, headers: Mapping[str, str]) -> MerchantEvidence | None:
        value = self._header(headers, "payment-receipt")
        if not value:
            return None
        receipt = self._receipt(value, "Payment-Receipt")
        if receipt.status != "success" or receipt.method != "tempo" or not receipt.reference:
            raise ValueError("MPP Payment-Receipt is not a successful Tempo receipt")
        return MerchantEvidence("payment-receipt", value)

    def presented_credential(self, headers: Mapping[str, str], limit: int) -> object | None:
        dedicated = self._header(headers, "payment-authorization")
        authorization = self._payment_authorization(self._header(headers, "authorization"))
        if dedicated and authorization:
            raise ValueError("multiple MPP payment credentials were supplied")
        value = dedicated or authorization
        if not value:
            return None
        if len(value) > limit or "\r" in value or "\n" in value:
            raise ValueError("MPP Payment authorization is invalid or oversized")
        credential = self._credential(value, "MPP Payment authorization")
        self._require_tempo_charge(credential.challenge.method, credential.challenge.intent)
        self._require_delivery_header(
            credential.challenge,
            "Payment-Authorization" if dedicated else "Authorization",
        )
        return {"authorization": credential.to_authorization()}

    def challenge(self, challenge: PaymentChallenge) -> HttpResponse:
        if challenge.protocol != self.name or not isinstance(challenge.payload, dict):
            raise ValueError("MPP challenge protocol or payload is invalid")
        value = challenge.payload.get("www_authenticate")
        if not isinstance(value, str) or "\r" in value or "\n" in value:
            raise ValueError("MPP challenge has no valid WWW-Authenticate value")
        try:
            parsed = Challenge.from_www_authenticate(value)
        except Exception as error:
            raise ValueError("MPP challenge has no valid WWW-Authenticate value") from error
        self._require_tempo_charge(parsed.method, parsed.intent)
        return HttpResponse(
            {"WWW-Authenticate": value},
            "application/json",
            b'{"error":{"code":"PAYMENT_REQUIRED"}}',
        )

    def receipt(self, payment: SettledIncomingPayment) -> HttpResponse:
        receipt = self._payment_receipt(payment)
        return HttpResponse({
            "Payment-Receipt": receipt.to_payment_receipt(),
            "Cache-Control": "private",
        })

    def mcp_request_metadata_key(self) -> str:
        return META_CREDENTIAL

    def mcp_root_request_metadata_key(self) -> str:
        return META_CREDENTIAL

    def mcp_receipt_metadata_key(self) -> str:
        return META_RECEIPT

    def mcp_challenge(self, message: object, limit: int) -> HttpResponse | None:
        encoded = self._bounded_json(message, limit, "MPP MCP message")
        parsed = json.loads(encoded)
        if not isinstance(parsed, dict):
            return None
        error_data = None
        error = parsed.get("error")
        if isinstance(error, dict) and error.get("code") in {
            CODE_PAYMENT_REQUIRED, CODE_PAYMENT_VERIFICATION_FAILED,
        }:
            error_data = error.get("data")
            if not isinstance(error_data, dict):
                raise ValueError("MPP MCP error omitted challenge data")
        result_data = None
        result = parsed.get("result")
        if isinstance(result, dict) and isinstance(result.get("_meta"), dict):
            result_data = result["_meta"].get(_MCP_PAYMENT_REQUIRED_META_KEY)
            if result_data is not None and not isinstance(result_data, dict):
                raise ValueError("MPP MCP result challenge is invalid")
        if error_data is None and result_data is None:
            return None
        if error_data is not None and result_data is not None and error_data != result_data:
            raise ValueError("MPP MCP error challenge conflicts with result metadata")
        projection = self._select_mcp_challenge(error_data or result_data)
        return HttpResponse(
            {"www-authenticate": projection.www_authenticate()},
            "application/json",
            b'{"error":{"code":"PAYMENT_REQUIRED"}}',
        )

    def mcp_credential(self, credential: PaymentCredential) -> McpMetadata:
        delivery = self.credential(credential)
        value = self._authorization_value(delivery.header_value)
        challenge = dict(value["challenge"])
        request_b64 = challenge.get("request")
        challenge["request"] = _McpChallengeProjection.decode_object(request_b64, "request")
        projection = _McpChallengeProjection.parse(challenge)
        if projection.core().request_b64 != request_b64:
            raise ValueError("MPP HTTP request is not canonical for MCP projection")
        native = MCPCredential.from_dict({**value, "challenge": projection.native.to_dict()})
        self._require_tempo_charge(native.challenge.method, native.challenge.intent)
        return McpMetadata(META_CREDENTIAL, {**native.to_dict(), "challenge": projection.wire()})

    def presented_mcp_credential(self, meta: object, limit: int) -> object | None:
        if not isinstance(meta, dict):
            raise ValueError("MCP params._meta must be an object")
        value = meta.get(META_CREDENTIAL)
        if value is None:
            return None
        self._bounded_json(value, limit, "MPP MCP credential")
        if not isinstance(value, dict):
            raise ValueError("MPP MCP credential must be an object")
        projection = _McpChallengeProjection.parse(value.get("challenge"))
        try:
            native = MCPCredential.from_dict({**value, "challenge": projection.native.to_dict()})
            core = native.to_core()
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError("MPP MCP credential is invalid") from error
        self._require_tempo_charge(core.challenge.method, core.challenge.intent)
        # pympp's MCP helper omits header and re-encodes opaque. Compose only
        # those missing fields onto its official HTTP echo; request uses JCS.
        core = replace(core, challenge=replace(
            core.challenge, header=projection.header, opaque=projection.opaque_raw,
        ))
        return {"authorization": core.to_authorization()}

    def mcp_payment_required(self, challenge: PaymentChallenge) -> McpPaymentRequired:
        projection = self._stored_mcp_challenge(challenge)
        error = PaymentRequiredError([projection.native]).to_jsonrpc_error()
        error["data"]["challenges"] = [projection.wire()]
        return McpPaymentRequired(error=error)

    def mcp_payment_rejected(self, challenge: PaymentChallenge, message: str) -> McpPaymentRequired:
        projection = self._stored_mcp_challenge(challenge)
        error = PaymentVerificationError(
            [projection.native],
            reason="verification-failed",
            detail=message,
        ).to_jsonrpc_error()
        error["data"]["challenges"] = [projection.wire()]
        return McpPaymentRequired(error=error)

    def mcp_receipt(
        self, challenge: PaymentChallenge, payment: SettledIncomingPayment,
    ) -> McpMetadata:
        if (
            challenge.protocol != self.name
            or payment.intent.challenge_hash != challenge.challenge_hash
        ):
            raise ValueError("MPP MCP receipt challenge binding is invalid")
        receipt = MCPReceipt.from_core(
            self._payment_receipt(payment), challenge.nonce, "tempo"
        )
        return McpMetadata(META_RECEIPT, receipt.to_dict())

    def mcp_merchant_evidence(self, meta: object, limit: int) -> MerchantEvidence | None:
        if not isinstance(meta, dict):
            raise ValueError("MCP result._meta must be an object")
        value = meta.get(META_RECEIPT)
        if value is None:
            return None
        self._bounded_json(value, limit, "MPP MCP receipt metadata")
        try:
            receipt = MCPReceipt.from_meta(meta)
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError("MPP MCP receipt metadata is invalid") from error
        if (
            receipt is None
            or receipt.status != "success"
            or receipt.method != "tempo"
            or not receipt.challenge_id
            or not receipt.reference
        ):
            raise ValueError("MPP MCP receipt metadata is invalid")
        return MerchantEvidence("payment-receipt", receipt.to_core().to_payment_receipt())

    def _stored_mcp_challenge(self, challenge: PaymentChallenge) -> _McpChallengeProjection:
        if challenge.protocol != self.name or not isinstance(challenge.payload, dict):
            raise ValueError("MPP challenge protocol or payload is invalid")
        projection = _McpChallengeProjection.parse(challenge.payload.get("mcp_challenge"))
        self._require_tempo_charge(projection.native.method, projection.native.intent)
        return projection

    def _select_mcp_challenge(self, data: object) -> _McpChallengeProjection:
        if (
            not isinstance(data, dict)
            or data.get("httpStatus") != 402
            or not isinstance(data.get("challenges"), list)
            or not data["challenges"]
        ):
            raise ValueError("MPP MCP challenge must carry HTTP 402 and at least one challenge")
        for value in data["challenges"]:
            projection = _McpChallengeProjection.parse(value)
            if projection.native.method == "tempo" and projection.native.intent == "charge":
                return projection
        raise ValueError("MPP MCP challenge has no supported Tempo charge")

    def _payment_receipt(self, payment: SettledIncomingPayment) -> Receipt:
        receipt = payment.receipt
        if receipt.protocol != self.name or not receipt.tx_hash or receipt.confirmed_at <= 0:
            raise ValueError("confirmed MPP receipt omitted transaction or confirmation time")
        return Receipt.success(
            receipt.tx_hash,
            datetime.fromtimestamp(receipt.confirmed_at, timezone.utc),
            method="tempo",
            external_id=payment.intent.id,
        )

    @staticmethod
    def _require_tempo_charge(method: str, intent: str) -> None:
        if method != "tempo" or intent != "charge":
            raise ValueError("MPP supports only the implemented Tempo charge flow")

    @staticmethod
    def _require_delivery_header(challenge: Challenge, actual: str) -> None:
        expected = challenge.header or "Authorization"
        if expected.lower() != actual.lower():
            raise ValueError("MPP credential delivery does not match its challenged header")

    @staticmethod
    def _credential(value: str, label: str) -> Credential:
        try:
            return Credential.from_authorization(value)
        except Exception as error:
            raise ValueError(f"{label} is not a valid Payment authorization") from error

    @staticmethod
    def _receipt(value: str, label: str) -> Receipt:
        try:
            return Receipt.from_payment_receipt(value)
        except Exception as error:
            raise ValueError(f"{label} is invalid") from error

    @staticmethod
    def _authorization_value(value: str) -> dict[str, object]:
        credential = MppAdapter._credential(value, "MPP credential")
        canonical = credential.to_authorization()
        token = canonical.split(" ", 1)[1]
        try:
            decoded = base64.urlsafe_b64decode(token + "=" * (-len(token) % 4))
            raw = json.loads(decoded)
        except (UnicodeDecodeError, ValueError) as error:
            raise ValueError("MPP credential cannot be projected to MCP metadata") from error
        if not isinstance(raw, dict):
            raise ValueError("MPP credential is not an object")
        return raw

    @staticmethod
    def _payment_authorization(value: str) -> str:
        for candidate in value.split(","):
            candidate = candidate.strip()
            if candidate[:8].lower() == "payment ":
                return "Payment " + candidate[8:].strip()
        return ""

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


    def a2a_challenge(self, message: dict, limit: int) -> HttpResponse | None:
        return self.mcp_challenge(message, limit) if message.get("error") is not None else None

    def a2a_apply_credential(self, message: dict, credential: PaymentCredential) -> None:
        meta = A2ACall.metadata(message, root=True)
        value = self.mcp_credential(credential)
        if value.key in meta:
            raise ValueError("A2A request already contains an MPP credential")
        meta[value.key] = value.value

    def a2a_presented_credential(self, message: dict, limit: int) -> object | None:
        key = self.mcp_request_metadata_key()
        if key in message.get("params", {}).get("_meta", {}) or key in A2ACall.metadata(message):
            raise ValueError("MPP A2A credentials require JSON-RPC root _meta")
        return self.presented_mcp_credential(A2ACall.metadata(message, root=True), limit)

    def a2a_strip_credential(self, message: dict) -> None:
        A2ACall.metadata(message, root=True).pop(self.mcp_request_metadata_key(), None)

    def a2a_payment_required(self, challenge: PaymentChallenge, task: object, rejected: bool) -> McpPaymentRequired:
        return self.mcp_payment_rejected(challenge, "Payment verification failed") if rejected else self.mcp_payment_required(challenge)

    def a2a_evidence(self, message: dict, limit: int) -> MerchantEvidence | None:
        return self.mcp_merchant_evidence(A2ACall.metadata(message, root=True), limit)

    def a2a_inject_receipt(self, message: dict, challenge: PaymentChallenge, payment: SettledIncomingPayment) -> None:
        meta = A2ACall.metadata(message, root=True)
        value = self.mcp_receipt(challenge, payment)
        if value.key in meta:
            raise ValueError("handler attempted to supply an MPP receipt")
        meta[value.key] = value.value
