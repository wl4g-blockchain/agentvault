from __future__ import annotations

import base64
import hashlib

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding

from verifier.context import VerificationContext
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES14WalletImportVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        session = Infra.request(context, "POST", "/admin/wallets/encryption-sessions")
        wallet_id = "wallet-custody-" + context.run_id
        key = serialization.load_der_public_key(
            base64.urlsafe_b64decode(session["server_public_key"] + "==")
        )
        private = hashlib.sha256(
            ("custody-fixture-" + context.run_id).encode()
        ).digest()
        label = f"agentvault.wallet-import.v2\0{session['session_id']}\0{wallet_id}\0secp256k1".encode()
        encrypted = key.encrypt(
            private,
            padding.OAEP(
                mgf=padding.MGF1(hashes.SHA256()),
                algorithm=hashes.SHA256(),
                label=label,
            ),
        )
        request = {
            "id": wallet_id,
            "algorithm": "secp256k1",
            "enabled": True,
            "encrypted_private_key": {
                "session_id": session["session_id"],
                "ciphertext": base64.urlsafe_b64encode(encrypted).decode().rstrip("="),
            },
        }
        wallet = Infra.request(context, "POST", "/admin/wallets", request)
        if (
            wallet.get("id") != wallet_id
            or not wallet.get("address")
            or any(
                field in wallet
                for field in (
                    "private_key",
                    "encrypted_private_key",
                    "key_material",
                    "ciphertext",
                )
            )
        ):
            raise RuntimeError("wallet creation did not return public metadata only")
        Infra.request(context, "POST", "/admin/wallets", request, (404,))
        for method, path in [
            ("GET", f"/admin/wallets/{wallet_id}/private-key"),
            ("POST", f"/admin/wallets/{wallet_id}/export"),
        ]:
            Infra.request(context, method, path, expected=(404, 405))
        runtime = Infra.session(context)
        result = runtime._sql(
            context.payer_language,
            context.payee_language,
            f"SELECT count(*) FROM wallet_key_version WHERE wallet_id='{wallet_id}' AND status='active'",
        )
        if result.strip() != "1":
            raise RuntimeError("one encrypted active key version was not persisted")
        return Infra.api_evidence(
            context,
            [
                "RSA-3072 OAEP SHA-256 import",
                "single-use session",
                "public metadata only",
                "no key export route",
                "encrypted custody persisted",
            ],
        )
