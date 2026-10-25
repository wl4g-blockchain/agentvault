import asyncio
import json
import unittest
from pathlib import Path

import httpx
from mpp.extensions.mcp import MCPCredential
from x402.http.utils import encode_payment_signature_header
from x402.schemas import PaymentPayload

from agentvault.config import PayerConfig
from agentvault.model import PayerRequestContext
from agentvault.payer.payer import PayerMiddleware
from agentvault.payer.standard import HttpxPayerMiddleware
from agentvault.protocol import MppAdapter


class _Transport:
    def request(self, *_args, **_kwargs):
        raise AssertionError("oversized challenges must be rejected before AgentVault is called")


class _McpTransport:
    def __init__(self, protocol: str, fixture: dict[str, object]) -> None:
        self.protocol = protocol
        self.fixture = fixture
        self.calls: list[tuple[str, str, object]] = []

    def request(self, method, path, body=None):
        self.calls.append((method, path, body))
        if path == "/api/v1/payments/authorize":
            if self.protocol == "x402":
                payload = self.fixture["credential_meta"]["x402/payment"]
                header_name = "Payment-Signature"
                header_value = encode_payment_signature_header(
                    PaymentPayload.model_validate(payload)
                )
            else:
                payload = self.fixture["credential_meta"]["org.paymentauth/credential"]
                header_name = "Payment-Authorization"
                header_value = MppAdapter().presented_mcp_credential(
                    {"org.paymentauth/credential": payload}, 64 * 1024,
                )["authorization"]
            return {
                "authorization": {
                    "outcome": "authorized",
                    "intent_id": "intent-1",
                },
                "intent": self._intent("signed"),
                "credential": {
                    "protocol": self.protocol,
                    "challenge_hash": "challenge-1",
                    "credential_hash": "credential-1",
                    "payload": payload,
                    "http": {
                        "bound_origin": "https://merchant.test",
                        "header_name": header_name,
                        "header_value": header_value,
                    },
                },
            }
        if path == "/api/v1/payments/intent-1/outbound-response":
            return {
                "intent": self._intent("pending_settlement"),
                "accepted": True,
                "replayed": False,
            }
        raise AssertionError(f"unexpected AgentVault path: {path}")

    def _intent(self, status: str) -> dict[str, object]:
        return {
            "id": "intent-1",
            "scenario_id": "scenario",
            "run_id": "run-1",
            "wallet_id": "wallet-1",
            "policy_id": "policy-1",
            "direction": "outbound",
            "protocol": self.protocol,
            "asset": {
                "network": "eip155:42431",
                "asset": "USDC",
                "decimals": 6,
            },
            "recipient": "0x" + "11" * 20,
            "amount": "1000",
            "settled_amount": "0",
            "resource": "https://merchant.test/mcp",
            "challenge_hash": "challenge-1",
            "request_hash": "request-1",
            "status": status,
            "reservation_status": "reserved",
            "reserved_amount": "1000",
            "created_at": 1,
            "updated_at": 2,
        }


class StandardPayerMiddlewareTest(unittest.TestCase):
    def test_run_id_is_stable(self):
        self.assertEqual(
            PayerRequestContext("run-1"),
            PayerRequestContext("run-1"),
        )

    def test_oversized_challenge_is_rejected_while_streaming(self):
        async def execute() -> None:
            async def respond(_request: httpx.Request) -> httpx.Response:
                return httpx.Response(402, content=b"x" * 9)

            client = httpx.AsyncClient(
                transport=httpx.MockTransport(respond), follow_redirects=False)
            core = PayerMiddleware(_Transport(), PayerConfig("scenario", max_challenge_bytes=8))
            middleware = HttpxPayerMiddleware(core, client=client)
            try:
                with self.assertRaisesRegex(RuntimeError, "challenge exceeds"):
                    await middleware.post(
                        "https://merchant.test/mcp",
                        operation=PayerRequestContext("run-1"),
                        content=b"{}",
                    )
            finally:
                await client.aclose()

        asyncio.run(execute())

    def test_observability_message_keeps_run_identity(self):
        from agentvault.payer.standard import _event

        with self.assertLogs("agentvault.sdk", level="INFO") as captured:
            _event("agentvault.sdk.payer.signed", run_id="run-1", intent_id="intent-1")
        self.assertIn("run.id=run-1", captured.output[0])
        self.assertIn("intent.id=intent-1", captured.output[0])

    def test_x402_and_mpp_mcp_calls_use_metadata_and_report_native_receipts(self):
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "tests" / "payment_protocol_mcp.json").read_text()
        )

        async def execute(protocol: str) -> None:
            contract = fixture["protocols"][protocol]
            transport = _McpTransport(protocol, contract)
            requests: list[httpx.Request] = []

            async def respond(request: httpx.Request) -> httpx.Response:
                requests.append(request)
                self.assertEqual(request.headers["authorization"], "Bearer application-token")
                self.assertNotIn("payment-signature", request.headers)
                self.assertNotIn("payment-authorization", request.headers)
                message = json.loads(request.content)
                if len(requests) == 1:
                    self.assertNotIn("_meta", message["params"])
                    return httpx.Response(
                        200,
                        headers={"content-type": "application/json"},
                        json=contract["challenge_response"],
                    )
                meta = message["params"]["_meta"]
                if protocol == "x402":
                    self.assertEqual(set(meta), {"x402/payment"})
                    payment = PaymentPayload.model_validate(meta["x402/payment"])
                    self.assertEqual(payment.x402_version, 2)
                else:
                    self.assertEqual(set(meta), {"org.paymentauth/credential"})
                    credential = MCPCredential.from_dict(
                        meta["org.paymentauth/credential"]
                    )
                    self.assertEqual(
                        (credential.challenge.method, credential.challenge.intent),
                        ("tempo", "charge"),
                    )
                return httpx.Response(
                    200,
                    headers={"content-type": "application/json"},
                    json={
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "content": [{"type": "text", "text": "paid"}],
                            "_meta": contract["receipt_meta"],
                        },
                    },
                )

            client = httpx.AsyncClient(
                transport=httpx.MockTransport(respond), follow_redirects=False
            )
            middleware = HttpxPayerMiddleware(
                PayerMiddleware(transport, PayerConfig("scenario")), client=client
            )
            try:
                response = await middleware.post(
                    "https://merchant.test/mcp",
                    operation=PayerRequestContext("run-1"),
                    headers={"authorization": "Bearer application-token"},
                    json={
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {
                            "name": "marketpulse.realtime",
                            "arguments": {"symbol": "AVT"},
                        },
                    },
                )
                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.json()["result"]["content"][0]["text"], "paid")
                self.assertEqual(len(requests), 2)
                self.assertIn("agentvault.payment", response.extensions)
                self.assertEqual(len(transport.calls), 2)
                report = transport.calls[-1][2]
                self.assertEqual(report["protocol"], protocol)
                self.assertEqual(
                    report["evidence"]["header_name"],
                    "payment-response" if protocol == "x402" else "payment-receipt",
                )
            finally:
                await client.aclose()

        for protocol in ("x402", "mpp"):
            with self.subTest(protocol=protocol):
                asyncio.run(execute(protocol))
