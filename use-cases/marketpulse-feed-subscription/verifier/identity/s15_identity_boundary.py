from __future__ import annotations

import base64
import hashlib
import hmac
import json
import time

from util.constant import E2EOtherDefinitions as Project

from verifier.context import ConsoleResourceFixture, VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES15IdentityBoundaryVerifier:
    @staticmethod
    def signed(principal: str, key: str, expires: int = 30) -> dict[str, str]:
        now = int(time.time())
        payload = (
            base64.urlsafe_b64encode(
                json.dumps(
                    {
                        "version": 3,
                        "principal_id": principal,
                        "issued_at_epoch_seconds": now - 120 if expires < 0 else now,
                        "expires_at_epoch_seconds": now + expires,
                    },
                    separators=(",", ":"),
                ).encode()
            )
            .decode()
            .rstrip("=")
        )
        value = "agctx1." + payload
        signature = (
            base64.urlsafe_b64encode(
                hmac.new(key.encode(), value.encode(), hashlib.sha256).digest()
            )
            .decode()
            .rstrip("=")
        )
        return {"x-authguard-context": value + "." + signature}

    @staticmethod
    def http_boundary(
        context: VerificationContext, owner_headers: dict, outsider_headers: dict
    ) -> None:
        Infra.request(
            context, "GET", "/unsupported", expected=(404,), headers=owner_headers
        )
        intent = context.require("purchase")["evidence"].audit["outbound_intent_ids"][0]
        scenario = payer_identity(context.payer_language).scenario_id
        path = f"/payments/{intent}?scenario_id={scenario}"
        Infra.request(context, "GET", path, expected=(401,))
        for headers, expected in ((outsider_headers, 200), (owner_headers, 403)):
            Infra.request(context, "GET", path, expected=(expected,), headers=headers)
        context.state.setdefault("requests", []).append(
            {
                "transport": "http",
                "unknown_route": 404,
                "missing_context": 401,
                "cross_client": "denied",
            }
        )

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = Infra.session(context)
        resources = ConsoleResourceFixture.load_agentvault_resources(
            Project.AGENTVAULT_CONFIG / "init.import.yaml"
        )
        merchant = next(
            s for s in resources["scenarios"] if s["id"] == "scenario-marketpulse-feed"
        )
        owner = next(
            c["auth_principal"]
            for c in resources["clients"]
            if c["id"] == merchant["client_id"]
        )
        outsider = next(
            c["auth_principal"]
            for c in resources["clients"]
            if c["id"] == payer_identity(context.payer_language).client_id
        )
        import re

        from verifier.infra.s00_infrastructure import COMPOSE_FILE

        template = COMPOSE_FILE.read_text()
        default_key = re.search(r"E2E_AGENTVAULT_APPROVAL_KEY:-([^}]+)", template)[1]
        key = runtime.environment.get("E2E_AGENTVAULT_APPROVAL_KEY", default_key)
        try:
            Infra.configure(
                context,
                {
                    "server.authguard.enabled": True,
                    "server.authguard.scope_resolver.enabled": False,
                    "server.authguard.direct_context_hmac_key": {
                        "file": "",
                        "env": "HELPER_SECRET_APPROVAL_JWT_KEY_V1",
                    },
                },
            )
            path = "/admin/prices/price-realtime-news"
            Infra.request(context, "GET", path, expected=(401,))
            Infra.request(
                context,
                "GET",
                path,
                expected=(401,),
                headers=E2ES15IdentityBoundaryVerifier.signed(
                    owner, "invalid-test-key"
                ),
            )
            Infra.request(
                context,
                "GET",
                path,
                expected=(401,),
                headers=E2ES15IdentityBoundaryVerifier.signed(owner, key, -60),
            )
            Infra.request(
                context,
                "GET",
                path,
                headers=E2ES15IdentityBoundaryVerifier.signed(owner, key),
            )
            Infra.request(
                context,
                "GET",
                path,
                expected=(403,),
                headers=E2ES15IdentityBoundaryVerifier.signed(outsider, key),
            )
            E2ES15IdentityBoundaryVerifier.http_boundary(
                context,
                E2ES15IdentityBoundaryVerifier.signed(owner, key),
                E2ES15IdentityBoundaryVerifier.signed(outsider, key),
            )
            conflicting = {
                **E2ES15IdentityBoundaryVerifier.signed(owner, key),
                "x-authguard-scope-token": "ags_" + "a" * 43,
            }
            Infra.request(context, "GET", path, expected=(401,), headers=conflicting)
        finally:
            Infra.configure(context, {"server.authguard.enabled": False})
        Infra.request(context, "GET", "/admin/prices/price-realtime-news")
        return Infra.api_evidence(
            context,
            [
                "real Vault direct-context signature verification",
                "missing/forged/expired identity denied",
                "cross-client Price denied",
                "conflicting identity headers denied",
                "explicit open mode restored",
                "issuer fixture: no AuthGuard IAM or gateway claim",
            ],
        )
