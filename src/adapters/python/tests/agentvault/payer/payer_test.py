import unittest

from agentvault.config import PayerConfig
from agentvault.model import (
    AssetIdentity, PayerHttpRequest, PayerHttpResponse, PayerRequestContext,
)
from agentvault.payer.payer import PayerMiddleware


class _Transport:
    def __init__(self):
        self.calls = []

    def request(self, method, path, body=None, idempotency_key=None):
        self.calls.append((method, path, body, idempotency_key))
        return {
            "authorization": {"outcome": "authorized", "intent_id": "intent-1"},
            "intent": {
                "id": "intent-1", "scenario_id": "scenario", "run_id": "run-1",
                "wallet_id": "wallet", "policy_id": "policy", "direction": "outbound",
                "protocol": "x402", "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
                "recipient": "0x0000000000000000000000000000000000000001", "amount": "1000",
                "settled_amount": "0", "resource": "https://merchant.example/mcp",
                "challenge_hash": "challenge", "idempotency_key": "run-1:paid", "request_hash": "request",
                "status": "authorized", "reservation_status": "reserved", "reserved_amount": "1000",
                "created_at": 1, "updated_at": 1,
            },
            "credential": {
                "protocol": "x402", "challenge_hash": "challenge", "credential_hash": "credential",
                "payload": {}, "http": {"bound_origin": "https://merchant.example",
                    "header_name": "payment-signature", "header_value": "signed"},
            },
        }


class PayerMiddlewareTest(unittest.TestCase):
    def test_authorization_reuses_stable_key(self):
        transport = _Transport()
        middleware = PayerMiddleware(transport, PayerConfig(
            "scenario", "wallet", "x402", AssetIdentity("eip155:31337", "USDC", 6),
            "1000", "0x0000000000000000000000000000000000000001"))
        payment = middleware.authorize(
            PayerHttpRequest("POST", "https://merchant.example/mcp", "https://merchant.example", {}, b"", False),
            PayerHttpResponse(402, {"payment-protocol": "x402"}, b"{}"),
            PayerRequestContext.for_operation("run-1", "paid"),
        )
        self.assertEqual(payment.intent.id, "intent-1")
        self.assertEqual(transport.calls[0][2]["idempotency_key"], transport.calls[0][3])

    def test_rejects_protocol_hint_mismatch_before_vault_call(self):
        transport = _Transport()
        middleware = PayerMiddleware(transport, PayerConfig(
            "scenario", "wallet", "x402", AssetIdentity("eip155:31337", "USDC", 6),
            "1000", "0x0000000000000000000000000000000000000001"))
        with self.assertRaisesRegex(RuntimeError, "protocol hint"):
            middleware.authorize(
                PayerHttpRequest("POST", "https://merchant.example/mcp",
                                 "https://merchant.example", {}, b"", False),
                PayerHttpResponse(402, {"payment-protocol": "mpp"}, b"{}"),
                PayerRequestContext.for_operation("run-1", "paid"),
            )
        self.assertEqual(transport.calls, [])
