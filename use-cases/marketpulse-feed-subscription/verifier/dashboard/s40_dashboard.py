from __future__ import annotations

import ast
import base64
import json
import os
import re
import socket
import subprocess
import time
import urllib.request
import uuid
from contextlib import ExitStack
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from playwright.sync_api import Page, Route, expect, sync_playwright
from util.constant import E2EOtherDefinitions as Project
from util.process import E2EProcessHelper

from verifier.context import VerificationContext

CASE_HEADER = re.compile(
    r"^\[payer=(?P<payer>\w+) payee=(?P<payee>\w+) transport=(?P<transport>\w+)\]$",
    re.MULTILINE,
)


class E2ES40DashboardVerifier:
    class WalletCreationApi:
        """Controlled HTTP fixture; it does not stand in for Vault/SQL acceptance."""

        PRIVATE_KEY = bytes.fromhex("11" * 32)

        def __init__(self, wallets: list[dict[str, Any]]) -> None:
            self.wallets = wallets
            self.session_mode = "ready"
            self.session_requests = 0
            self.create_requests = 0
            self.decrypted_wallets = 0
            self.sessions: dict[str, tuple[rsa.RSAPrivateKey, int]] = {}

        def start_session(self) -> tuple[dict[str, Any], int]:
            self.session_requests += 1
            if self.session_mode == "unavailable":
                return {"code": "SESSION_UNAVAILABLE"}, 503
            key = rsa.generate_private_key(public_exponent=65537, key_size=3072)
            public_key = key.public_key().public_bytes(
                serialization.Encoding.DER,
                serialization.PublicFormat.SubjectPublicKeyInfo,
            )
            session_id = str(uuid.uuid4())
            expires_at = int(time.time()) + (
                120 if self.session_mode != "expired" else -1
            )
            self.sessions[session_id] = (key, expires_at)
            return {
                "session_id": session_id,
                "server_public_key": (
                    base64.urlsafe_b64encode(public_key).decode().rstrip("=")
                    if self.session_mode != "invalid_key"
                    else "invalid"
                ),
                "expires_at": expires_at,
            }, 200

        def create_wallet(self, body: dict[str, Any]) -> tuple[dict[str, Any], int]:
            self.create_requests += 1
            envelope = body.get("encrypted_private_key", {})
            if set(body) != {
                "id",
                "algorithm",
                "enabled",
                "encrypted_private_key",
            } or set(envelope) != {"session_id", "ciphertext"}:
                return {"code": "INVALID_ENCRYPTED_REQUEST"}, 400
            session = self.sessions.pop(envelope["session_id"], None)
            if session is None or session[1] <= int(time.time()):
                return {"code": "INVALID_SESSION"}, 400
            if body["algorithm"] != "secp256k1" or self.PRIVATE_KEY.hex() in json.dumps(
                body
            ):
                return {"code": "INVALID_WALLET_REQUEST"}, 400
            try:
                ciphertext = base64.urlsafe_b64decode(envelope["ciphertext"])
                label = f"agentvault.wallet-import.v2\0{envelope['session_id']}\0{body['id']}\0secp256k1".encode()
                plaintext = session[0].decrypt(
                    ciphertext,
                    padding.OAEP(
                        mgf=padding.MGF1(hashes.SHA256()),
                        algorithm=hashes.SHA256(),
                        label=label,
                    ),
                )
            except (ValueError, TypeError):
                return {"code": "INVALID_CIPHERTEXT"}, 400
            if plaintext != self.PRIVATE_KEY:
                return {"code": "UNEXPECTED_TEST_KEY"}, 400
            self.decrypted_wallets += 1
            wallet = {
                "id": body["id"],
                "algorithm": body["algorithm"],
                "address": "0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a",
                "public_key": "browser-fixture-public-key",
                "created_at": int(time.time()),
            }
            self.wallets.insert(0, wallet)
            return wallet, 200

    class WalletCreationJourney:
        def __init__(self, api: E2ES40DashboardVerifier.WalletCreationApi) -> None:
            self.api = api

        def verify(self, page: Page, screenshot_root: Path) -> list[tuple[str, Path]]:
            requests: list[str] = []
            console: list[str] = []
            page.on("request", lambda request: requests.append(request.post_data or ""))
            page.on("console", lambda message: console.append(message.text))
            # Instrument the real browser API, without replacing its implementation.
            page.evaluate("""() => {
              window.__e2eeEncryptions = 0;
              const encrypt = crypto.subtle.encrypt.bind(crypto.subtle);
              crypto.subtle.encrypt = (...args) => {
                window.__e2eeEncryptions += 1;
                return encrypt(...args);
              };
            }""")
            page.get_by_role("link", name="Wallets", exact=True).click()
            expect(
                page.get_by_role("button", name="Create encrypted Wallet")
            ).to_be_enabled()
            self._fill(
                page, "wallet-browser-encrypted", "0x" + self.api.PRIVATE_KEY.hex()
            )
            assert self.api.session_requests == self.api.create_requests == 0
            assert page.evaluate("window.__e2eeEncryptions") == 0

            page.get_by_role("button", name="Create encrypted Wallet").click()
            expect(page.get_by_text("WALLET COMMITTED", exact=True)).to_be_visible()
            expect(page.get_by_label("EOA private key", exact=True)).to_have_value("")
            assert (
                self.api.session_requests
                == self.api.create_requests
                == self.api.decrypted_wallets
                == 1
            )
            assert page.evaluate("window.__e2eeEncryptions") == 1
            screenshot = screenshot_root / "UI-08-wallet-created-encrypted.png"
            page.screenshot(path=str(screenshot), full_page=True, animations="disabled")

            page.get_by_role("link", name="Clients", exact=True).click()
            page.get_by_role("link", name="Wallets", exact=True).click()
            expect(
                page.get_by_role("button", name="Create encrypted Wallet")
            ).to_be_enabled()
            assert self.api.session_requests == self.api.create_requests == 1
            assert page.evaluate("window.__e2eeEncryptions") == 1

            self._fill(page, "wallet-invalid-input", "not-a-private-key")
            page.get_by_role("button", name="Create encrypted Wallet").click()
            expect(
                page.get_by_text(
                    "EOA private key must contain exactly 32 hexadecimal bytes.",
                    exact=True,
                )
            ).to_be_visible()
            assert self.api.session_requests == self.api.create_requests == 1
            for mode, error in (
                ("unavailable", "AgentVault request failed (503, SESSION_UNAVAILABLE)"),
                ("expired", "Wallet encryption session expired."),
                ("invalid_key", ""),
            ):
                self._reject_session(page, mode, error)
            assert self.api.decrypted_wallets == self.api.create_requests == 1
            assert page.evaluate("window.__e2eeEncryptions") == 1
            # No browser request, console message, or Web Storage entry contains the EOA.
            storage = page.evaluate(
                "JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)])"
            )
            assert all(
                self.api.PRIVATE_KEY.hex() not in value
                for value in [*requests, *console, storage]
            )
            return [
                ("Wallet created using temporary public-key encryption", screenshot)
            ]

        def _reject_session(self, page: Page, mode: str, error: str) -> None:
            self.api.session_mode = mode
            sessions_before = self.api.session_requests
            self._fill(page, f"wallet-{mode}", self.api.PRIVATE_KEY.hex())
            page.get_by_role("button", name="Create encrypted Wallet").click()
            expect(page.locator(".error-banner")).to_be_visible()
            if error:
                expect(page.locator(".error-banner")).to_have_text(error)
            expect(page.get_by_label("EOA private key", exact=True)).to_have_value("")
            expect(
                page.get_by_role("button", name="Create encrypted Wallet")
            ).to_be_enabled()
            assert self.api.session_requests == sessions_before + 1
            assert self.api.create_requests == 1

        @staticmethod
        def _fill(page: Page, wallet_id: str, private_key: str) -> None:
            page.get_by_label("Wallet ID", exact=True).fill(wallet_id)
            page.get_by_label("EOA private key", exact=True).fill(private_key)

    @dataclass(frozen=True)
    class MatrixFact:
        payer: str
        payee: str
        transport: str
        run_id: str
        outbound_intent: str
        inbound_intent: str
        wallet_id: str
        amount: int

        @property
        def case_id(self) -> str:
            return f"{self.payer}-{self.payee}-{self.transport}"

    @staticmethod
    def _sections(path: Path) -> dict[tuple[str, str, str], str]:
        text = path.read_text(encoding="utf-8")
        matches = list(CASE_HEADER.finditer(text))
        sections: dict[tuple[str, str, str], str] = {}
        for index, match in enumerate(matches):
            key = (match.group("payer"), match.group("payee"), match.group("transport"))
            end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
            if key in sections:
                raise RuntimeError(f"duplicate Matrix case in {path.name}: {key}")
            sections[key] = text[match.end() : end].strip()
        return sections

    @staticmethod
    def _matrix_facts(
        evidence_root: Path, sdk_scope: str
    ) -> list[E2ES40DashboardVerifier.MatrixFact]:
        expected = Project.matrix_case_keys(sdk_scope)
        payer = E2ES40DashboardVerifier._sections(
            evidence_root / "s21_payer_signing.md"
        )
        payee = E2ES40DashboardVerifier._sections(
            evidence_root / "s22_payee_settlement.md"
        )
        balance = E2ES40DashboardVerifier._sections(
            evidence_root / "s24_gateway_balance.md"
        )
        matrix_text = (evidence_root / "s30_matrix.md").read_text(encoding="utf-8")
        declared_scope = re.search(r"^scope=(\w+) ", matrix_text, re.MULTILINE)
        if declared_scope is not None and declared_scope.group(1) != sdk_scope:
            raise RuntimeError(
                f"Matrix evidence scope mismatch: expected={sdk_scope}, "
                f"actual={declared_scope.group(1)}"
            )
        matrix = {
            match.groups()
            for match in re.finditer(
                r"^payer=(\w+) payee=(\w+) transport=(\w+)$",
                matrix_text,
                re.MULTILINE,
            )
        }
        for name, sections in (
            ("payer", set(payer)),
            ("payee", set(payee)),
            ("balance", set(balance)),
            ("matrix", matrix),
        ):
            if sections != expected:
                raise RuntimeError(
                    f"{name} evidence does not match scope={sdk_scope}: "
                    f"missing={sorted(expected - sections)}, extra={sorted(sections - expected)}"
                )

        facts: list[E2ES40DashboardVerifier.MatrixFact] = []
        for key in sorted(expected):
            payer_match = re.search(
                r"runs=([^\s]+) outbound_intents=([^\s]+) wallets=([^\s]+)",
                payer[key],
            )
            payee_match = re.search(
                r"runs=([^\s]+) inbound_intents=([^\s]+)", payee[key]
            )
            balance_match = re.search(
                r"initial=(\{.*?\}), final=(\{.*?\}), unit_price=([0-9]+)",
                balance[key],
            )
            if payer_match is None or payee_match is None or balance_match is None:
                raise RuntimeError(f"Matrix evidence is malformed for {key}")
            payer_runs = payer_match.group(1).split(",")
            payee_runs = payee_match.group(1).split(",")
            outbound = payer_match.group(2).split(",")
            inbound = payee_match.group(2).split(",")
            wallets = payer_match.group(3).split(",")
            initial = ast.literal_eval(balance_match.group(1))
            final = ast.literal_eval(balance_match.group(2))
            unit_price = int(balance_match.group(3))
            if not (
                len(payer_runs) == len(payee_runs) == len(outbound) == len(inbound) == 2
            ):
                raise RuntimeError(
                    f"Matrix case does not contain two settled runs: {key}"
                )
            if payer_runs != payee_runs or len(wallets) != 2:
                raise RuntimeError(
                    f"Payer/Payee run or wallet evidence diverged: {key}"
                )
            paid = sum(int(initial[wallet]) - int(final[wallet]) for wallet in wallets)
            received = int(final["payee"]) - int(initial["payee"])
            if paid != received or paid != 2 * unit_price:
                raise RuntimeError(f"balance conservation evidence diverged for {key}")
            facts.append(
                E2ES40DashboardVerifier.MatrixFact(
                    *key, payer_runs[0], outbound[0], inbound[0], wallets[0], unit_price
                )
            )
        return facts

    @staticmethod
    def _intent(
        fact: E2ES40DashboardVerifier.MatrixFact, direction: str, index: int
    ) -> dict[str, Any]:
        intent_id = (
            fact.outbound_intent if direction == "outbound" else fact.inbound_intent
        )
        wallet = (
            fact.wallet_id if direction == "outbound" else "wallet-marketpulse-payee"
        )
        return {
            "id": intent_id,
            "scenario_id": f"scenario-{fact.payer if direction == 'outbound' else fact.payee}",
            "run_id": fact.run_id,
            "wallet_id": wallet,
            "policy_id": f"policy-{fact.payer if direction == 'outbound' else fact.payee}",
            "direction": direction,
            "protocol": "x402",
            "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
            "recipient": "0x0000000000000000000000000000000000000001",
            "amount": str(fact.amount),
            "settled_amount": str(fact.amount),
            "resource": f"marketpulse.realtime_news · {fact.payer} Payer × {fact.payee} Payee · {fact.transport}",
            "request_hash": f"matrix-{fact.case_id}",
            "status": "settled",
            "reservation_status": "consumed",
            "reserved_amount": "0",
            "settlement_ref": f"matrix:{fact.case_id}",
            "created_at": 1792301900 + index,
            "updated_at": 1792302000 + index,
            "settled_at": 1792302000 + index,
        }

    @staticmethod
    def _ledger(
        fact: E2ES40DashboardVerifier.MatrixFact, direction: str, index: int
    ) -> dict[str, Any]:
        intent_id = (
            fact.outbound_intent if direction == "outbound" else fact.inbound_intent
        )
        return {
            "id": f"ledger-{direction}-{fact.case_id}",
            "intent_id": intent_id,
            "scenario_id": f"scenario-{fact.payer if direction == 'outbound' else fact.payee}",
            "wallet_id": fact.wallet_id
            if direction == "outbound"
            else "wallet-marketpulse-payee",
            "entry_type": "payment",
            "direction": direction,
            "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
            "amount": str(fact.amount),
            "posting_key": f"matrix:{fact.case_id}:{direction}",
            "settlement_ref": f"matrix:{fact.case_id}",
            "effective_at": 1792302000 + index,
            "created_at": 1792302000 + index,
        }

    class DashboardApi:
        def __init__(self, facts: list[E2ES40DashboardVerifier.MatrixFact]) -> None:
            self.outbound = [
                E2ES40DashboardVerifier._intent(fact, "outbound", index)
                for index, fact in enumerate(facts)
            ]
            self.inbound = [
                E2ES40DashboardVerifier._intent(fact, "inbound", index)
                for index, fact in enumerate(facts)
            ]
            self.ledger = [
                *[
                    E2ES40DashboardVerifier._ledger(fact, "outbound", index)
                    for index, fact in enumerate(facts)
                ],
                *[
                    E2ES40DashboardVerifier._ledger(fact, "inbound", index)
                    for index, fact in enumerate(facts)
                ],
            ]
            self.clients = [
                {
                    "id": "client-quantstream",
                    "name": "QuantStream",
                    "auth_principal": "quantstream-e2e",
                    "enabled": True,
                    "created_at": 1792301900,
                    "updated_at": 1792301900,
                }
            ]
            self.wallets = [
                {
                    "id": f"wallet-quantstream-{index + 1:02d}",
                    "algorithm": "secp256k1",
                    "address": f"0x{index + 1:040x}",
                    "public_key": f"02{'00' * 31}{index + 1:02x}",
                    "created_at": 1792301900,
                }
                for index in range(12)
            ]
            self.scenarios: list[dict[str, Any]] = []
            self.created_request: dict[str, Any] | None = None
            self.wallet_creation = E2ES40DashboardVerifier.WalletCreationApi(
                self.wallets
            )

        def handle(self, route: Route) -> None:
            request = route.request
            parsed = urlparse(request.url)
            path = parsed.path
            query = parse_qs(parsed.query)
            if (
                path == "/api/v1/admin/wallets/encryption-sessions"
                and request.method == "POST"
            ):
                payload, status = self.wallet_creation.start_session()
                return self._json(route, payload, status)
            if path == "/api/v1/admin/wallets" and request.method == "POST":
                payload, status = self.wallet_creation.create_wallet(
                    request.post_data_json
                )
                return self._json(route, payload, status)
            if path == "/api/v1/admin/payments":
                items = (
                    self.outbound
                    if query.get("direction") == ["outbound"]
                    else self.inbound
                )
                return self._json(route, self._page(items, query))
            if path == "/api/v1/admin/ledger":
                return self._json(route, self._page(self.ledger, query))
            if path == "/api/v1/admin/clients":
                return self._json(route, self._page(self.clients, query))
            if path == "/api/v1/admin/wallets":
                return self._json(route, self._page(self.wallets, query))
            if path == "/api/v1/admin/scenarios" and request.method == "GET":
                return self._json(route, self._page(self.scenarios, query))
            if path == "/api/v1/admin/scenarios" and request.method == "POST":
                body = request.post_data_json
                wallet_ids = body.get("policy", {}).get("wallet_ids", [])
                if len(wallet_ids) != 2 or len(set(wallet_ids)) != 2:
                    return self._json(
                        route, {"code": "INVALID_WALLET_POOL"}, status=400
                    )
                self.created_request = body
                scenario = {
                    "id": body["id"],
                    "client_id": body["client_id"],
                    "name": body["name"],
                    "enabled": True,
                    "created_at": 1792303000,
                    "updated_at": 1792303000,
                }
                policy = {
                    **body["policy"],
                    "scenario_id": body["id"],
                    "created_at": 1792303000,
                    "updated_at": 1792303000,
                }
                self.scenarios = [scenario]
                return self._json(route, {"scenario": scenario, "policy": policy})
            if path == "/api/v1/payments/balance":
                wallet = query.get("wallet_id", [self.wallets[0]["id"]])[0]
                return self._json(
                    route,
                    {
                        "scenario_id": query.get("scenario_id", ["scenario-rust"])[0],
                        "wallet_id": wallet,
                        "asset": {
                            "network": "eip155:31337",
                            "asset": "USDC",
                            "decimals": 6,
                        },
                        "confirmed_inbound": "0",
                        "confirmed_outbound": "1000000",
                        "reserved_outbound": "0",
                        "available": "999999000000",
                        "as_of": 1792303000,
                    },
                )
            return self._json(route, {"code": "UNHANDLED_BROWSER_FIXTURE"}, status=404)

        @staticmethod
        def _page(
            items: list[dict[str, Any]], query: dict[str, list[str]]
        ) -> dict[str, Any]:
            offset = int(query.get("offset", ["0"])[0])
            limit = int(query.get("limit", ["10"])[0])
            if offset < 0 or limit < 1 or limit > 1_000:
                raise RuntimeError(
                    f"invalid browser pagination offset={offset}, limit={limit}"
                )
            return {"items": items[offset : offset + limit], "total": len(items)}

        @staticmethod
        def _json(route: Route, payload: Any, status: int = 200) -> None:
            route.fulfill(
                status=status,
                content_type="application/json",
                body=json.dumps(payload, separators=(",", ":")),
            )

    @staticmethod
    def _free_port() -> int:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            return int(listener.getsockname()[1])

    @staticmethod
    def _wait_for(
        origin: str, process: subprocess.Popen[str], timeout: float = 30
    ) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(
                    f"Dashboard preview exited with status {process.returncode}"
                )
            try:
                with urllib.request.urlopen(origin, timeout=1) as response:
                    if response.status == 200:
                        return
            except OSError:
                time.sleep(0.2)
        raise RuntimeError("Dashboard preview did not become ready")

    @staticmethod
    def _screenshot(page: Page, root: Path, name: str) -> Path:
        path = root / f"{name}.png"
        page.evaluate("window.scrollTo(0, 0)")
        page.screenshot(path=str(path), full_page=True, animations="disabled")
        return path

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        web_root = Project.REPOSITORY_ROOT / "web"
        if not context.skip_build:
            environment = {
                **os.environ,
                "HTTPS_PROXY": os.environ.get("HTTPS_PROXY", "http://127.0.0.1:8800"),
            }
            E2EProcessHelper.run(
                ["npm", "ci", "--ignore-scripts"],
                web_root,
                environment,
                context.timeout_seconds,
            )
            E2EProcessHelper.run(
                ["npm", "run", "build"], web_root, environment, context.timeout_seconds
            )
        if not (web_root / "dist/index.html").is_file():
            raise RuntimeError(
                "Dashboard dist is absent; omit --skip-build or build the Web project first"
            )
        port = E2ES40DashboardVerifier._free_port()
        origin = f"http://127.0.0.1:{port}"
        session = context.state.setdefault("session", ExitStack())
        process = session.enter_context(
            E2EProcessHelper.background(
                [
                    "npm",
                    "run",
                    "preview",
                    "--",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    str(port),
                ],
                web_root,
                context.report_root / "web/preview.log",
            )
        )
        E2ES40DashboardVerifier._wait_for(origin, process)
        context.state["web_origin"] = origin

    @staticmethod
    def init(context: VerificationContext) -> None:
        evidence_run = context.evidence_run_id or context.run_id
        facts = E2ES40DashboardVerifier._matrix_facts(
            Project.REPORT_ROOT / evidence_run / "payment", context.e2e_sdk_scope
        )
        total = len(Project.matrix_case_keys(context.e2e_sdk_scope))
        if len(facts) != total:
            raise RuntimeError(
                f"expected exactly {total} scope={context.e2e_sdk_scope} Matrix cases, found {len(facts)}"
            )
        context.state["web_facts"] = facts

    @staticmethod
    def verify(context: VerificationContext) -> str:
        evidence_run = context.evidence_run_id or context.run_id
        facts = context.state["web_facts"]
        total = len(facts)
        page_size = min(10, total)
        page_count = (total + 9) // 10
        ledger_total = total * 2
        origin = context.state["web_origin"]
        screenshot_root = context.report_root / "web/screenshots"
        screenshot_root.mkdir(parents=True, exist_ok=True)
        screenshots: list[tuple[str, Path]] = []
        api = E2ES40DashboardVerifier.DashboardApi(facts)
        with sync_playwright() as playwright:
            try:
                browser = playwright.chromium.launch(
                    headless=True,
                    executable_path=os.environ.get("E2E_CHROMIUM_EXECUTABLE") or None,
                )
                browser_version = browser.version
            except Exception as error:
                raise RuntimeError(
                    "Playwright Chromium is unavailable; run `python -m playwright install chromium`"
                ) from error
            try:
                page = browser.new_page(
                    viewport={"width": 1600, "height": 1000}, locale="en-US"
                )
                page.route("**/api/v1/**", api.handle)

                page.goto(f"{origin}/payer", wait_until="networkidle")
                payer_table = page.get_by_test_id("outbound-transactions")
                expect(payer_table).to_have_attribute("data-total", str(total))
                expect(payer_table.locator("article.table-row")).to_have_count(
                    page_size
                )
                payer_pagination = page.get_by_role(
                    "navigation", name="Payer transactions pagination"
                )
                expect(payer_pagination).to_contain_text(f"Page 1 of {page_count}")
                first_case = f"{facts[0].payer} Payer × {facts[0].payee} Payee · {facts[0].transport}"
                expect(payer_table).to_contain_text(first_case)
                payer_page_one_shot = E2ES40DashboardVerifier._screenshot(
                    page, screenshot_root, "UI-01-payer-page-1"
                )
                screenshots.append(("Payer Matrix page 1", payer_page_one_shot))
                if total > 10:
                    next_case = (
                        f"{facts[10].payer} Payer × {facts[10].payee} Payee · "
                        f"{facts[10].transport}"
                    )
                    payer_pagination.get_by_role("button", name="Next").click()
                    expect(payer_table).to_contain_text(next_case)
                    expect(payer_table).not_to_contain_text(first_case)
                    payer_page_two_shot = E2ES40DashboardVerifier._screenshot(
                        page, screenshot_root, "UI-02-payer-page-2"
                    )
                    screenshots.append(("Payer Matrix page 2", payer_page_two_shot))

                page.get_by_role("link", name="Payee transactions").click()
                payee_table = page.get_by_test_id("inbound-transactions")
                expect(payee_table).to_have_attribute("data-total", str(total))
                expect(payee_table.locator("article.table-row")).to_have_count(
                    page_size
                )
                expect(
                    page.get_by_role("navigation", name="Payee transactions pagination")
                ).to_contain_text(f"1–{page_size} of {total}")
                payee_shot = E2ES40DashboardVerifier._screenshot(
                    page, screenshot_root, "UI-03-payee-page-1"
                )
                screenshots.append(("Payee Matrix page 1", payee_shot))

                page.get_by_role("link", name="Balances & ledger").click()
                expect(page.get_by_test_id("balance-conservation")).to_have_attribute(
                    "data-conserved", "true"
                )
                expect(page.get_by_test_id("balance-conservation")).to_contain_text(
                    "PASS"
                )
                expect(
                    page.get_by_test_id("ledger-records").locator("article.table-row")
                ).to_have_count(min(10, ledger_total))
                expect(
                    page.get_by_role("navigation", name="Settlement ledger pagination")
                ).to_contain_text(f"1–{min(10, ledger_total)} of {ledger_total}")
                page.get_by_test_id("balance-scenario").fill("scenario-rust")
                page.get_by_test_id("balance-wallet").fill("wallet-quantstream-01")
                page.get_by_role("button", name="Calculate balance").click()
                expect(page.get_by_test_id("wallet-balance")).to_contain_text(
                    "999999000000"
                )
                balance_shot = E2ES40DashboardVerifier._screenshot(
                    page, screenshot_root, "UI-04-balance-conservation"
                )
                screenshots.append(("Balance conservation", balance_shot))

                page.get_by_role("link", name="Scenarios").click()
                form = page.get_by_test_id("scenario-create-form")
                form.locator('input[name="scenario_id"]').fill("scenario-browser-e2e")
                form.locator('input[name="name"]').fill("Browser E2E wallet pool")
                form.locator('input[name="policy_id"]').fill("policy-browser-e2e")
                form.locator('textarea[name="recipients"]').fill(
                    "0x0000000000000000000000000000000000000001"
                )
                form.locator('textarea[name="origins"]').fill(
                    "http://marketpulse.example"
                )
                wallet_selector = page.get_by_test_id("wallet-selector")
                wallet_selector.locator("summary").click()
                wallet_choices = wallet_selector.locator('input[type="checkbox"]')
                expect(wallet_choices).to_have_count(12)
                layout = wallet_selector.locator(".wallet-options").evaluate(
                    """element => ({
                      clientHeight: element.clientHeight,
                      scrollHeight: element.scrollHeight,
                      lefts: [...element.querySelectorAll('.choice-card')].map(item => Math.round(item.getBoundingClientRect().left)),
                    })"""
                )
                if (
                    layout["scrollHeight"] <= layout["clientHeight"]
                    or len(set(layout["lefts"])) != 1
                ):
                    raise RuntimeError(
                        f"Wallet selector is not a single-column scrolling dropdown: {layout}"
                    )
                wallet_choices.nth(0).check()
                wallet_choices.nth(1).check()
                expect(wallet_selector.locator("summary")).to_contain_text("2 selected")
                wallet_dropdown_shot = E2ES40DashboardVerifier._screenshot(
                    page, screenshot_root, "UI-05-wallet-dropdown-12-options"
                )
                screenshots.append(
                    ("Wallet dropdown with 12 options", wallet_dropdown_shot)
                )
                wallet_selector.locator("summary").click()
                page.get_by_role("button", name="Create atomically").click()
                expect(page.get_by_test_id("scenario-created")).to_contain_text(
                    "2 Wallets"
                )
                if api.created_request is None:
                    raise RuntimeError(
                        "Scenario form did not issue the atomic configuration request"
                    )
                scenario_shot = E2ES40DashboardVerifier._screenshot(
                    page, screenshot_root, "UI-06-scenario-created"
                )
                screenshots.append(("Scenario created", scenario_shot))

                page.get_by_role("link", name="Clients").click()
                expect(page.get_by_test_id("client-list")).to_contain_text(
                    "QuantStream"
                )
                expect(page.get_by_test_id("client-scenarios")).to_contain_text(
                    "scenario-browser-e2e"
                )
                expect(
                    page.get_by_role("navigation", name="Clients pagination")
                ).to_contain_text("1–1 of 1")
                client_shot = E2ES40DashboardVerifier._screenshot(
                    page, screenshot_root, "UI-07-client-scenario-ownership"
                )
                screenshots.append(("Client to Scenario ownership", client_shot))
                screenshots.extend(
                    E2ES40DashboardVerifier.WalletCreationJourney(
                        api.wallet_creation
                    ).verify(page, screenshot_root)
                )
            finally:
                browser.close()

        relative = [
            path.relative_to(context.report_root).as_posix() for _, path in screenshots
        ]
        gallery = screenshot_root / "index.md"
        gallery_lines = [
            "# AgentVault Dashboard Chromium evidence",
            "",
            f"Matrix evidence run: `{evidence_run}`",
            f"SDK scope: `{context.e2e_sdk_scope}` ({total} cases)",
            "",
        ]
        for label, path in screenshots:
            gallery_lines.extend((f"![{label}]({path.name})", ""))
        gallery.write_text(
            "\n".join(gallery_lines),
            encoding="utf-8",
        )
        return "\n".join(
            (
                f"validated real Chromium against completed Matrix evidence run={evidence_run}",
                f"browser_version={browser_version}",
                f"validated exact scope={context.e2e_sdk_scope} evidence ({total} cases) before UI rendering",
                f"Payer and Payee pages each exposed {total} settled Matrix cases with page_size=10",
                f"Balances evaluated all {ledger_total} postings for conservation PASS",
                "Scenario Wallet selector exposed 12 options in one scrolling dropdown column",
                "Scenario creation selected two Wallets and issued one atomic Scenario + Policy request",
                "Client management displayed the trusted identity owner and its associated Scenario",
                "Wallet listing/navigation/input did not request encryption sessions or encrypt data",
                "Wallet create fetched one RSA public key and submitted one server-decryptable ciphertext",
                "Malformed input, unavailable/expired sessions and invalid public keys did not submit a Wallet",
                "EOA plaintext was absent from HTTP requests, browser console and Web Storage",
                "Wallet encryption used real Chromium Web Crypto and a controlled RSA API fixture; SQL custody is a separate Rust gate",
                "screenshot_gallery=web/screenshots/index.md",
                *(f"screenshot={path}" for path in relative),
            )
        )
