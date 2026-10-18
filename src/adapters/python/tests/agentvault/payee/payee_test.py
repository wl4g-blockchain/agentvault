import unittest

from agentvault.config import PayeeConfig
from agentvault.model import PayeeOperation
from agentvault.payee.payee import PayeeMiddleware


class _RejectingTransport:
    def __init__(self):
        self.paths = []

    def request(self, _method, path, _body=None):
        self.paths.append(path)
        if path == "/api/v1/payee/challenges":
            return {
                "scenario_id": "scenario", "offer_id": "offer",
                "resource": "mcp://marketpulse/realtime", "protocol": "x402",
                "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
                "amount": "1000", "recipient": "0x1", "nonce": "nonce",
                "challenge_hash": "challenge", "expires_at": 1,
                "payload": {"payment_required": {}},
            }
        if path == "/api/v1/payee/verify":
            return {
                "intent": {
                    "id": "intent", "scenario_id": "scenario", "run_id": "run",
                    "offer_id": "offer", "direction": "inbound", "protocol": "x402",
                    "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
                    "recipient": "0x1", "amount": "1000", "settled_amount": "0",
                    "resource": "mcp://marketpulse/realtime", "challenge_hash": "challenge",
                    "request_hash": "request-hash",
                    "status": "pending_settlement", "reservation_status": "reserved",
                    "reserved_amount": "1000", "created_at": 1, "updated_at": 1,
                },
                "valid": False,
            }
        raise AssertionError(f"unexpected Vault call after invalid verification: {path}")


class PayeeMiddlewareTest(unittest.TestCase):
    def test_phases_reuse_the_same_run(self):
        operation = PayeeOperation(
            "scenario", "run", "offer", "mcp://marketpulse/realtime",
            "https://marketpulse.example", "x402", "hash")
        self.assertEqual(operation.challenge_context().run_id, "run")
        self.assertEqual(operation.settlement_context().run_id, "run")

    def test_rejects_invalid_payment_before_settlement(self):
        transport = _RejectingTransport()
        operation = PayeeOperation(
            "scenario", "run", "offer", "mcp://marketpulse/realtime",
            "https://marketpulse.example", "x402", "request-hash", {"signature": "present"})
        with self.assertRaises(PermissionError):
            PayeeMiddleware(transport, PayeeConfig()).protect(operation)
        self.assertEqual(
            transport.paths,
            ["/api/v1/payee/challenges", "/api/v1/payee/verify"],
        )

    def test_observability_message_keeps_run_and_intent_identity(self):
        from agentvault.payee.payee import _event

        with self.assertLogs("agentvault.sdk", level="INFO") as captured:
            _event("agentvault.sdk.payee.settled", run_id="run-1", intent_id="intent-1")
        self.assertIn("run.id=run-1", captured.output[0])
        self.assertIn("intent.id=intent-1", captured.output[0])
