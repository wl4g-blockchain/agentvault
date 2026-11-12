from __future__ import annotations

import hashlib
import json
import os
from urllib.parse import urlsplit

from playwright.sync_api import expect, sync_playwright

from verifier.context import VerificationContext
from verifier.dashboard.s40_dashboard import E2ES40DashboardVerifier
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES41LiveWalletVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")
        E2ES40DashboardVerifier.deploy(context)

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = Infra.session(context)
        directory = context.report_root / "live-wallet"
        directory.mkdir(parents=True, exist_ok=True)
        private = hashlib.sha256(
            ("browser-custody-fixture-" + context.run_id).encode()
        ).hexdigest()
        wallet_id = "wallet-browser-live-" + context.run_id
        statuses = []
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                headless=True,
                executable_path=os.environ.get("E2E_CHROMIUM_EXECUTABLE") or None,
            )
            try:
                page = browser.new_page(viewport={"width": 1440, "height": 1000})
                console = []
                page.on("console", lambda message: console.append(message.text))

                def proxy(route):
                    request = route.request
                    parsed = urlsplit(request.url)
                    body = request.post_data
                    if private in (body or ""):
                        raise RuntimeError("browser sent a plaintext EOA")
                    status, headers, raw = runtime._http(
                        runtime._url(
                            "E2E_AGENTVAULT_PORT",
                            parsed.path + ("?" + parsed.query if parsed.query else ""),
                        ),
                        request.method,
                        body.encode() if body else None,
                        {
                            "accept": "application/json",
                            "content-type": "application/json",
                        },
                    )
                    statuses.append(
                        {
                            "method": request.method,
                            "path": parsed.path,
                            "status": status,
                        }
                    )
                    route.fulfill(
                        status=status,
                        body=raw,
                        content_type=headers.get("Content-Type", "application/json"),
                    )

                page.route("**/api/v1/**", proxy)
                page.goto(context.state["web_origin"] + "/wallets")
                expect(
                    page.get_by_role("button", name="Create encrypted Wallet")
                ).to_be_enabled()
                page.get_by_label("Wallet ID", exact=True).fill(wallet_id)
                page.get_by_label("EOA private key", exact=True).fill(private)
                page.get_by_role("button", name="Create encrypted Wallet").click()
                expect(page.get_by_text("WALLET COMMITTED", exact=True)).to_be_visible()
                expect(page.get_by_label("EOA private key", exact=True)).to_have_value(
                    ""
                )
                if any(private in value for value in console):
                    raise RuntimeError("browser console contains private key")
                storage = page.evaluate(
                    "JSON.stringify([Object.entries(localStorage),Object.entries(sessionStorage)])"
                )
                if private in storage:
                    raise RuntimeError("browser persisted plaintext EOA")
                page.screenshot(
                    path=str(directory / "wallet-committed.png"), full_page=True
                )
                version = browser.version
            finally:
                browser.close()
        imports = [r for r in statuses if r["method"] == "POST"]
        if [(r["path"], r["status"]) for r in imports] != [
            ("/api/v1/admin/wallets/encryption-sessions", 200),
            ("/api/v1/admin/wallets", 200),
        ]:
            raise RuntimeError(f"unexpected live browser import requests: {imports}")
        count = runtime._sql(
            context.payer_language,
            context.payee_language,
            f"SELECT count(*) FROM wallet_key_version WHERE wallet_id='{wallet_id}' AND status='active'",
        )
        if count.strip() != "1":
            raise RuntimeError(
                "browser wallet was not committed by Vault's custody API"
            )
        evidence = {
            "scope": "real Chromium Web Crypto -> live Vault HTTP API -> PostgreSQL custody",
            "browser_version": version,
            "wallet_id": wallet_id,
            "requests": statuses,
            "key_versions": 1,
        }
        (directory / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
        return json.dumps(evidence, sort_keys=True)
