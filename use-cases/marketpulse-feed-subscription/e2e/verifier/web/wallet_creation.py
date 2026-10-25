"""Wallet creation browser assertions with an independent RSA API fixture."""

from __future__ import annotations

import base64
import json
import time
import uuid
from pathlib import Path
from typing import Any

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from playwright.sync_api import Page, expect


class WalletCreationApi:
    """Controlled HTTP fixture; it does not stand in for Vault/SQL acceptance."""

    PRIVATE_KEY = bytes.fromhex("11" * 32)

    def __init__(self, wallets: list[dict[str, Any]]) -> None:
        self.wallets = wallets
        self.session_mode = "ready"
        self.session_requests = 0
        self.create_requests = 0
        self.decrypted_wallets = 0
        self.sessions: dict[str, tuple[rsa.RSAPrivateKey, int]] = {}

    def start_session(self) -> tuple[dict[str, Any], int]:
        self.session_requests += 1
        if self.session_mode == "unavailable":
            return {"code": "SESSION_UNAVAILABLE"}, 503
        key = rsa.generate_private_key(public_exponent=65537, key_size=3072)
        public_key = key.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo,
        )
        session_id = str(uuid.uuid4())
        expires_at = int(time.time()) + (120 if self.session_mode != "expired" else -1)
        self.sessions[session_id] = (key, expires_at)
        return {
            "session_id": session_id,
            "server_public_key": (
                base64.urlsafe_b64encode(public_key).decode().rstrip("=")
                if self.session_mode != "invalid_key" else "invalid"
            ),
            "expires_at": expires_at,
        }, 200

    def create_wallet(self, body: dict[str, Any]) -> tuple[dict[str, Any], int]:
        self.create_requests += 1
        envelope = body.get("encrypted_private_key", {})
        if set(body) != {"id", "algorithm", "enabled", "encrypted_private_key"} or set(envelope) != {"session_id", "ciphertext"}:
            return {"code": "INVALID_ENCRYPTED_REQUEST"}, 400
        session = self.sessions.pop(envelope["session_id"], None)
        if session is None or session[1] <= int(time.time()):
            return {"code": "INVALID_SESSION"}, 400
        if body["algorithm"] != "secp256k1" or self.PRIVATE_KEY.hex() in json.dumps(body):
            return {"code": "INVALID_WALLET_REQUEST"}, 400
        try:
            ciphertext = base64.urlsafe_b64decode(envelope["ciphertext"])
            label = f"agentvault.wallet-import.v2\0{envelope['session_id']}\0{body['id']}\0secp256k1".encode()
            plaintext = session[0].decrypt(ciphertext, padding.OAEP(
                mgf=padding.MGF1(hashes.SHA256()), algorithm=hashes.SHA256(), label=label,
            ))
        except (ValueError, TypeError):
            return {"code": "INVALID_CIPHERTEXT"}, 400
        if plaintext != self.PRIVATE_KEY:
            return {"code": "UNEXPECTED_TEST_KEY"}, 400
        self.decrypted_wallets += 1
        wallet = {
            "id": body["id"], "algorithm": body["algorithm"],
            "address": "0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a",
            "public_key": "browser-fixture-public-key", "created_at": int(time.time()),
        }
        self.wallets.insert(0, wallet)
        return wallet, 200


class WalletCreationJourney:
    def __init__(self, api: WalletCreationApi) -> None:
        self.api = api

    def verify(self, page: Page, screenshot_root: Path) -> list[tuple[str, Path]]:
        requests: list[str] = []
        console: list[str] = []
        page.on("request", lambda request: requests.append(request.post_data or ""))
        page.on("console", lambda message: console.append(message.text))
        # Instrument the real browser API, without replacing its implementation.
        page.evaluate("""() => {
          window.__e2eeEncryptions = 0;
          const encrypt = crypto.subtle.encrypt.bind(crypto.subtle);
          crypto.subtle.encrypt = (...args) => {
            window.__e2eeEncryptions += 1;
            return encrypt(...args);
          };
        }""")
        page.get_by_role("link", name="Wallets", exact=True).click()
        expect(page.get_by_role("button", name="Create encrypted Wallet")).to_be_enabled()
        self._fill(page, "wallet-browser-encrypted", "0x" + self.api.PRIVATE_KEY.hex())
        assert self.api.session_requests == self.api.create_requests == 0
        assert page.evaluate("window.__e2eeEncryptions") == 0

        page.get_by_role("button", name="Create encrypted Wallet").click()
        expect(page.get_by_text("WALLET COMMITTED", exact=True)).to_be_visible()
        expect(page.get_by_label("EOA private key", exact=True)).to_have_value("")
        assert self.api.session_requests == self.api.create_requests == self.api.decrypted_wallets == 1
        assert page.evaluate("window.__e2eeEncryptions") == 1
        screenshot = screenshot_root / "UI-08-wallet-created-encrypted.png"
        page.screenshot(path=str(screenshot), full_page=True, animations="disabled")

        page.get_by_role("link", name="Clients", exact=True).click()
        page.get_by_role("link", name="Wallets", exact=True).click()
        expect(page.get_by_role("button", name="Create encrypted Wallet")).to_be_enabled()
        assert self.api.session_requests == self.api.create_requests == 1
        assert page.evaluate("window.__e2eeEncryptions") == 1

        self._fill(page, "wallet-invalid-input", "not-a-private-key")
        page.get_by_role("button", name="Create encrypted Wallet").click()
        expect(page.get_by_text("EOA private key must contain exactly 32 hexadecimal bytes.", exact=True)).to_be_visible()
        assert self.api.session_requests == self.api.create_requests == 1
        for mode, error in (
            ("unavailable", "AgentVault request failed (503, SESSION_UNAVAILABLE)"),
            ("expired", "Wallet encryption session expired."),
            ("invalid_key", ""),
        ):
            self._reject_session(page, mode, error)
        assert self.api.decrypted_wallets == self.api.create_requests == 1
        assert page.evaluate("window.__e2eeEncryptions") == 1
        # No browser request, console message, or Web Storage entry contains the EOA.
        storage = page.evaluate("JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)])")
        assert all(self.api.PRIVATE_KEY.hex() not in value for value in [*requests, *console, storage])
        return [("Wallet created using temporary public-key encryption", screenshot)]

    def _reject_session(self, page: Page, mode: str, error: str) -> None:
        self.api.session_mode = mode
        sessions_before = self.api.session_requests
        self._fill(page, f"wallet-{mode}", self.api.PRIVATE_KEY.hex())
        page.get_by_role("button", name="Create encrypted Wallet").click()
        expect(page.locator(".error-banner")).to_be_visible()
        if error:
            expect(page.locator(".error-banner")).to_have_text(error)
        expect(page.get_by_label("EOA private key", exact=True)).to_have_value("")
        expect(page.get_by_role("button", name="Create encrypted Wallet")).to_be_enabled()
        assert self.api.session_requests == sessions_before + 1
        assert self.api.create_requests == 1

    @staticmethod
    def _fill(page: Page, wallet_id: str, private_key: str) -> None:
        page.get_by_label("Wallet ID", exact=True).fill(wallet_id)
        page.get_by_label("EOA private key", exact=True).fill(private_key)
