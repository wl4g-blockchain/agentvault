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
    def grpc_boundary(
        context: VerificationContext, owner_headers: dict, outsider_headers: dict
    ) -> None:
        import uuid

        import grpc

        runtime = Infra.session(context)
        if runtime.backend == "kubernetes":
            context.state["session"].enter_context(
                runtime._runtime_forwards(("agentvault-grpc",))
            )
        port = int(runtime.environment["E2E_AGENTVAULT_GRPC_PORT"])

        def encode(payload):
            length = len(payload)
            prefix = bytearray([10])
            while length > 127:
                prefix.append((length & 127) | 128)
                length >>= 7
            prefix.append(length)
            return bytes(prefix) + payload

        with grpc.insecure_channel(f"127.0.0.1:{port}") as channel:
            execute = channel.unary_unary(
                "/agentvault.transport.v1.PaymentTransport/Execute",
                request_serializer=encode,
                response_deserializer=lambda value: value,
            )
            for path in ("/api/v1/admin/prices", "/api/v1/unsupported"):
                payload = json.dumps(
                    {"method": "GET", "path": path, "headers": {}, "body": ""}
                ).encode()
                metadata = [
                    (
                        "traceparent",
                        f"00-{uuid.uuid4().hex}-{uuid.uuid4().hex[:16]}-01",
                    ),
                    *owner_headers.items(),
                ]
                try:
                    execute(payload, metadata=metadata, timeout=10)
                except grpc.RpcError as error:
                    if error.code() != grpc.StatusCode.PERMISSION_DENIED:
                        raise RuntimeError(
                            f"unexpected gRPC boundary rejection: {error.code()}"
                        ) from error
                else:
                    raise RuntimeError("gRPC exposed an admin or unknown route")
            intent = context.require("purchase")["evidence"].audit[
                "outbound_intent_ids"
            ][0]
            scenario = payer_identity(context.payer_language).scenario_id
            path = f"/api/v1/payments/{intent}?scenario_id={scenario}"
            payload = json.dumps(
                {"method": "GET", "path": path, "headers": {}, "body": ""}
            ).encode()
            # Missing identity must be denied even for an allowed payment route.
            try:
                execute(
                    payload,
                    metadata=[
                        (
                            "traceparent",
                            f"00-{uuid.uuid4().hex}-{uuid.uuid4().hex[:16]}-01",
                        )
                    ],
                    timeout=10,
                )
            except grpc.RpcError as error:
                if error.code() != grpc.StatusCode.UNAUTHENTICATED:
                    raise
            else:
                raise RuntimeError("gRPC accepted a payment without trusted context")
            for headers, expected in ((outsider_headers, 200), (owner_headers, 403)):
                reply = execute(
                    payload,
                    metadata=[
                        (
                            "traceparent",
                            f"00-{uuid.uuid4().hex}-{uuid.uuid4().hex[:16]}-01",
                        ),
                        *headers.items(),
                    ],
                    timeout=10,
                )
                if not reply or reply[0] != 10:
                    raise RuntimeError("invalid gRPC Envelope response")
                offset, length, shift = 1, 0, 0
                while True:
                    value = reply[offset]
                    offset += 1
                    length |= (value & 127) << shift
                    if value < 128:
                        break
                    shift += 7
                response = json.loads(reply[offset : offset + length])
                if response["status"] != expected:
                    raise RuntimeError(
                        f"gRPC cross-client authorization: expected {expected}, got {response['status']}"
                    )
        context.state.setdefault("requests", []).append(
            {
                "transport": "grpc",
                "admin_and_unknown": "PERMISSION_DENIED",
                "missing_context": "UNAUTHENTICATED",
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
            E2ES15IdentityBoundaryVerifier.grpc_boundary(
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
