from __future__ import annotations

import json

from verifier.context import VerificationContext


class E2ES52LogsVerifier:
    @staticmethod
    def check(evidence) -> str:
        logs = evidence.logs["agentvault"]
        required = (
            "agentvault.facilitator.verify.completed",
            "agentvault.facilitator.settle.completed",
            "agentvault.wallet.balance_sync_completed",
        )
        # Signing event spelling is sourced from the same verified purchase stage.
        from verifier.purchase.s21_payer_signing import E2ES21PayerSigningVerifier
        from verifier.purchase.s22_payee_settlement import E2ES22PayeeSettlementVerifier

        E2ES21PayerSigningVerifier.verify_evidence(evidence)
        E2ES22PayeeSettlementVerifier.verify_evidence(evidence)
        counts = {
            event: sum(event in line for line in logs.splitlines())
            for event in required
        }
        if not all(counts.values()):
            raise RuntimeError(
                f"missing purchase business transitions in deployment logs: {counts}"
            )
        for run_id in evidence.audit["run_ids"]:
            if (
                run_id not in logs
                or run_id not in evidence.logs["payee"]
                or run_id not in evidence.logs["payer"]
            ):
                raise RuntimeError(
                    f"workflow run not propagated across deployment logs: {run_id}"
                )
        return json.dumps(
            {
                "source": "kubectl deployment/job logs"
                if evidence.runtime.get("backend") == "kubernetes"
                else "docker compose logs",
                "run_ids": evidence.audit["run_ids"],
                "events": counts,
            },
            sort_keys=True,
        )

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return context.require("purchase")["otel"]["logs"]
