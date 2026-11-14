from verifier.context import VerificationContext
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES16BudgetRaceVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.transaction_database(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return Infra.transaction_contract(
            context,
            {
                "sqlite": "storage::payment::sqlite::tests::serializes_budget_reservations_and_posts_settlement_atomically",
                "postgres": "storage::payment::postgres::tests::serializes_budget_and_commits_settlement_ledger_and_outbox_atomically",
            },
        )
