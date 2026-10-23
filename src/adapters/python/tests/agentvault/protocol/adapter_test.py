import json
import unittest
from datetime import datetime, timezone
from pathlib import Path

from mpp import Receipt
from x402.http.utils import encode_payment_response_header
from x402.schemas import SettleResponse

from agentvault.protocol import PaymentProtocolFactory


class ProtocolAdapterTest(unittest.TestCase):
    def test_shared_protocol_http_fixture(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "fixtures" / "payment_protocol_http.json").read_text()
        )
        self.assertEqual(fixture["schema"], "agentvault.sdk.protocol-http.v1")
        registry = PaymentProtocolFactory().create(("x402", "mpp"))
        encoded_evidence = {
            "x402": encode_payment_response_header(SettleResponse(
                success=True,
                payer="0x" + "33" * 20,
                transaction="0x1111",
                network="eip155:42431",
            )),
            "mpp": Receipt.success(
                "0x1111", datetime(2033, 5, 18, 3, 33, 20, tzinfo=timezone.utc)
            ).to_payment_receipt(),
        }
        for name, contract in fixture["protocols"].items():
            evidence = registry.get(name).merchant_evidence({
                contract["evidence_header"]: encoded_evidence[name]
            })
            self.assertIsNotNone(evidence)
            self.assertEqual(evidence.header_name, contract["evidence_header"])
        with self.assertRaises(ValueError):
            registry.merchant_evidence({
                fixture["protocols"][name]["evidence_header"]: value
                for name, value in encoded_evidence.items()
            })
        self.assertEqual(
            registry.credential_header_names(),
            ("payment-signature", "payment-authorization", "authorization"),
        )
        self.assertEqual(
            registry.merchant_evidence_header_names(),
            ("payment-response", "payment-receipt"),
        )
        expected = {
            (transport, protocol, role)
            for transport in ("grpc", "mqtt")
            for protocol in ("x402", "mpp")
            for role in ("payer", "payee")
        }
        actual = {
            (row["transport"], row["protocol"], row["role"])
            for row in fixture["matrix"]
        }
        self.assertEqual(len(fixture["matrix"]), 8)
        self.assertEqual(actual, expected)

    def test_shared_protocol_mcp_fixture_uses_native_models(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "fixtures" / "payment_protocol_mcp.json").read_text()
        )
        self.assertEqual(fixture["schema"], "agentvault.sdk.protocol-mcp.v1")
        registry = PaymentProtocolFactory().create(("x402", "mpp"))
        combined_credentials: dict[str, object] = {}
        combined_receipts: dict[str, object] = {}
        for name, contract in fixture["protocols"].items():
            with self.subTest(protocol=name):
                challenge = registry.mcp_challenge(contract["challenge_response"], 16_384)
                self.assertIsNotNone(challenge)
                self.assertEqual(challenge[0].name, name)
                credential_protocol, credential = registry.presented_mcp_credential(
                    contract["credential_meta"], 16_384
                )
                self.assertEqual(credential_protocol, name)
                self.assertIsNotNone(credential)
                receipt_protocol, receipt = registry.mcp_merchant_evidence(
                    contract["receipt_meta"], 16_384
                )
                self.assertEqual(receipt_protocol, name)
                self.assertIsNotNone(receipt)
                combined_credentials.update(contract["credential_meta"])
                combined_receipts.update(contract["receipt_meta"])
        with self.assertRaises(ValueError):
            registry.presented_mcp_credential(combined_credentials, 16_384)
        with self.assertRaises(ValueError):
            registry.mcp_merchant_evidence(combined_receipts, 16_384)
        self.assertEqual(
            registry.mcp_request_metadata_keys(),
            ("x402/payment", "org.paymentauth/credential"),
        )
        self.assertEqual(
            registry.mcp_root_request_metadata_keys(),
            ("org.paymentauth/credential",),
        )
        self.assertEqual(
            registry.mcp_receipt_metadata_keys(),
            ("x402/payment-response", "org.paymentauth/receipt"),
        )
        expected = {
            (transport, protocol, role)
            for transport in ("grpc", "mqtt")
            for protocol in ("x402", "mpp")
            for role in ("payer", "payee")
        }
        actual = {
            (row["transport"], row["protocol"], row["role"])
            for row in fixture["matrix"]
        }
        self.assertEqual(actual, expected)

    def test_factory_instances_are_isolated(self) -> None:
        factory = PaymentProtocolFactory()
        first = factory.create(("x402", "mpp"))
        second = factory.create(("mpp",))
        self.assertEqual(first.get("x402").name, "x402")
        with self.assertRaises(ValueError):
            second.get("x402")
