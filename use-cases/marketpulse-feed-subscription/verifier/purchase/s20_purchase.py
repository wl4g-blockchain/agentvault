from __future__ import annotations

import json
import time

from util.constant import E2EOtherDefinitions as Project
from util.images import E2EImagesDefinitions
from util.postgres import E2EPostgresChecker

from verifier.context import (
    LANGUAGES,
    ConsoleResourceFixture,
    FullFlowEvidence,
    VerificationContext,
    create_deployer,
    payer_identity,
)
from verifier.delivery.s32_delivery_recovery import E2ES32DeliveryRecoveryVerifier
from verifier.infra.s00_infrastructure import (
    FACILITATOR_ADDRESS,
    E2ES00InfrastructureVerifier,
)
from verifier.purchase.s21_payer_signing import E2ES21PayerSigningVerifier
from verifier.purchase.s23_balance_sync import E2ES23BalanceSyncVerifier
from verifier.purchase.s24_accounting import E2ES24AccountingVerifier
from verifier.purchase.s30_matrix import E2ES30MatrixVerifier
from verifier.solana_settlement.s26_exact_settlement import (
    E2ES26ExactSettlementVerifier,
)
from verifier.tempo_charge.s25_charge_recovery import E2ES25ChargeRecoveryVerifier


class E2ES20PurchaseVerifier:
    class Flow:
        def _preflight_mcp(self) -> str:
            endpoint = self._url("E2E_MARKETPULSE_PORT", "/mcp")
            headers = {"content-type": "application/json", "x-run-id": "preflight-free"}
            free = {
                "jsonrpc": "2.0",
                "id": "free",
                "method": "tools/call",
                "params": {"name": "marketpulse.yesterday_news", "arguments": {}},
            }
            status, _, raw = self._http(
                endpoint, "POST", json.dumps(free).encode(), headers
            )
            if status != 200:
                raise RuntimeError(f"free MCP preflight returned HTTP {status}")
            value = json.loads(raw)
            result = value.get("result", {})
            delivery = result.get("structuredContent", {})
            if (
                delivery.get("paid") is not False
                or not delivery.get("news")
                or result.get("isError") is not False
            ):
                raise RuntimeError(
                    "free MCP response is not a canonical business delivery"
                )
            paid = {
                "jsonrpc": "2.0",
                "id": "paid",
                "method": "tools/call",
                "params": {"name": "marketpulse.realtime_news", "arguments": {}},
            }
            headers["x-run-id"] = "preflight-paid"
            status, response_headers, raw = self._http(
                endpoint, "POST", json.dumps(paid).encode(), headers
            )
            message = json.loads(raw)
            result = message.get("result", {}) if isinstance(message, dict) else {}
            challenge = (
                result.get("structuredContent", {}) if isinstance(result, dict) else {}
            )
            content = result.get("content", []) if isinstance(result, dict) else []
            text_challenge: object = None
            if isinstance(content, list) and content and isinstance(content[0], dict):
                text = content[0].get("text")
                if isinstance(text, str):
                    try:
                        text_challenge = json.loads(text)
                    except json.JSONDecodeError:
                        pass
            if (
                status != 200
                or message.get("jsonrpc") != "2.0"
                or message.get("id") != "paid"
                or result.get("isError") is not True
                or challenge.get("x402Version") != 2
                or not challenge.get("accepts")
                or text_challenge != challenge
            ):
                raise RuntimeError(
                    f"paid MCP preflight is not an x402 v2 challenge: HTTP {status}: {raw[:2048]}"
                )
            if any(name.lower() == "payment-required" for name in response_headers):
                raise RuntimeError(
                    "paid MCP challenge incorrectly mixed the HTTP PAYMENT-REQUIRED transport"
                )
            expires_at = next(
                (
                    value
                    for name, value in response_headers.items()
                    if name.lower() == "payment-expires-at"
                ),
                "",
            )
            if not expires_at.isdigit() or int(expires_at) <= 0:
                raise RuntimeError(
                    "paid MCP challenge omitted its stable payment-expires-at profile metadata"
                )
            return "free tool delivered without payment; paid tool returned a canonical x402 v2 MCP challenge"

        def _persist_full_evidence(self, evidence: FullFlowEvidence) -> None:
            """Retain only verified financial fields and the existing redacted service logs."""
            directory = (
                self.context.report_root
                / "payment"
                / "full-flow"
                / (
                    f"{evidence.payer_language}-{evidence.payee_language}-{evidence.transport}"
                )
            )
            directory.mkdir(parents=True, exist_ok=True)
            payload = {
                "backend": self.backend,
                "payer_language": evidence.payer_language,
                "payee_language": evidence.payee_language,
                "transport": evidence.transport,
                "audit": evidence.audit,
                "initial_balances": evidence.initial_balances,
                "final_balances": evidence.final_balances,
                "chain_accounting": evidence.chain_accounting,
                "runtime": evidence.runtime,
                "balance_sync": self.context.state["balance_sync"],
            }
            (directory / "evidence.json").write_text(
                json.dumps(payload, indent=2) + "\n", encoding="utf-8"
            )
            for service in ("agentvault", "payer", "payee", "facilitator"):
                (directory / f"{service}.log").write_text(
                    evidence.logs[service], encoding="utf-8"
                )

            from verifier.otel.s50_metrics import E2ES50MetricsVerifier
            from verifier.otel.s51_tracing import E2ES51TracingVerifier
            from verifier.otel.s52_logs import E2ES52LogsVerifier

            observations = {
                "metrics": E2ES50MetricsVerifier.finish(
                    self, self.context.state["metrics_before"]
                ),
                "tracing": E2ES51TracingVerifier.check(self, evidence),
                "logs": E2ES52LogsVerifier.check(evidence),
            }
            from util.report import E2EReportHelper

            images = {}
            for component in (
                "agentvault",
                f"payer-{evidence.payer_language}",
                f"payee-{evidence.payee_language}",
            ):
                reference = E2EImagesDefinitions.reference(component, self.environment)
                images[component] = json.loads(
                    self._run(
                        [
                            *self.docker,
                            "image",
                            "inspect",
                            "--format={{json .Id}}",
                            reference,
                        ],
                        30,
                    )
                )
            import shutil

            stem = f"{evidence.payer_language}-{evidence.payee_language}-{evidence.transport}"
            observations_root = self.context.report_root / "otel"
            raw_root = directory / "observations"
            raw_root.mkdir(exist_ok=True)
            for path in observations_root.glob(stem + "*"):
                if path.is_dir():
                    shutil.copytree(path, raw_root / path.name, dirs_exist_ok=True)
                else:
                    shutil.copyfile(path, raw_root / path.name)
            public_sql = self._sql(
                evidence.payer_language,
                evidence.payee_language,
                "SELECT id,scenario_id,run_id,wallet_id,status,settled_amount FROM pay_intent WHERE status='settled' ORDER BY id",
            )
            (directory / "settled-intents.sql.txt").write_text(public_sql)
            metadata = {
                "source": E2EReportHelper.source_identity(Project.REPOSITORY_ROOT),
                "images": images,
                "backend": self.backend,
                "run_id": self.context.run_id,
            }
            (directory / "otel.json").write_text(
                json.dumps(observations, indent=2) + "\n"
            )
            E2EReportHelper.seal(directory, metadata)
            self.context.completed["purchase"] = {
                "evidence": evidence,
                "runtime": self,
                "balance_sync": self.context.state["balance_sync"],
                "otel": observations,
            }

        def full(
            self,
            payer_language: str,
            payee_language: str,
            *,
            clean_start: bool = True,
            render_config: bool = True,
        ) -> dict[str, str]:
            self.prepare_full(
                payer_language,
                payee_language,
                clean_start=clean_start,
                render_config=render_config,
            )
            self.initialize_full(payer_language, payee_language)
            return self.verify_full(payer_language, payee_language)

        def _complete_purchase(
            self,
            payer_language: str,
            payee_language: str,
            preflight: str,
            initial_balances: dict,
            initial_facilitator_native: int,
            payer_logs: str,
        ) -> dict[str, str]:
            """One authoritative financial verifier for every deployment environment."""
            identity = payer_identity(payer_language)
            audit = self._audit(payer_language, payee_language)
            outbound = list(audit["outbound_intent_ids"])
            inbound = list(audit["inbound_intent_ids"])
            financial = self._wait_financial_evidence(
                payer_language, payee_language, [*outbound, *inbound]
            )
            for intent in outbound:
                self._receipt(identity.scenario_id, intent, "outbound")
            for intent in inbound:
                self._receipt("scenario-marketpulse-feed", intent, "inbound")
            durable = self._assert_durable_invariants(payer_language, payee_language)
            final_balances = self._token_balances(payer_language, payee_language)
            chain_accounting = self._settlement_accounting(
                payer_language,
                payee_language,
                [*outbound, *inbound],
                initial_facilitator_native,
            )
            balance_sync = self._wait_balance_snapshots(
                payer_language, payee_language, final_balances
            )
            evidence = FullFlowEvidence(
                payer_language=payer_language,
                payee_language=payee_language,
                transport=self.context.transport,
                audit=audit,
                initial_balances=initial_balances,
                final_balances=final_balances,
                chain_accounting=chain_accounting,
                logs=self._service_logs(
                    payer_language,
                    payee_language,
                    payer_logs,
                ),
                runtime={
                    "facilitator": self._facilitator_capabilities(),
                    "backend": self.backend,
                },
            )
            self.context.state["balance_sync"] = balance_sync
            self._persist_full_evidence(evidence)
            from verifier.purchase import (
                s21_payer_signing,
                s22_payee_settlement,
                s23_balance_sync,
                s24_accounting,
            )

            reports = {
                "s20_full.md": "\n".join(
                    (
                        f"payer={payer_language} payee={payee_language} transport={self.context.transport} deployer={self.backend}",
                        preflight,
                        financial,
                        durable,
                        json.dumps(audit, sort_keys=True),
                    )
                ),
                "s21_payer_signing.md": s21_payer_signing.E2ES21PayerSigningVerifier.verify_evidence(
                    evidence
                ),
                "s22_payee_settlement.md": s22_payee_settlement.E2ES22PayeeSettlementVerifier.verify_evidence(
                    evidence
                ),
                "s23_balance_sync.md": s23_balance_sync.E2ES23BalanceSyncVerifier.verify_evidence(
                    evidence, balance_sync
                ),
                "s24_gateway_balance.md": s24_accounting.E2ES24AccountingVerifier.verify_evidence(
                    evidence
                ),
            }
            return reports

    class DockerExecution:
        def prepare_full(
            self,
            payer_language: str,
            payee_language: str,
            *,
            clean_start: bool = True,
            render_config: bool = True,
        ) -> None:
            if payer_language not in LANGUAGES or payee_language not in LANGUAGES:
                raise ValueError("unsupported payer/payee language")
            if self.context.transport not in Project.TRANSPORTS:
                raise ValueError("unsupported AgentVault signing transport")
            self.environment["E2E_AGENTVAULT_TRANSPORT"] = self.context.transport
            if render_config:
                self._render_agentvault_config()
            if clean_start:
                self.cleanup()
            try:
                self._build_images(payer_language, payee_language)
                self._dependency_deploy("payee", include_solana=False)
            except BaseException:
                self._diagnostics(payer_language, payee_language)
                raise

        def initialize_full(self, payer_language: str, payee_language: str) -> None:
            self._dependency_initialize("payee", include_solana=False)
            self._dependency_health("payee", include_solana=False)
            self._full(
                payer_language, payee_language, "up", "--no-deps", "agentvault-import"
            )
            self._full(
                payer_language, payee_language, "up", "-d", "--no-deps", "agentvault"
            )
            self._wait_http(self._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 90)
            self._full(
                payer_language,
                payee_language,
                "up",
                "-d",
                "--no-deps",
                f"marketpulse-{payee_language}",
                "admin-ui",
            )
            self._wait_http(self._url("E2E_MARKETPULSE_PORT", "/healthz"), 60)
            self._wait_http(self._url("E2E_QUANTSTREAM_UI_PORT", "/healthz"), 60)

        def verify_full(
            self, payer_language: str, payee_language: str
        ) -> dict[str, str]:
            try:
                from verifier.otel.s50_metrics import E2ES50MetricsVerifier

                self.context.state["metrics_before"] = E2ES50MetricsVerifier.begin(self)
                preflight = self._preflight_mcp()
                initial_balances = self._token_balances(payer_language, payee_language)
                initial_facilitator_native = self._native_balance(FACILITATOR_ADDRESS)
                automatic_logs = self._full(
                    payer_language,
                    payee_language,
                    "run",
                    "--rm",
                    "--no-deps",
                    "-e",
                    "QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT=once",
                    f"quantstream-{payer_language}",
                )
                time.sleep(2)
                container = f"{self.project}-{payer_language}-{payee_language}-{self.context.transport}-approval"
                if (
                    container
                    in self._run(
                        [*self.docker, "ps", "-a", "--format", "{{.Names}}"], 30
                    ).splitlines()
                ):
                    self._run([*self.docker, "rm", "-f", container], 30)
                self._full(
                    payer_language,
                    payee_language,
                    "run",
                    "-d",
                    "--name",
                    container,
                    "--no-deps",
                    f"quantstream-{payer_language}",
                )
                approval_logs = self._wait_and_approve(
                    payer_language, payee_language, container
                )
                return self._complete_purchase(
                    payer_language,
                    payee_language,
                    preflight,
                    initial_balances,
                    initial_facilitator_native,
                    automatic_logs + "\n" + approval_logs,
                )
            except BaseException:
                self._diagnostics(payer_language, payee_language)
                raise

    class KubernetesExecution:
        def prepare_full(
            self,
            payer_language: str,
            payee_language: str,
            *,
            clean_start: bool = True,
            render_config: bool = True,
        ) -> None:
            if payer_language not in LANGUAGES or payee_language not in LANGUAGES:
                raise ValueError("unsupported payer/payee language")
            if self.context.transport not in Project.TRANSPORTS:
                raise ValueError("unsupported AgentVault signing transport")
            if render_config:
                self.render()
            if clean_start:
                self.cleanup()
            try:
                self._build_images(payer_language, payee_language)
                self._deploy_base(
                    "payee", include_solana=False, payee_language=payee_language
                )
                self._apply_business_claims(payer_language, payee_language)
            except BaseException:
                self._diagnostics(payer_language, payee_language)
                raise

        def initialize_full(self, payer_language: str, payee_language: str) -> None:
            self._initialize_base()
            self._install_agentvault()
            self._install_payee(payee_language)
            self._install_admin_ui(payer_language)

        def verify_full(
            self, payer_language: str, payee_language: str
        ) -> dict[str, str]:
            try:
                forward_names = (
                    "anvil",
                    "facilitator",
                    "agentvault-api",
                    "agentvault-management",
                    "jaeger",
                    f"payee:{payee_language}",
                    "admin-ui",
                )
                with self._runtime_forwards(forward_names):
                    self._wait_http(
                        self._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 30
                    )
                    self._wait_http(self._url("E2E_MARKETPULSE_PORT", "/healthz"), 30)
                    self._wait_http(
                        self._url("E2E_QUANTSTREAM_UI_PORT", "/healthz"), 30
                    )
                    from verifier.otel.s50_metrics import E2ES50MetricsVerifier

                    self.context.state["metrics_before"] = E2ES50MetricsVerifier.begin(
                        self
                    )
                    preflight = self._preflight_mcp()
                    initial_balances = self._token_balances(
                        payer_language, payee_language
                    )
                    initial_facilitator_native = self._native_balance(
                        FACILITATOR_ADDRESS
                    )

                    automatic_job = self._start_payer_job(
                        payer_language,
                        payee_language,
                        failure_once=True,
                    )
                    automatic_logs = self._wait_job(automatic_job)
                    time.sleep(2)
                    approval_job = self._start_payer_job(
                        payer_language,
                        payee_language,
                        failure_once=False,
                    )
                    approval_logs = self._wait_and_approve_job(
                        payer_language,
                        payee_language,
                        approval_job,
                    )

                    return self._complete_purchase(
                        payer_language,
                        payee_language,
                        preflight,
                        initial_balances,
                        initial_facilitator_native,
                        automatic_logs + "\n" + approval_logs,
                    )
            except BaseException:
                self._diagnostics(payer_language, payee_language)
                raise

    class DockerRuntime(
        DockerExecution,
        Flow,
        E2ES21PayerSigningVerifier.Operations,
        E2ES23BalanceSyncVerifier.Operations,
        E2ES24AccountingVerifier.Operations,
        E2ES32DeliveryRecoveryVerifier.Operations,
        E2ES30MatrixVerifier.Operations,
        E2ES25ChargeRecoveryVerifier.MppTempoDockerLifecycle,
        E2ES26ExactSettlementVerifier.X402SolanaDockerLifecycle,
        E2ES00InfrastructureVerifier.DockerEnvironment,
    ):
        pass

    class KubernetesRuntime(
        KubernetesExecution,
        E2ES21PayerSigningVerifier.KubernetesOperations,
        E2ES23BalanceSyncVerifier.KubernetesOperations,
        E2ES32DeliveryRecoveryVerifier.KubernetesOperations,
        E2ES30MatrixVerifier.KubernetesOperations,
        Flow,
        E2ES23BalanceSyncVerifier.Operations,
        E2ES24AccountingVerifier.Operations,
        E2ES32DeliveryRecoveryVerifier.Operations,
        E2ES30MatrixVerifier.Operations,
        E2ES25ChargeRecoveryVerifier.MppTempoKubernetesLifecycle,
        E2ES26ExactSettlementVerifier.X402SolanaKubernetesLifecycle,
        E2ES00InfrastructureVerifier.KubernetesEnvironment,
    ):
        pass

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        runtime = create_deployer(context)
        runtime.prepare_full(context.payer_language, context.payee_language)

    @staticmethod
    def init(context: VerificationContext) -> None:
        runtime = create_deployer(context)
        runtime.initialize_full(context.payer_language, context.payee_language)
        checker = E2EPostgresChecker(
            lambda sql: runtime._sql(
                context.payer_language, context.payee_language, sql
            )
        )
        fixtures = ConsoleResourceFixture.load_agentvault_resources(
            Project.AGENTVAULT_CONFIG / "init.import.yaml"
        )
        checker.require_scalar(
            "SELECT count(*) FROM pay_wallet", str(len(fixtures["wallets"]))
        )

    @staticmethod
    def verify(context: VerificationContext) -> dict[str, str]:
        return create_deployer(context).verify_full(
            context.payer_language, context.payee_language
        )
