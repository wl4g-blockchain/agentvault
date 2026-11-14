from __future__ import annotations

from verifier.context import VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES12ApprovalBoundaryVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = Infra.session(context)
        identity = payer_identity(context.payer_language)
        row = (
            runtime._sql(
                context.payer_language,
                context.payee_language,
                "SELECT a.id,i.run_id FROM pay_approval a JOIN pay_intent i ON i.id=a.intent_id WHERE a.status='consumed' LIMIT 1",
            )
            .strip()
            .split("|")
        )
        if len(row) != 2:
            raise RuntimeError(
                "purchase prerequisite did not leave one consumed approval"
            )
        approval_id, run_id = row
        body = {
            "scenario_id": identity.scenario_id,
            "run_id": run_id,
            "approval_id": approval_id,
            "decision": "approve",
            "approved_amount": "1000000",
        }
        # Open HTTP mode still cannot manufacture an approval Principal.
        Infra.request(
            context, "POST", f"/approvals/{approval_id}/decision", body, (400, 403, 409)
        )
        Infra.request(
            context,
            "POST",
            f"/approvals/{approval_id}/decision",
            {**body, "run_id": run_id + "-conflict"},
            (400, 403, 409),
        )
        for field in ("principal_id", "approved_by", "approver"):
            Infra.request(
                context,
                "POST",
                f"/approvals/{approval_id}/decision",
                {**body, field: identity.approver_principal},
                (400, 422),
            )
        if (
            runtime._sql(
                context.payer_language,
                context.payee_language,
                "SELECT count(*) FROM pay_approval WHERE status='consumed'",
            ).strip()
            != "1"
        ):
            raise RuntimeError("approval replay changed grant consumption")
        return Infra.api_evidence(
            context,
            [
                "consumed approval cannot fund another payment",
                "run binding",
                "body cannot impersonate approver",
                "one grant consumption",
            ],
        )
