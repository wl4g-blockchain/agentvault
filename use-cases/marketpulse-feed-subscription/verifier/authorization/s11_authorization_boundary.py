from __future__ import annotations

import base64
import copy
import json

from verifier.context import VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES11AuthorizationBoundaryVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = Infra.session(context)
        identity = payer_identity(context.payer_language)
        # A canonical challenge obtained from the live merchant is the authority;
        # injected wallet/policy/amount projections must never reach signing.
        status, headers, raw = runtime._http(
            runtime._url("E2E_MARKETPULSE_PORT", "/mcp"),
            "POST",
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": "boundary",
                    "method": "tools/call",
                    "params": {"name": "marketpulse.realtime_news", "arguments": {}},
                }
            ).encode(),
            {
                "content-type": "application/json",
                "x-run-id": "boundary-" + context.run_id,
            },
        )
        if status != 200:
            raise RuntimeError("merchant did not return MCP payment challenge")
        origin = json.loads(
            runtime._sql(
                context.payer_language,
                context.payee_language,
                f"SELECT allowed_origins FROM pay_policy WHERE id='{identity.policy_id}'",
            )
        )[0]
        expires = next(
            value
            for key, value in headers.items()
            if key.lower() == "payment-expires-at"
        )
        required = json.loads(raw)["result"]["structuredContent"]
        canonical = json.dumps(required, separators=(",", ":"))
        headers = {
            "payment-required": base64.b64encode(canonical.encode()).decode(),
            "payment-expires-at": expires,
        }
        body = {
            "scenario_id": identity.scenario_id,
            "run_id": "boundary-" + context.run_id,
            "challenge": {
                "protocol_hint": "x402",
                "origin": origin,
                "resource": origin + "/mcp",
                "content_type": "application/json",
                "headers": headers,
                "body": canonical,
                "expires_at": expires,
            },
        }
        original = Infra.request(context, "POST", "/payments/authorize", body)
        replay = Infra.request(context, "POST", "/payments/authorize", body)
        if original["intent"]["id"] != replay["intent"]["id"]:
            raise RuntimeError(
                "same run and same challenge did not replay the same intent"
            )
        before = runtime._sql(
            context.payer_language,
            context.payee_language,
            "SELECT (SELECT count(*) FROM pay_ledger),count(*) FILTER (WHERE status IN ('signed','settled')),COALESCE(sum(reserved_amount) FILTER (WHERE reservation_status='reserved'),0) FROM pay_intent",
        )
        for field, value in (
            ("wallet_id", identity.primary_wallet_id),
            ("policy_id", identity.policy_id),
            ("amount", "1"),
            ("recipient", "0x" + "11" * 20),
        ):
            Infra.request(
                context,
                "POST",
                "/payments/authorize",
                {**body, field: value},
                (400, 422),
            )
        tampered = copy.deepcopy(body)
        tampered["challenge"]["origin"] = "https://foreign.invalid"
        Infra.request(context, "POST", "/payments/authorize", tampered, (400, 403, 422))
        expired = copy.deepcopy(body)
        expired["challenge"]["expires_at"] = "1"
        Infra.request(context, "POST", "/payments/authorize", expired, (400, 403, 422))
        after = runtime._sql(
            context.payer_language,
            context.payee_language,
            "SELECT (SELECT count(*) FROM pay_ledger),count(*) FILTER (WHERE status IN ('signed','settled')),COALESCE(sum(reserved_amount) FILTER (WHERE reservation_status='reserved'),0) FROM pay_intent",
        )
        if before != after:
            raise RuntimeError(
                "rejected authorization created a signature, reserved funds or ledger entries"
            )
        return Infra.api_evidence(
            context,
            [
                "same run and same challenge replays the same intent",
                "caller cannot choose wallet/policy/amount/recipient",
                "foreign origin rejected",
                "expired challenge rejected",
                "no signed credential, reserved funds or ledger mutation",
            ],
        )
