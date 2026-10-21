import json
import unittest
from pathlib import Path

from x402.http.utils import decode_payment_required_header

from agentvault.model import AssetIdentity, PaymentChallenge
from agentvault.protocol import X402Adapter


class X402AdapterTest(unittest.TestCase):
    def test_native_challenge_header(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "fixtures" / "payment_protocol_mcp.json").read_text()
        )
        required = fixture["protocols"]["x402"]["challenge_response"]["result"][
            "structuredContent"
        ]
        response = X402Adapter().challenge(PaymentChallenge(
            "scenario", None, "https://merchant.example", "x402",
            AssetIdentity("eip155:42431", "USDC", 6, "0x" + "11" * 20),
            "1", "recipient", "nonce", "hash", 2,
            {"payment_required": required},
        ))
        decoded = decode_payment_required_header(response.headers["Payment-Required"])
        self.assertEqual(decoded.x402_version, 2)
        self.assertEqual(len(decoded.accepts), 1)
        self.assertNotIn("WWW-Authenticate", response.headers)

    def test_unrelated_tool_error_is_not_a_payment_challenge(self) -> None:
        challenge = X402Adapter().mcp_challenge({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "isError": True,
                "structuredContent": {"error": "permission denied"},
                "content": [{"type": "text", "text": "permission denied"}],
            },
        }, 64 * 1024)
        self.assertIsNone(challenge)
