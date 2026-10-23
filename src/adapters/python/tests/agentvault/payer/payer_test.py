import unittest
from datetime import datetime, timezone

from mpp import Challenge, Credential, Receipt

from agentvault.config import PayerConfig
from agentvault.model import (
    AuthorizedPayment, HttpCredential, PayerHttpRequest, PayerHttpResponse,
    PayerRequestContext, PaymentCredential, PaymentIntent,
)
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

    def test_native_challenge_headers_are_forwarded_to_agentvault(self):
        for header, value in (
            ("payment-required", "eyJ4NDAyVmVyc2lvbiI6Mn0="),
            ("www-authenticate", 'Payment id="charge", method="tempo"'),
        ):
            with self.subTest(header=header):
                transport = _Transport()

                def fail_after_capture(method, path, body=None):
                    transport.calls.append((method, path, body))
                    raise RuntimeError("vault called")

                transport.request = fail_after_capture
                middleware = PayerMiddleware(transport, PayerConfig("scenario"))
                with self.assertRaisesRegex(RuntimeError, "vault called"):
                    middleware.authorize(
                        PayerHttpRequest("POST", "https://merchant.example/mcp",
                                         "https://merchant.example", {}, b"", False),
                        PayerHttpResponse(402, {header: value}, b"{}"),
                        PayerRequestContext("run-1"),
                    )
                self.assertEqual(transport.calls[0][2]["challenge"]["headers"][header], value)

    def test_mcp_challenge_is_bound_to_jsonrpc_version_and_request_id(self):
        transport = _Transport()
        middleware = PayerMiddleware(transport, PayerConfig("scenario"))
        request = PayerHttpRequest(
            "POST", "https://merchant.example/mcp", "https://merchant.example", {},
            b'{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime"}}',
            False,
        )
        for message in (
            {"jsonrpc": "2.0", "id": 2, "result": {"isError": True}},
            {"jsonrpc": "1.0", "id": 1, "result": {"isError": True}},
        ):
            with self.subTest(message=message):
                self.assertIsNone(middleware.authorize_mcp(
                    request, message, PayerRequestContext("run-1")
                ))
        self.assertEqual(transport.calls, [])
        self.assertTrue(middleware._bound_mcp_response(
            {"jsonrpc": "2.0", "id": 1, "result": {}}, 1,
            require_result=True,
        ))
        self.assertFalse(middleware._bound_mcp_response(
            {"jsonrpc": "2.0", "id": 2, "result": {}}, 1,
            require_result=True,
        ))

    def test_mpp_receipt_and_dedicated_authorization_are_protocolized(self):
        transport = _Transport()
        middleware = PayerMiddleware(transport, PayerConfig("scenario"))
        transport.request = lambda method, path, body=None: (
            transport.calls.append((method, path, body))
            or {"intent": {
                "id": "intent-1", "scenario_id": "scenario", "run_id": "run-1",
                "direction": "outbound", "protocol": "mpp",
                "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
                "recipient": "recipient", "amount": "1", "settled_amount": "0",
                "resource": "https://merchant.example/mcp", "request_hash": "request",
                "status": "pending_settlement", "reservation_status": "reserved", "reserved_amount": "1",
                "created_at": 1, "updated_at": 2,
            }, "accepted": True, "replayed": False}
        )
        intent = PaymentIntent.from_wire({
            "id": "intent-1", "scenario_id": "scenario", "run_id": "run-1",
            "direction": "outbound", "protocol": "mpp",
            "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
            "recipient": "recipient", "amount": "1", "settled_amount": "0",
            "resource": "https://merchant.example/mcp", "request_hash": "request",
            "status": "signed", "reservation_status": "reserved", "reserved_amount": "1",
            "created_at": 1, "updated_at": 1,
        })
        credential = PaymentCredential(
            "mpp", "challenge", "credential", {},
            HttpCredential(
                "https://merchant.example",
                "Payment-Authorization",
                Credential(
                    Challenge(
                        "challenge-0123456789abcdef",
                        "tempo",
                        "charge",
                        {"amount": "1", "currency": "USDC"},
                        realm="merchant.example",
                        header="Payment-Authorization",
                    ).to_echo(),
                    {"type": "hash", "hash": "0x1111"},
                ).to_authorization(),
            ),
        )
        payment = AuthorizedPayment(intent, credential)
        receipt = Receipt.success(
            "0x1111", datetime(2033, 5, 18, 3, 33, 20, tzinfo=timezone.utc)
        ).to_payment_receipt()
        middleware.report(PayerRequestContext("run-1"), payment,
                          PayerHttpResponse(200, {"Payment-Receipt": receipt}))
        body = transport.calls[-1][2]
        self.assertEqual(body["protocol"], "mpp")
        self.assertNotIn("payment_response", body)
        self.assertEqual(body["evidence"]["header_name"], "payment-receipt")
        name, _ = middleware.credential_header(
            payment, "https://merchant.example", {"Authorization": "Bearer app"})
        self.assertEqual(name, "Payment-Authorization")
