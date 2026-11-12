from __future__ import annotations

import json
import time

from verifier.context import VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import AGENTVAULT_NAMESPACE
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES33SettlementRestartVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = Infra.session(context)
        query = "SELECT id,status,settled_amount FROM pay_intent WHERE status='settled' ORDER BY id"
        ledger = "SELECT id,intent_id,amount,tx_hash FROM pay_ledger ORDER BY id"
        before = (
            runtime._sql(context.payer_language, context.payee_language, query),
            runtime._sql(context.payer_language, context.payee_language, ledger),
        )
        balances = runtime._token_balances(
            context.payer_language, context.payee_language
        )
        if runtime.backend == "docker":
            runtime._full(
                context.payer_language,
                context.payee_language,
                "kill",
                "-s",
                "SIGKILL",
                "agentvault",
            )
            runtime._full(
                context.payer_language,
                context.payee_language,
                "up",
                "-d",
                "--no-deps",
                "agentvault",
            )
        else:
            session = context.state.pop("session", None)
            if session:
                session.close()
            context.state["forwarding"] = False
            selector = "app.kubernetes.io/instance=agentvault,app.kubernetes.io/component=server"
            inventory = json.loads(
                runtime.kubectl(
                    "get",
                    "pods",
                    "-n",
                    AGENTVAULT_NAMESPACE,
                    "-l",
                    selector,
                    "-o",
                    "json",
                )
            )["items"]
            if len(inventory) != 1:
                raise RuntimeError(
                    "restart scenario requires exactly one owned Vault Pod"
                )
            previous = inventory[0]["metadata"]["uid"]
            runtime.kubectl(
                "delete",
                "pod",
                inventory[0]["metadata"]["name"],
                "-n",
                AGENTVAULT_NAMESPACE,
                "--grace-period=0",
                "--force",
            )
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                inventory = json.loads(
                    runtime.kubectl(
                        "get",
                        "pods",
                        "-n",
                        AGENTVAULT_NAMESPACE,
                        "-l",
                        selector,
                        "-o",
                        "json",
                    )
                )["items"]
                if any(
                    pod["metadata"]["uid"] != previous
                    and any(
                        condition["type"] == "Ready" and condition["status"] == "True"
                        for condition in pod.get("status", {}).get("conditions", [])
                    )
                    for pod in inventory
                ):
                    break
                time.sleep(1)
            else:
                raise RuntimeError("Vault workload did not recover as a new Ready Pod")
            Infra.session(context)
        runtime._wait_http(runtime._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 60)
        evidence = context.require("purchase")["evidence"]
        for intent in evidence.audit["outbound_intent_ids"]:
            Infra.request(
                context,
                "GET",
                f"/payments/{intent}/receipt?scenario_id={payer_identity(context.payer_language).scenario_id}",
            )
        after = (
            runtime._sql(context.payer_language, context.payee_language, query),
            runtime._sql(context.payer_language, context.payee_language, ledger),
        )
        if before != after or balances != runtime._token_balances(
            context.payer_language, context.payee_language
        ):
            raise RuntimeError(
                "process restart changed settled intents, ledger or chain balances"
            )
        # Isolate only this suite's SQL dependency, then restore in a finally block.
        if runtime.backend == "docker":
            command = [*runtime.docker, "pause", runtime.project + "-postgres-1"]
            restore = [*runtime.docker, "unpause", runtime.project + "-postgres-1"]
            runtime._run(command, 30)
            try:
                status, _, _ = runtime._http(
                    runtime._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz")
                )
                if status != 503:
                    raise RuntimeError(
                        f"SQL outage readiness should be 503, got {status}"
                    )
                status, _, _ = runtime._http(
                    runtime._url("E2E_AGENTVAULT_MGMT_PORT", "/healthz")
                )
                if status != 200:
                    raise RuntimeError("SQL outage incorrectly destroyed liveness")
            finally:
                runtime._run(restore, 30)
            runtime._wait_http(runtime._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 60)
        return Infra.api_evidence(
            context,
            [
                "SIGKILL/restart preserves settled intents and append-only ledger",
                "receipts rehydrated from durable authority",
                "no repeated chain transfer",
                "SQL outage fails readiness and preserves liveness"
                if runtime.backend == "docker"
                else "Kubernetes workload replacement",
            ],
        )
