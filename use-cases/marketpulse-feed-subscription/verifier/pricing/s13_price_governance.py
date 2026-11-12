from __future__ import annotations

from verifier.context import VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier as Infra


class E2ES13PriceGovernanceVerifier:
    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        Infra.session(context)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        original = Infra.request(context, "GET", "/admin/prices/price-realtime-news")
        fields = (
            "id",
            "scenario_id",
            "policy_id",
            "resource",
            "protocols",
            "asset",
            "amount",
            "wallet_id",
            "enabled",
        )
        spec = {key: original[key] for key in fields}
        spec["id"] = "price-governance-" + context.run_id
        spec["resource"] = original["resource"] + "/governance"
        path = "/admin/prices/" + spec["id"]
        created = Infra.request(context, "POST", "/admin/prices", spec, (201,))
        if created["created_at"] != created["updated_at"]:
            raise RuntimeError("server did not establish initial Price revision")
        update = {"price": spec, "expected_updated_at": created["updated_at"]}
        updated = Infra.request(context, "PUT", path, update)
        if (
            updated["created_at"] != created["created_at"]
            or updated["updated_at"] <= created["updated_at"]
        ):
            raise RuntimeError(
                "Price update did not preserve creation and advance revision"
            )
        Infra.request(context, "PUT", path, update, (409,))
        Infra.request(
            context,
            "DELETE",
            path + f"?expected_updated_at={created['updated_at']}",
            expected=(409,),
        )
        foreign = {
            **spec,
            "scenario_id": payer_identity(context.payer_language).scenario_id,
        }
        Infra.request(
            context,
            "PUT",
            path,
            {"price": foreign, "expected_updated_at": updated["updated_at"]},
            (400,),
        )
        Infra.request(
            context,
            "DELETE",
            path + f"?expected_updated_at={updated['updated_at']}",
            expected=(204,),
        )
        Infra.request(context, "GET", path, expected=(404,))
        Infra.request(
            context,
            "DELETE",
            "/admin/prices/price-realtime-news?expected_updated_at="
            + str(original["updated_at"]),
            expected=(409,),
        )
        return Infra.api_evidence(
            context,
            [
                "server-owned revisions",
                "stale update/delete conflict",
                "immutable ownership",
                "delete unreferenced price",
                "protect settled history",
            ],
        )
