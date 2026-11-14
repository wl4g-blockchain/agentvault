from verifier.context import VerificationContext
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES34OutboxRedeliveryVerifier:
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
                "sqlite": "storage::outbox::sqlite::tests::outbox_class_claim_does_not_lease_foreign_rows_and_replays_after_restart",
                "postgres": "storage::outbox::postgres::tests::outbox_class_claim_isolated_and_replayed_after_worker_restart",
            },
        )
