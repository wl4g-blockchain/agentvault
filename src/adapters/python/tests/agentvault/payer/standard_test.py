import asyncio
import copy
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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


class _SocketTransport(_McpTransport):
    def __init__(self, protocol, fixture, origin, carrier):
        super().__init__(protocol, fixture)
        self.origin, self.carrier = origin, carrier

    def request(self, method, path, body=None):
        value = super().request(method, path, body)
        if path == "/api/v1/payments/authorize":
            value["credential"]["http"]["bound_origin"] = self.origin
        return value

    def _intent(self, status):
        value = super()._intent(status)
        value["resource"] = self.origin + "/" + self.carrier
        return value


class _FramedMerchant(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        self.connection.settimeout(2)
        try:
            if self.headers.get("transfer-encoding", "").lower() == "chunked":
                chunks = []
                while (size := int(self.rfile.readline().strip(), 16)):
                    chunks.append(self.rfile.read(size))
                    self.rfile.read(2)
                self.rfile.read(2)
                body = b"".join(chunks)
            else:
                body = self.rfile.read(int(self.headers.get("content-length", "0")))
            self.server.observed.append({"body": body, "message": json.loads(body),
                                         "headers": dict(self.headers)})
            index = len(self.server.observed)
            status = 503 if index == 2 else 200
            result = self.server.challenge if index == 1 else self.server.settled
            encoded = json.dumps(result).encode() if status == 200 else b'{"error":"delivery failed once"}'
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.send_header("X-A2A-Extensions", self.server.extension)
            self.end_headers()
            self.wfile.write(encoded)
        except Exception as error:
            self.server.errors.append(type(error).__name__)
            self.close_connection = True


class StandardPayerMiddlewareTest(unittest.TestCase):
    def test_real_http_reframes_paid_mcp_a2a_and_delivery_retry_for_both_protocols(self):
        root = Path(__file__).resolve().parents[4] / "tests"
        mcp = json.loads((root / "payment_protocol_mcp.json").read_text())
        a2a = json.loads((root / "payment_protocol_a2a.json").read_text())
        for carrier in ("mcp", "a2a"):
            for protocol in ("x402", "mpp"):
                for framing in ("cOnTeNt-LeNgTh", "tRaNsFeR-EnCoDiNg"):
                    with self.subTest(carrier=carrier, protocol=protocol, framing=framing):
                        server = ThreadingHTTPServer(("127.0.0.1", 0), _FramedMerchant)
                        contract = mcp["protocols"][protocol]
                        server.observed, server.errors = [], []
                        server.extension = a2a["x402_extension"]
                        server.challenge = copy.deepcopy((mcp if carrier == "mcp" else a2a)["protocols"][protocol]["challenge_response"])
                        server.settled = {"jsonrpc": "2.0", "id": 1, "result": {
                            "content": [{"type": "text", "text": "paid"}], "_meta": contract["receipt_meta"],
                        }} if carrier == "mcp" else copy.deepcopy(a2a["protocols"][protocol]["settled_response"])
                        thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
                        thread.start()
                        origin = f"http://127.0.0.1:{server.server_port}"
                        transport = _SocketTransport(protocol, contract, origin, carrier)
                        message = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                            "name": "marketpulse.realtime", "arguments": {},
                        }} if carrier == "mcp" else copy.deepcopy(a2a["request"])
                        original = json.dumps(message, separators=(",", ":")).encode()
                        headers = {"Authorization": "Bearer application-token", "Content-Type": "application/json",
                                   framing: str(len(original)) if framing.lower() == "content-length" else "chunked"}

                        async def execute():
                            core = PayerMiddleware(transport, PayerConfig("scenario", delivery_retries=1, protocols=(protocol,)))
                            async with core.standard(timeout=3) as client:
                                response = await client.post(origin + "/" + carrier,
                                    operation=PayerRequestContext("run-1"), headers=headers, content=original)
                            self.assertEqual(response.status_code, 200)
                            self.assertIn("agentvault.payment", response.extensions)

                        try:
                            asyncio.run(execute())
                            self.assertEqual(server.errors, [])
                            self.assertEqual(len(server.observed), 3)
                            self.assertEqual(server.observed[0]["body"], original)
                            self.assertGreater(len(server.observed[1]["body"]), len(original))
                            self.assertEqual(server.observed[1]["body"], server.observed[2]["body"])
                            for request in server.observed:
                                wire_headers = {name.lower(): value for name, value in request["headers"].items()}
                                self.assertEqual(int(wire_headers["content-length"]), len(request["body"]))
                                self.assertNotIn("transfer-encoding", wire_headers)
                                self.assertEqual(wire_headers["authorization"], "Bearer application-token")
                            self.assertEqual(len(transport.calls), 2)
                            self.assertEqual(transport.calls[-1][2]["protocol"], protocol)
                            self.assertEqual(transport.calls[-1][2]["evidence"]["header_name"],
                                             "payment-response" if protocol == "x402" else "payment-receipt")
                        finally:
                            server.shutdown()
                            server.server_close()
                            thread.join(timeout=2)

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

        with self.assertLogs("agentvault.sdk", level="INFO") as captured:
            HttpxPayerMiddleware._event("agentvault.sdk.payer.signed", run_id="run-1", intent_id="intent-1")
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
