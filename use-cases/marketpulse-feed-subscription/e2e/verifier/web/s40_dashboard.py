from __future__ import annotations

import ast
import json
import os
import re
import socket
import subprocess
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlparse

from playwright.sync_api import Page, Route, expect, sync_playwright

from common.context import VerificationContext
from common.project import REPORT_ROOT, REPOSITORY_ROOT
from common.sdk_matrix import matrix_case_keys


CASE_HEADER = re.compile(
    r"^\[payer=(?P<payer>\w+) payee=(?P<payee>\w+) transport=(?P<transport>\w+)\]$",
    re.MULTILINE,
)


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


def _sections(path: Path) -> dict[tuple[str, str, str], str]:
    text = path.read_text(encoding="utf-8")
    matches = list(CASE_HEADER.finditer(text))
    sections: dict[tuple[str, str, str], str] = {}
    for index, match in enumerate(matches):
        key = (match.group("payer"), match.group("payee"), match.group("transport"))
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        if key in sections:
            raise RuntimeError(f"duplicate Matrix case in {path.name}: {key}")
        sections[key] = text[match.end():end].strip()
    return sections


def _matrix_facts(evidence_root: Path, sdk_scope: str) -> list[MatrixFact]:
    expected = matrix_case_keys(sdk_scope)
    payer = _sections(evidence_root / "s21_payer_signing.md")
    payee = _sections(evidence_root / "s22_payee_settlement.md")
    balance = _sections(evidence_root / "s24_gateway_balance.md")
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
    for name, sections in (("payer", set(payer)), ("payee", set(payee)), ("balance", set(balance)), ("matrix", matrix)):
        if sections != expected:
            raise RuntimeError(
                f"{name} evidence does not match scope={sdk_scope}: "
                f"missing={sorted(expected - sections)}, extra={sorted(sections - expected)}"
            )

    facts: list[MatrixFact] = []
    for key in sorted(expected):
        payer_match = re.search(
            r"runs=([^\s]+) outbound_intents=([^\s]+) wallets=([^\s]+)",
            payer[key],
        )
        payee_match = re.search(r"runs=([^\s]+) inbound_intents=([^\s]+)", payee[key])
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
        if not (len(payer_runs) == len(payee_runs) == len(outbound) == len(inbound) == 2):
            raise RuntimeError(f"Matrix case does not contain two settled runs: {key}")
        if payer_runs != payee_runs or len(wallets) != 2:
            raise RuntimeError(f"Payer/Payee run or wallet evidence diverged: {key}")
        paid = sum(int(initial[wallet]) - int(final[wallet]) for wallet in wallets)
        received = int(final["payee"]) - int(initial["payee"])
        if paid != received or paid != 2 * unit_price:
            raise RuntimeError(f"balance conservation evidence diverged for {key}")
        facts.append(MatrixFact(*key, payer_runs[0], outbound[0], inbound[0], wallets[0], unit_price))
    return facts


def _intent(fact: MatrixFact, direction: str, index: int) -> dict[str, Any]:
    intent_id = fact.outbound_intent if direction == "outbound" else fact.inbound_intent
    wallet = fact.wallet_id if direction == "outbound" else "wallet-marketpulse-payee"
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


def _ledger(fact: MatrixFact, direction: str, index: int) -> dict[str, Any]:
    intent_id = fact.outbound_intent if direction == "outbound" else fact.inbound_intent
    return {
        "id": f"ledger-{direction}-{fact.case_id}",
        "intent_id": intent_id,
        "scenario_id": f"scenario-{fact.payer if direction == 'outbound' else fact.payee}",
        "wallet_id": fact.wallet_id if direction == "outbound" else "wallet-marketpulse-payee",
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
    def __init__(self, facts: list[MatrixFact]) -> None:
        self.outbound = [_intent(fact, "outbound", index) for index, fact in enumerate(facts)]
        self.inbound = [_intent(fact, "inbound", index) for index, fact in enumerate(facts)]
        self.ledger = [
            *[_ledger(fact, "outbound", index) for index, fact in enumerate(facts)],
            *[_ledger(fact, "inbound", index) for index, fact in enumerate(facts)],
        ]
        self.clients = [{
            "id": "client-quantstream",
            "name": "QuantStream",
            "auth_principal": "quantstream-e2e",
            "enabled": True,
            "created_at": 1792301900,
            "updated_at": 1792301900,
        }]
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

    def handle(self, route: Route) -> None:
        request = route.request
        parsed = urlparse(request.url)
        path = parsed.path
        query = parse_qs(parsed.query)
        if path == "/api/v1/admin/payments":
            items = self.outbound if query.get("direction") == ["outbound"] else self.inbound
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
                return self._json(route, {"code": "INVALID_WALLET_POOL"}, status=400)
            self.created_request = body
            scenario = {
                "id": body["id"], "client_id": body["client_id"],
                "name": body["name"],
                "enabled": True, "created_at": 1792303000, "updated_at": 1792303000,
            }
            policy = {**body["policy"], "scenario_id": body["id"], "created_at": 1792303000, "updated_at": 1792303000}
            self.scenarios = [scenario]
            return self._json(route, {"scenario": scenario, "policy": policy})
        if path == "/api/v1/payments/balance":
            wallet = query.get("wallet_id", [self.wallets[0]["id"]])[0]
            return self._json(route, {
                "scenario_id": query.get("scenario_id", ["scenario-rust"])[0],
                "wallet_id": wallet,
                "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
                "confirmed_inbound": "0", "confirmed_outbound": "1000000",
                "reserved_outbound": "0", "available": "999999000000", "as_of": 1792303000,
            })
        return self._json(route, {"code": "UNHANDLED_BROWSER_FIXTURE"}, status=404)

    @staticmethod
    def _page(items: list[dict[str, Any]], query: dict[str, list[str]]) -> dict[str, Any]:
        offset = int(query.get("offset", ["0"])[0])
        limit = int(query.get("limit", ["10"])[0])
        if offset < 0 or limit < 1 or limit > 1_000:
            raise RuntimeError(f"invalid browser pagination offset={offset}, limit={limit}")
        return {"items": items[offset:offset + limit], "total": len(items)}

    @staticmethod
    def _json(route: Route, payload: Any, status: int = 200) -> None:
        route.fulfill(
            status=status,
            content_type="application/json",
            body=json.dumps(payload, separators=(",", ":")),
        )


def _free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def _wait_for(origin: str, process: subprocess.Popen[str], timeout: float = 30) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"Dashboard preview exited with status {process.returncode}")
        try:
            with urllib.request.urlopen(origin, timeout=1) as response:
                if response.status == 200:
                    return
        except OSError:
            time.sleep(0.2)
    raise RuntimeError("Dashboard preview did not become ready")


def _screenshot(page: Page, root: Path, name: str) -> Path:
    path = root / f"{name}.png"
    page.evaluate("window.scrollTo(0, 0)")
    page.screenshot(path=str(path), full_page=True, animations="disabled")
    return path


def verify(context: VerificationContext) -> str:
    evidence_run = context.evidence_run_id or context.run_id
    evidence_root = REPORT_ROOT / evidence_run / "payment"
    facts = _matrix_facts(evidence_root, context.e2e_sdk_scope)
    total = len(matrix_case_keys(context.e2e_sdk_scope))
    if len(facts) != total:
        raise RuntimeError(
            f"expected exactly {total} scope={context.e2e_sdk_scope} Matrix cases, "
            f"found {len(facts)}"
        )
    page_size = min(10, total)
    page_count = (total + 9) // 10
    ledger_total = total * 2

    web_root = REPOSITORY_ROOT / "web"
    if not context.skip_build:
        environment = {**os.environ, "HTTPS_PROXY": os.environ.get("HTTPS_PROXY", "http://127.0.0.1:8800")}
        subprocess.run(["npm", "ci", "--ignore-scripts"], cwd=web_root, env=environment, check=True)
        subprocess.run(["npm", "run", "build"], cwd=web_root, env=environment, check=True)
    if not (web_root / "dist/index.html").is_file():
        raise RuntimeError("Dashboard dist is absent; omit --skip-build or build the Web project first")

    screenshot_root = context.report_root / "web" / "screenshots"
    screenshot_root.mkdir(parents=True, exist_ok=True)
    log_path = context.report_root / "web" / "preview.log"
    port = _free_port()
    origin = f"http://127.0.0.1:{port}"
    screenshots: list[tuple[str, Path]] = []
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(
            ["npm", "run", "preview", "--", "--host", "127.0.0.1", "--port", str(port)],
            cwd=web_root,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            _wait_for(origin, process)
            api = DashboardApi(facts)
            with sync_playwright() as playwright:
                try:
                    browser = playwright.chromium.launch(headless=True)
                except Exception as error:
                    raise RuntimeError(
                        "Playwright Chromium is unavailable; run `python -m playwright install chromium`"
                    ) from error
                try:
                    page = browser.new_page(viewport={"width": 1600, "height": 1000}, locale="en-US")
                    page.route("**/api/v1/**", api.handle)

                    page.goto(f"{origin}/payer", wait_until="networkidle")
                    payer_table = page.get_by_test_id("outbound-transactions")
                    expect(payer_table).to_have_attribute("data-total", str(total))
                    expect(payer_table.locator("article.table-row")).to_have_count(page_size)
                    payer_pagination = page.get_by_role("navigation", name="Payer transactions pagination")
                    expect(payer_pagination).to_contain_text(f"Page 1 of {page_count}")
                    first_case = f"{facts[0].payer} Payer × {facts[0].payee} Payee · {facts[0].transport}"
                    expect(payer_table).to_contain_text(first_case)
                    payer_page_one_shot = _screenshot(page, screenshot_root, "UI-01-payer-page-1")
                    screenshots.append(("Payer Matrix page 1", payer_page_one_shot))
                    if total > 10:
                        next_case = (
                            f"{facts[10].payer} Payer × {facts[10].payee} Payee · "
                            f"{facts[10].transport}"
                        )
                        payer_pagination.get_by_role("button", name="Next").click()
                        expect(payer_table).to_contain_text(next_case)
                        expect(payer_table).not_to_contain_text(first_case)
                        payer_page_two_shot = _screenshot(page, screenshot_root, "UI-02-payer-page-2")
                        screenshots.append(("Payer Matrix page 2", payer_page_two_shot))

                    page.get_by_role("link", name="Payee transactions").click()
                    payee_table = page.get_by_test_id("inbound-transactions")
                    expect(payee_table).to_have_attribute("data-total", str(total))
                    expect(payee_table.locator("article.table-row")).to_have_count(page_size)
                    expect(page.get_by_role("navigation", name="Payee transactions pagination")).to_contain_text(
                        f"1–{page_size} of {total}"
                    )
                    payee_shot = _screenshot(page, screenshot_root, "UI-03-payee-page-1")
                    screenshots.append(("Payee Matrix page 1", payee_shot))

                    page.get_by_role("link", name="Balances & ledger").click()
                    expect(page.get_by_test_id("balance-conservation")).to_have_attribute("data-conserved", "true")
                    expect(page.get_by_test_id("balance-conservation")).to_contain_text("PASS")
                    expect(page.get_by_test_id("ledger-records").locator("article.table-row")).to_have_count(
                        min(10, ledger_total)
                    )
                    expect(page.get_by_role("navigation", name="Settlement ledger pagination")).to_contain_text(
                        f"1–{min(10, ledger_total)} of {ledger_total}"
                    )
                    page.get_by_test_id("balance-scenario").fill("scenario-rust")
                    page.get_by_test_id("balance-wallet").fill("wallet-quantstream-01")
                    page.get_by_role("button", name="Calculate balance").click()
                    expect(page.get_by_test_id("wallet-balance")).to_contain_text("999999000000")
                    balance_shot = _screenshot(page, screenshot_root, "UI-04-balance-conservation")
                    screenshots.append(("Balance conservation", balance_shot))

                    page.get_by_role("link", name="Scenarios").click()
                    form = page.get_by_test_id("scenario-create-form")
                    form.locator('input[name="scenario_id"]').fill("scenario-browser-e2e")
                    form.locator('input[name="name"]').fill("Browser E2E wallet pool")
                    form.locator('input[name="policy_id"]').fill("policy-browser-e2e")
                    form.locator('textarea[name="recipients"]').fill("0x0000000000000000000000000000000000000001")
                    form.locator('textarea[name="origins"]').fill("http://marketpulse.example")
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
                    if layout["scrollHeight"] <= layout["clientHeight"] or len(set(layout["lefts"])) != 1:
                        raise RuntimeError(f"Wallet selector is not a single-column scrolling dropdown: {layout}")
                    wallet_choices.nth(0).check()
                    wallet_choices.nth(1).check()
                    expect(wallet_selector.locator("summary")).to_contain_text("2 selected")
                    wallet_dropdown_shot = _screenshot(page, screenshot_root, "UI-05-wallet-dropdown-12-options")
                    screenshots.append(("Wallet dropdown with 12 options", wallet_dropdown_shot))
                    wallet_selector.locator("summary").click()
                    page.get_by_role("button", name="Create atomically").click()
                    expect(page.get_by_test_id("scenario-created")).to_contain_text("2 Wallets")
                    if api.created_request is None:
                        raise RuntimeError("Scenario form did not issue the atomic configuration request")
                    scenario_shot = _screenshot(page, screenshot_root, "UI-06-scenario-created")
                    screenshots.append(("Scenario created", scenario_shot))

                    page.get_by_role("link", name="Clients").click()
                    expect(page.get_by_test_id("client-list")).to_contain_text("QuantStream")
                    expect(page.get_by_test_id("client-scenarios")).to_contain_text("scenario-browser-e2e")
                    expect(page.get_by_role("navigation", name="Clients pagination")).to_contain_text("1–1 of 1")
                    client_shot = _screenshot(page, screenshot_root, "UI-07-client-scenario-ownership")
                    screenshots.append(("Client to Scenario ownership", client_shot))
                finally:
                    browser.close()
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    relative = [path.relative_to(context.report_root).as_posix() for _, path in screenshots]
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
    return "\n".join((
        f"validated real Chromium against completed Matrix evidence run={evidence_run}",
        f"validated exact scope={context.e2e_sdk_scope} evidence ({total} cases) before UI rendering",
        f"Payer and Payee pages each exposed {total} settled Matrix cases with page_size=10",
        f"Balances evaluated all {ledger_total} postings for conservation PASS",
        "Scenario Wallet selector exposed 12 options in one scrolling dropdown column",
        "Scenario creation selected two Wallets and issued one atomic Scenario + Policy request",
        "Client management displayed the trusted identity owner and its associated Scenario",
        "screenshot_gallery=web/screenshots/index.md",
        *(f"screenshot={path}" for path in relative),
    ))
