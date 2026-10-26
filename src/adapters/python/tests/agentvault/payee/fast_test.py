import asyncio
import base64
import json
import unittest
from pathlib import Path
from types import SimpleNamespace

from agentvault.config import PayeeConfig
from agentvault.payee.fast import FastPayeeMiddleware, FastPayeeRoute
from agentvault.payee.payee import PayeeMiddleware


class _NoopTransport:
    def request(self, *_args, **_kwargs):
        raise AssertionError("Vault must not be called for the free tool")


class _AuthorizedMiddleware:
    class _Protocol:
        def presented_credential(self, _headers, _limit):
            return {}

        def receipt(self, _payment):
            return SimpleNamespace(headers={"Payment-Response": "receipt"})

    def protocol(self, _name):
        return self._Protocol()

    def credential_header_names(self):
        return ("payment-signature", "payment-authorization", "authorization")

    def merchant_evidence_header_names(self):
        return ("payment-response", "payment-receipt")

    def presented_credential(self, _protocol, headers):
        return {} if headers.get("payment-signature") else None

    def merchant_evidence(self, headers):
        value = headers.get("payment-response")
        return ("x402", value) if value else (None, None)

    def protect(self, _operation):
        payment = SimpleNamespace(intent=SimpleNamespace(id="intent-1"))
        return SimpleNamespace(outcome="authorized", challenge=None, payment=payment)


class _McpPayeeTransport:
    def __init__(self, protocol: str, contract: dict[str, object]) -> None:
        self.protocol = protocol
        self.contract = contract
        self.reject = False
        self.calls: list[tuple[str, object]] = []
        self.request_hashes: list[str] = []

    def request(self, _method, path, body=None):
        self.calls.append((path, body))
        if path == "/api/v1/payee/challenges":
            self.request_hashes.append(body["request_hash"])
            return self._challenge()
        if path == "/api/v1/payee/verify":
            intent = self._intent(body["request_hash"], "pending_settlement", "0")
            result = {"intent": intent, "valid": not self.reject}
            if not self.reject:
                result["verification_ref"] = "verification-1"
            return result
        if path == "/api/v1/payee/settle":
            intent = self._intent(self.request_hashes[-1], "settled", "1000")
            return {
                "intent": intent,
                "receipt": {
                    "intent_id": "intent-1",
                    "scenario_id": "scenario",
                    "direction": "inbound",
                    "protocol": self.protocol,
                    "asset": self._asset(),
                    "recipient": "0x" + "11" * 20,
                    "payer": "0x" + "33" * 20,
                    "requested_amount": "1000",
                    "settled_amount": "1000",
                    "status": "settled",
                    "tx_hash": "0x1111",
                    "settlement_ref": "settlement-1",
                    "evidence_hash": "evidence-1",
                    "confirmed_at": 2_000_000_000,
                },
            }
        raise AssertionError(f"unexpected AgentVault path: {path}")

    def _challenge(self) -> dict[str, object]:
        if self.protocol == "x402":
            payload = {
                "payment_required": self.contract["challenge_response"]["result"][
                    "structuredContent"
                ]
            }
            nonce = "nonce-1"
        else:
            mcp_challenge = self.contract["challenge_response"]["error"]["data"][
                "challenges"
            ][0]
            request = mcp_challenge["request"]
            encoded = json.dumps(request, separators=(",", ":")).encode()
            request_b64 = base64.urlsafe_b64encode(encoded).rstrip(b"=").decode()
            www_authenticate = (
                f'Payment id="{mcp_challenge["id"]}", '
                f'realm="{mcp_challenge["realm"]}", method="tempo", '
                f'intent="charge", request="{request_b64}", '
                'header="Payment-Authorization"'
            )
            payload = {
                "mcp_challenge": mcp_challenge,
                "www_authenticate": www_authenticate,
            }
            nonce = mcp_challenge["id"]
        return {
            "scenario_id": "scenario",
            "price_id": "price",
            "resource": "mcp://marketpulse/realtime",
            "protocol": self.protocol,
            "asset": self._asset(),
            "amount": "1000",
            "recipient": "0x" + "11" * 20,
            "nonce": nonce,
            "challenge_hash": "challenge-1",
            "expires_at": 2_000_000_060,
            "payload": payload,
        }

    def _intent(
        self, request_hash: str, status: str, settled_amount: str,
    ) -> dict[str, object]:
        return {
            "id": "intent-1",
            "scenario_id": "scenario",
            "run_id": "run-1",
            "price_id": "price",
            "direction": "inbound",
            "protocol": self.protocol,
            "asset": self._asset(),
            "recipient": "0x" + "11" * 20,
            "amount": "1000",
            "settled_amount": settled_amount,
            "resource": "mcp://marketpulse/realtime",
            "challenge_hash": "challenge-1",
            "request_hash": request_hash,
            "status": status,
            "reservation_status": "unreserved",
            "reserved_amount": "0",
            "tx_hash": "0x1111" if status == "settled" else None,
            "settlement_ref": "settlement-1" if status == "settled" else None,
            "created_at": 1,
            "updated_at": 2,
            "settled_at": 2_000_000_000 if status == "settled" else None,
        }

    @staticmethod
    def _asset() -> dict[str, object]:
        return {
            "network": "eip155:42431",
            "asset": "USDC",
            "decimals": 6,
            "token_contract": "0x" + "22" * 20,
        }


class FastPayeeMiddlewareTest(unittest.TestCase):
    def test_route_is_exact(self):
        route = FastPayeeRoute.mcp_tool(
            "/mcp", "marketpulse.realtime_news", "scenario", "price",
            "mcp://marketpulse/realtime", "https://marketpulse.example")
        self.assertEqual((route.method, route.path), ("POST", "/mcp"))
        self.assertEqual(route.mcp_tool_name, "marketpulse.realtime_news")

    def test_bypasses_free_tool_on_shared_mcp_route(self):
        called: list[bytes] = []

        async def app(_scope, receive, _send):
            called.append(await _body(receive))

        route = FastPayeeRoute.mcp_tool(
            "/mcp", "marketpulse.realtime_news", "scenario", "price",
            "mcp://marketpulse/realtime", "https://marketpulse.example")
        middleware = FastPayeeMiddleware(
            app, middleware=PayeeMiddleware(_NoopTransport(), PayeeConfig()), routes=[route])
        body = json.dumps({"method": "tools/call", "params": {"name": "marketpulse.yesterday_news"}}).encode()
        delivered = False

        async def receive():
            nonlocal delivered
            if delivered:
                return {"type": "http.request", "body": b"", "more_body": False}
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}

        async def send(_message):
            return None

        asyncio.run(middleware(
            {"type": "http", "method": "POST", "path": "/mcp", "headers": []}, receive, send))
        self.assertEqual(called, [body])

    def test_resource_release_log_keeps_run_and_intent_identity(self):
        async def app(_scope, _receive, send):
            await send({"type": "http.response.start", "status": 200, "headers": []})
            await send({"type": "http.response.body", "body": b"{}"})

        route = FastPayeeRoute(
            "POST", "/paid", "scenario", "price",
            "https://marketpulse.example/paid", "https://marketpulse.example")
        middleware = FastPayeeMiddleware(app, middleware=_AuthorizedMiddleware(), routes=[route])
        body = b"{}"
        delivered = False

        async def receive():
            nonlocal delivered
            if delivered:
                return {"type": "http.request", "body": b"", "more_body": False}
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}

        async def send(_message):
            return None

        scope = {
            "type": "http", "method": "POST", "path": "/paid",
            "headers": [(b"x-run-id", b"run-1"), (b"payment-signature", b"e30=")],
        }
        with self.assertLogs("agentvault.sdk", level="INFO") as captured:
            asyncio.run(middleware(scope, receive, send))
        self.assertIn("run.id=run-1", captured.output[-1])
        self.assertIn("intent.id=intent-1", captured.output[-1])

    def test_rejects_duplicate_payment_authorization_headers(self):
        async def app(_scope, _receive, _send):
            raise AssertionError("protected application must not receive an ambiguous credential")

        route = FastPayeeRoute.mcp_tool(
            "/mcp", "marketpulse.realtime_news", "scenario", "price",
            "mcp://marketpulse/realtime", "https://marketpulse.example")
        middleware = FastPayeeMiddleware(app, middleware=_AuthorizedMiddleware(), routes=[route])
        body = json.dumps({
            "method": "tools/call", "params": {"name": route.mcp_tool_name}
        }).encode()
        delivered = False
        messages = []

        async def receive():
            nonlocal delivered
            if delivered:
                return {"type": "http.request", "body": b"", "more_body": False}
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}

        async def send(message):
            messages.append(message)

        scope = {
            "type": "http", "method": "POST", "path": "/mcp",
            "headers": [
                (b"x-run-id", b"run-1"),
                (b"payment-authorization", b"Payment first"),
                (b"payment-authorization", b"Payment second"),
            ],
        }
        asyncio.run(middleware(scope, receive, send))

        self.assertEqual(messages[0]["status"], 400)
        self.assertIn(b"duplicate payment request header", messages[1]["body"])

    def test_x402_and_mpp_mcp_payee_flows_verify_settle_and_preserve_bearer(self):
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "tests" / "payment_protocol_mcp.json").read_text()
        )
        for protocol in ("x402", "mpp"):
            with self.subTest(protocol=protocol):
                contract = fixture["protocols"][protocol]
                transport = _McpPayeeTransport(protocol, contract)
                app_calls: list[dict[str, object]] = []

                async def app(scope, receive, send):
                    app_calls.append({
                        "headers": dict(scope["headers"]),
                        "body": json.loads(await _body(receive)),
                    })
                    await send({
                        "type": "http.response.start",
                        "status": 200,
                        "headers": [
                            (b"content-type", b"application/json"),
                            (b"content-length", b"999"),
                        ],
                    })
                    await send({
                        "type": "http.response.body",
                        "body": json.dumps({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "result": {
                                "content": [{"type": "text", "text": "paid"}]
                            },
                        }).encode(),
                    })

                route = FastPayeeRoute.mcp_tool_with_protocol(
                    "/mcp",
                    "marketpulse.realtime",
                    "scenario",
                    "price",
                    "mcp://marketpulse/realtime",
                    "https://merchant.test",
                    protocol,
                )
                middleware = FastPayeeMiddleware(
                    app,
                    middleware=PayeeMiddleware(
                        transport, PayeeConfig(protocols=(protocol,))
                    ),
                    routes=[route],
                )
                request = {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "marketpulse.realtime",
                        "arguments": {"symbol": "AVT"},
                    },
                }
                if protocol == "mpp":
                    request["_meta"] = {"application/root": "preserved"}
                headers = [
                    (b"content-type", b"application/json"),
                    (b"x-run-id", b"run-1"),
                    (b"authorization", b"Bearer application-token"),
                ]
                unpaid = self._invoke(middleware, request, headers)
                self.assertEqual(unpaid[0]["status"], 200)
                self.assertFalse(app_calls)
                challenge_headers = {
                    name.lower(): value for name, value in unpaid[0]["headers"]
                }
                self.assertEqual(challenge_headers[b"payment-protocol"], protocol.encode())
                self.assertEqual(challenge_headers[b"payment-expires-at"], b"2000000060")
                challenge = json.loads(unpaid[1]["body"])
                if protocol == "x402":
                    self.assertTrue(challenge["result"]["isError"])
                    self.assertNotIn("error", challenge)
                else:
                    self.assertEqual(challenge["error"]["code"], -32042)
                    self.assertNotIn("result", challenge)

                if protocol == "mpp":
                    request["_meta"].update(contract["credential_meta"])
                else:
                    request["params"]["_meta"] = contract["credential_meta"]
                paid = self._invoke(middleware, request, headers)
                self.assertEqual(paid[0]["status"], 200)
                response_headers = {
                    name.lower(): value for name, value in paid[0]["headers"]
                }
                self.assertNotIn(b"content-length", response_headers)
                self.assertNotIn(b"payment-response", response_headers)
                self.assertNotIn(b"payment-receipt", response_headers)
                response = json.loads(paid[1]["body"])
                receipt_key = (
                    "x402/payment-response"
                    if protocol == "x402"
                    else "org.paymentauth/receipt"
                )
                self.assertIn(receipt_key, response["result"]["_meta"])
                self.assertEqual(
                    app_calls[0]["headers"][b"authorization"],
                    b"Bearer application-token",
                )
                business_meta = app_calls[0]["body"]["params"].get("_meta", {})
                self.assertNotIn("x402/payment", business_meta)
                self.assertNotIn("org.paymentauth/credential", business_meta)
                business_root_meta = app_calls[0]["body"].get("_meta", {})
                self.assertNotIn("org.paymentauth/credential", business_root_meta)
                if protocol == "mpp":
                    self.assertEqual(
                        business_root_meta.get("application/root"), "preserved"
                    )
                self.assertEqual(len(app_calls), 1)
                self.assertEqual(transport.request_hashes[0], transport.request_hashes[1])
                self.assertEqual(
                    [path for path, _ in transport.calls],
                    [
                        "/api/v1/payee/challenges",
                        "/api/v1/payee/challenges",
                        "/api/v1/payee/verify",
                        "/api/v1/payee/settle",
                    ],
                )
                if protocol == "mpp":
                    request["params"]["_meta"] = contract["credential_meta"]
                    ambiguous = self._invoke(middleware, request, headers)
                    self.assertEqual(ambiguous[0]["status"], 400)
                    self.assertIn(
                        b"duplicate MCP payment credential metadata",
                        ambiguous[1]["body"],
                    )
                    self.assertEqual(len(app_calls), 1)
                    self.assertEqual(len(transport.calls), 4)

    def test_mpp_invalid_mcp_credential_returns_verification_error(self):
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "tests" / "payment_protocol_mcp.json").read_text()
        )
        contract = fixture["protocols"]["mpp"]
        transport = _McpPayeeTransport("mpp", contract)
        transport.reject = True

        async def app(_scope, _receive, _send):
            raise AssertionError("invalid payment must not release the protected tool")

        middleware = FastPayeeMiddleware(
            app,
            middleware=PayeeMiddleware(
                transport, PayeeConfig(protocols=("mpp",))
            ),
            routes=[FastPayeeRoute.mcp_tool_with_protocol(
                "/mcp",
                "marketpulse.realtime",
                "scenario",
                "price",
                "mcp://marketpulse/realtime",
                "https://merchant.test",
                "mpp",
            )],
        )
        request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "marketpulse.realtime",
                "arguments": {"symbol": "AVT"},
                "_meta": contract["credential_meta"],
            },
        }
        messages = self._invoke(middleware, request, [
            (b"content-type", b"application/json"),
            (b"x-run-id", b"run-1"),
            (b"authorization", b"Bearer application-token"),
        ])
        response = json.loads(messages[1]["body"])
        self.assertEqual(messages[0]["status"], 200)
        self.assertEqual(response["error"]["code"], -32043)
        self.assertTrue(response["error"]["data"]["challenges"])
        self.assertEqual(
            [path for path, _ in transport.calls],
            ["/api/v1/payee/challenges", "/api/v1/payee/verify"],
        )

    def test_paid_mcp_business_failure_is_preserved_for_delivery_retry(self):
        fixture = json.loads(
            (Path(__file__).resolve().parents[4] / "tests" / "payment_protocol_mcp.json").read_text()
        )
        contract = fixture["protocols"]["x402"]

        async def app(_scope, _receive, send):
            await send({
                "type": "http.response.start",
                "status": 503,
                "headers": [(b"content-type", b"application/json")],
            })
            await send({
                "type": "http.response.body",
                "body": json.dumps({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "error": {"code": -32004, "message": "retry delivery"},
                }).encode(),
            })

        middleware = FastPayeeMiddleware(
            app,
            middleware=PayeeMiddleware(
                _McpPayeeTransport("x402", contract),
                PayeeConfig(protocols=("x402",)),
            ),
            routes=[FastPayeeRoute.mcp_tool(
                "/mcp",
                "marketpulse.realtime",
                "scenario",
                "price",
                "mcp://marketpulse/realtime",
                "https://merchant.test",
            )],
        )
        messages = self._invoke(middleware, {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "marketpulse.realtime",
                "_meta": contract["credential_meta"],
            },
        }, [
            (b"content-type", b"application/json"),
            (b"x-run-id", b"run-1"),
        ])
        self.assertEqual(messages[0]["status"], 503)
        response = json.loads(messages[1]["body"])
        self.assertEqual(response["error"]["message"], "retry delivery")
        self.assertNotIn("result", response)

    @staticmethod
    def _invoke(
        middleware: FastPayeeMiddleware,
        body: dict[str, object],
        headers: list[tuple[bytes, bytes]],
    ) -> list[dict[str, object]]:
        async def execute() -> list[dict[str, object]]:
            delivered = False
            messages: list[dict[str, object]] = []
            encoded = json.dumps(body, separators=(",", ":")).encode()

            async def receive():
                nonlocal delivered
                if delivered:
                    return {"type": "http.request", "body": b"", "more_body": False}
                delivered = True
                return {
                    "type": "http.request",
                    "body": encoded,
                    "more_body": False,
                }

            async def send(message):
                messages.append(dict(message))

            await middleware(
                {
                    "type": "http",
                    "method": "POST",
                    "path": "/mcp",
                    "headers": headers,
                },
                receive,
                send,
            )
            return messages

        return asyncio.run(execute())


async def _body(receive):
    return (await receive()).get("body", b"")
