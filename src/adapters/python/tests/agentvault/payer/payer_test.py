import unittest

from agentvault.config import PayerConfig
from agentvault.model import PayerHttpRequest, PayerHttpResponse, PayerRequestContext
from agentvault.payer.payer import PayerMiddleware


class _Transport:
    def __init__(self):
        self.calls = []

    def request(self, method, path, body=None):
        self.calls.append((method, path, body))
        return {
            "authorization": {"outcome": "authorized", "intent_id": "intent-1"},
            "intent": {
                "id": "intent-1", "scenario_id": "scenario", "run_id": "run-1",
                "wallet_id": "wallet", "policy_id": "policy", "direction": "outbound",
                "protocol": "x402", "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
                "recipient": "0x0000000000000000000000000000000000000001", "amount": "1000",
                "settled_amount": "0", "resource": "https://merchant.example/mcp",
                "challenge_hash": "challenge", "request_hash": "request",
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
    def test_authorization_reuses_stable_run_id(self):
        transport = _Transport()
        middleware = PayerMiddleware(transport, PayerConfig("scenario"))
        payment = middleware.authorize(
            PayerHttpRequest("POST", "https://merchant.example/mcp", "https://merchant.example", {}, b"", False),
            PayerHttpResponse(402, {"payment-protocol": "x402"}, b"{}"),
            PayerRequestContext("run-1"),
        )
        self.assertEqual(payment.intent.id, "intent-1")
        self.assertEqual(transport.calls[0][2]["run_id"], "run-1")
        self.assertEqual(set(transport.calls[0][2]), {"scenario_id", "run_id", "challenge"})

    def test_captured_protocol_hint_is_checked_against_vault_result(self):
        transport = _Transport()
        middleware = PayerMiddleware(transport, PayerConfig("scenario"))
        with self.assertRaisesRegex(RuntimeError, "not bound"):
            middleware.authorize(
                PayerHttpRequest("POST", "https://merchant.example/mcp",
                                 "https://merchant.example", {}, b"", False),
                PayerHttpResponse(402, {"payment-protocol": "mpp"}, b"{}"),
                PayerRequestContext("run-1"),
            )
        self.assertEqual(transport.calls[0][2]["challenge"]["protocol_hint"], "mpp")
