from __future__ import annotations

import hashlib
import json
import os
from contextlib import ExitStack
from pathlib import Path
from urllib.parse import urlsplit

import yaml
from playwright.sync_api import Page, expect, sync_playwright
from util.constant import E2EOtherDefinitions as Project

from verifier.context import VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import (
    AGENTVAULT_CHART,
    AGENTVAULT_NAMESPACE,
    AGENTVAULT_VALUES,
    PAYEE_ADDRESS,
    RUNTIME_ROOT,
    TOKEN_CONTRACT,
)
from verifier.infra.s00_infrastructure import (
    E2ES00InfrastructureVerifier as Infra,
)


class E2ES41LiveWalletVerifier:
    class Deployment:
        """Run the production Web image beside the live purchase deployment."""

        @staticmethod
        def routes(api: str, management: str) -> str:
            template = Project.AGENTVAULT_CONFIG / "web.routes.conf"
            return (
                template.read_text()
                .replace("__API_UPSTREAM__", api)
                .replace("__MANAGEMENT_UPSTREAM__", management)
            )

        @classmethod
        def deploy(cls, context: VerificationContext) -> None:
            runtime = context.require("purchase")["runtime"]
            directory = RUNTIME_ROOT / "web"
            directory.mkdir(parents=True, exist_ok=True)
            runtime.environment["E2E_AGENTVAULT_WEB_ROUTES_PATH"] = str(directory)
            if (
                not context.skip_build
                and runtime.environment.get("E2E_RUNNER_PREBUILT_WEB") != "1"
            ):
                arguments = ["build"]
                if not context.reuse_build:
                    arguments.append("--pull")
                runtime._compose(("web",), *arguments, "agentvault-web")
            image = runtime.environment["E2E_AGENTVAULT_WEB_IMAGE"]
            if context.deployer == "docker":
                routes = cls.routes("anvil:8080", "anvil:9091")
                (directory / "routes.conf").write_text(routes)
                runtime._compose(
                    ("web",),
                    "up",
                    "--detach",
                    "--no-build",
                    "--no-deps",
                    "agentvault-web",
                )
                port = runtime.environment.get("E2E_AGENTVAULT_WEB_PORT", "38088")
            else:
                routes = cls.routes("agentvault:8080", "agentvault-management:9091")
                runtime._ensure_cluster_image(image)
                runtime._apply_document(
                    yaml.safe_load(
                        (
                            Project.AGENTVAULT_CONFIG / "web-network-policy.yaml"
                        ).read_text()
                    )
                )
                runtime._apply_document(
                    {
                        "apiVersion": "v1",
                        "kind": "ConfigMap",
                        "metadata": {
                            "name": "agentvault-web-routes",
                            "namespace": AGENTVAULT_NAMESPACE,
                        },
                        "data": {"routes.conf": routes},
                    }
                )
                values = directory / "values.yaml"
                values.write_text(
                    yaml.safe_dump(
                        {
                            "agentvault": {
                                "frontend": {
                                    "enabled": True,
                                    "replicaCount": 1,
                                    "route": {"enabled": False},
                                    "extraVolumes": [
                                        {
                                            "name": "routes",
                                            "configMap": {
                                                "name": "agentvault-web-routes"
                                            },
                                        }
                                    ],
                                    "extraVolumeMounts": [
                                        {
                                            "name": "routes",
                                            "mountPath": "/etc/nginx/agentvault",
                                            "readOnly": True,
                                        }
                                    ],
                                }
                            }
                        }
                    )
                )
                runtime._helm_install(
                    "agentvault",
                    AGENTVAULT_CHART,
                    AGENTVAULT_NAMESPACE,
                    values=(AGENTVAULT_VALUES, values),
                    set_strings={
                        **runtime._image_values(
                            "agentvault", "agentvault.backend.image"
                        ),
                        **runtime._image_reference_values(
                            image, "agentvault.frontend.image"
                        ),
                    },
                )
                session = context.state.setdefault("session", ExitStack())
                port = session.enter_context(
                    runtime._forward_service(
                        AGENTVAULT_NAMESPACE, "agentvault-web", 8080
                    )
                )
            origin = f"http://127.0.0.1:{port}"
            context.state["web_origin"] = origin
            runtime._wait_http(origin, 30)
            directory = context.report_root / "live-wallet"
            directory.mkdir(parents=True, exist_ok=True)
            (directory / "deployment.json").write_text(
                json.dumps(
                    {
                        "image": image,
                        "deployer": context.deployer,
                        "origin": origin,
                        "dockerfile": "web/Dockerfile",
                        "routes": routes,
                        "browser_api_interception": False,
                    },
                    indent=2,
                )
                + "\n"
            )

    class Journey:
        """Compare the deployed browser's read/write views with durable SQL state."""

        def __init__(self, context: VerificationContext, runtime, directory: Path):
            self.context = context
            self.runtime = runtime
            self.directory = directory
            self.screenshots: list[str] = []

        def count(self, table: str, predicate: str = "true") -> int:
            return int(
                self.runtime._sql(
                    self.context.payer_language,
                    self.context.payee_language,
                    f"SELECT count(*) FROM {table} WHERE {predicate}",
                ).strip()
            )

        def visit(self, page: Page, path: str) -> None:
            page.goto(self.context.state["web_origin"] + path)
            page.wait_for_load_state("networkidle")
            expect(page.locator("h1")).to_be_visible()
            expect(page.locator(".error-banner")).to_have_count(0)

        def capture(self, page: Page, name: str) -> None:
            page.screenshot(path=str(self.directory / f"{name}.png"), full_page=True)
            self.screenshots.append(name)

        def reads(self, page: Page) -> None:
            identity = payer_identity(self.context.payer_language)
            self.visit(page, "/")
            expect(
                page.get_by_role("heading", name="Payments with a memory.")
            ).to_be_visible()
            self.capture(page, "overview")
            for path, direction in (("payer", "outbound"), ("payee", "inbound")):
                self.visit(page, "/" + path)
                total = self.count("pay_intent", f"direction='{direction}'")
                if total < 2:
                    raise RuntimeError(f"live {direction} purchase evidence is missing")
                records = page.get_by_test_id(f"{direction}-transactions")
                expect(records).to_have_attribute("data-total", str(total))
                expect(records.locator("article")).to_have_count(min(total, 10))
                expect(records).to_contain_text("settled")
                self.capture(page, path)
                # Exercise the real list query, rather than only rendering a static table.
                run_id = records.locator("article").first.get_attribute("data-run-id")
                page.get_by_label("Run ID", exact=True).fill(run_id)
                page.get_by_role("button", name="Refresh", exact=True).click()
                expect(records).to_have_attribute("data-total", "1")
                expect(records.locator("article")).to_have_count(1)
            self.visit(page, "/balances")
            expect(page.get_by_test_id("ledger-records")).to_have_attribute(
                "data-total", str(self.count("pay_ledger"))
            )
            expect(page.get_by_test_id("balance-conservation")).to_have_attribute(
                "data-conserved", "true"
            )
            page.get_by_test_id("balance-scenario").fill(identity.scenario_id)
            page.get_by_test_id("balance-wallet").fill(identity.primary_wallet_id)
            page.get_by_label("Token contract", exact=True).fill(TOKEN_CONTRACT)
            page.get_by_role("button", name="Calculate balance").click()
            expect(page.get_by_test_id("wallet-balance")).to_contain_text(
                identity.primary_wallet_id
            )
            expect(page.locator(".error-banner")).to_have_count(0)
            self.capture(page, "balances")
            self.visit(page, "/approvals")
            expect(
                page.get_by_role("heading", name="One decision. One grant.")
            ).to_be_visible()
            page.get_by_label("Scenario ID", exact=True).fill(identity.scenario_id)
            page.get_by_label("Business run ID", exact=True).fill(self.context.run_id)
            with page.expect_response(
                lambda r: "/api/v1/approvals?" in r.url
            ) as pending:
                page.get_by_role("button", name="Load pending").click()
            assert pending.value.status == 200
            expect(page.locator(".error-banner")).to_have_count(0)
            self.capture(page, "approvals")
            self.visit(page, "/clients")
            page.get_by_test_id("client-list").get_by_role("button").filter(
                has_text=identity.client_id
            ).click()
            expect(page.get_by_test_id("client-scenarios")).to_contain_text(
                identity.scenario_id
            )
            self.capture(page, "clients")
            self.visit(page, "/runtime")
            expect(page.get_by_text("ok", exact=True)).to_be_visible()
            expect(page.get_by_text("ready", exact=True)).to_be_visible()
            # Only the two browser health endpoints are exposed by the E2E route.
            assert (
                page.request.get(
                    self.context.state["web_origin"] + "/agentvault-management/metrics"
                ).status
                == 404
            )
            self.capture(page, "runtime")

        def scenario(self, page: Page, wallet_id: str) -> str:
            identity = payer_identity(self.context.payer_language)
            scenario_id = "scenario-browser-" + self.context.run_id
            self.visit(page, "/scenarios")
            page.get_by_test_id("scenario-client").select_option(identity.client_id)
            page.get_by_label("Scenario ID", exact=True).fill(scenario_id)
            page.get_by_label("Display name", exact=True).fill("Live browser scenario")
            page.get_by_label("Initial Policy ID", exact=True).fill(
                "policy-browser-" + self.context.run_id
            )
            page.get_by_label("Allowed recipients", exact=True).fill(PAYEE_ADDRESS)
            page.get_by_label("Allowed origins", exact=True).fill(
                self.runtime._url("E2E_MARKETPULSE_PORT")
            )
            page.get_by_label("Token contract", exact=True).fill(TOKEN_CONTRACT)
            selector = page.get_by_test_id("wallet-selector")
            selector.locator("summary").click()
            for selected in (wallet_id, identity.primary_wallet_id):
                selector.locator(f'input[value="{selected}"]').check()
            expect(selector.locator("summary")).to_contain_text("2 selected")
            selector.locator("summary").click()
            page.get_by_role("button", name="Create atomically").click()
            expect(page.get_by_test_id("scenario-created")).to_contain_text("2 Wallets")
            expect(page.locator(".error-banner")).to_have_count(0)
            assert (
                self.count(
                    "pay_scenario",
                    f"id='{scenario_id}' AND client_id='{identity.client_id}'",
                )
                == 1
            )
            assert (
                self.count(
                    "pay_policy_wallet",
                    f"policy_id='policy-browser-{self.context.run_id}'",
                )
                == 2
            )
            self.capture(page, "scenario-created")
            self.visit(page, "/clients")
            page.get_by_test_id("client-list").get_by_role("button").filter(
                has_text=identity.client_id
            ).click()
            expect(page.get_by_test_id("client-scenarios")).to_contain_text(scenario_id)
            self.capture(page, "client-ownership")
            return scenario_id

        def gallery(self) -> None:
            (self.directory / "index.md").write_text(
                "# Live AgentVault Web UI\n\n"
                "Production Nginx image → live Vault API → PostgreSQL; no API fixtures.\n\n"
                + "\n\n".join(f"![{name}]({name}.png)" for name in self.screenshots)
                + "\n"
            )

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")
        E2ES41LiveWalletVerifier.Deployment.deploy(context)

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
        errors = []
        plaintext = []
        journey = E2ES41LiveWalletVerifier.Journey(context, runtime, directory)
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch(
                headless=True,
                executable_path=os.environ.get("E2E_CHROMIUM_EXECUTABLE") or None,
            )
            try:
                page = browser.new_page(viewport={"width": 1440, "height": 1000})
                console = []
                page.on("console", lambda message: console.append(message.text))
                page.on("pageerror", lambda error: errors.append(str(error)))

                def observe(response):
                    request = response.request
                    parsed = urlsplit(request.url)
                    if not parsed.path.startswith(
                        ("/api/v1/", "/agentvault-management/")
                    ):
                        return
                    body = request.post_data
                    if private in (body or ""):
                        plaintext.append(parsed.path)
                    statuses.append(
                        {
                            "method": request.method,
                            "path": parsed.path,
                            "status": response.status,
                        }
                    )

                page.on("response", observe)
                journey.reads(page)
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
                journey.capture(page, "wallet-committed")
                scenario_id = journey.scenario(page, wallet_id)
                if plaintext or errors:
                    raise RuntimeError(
                        f"browser failures: plaintext={plaintext}, errors={errors}"
                    )
                failures = [r for r in statuses if r["status"] >= 400]
                if failures:
                    raise RuntimeError(f"live browser API failed: {failures}")
                version = browser.version
            except BaseException:
                page.screenshot(path=str(directory / "failure.png"), full_page=True)
                raise
            finally:
                journey.gallery()
                (directory / "requests.json").write_text(
                    json.dumps(statuses, indent=2) + "\n"
                )
                browser.close()
        imports = [r for r in statuses if r["method"] == "POST"]
        if [(r["path"], r["status"]) for r in imports] != [
            ("/api/v1/admin/wallets/encryption-sessions", 200),
            ("/api/v1/admin/wallets", 200),
            ("/api/v1/admin/scenarios", 200),
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
            "scope": "real Chromium -> production Nginx image -> live Vault HTTP API -> PostgreSQL",
            "browser_version": version,
            "wallet_id": wallet_id,
            "scenario_id": scenario_id,
            "screenshots": journey.screenshots,
            "requests": statuses,
            "key_versions": 1,
        }
        (directory / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
        return json.dumps(evidence, sort_keys=True)
