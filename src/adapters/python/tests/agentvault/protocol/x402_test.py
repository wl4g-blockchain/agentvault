import copy
import json
import unittest
from pathlib import Path

from x402.http.utils import decode_payment_required_header, encode_payment_signature_header
from x402.schemas import PaymentPayload

from agentvault.model import (
    AssetIdentity,
    HttpCredential,
    PaymentChallenge,
    PaymentCredential,
)
from agentvault.protocol import X402Adapter


class X402AdapterTest(unittest.TestCase):
    def test_native_challenge_header(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "tests" / "payment_protocol_mcp.json").read_text()
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

    def test_chain_specific_v2_payloads_and_crossovers(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "tests" / "x402_v2_chain_payloads.json")
            .read_text()
        )
        evm = fixture["cases"]["evm_eip3009"]["payment_payload"]
        solana = fixture["cases"]["solana_spl_exact"]["payment_payload"]
        adapter = X402Adapter()

        for name, payload in (("evm_eip3009", evm), ("solana_spl_exact", solana)):
            with self.subTest(name=name):
                header = encode_payment_signature_header(PaymentPayload.model_validate(payload))
                delivery = HttpCredential(
                    "https://merchant.example", "PAYMENT-SIGNATURE", header,
                )
                credential = PaymentCredential(
                    "x402", "challenge", "credential", payload, delivery,
                )

                self.assertEqual(adapter.credential(credential), delivery)
                metadata = adapter.mcp_credential(credential)
                self.assertEqual(metadata.key, "x402/payment")
                self.assertEqual(metadata.value, payload)
                self.assertEqual(
                    adapter.presented_credential(
                        {"PAYMENT-SIGNATURE": header}, 64 * 1024,
                    ),
                    payload,
                )
                self.assertEqual(
                    adapter.presented_mcp_credential(
                        {metadata.key: metadata.value}, 64 * 1024,
                    ),
                    payload,
                )

        crossover = copy.deepcopy(solana)
        crossover["accepted"] = evm["accepted"]
        with self.assertRaisesRegex(ValueError, "EIP-3009"):
            adapter.presented_mcp_credential({"x402/payment": crossover}, 64 * 1024)

        expanded = copy.deepcopy(evm)
        expanded["payload"]["unexpected"] = True
        with self.assertRaisesRegex(ValueError, "EIP-3009"):
            adapter.presented_mcp_credential({"x402/payment": expanded}, 64 * 1024)

        wrong_type = copy.deepcopy(evm)
        wrong_type["payload"]["authorization"]["validAfter"] = 0
        with self.assertRaisesRegex(ValueError, "EIP-3009"):
            adapter.presented_mcp_credential({"x402/payment": wrong_type}, 64 * 1024)
