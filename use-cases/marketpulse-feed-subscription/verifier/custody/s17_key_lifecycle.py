from verifier.context import VerificationContext
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES17KeyLifecycleVerifier:
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
                "sqlite": "storage::wallet::sqlite::tests::encrypted_versions_sign_only_through_intent_bound_active_keys",
                "postgres": "storage::wallet::postgres::tests::migrates_schema_and_signs_only_through_intent_bound_active_keys",
            },
        )
