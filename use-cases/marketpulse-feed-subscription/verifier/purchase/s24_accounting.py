from __future__ import annotations

from verifier.context import (
    PAYER_IDENTITIES,
    FullFlowEvidence,
    VerificationContext,
    payer_identity,
)

UNIT_PRICE = 1_000_000


import json
import re
import time

import yaml
from typing import Any

from verifier.infra.s00_infrastructure import (
    FACILITATOR_ADDRESS,
    PAYEE_ADDRESS,
    TOKEN_CONTRACT,
    E2ES00InfrastructureVerifier,
)


class E2ES24AccountingVerifier:
    class Operations:
        def _wait_financial_evidence(
            self, payer: str, payee: str, intents: list[str]
        ) -> str:
            if len(intents) != 4 or any(
                not E2ES00InfrastructureVerifier.EnvironmentOperations._safe_id(value)
                for value in intents
            ):
                raise RuntimeError("four safe settled intent IDs are required")
            quoted = ",".join(f"'{value}'" for value in intents)
            query = (
                "SELECT (SELECT count(*) FROM pay_intent WHERE id IN ("
                + quoted
                + ") AND status='settled' AND settled_amount=1000000),"
                "(SELECT count(*) FROM pay_ledger WHERE intent_id IN ("
                + quoted
                + ") AND entry_type='payment' AND amount=1000000 AND tx_hash IS NOT NULL),"
                "(SELECT count(*) FROM pay_ledger WHERE intent_id IN ("
                + quoted
                + ") AND entry_type='payment' AND direction='outbound'),"
                "(SELECT count(*) FROM pay_ledger WHERE intent_id IN ("
                + quoted
                + ") AND entry_type='payment' AND direction='inbound'),"
                "(SELECT count(*) FROM pay_event WHERE intent_id IN ("
                + quoted
                + ") AND event_type IN ('settled','reconciled'));"
            )
            result = ""
            for _ in range(45):
                result = self._sql(payer, payee, query)
                fields = result.split("|")
                if (
                    len(fields) == 5
                    and fields[:4] == ["4", "4", "2", "2"]
                    and int(fields[4] or 0) >= 4
                ):
                    return f"authoritative evidence converged: {result}"
                time.sleep(1)
            raise RuntimeError(f"financial evidence did not converge: {result}")

        def _settlement_accounting(
            self,
            payer: str,
            payee: str,
            intents: list[str],
            initial_facilitator_native: int,
        ) -> dict[str, Any]:
            if (
                len(intents) != 4
                or len(set(intents)) != 4
                or any(
                    not E2ES00InfrastructureVerifier.EnvironmentOperations._safe_id(
                        value
                    )
                    for value in intents
                )
            ):
                raise RuntimeError(
                    "four distinct safe settled intent IDs are required for chain accounting"
                )
            quoted = ",".join(f"'{value}'" for value in intents)
            raw = self._sql(
                payer,
                payee,
                "SELECT p.run_id,l.intent_id,l.direction,l.network,l.asset,"
                "coalesce(lower(l.token_contract),''),l.asset_decimals,l.amount,lower(l.tx_hash),"
                "coalesce(l.effect_ref,'') FROM pay_ledger l JOIN pay_intent p ON p.id=l.intent_id "
                "WHERE l.intent_id IN (" + quoted + ") AND l.entry_type='payment' "
                "ORDER BY p.run_id,l.direction,l.intent_id;",
            )
            columns = (
                "run_id",
                "intent_id",
                "direction",
                "network",
                "asset",
                "token_contract",
                "asset_decimals",
                "amount",
                "tx_hash",
                "effect_ref",
            )
            rows: list[dict[str, str]] = []
            for line in raw.splitlines():
                values = line.split("|")
                if len(values) != len(columns):
                    raise RuntimeError(
                        f"invalid payment-ledger accounting row: {line!r}"
                    )
                rows.append(dict(zip(columns, values, strict=True)))
            if len(rows) != 4 or {row["intent_id"] for row in rows} != set(intents):
                raise RuntimeError(
                    f"payment-ledger accounting is incomplete or duplicated: {rows}"
                )

            by_run: dict[str, list[dict[str, str]]] = {}
            for row in rows:
                if (
                    row["network"] != "eip155:31337"
                    or row["asset"] != "USDC"
                    or row["token_contract"] != TOKEN_CONTRACT
                    or row["asset_decimals"] != "6"
                    or row["amount"] != "1000000"
                    or not re.fullmatch(r"0x[0-9a-f]{64}", row["tx_hash"])
                    or not row["effect_ref"]
                ):
                    raise RuntimeError(
                        f"payment-ledger row is not exact on-chain USDC evidence: {row}"
                    )
                by_run.setdefault(row["run_id"], []).append(row)
            if len(by_run) != 2:
                raise RuntimeError(
                    f"expected two stable workflow run IDs in the payment ledger: {sorted(by_run)}"
                )
            for run_id, run_rows in by_run.items():
                if (
                    len(run_rows) != 2
                    or {row["direction"] for row in run_rows} != {"outbound", "inbound"}
                    or len({row["intent_id"] for row in run_rows}) != 2
                    or len({row["tx_hash"] for row in run_rows}) != 1
                ):
                    raise RuntimeError(
                        f"run {run_id} is not one bidirectional ledger pair for one chain effect: {run_rows}"
                    )

            transaction_hashes = sorted({row["tx_hash"] for row in rows})
            if len(transaction_hashes) != 2:
                raise RuntimeError(
                    f"expected exactly two idempotent settlement transactions: {transaction_hashes}"
                )
            receipts: list[dict[str, object]] = []
            total_gas_fee = 0
            for transaction_hash in transaction_hashes:
                receipt = self._rpc_result(
                    "eth_getTransactionReceipt", [transaction_hash]
                )
                if not isinstance(receipt, dict):
                    raise RuntimeError(
                        f"settlement receipt is missing for {transaction_hash}"
                    )
                receipt_hash = str(receipt.get("transactionHash", "")).lower()
                sender = str(receipt.get("from", "")).lower()
                target = str(receipt.get("to", "")).lower()
                status = receipt.get("status")
                gas_used = receipt.get("gasUsed")
                gas_price = receipt.get("effectiveGasPrice")
                if (
                    receipt_hash != transaction_hash
                    or sender != FACILITATOR_ADDRESS
                    or target != TOKEN_CONTRACT
                    or status != "0x1"
                    or not isinstance(gas_used, str)
                    or not re.fullmatch(r"0x[0-9a-fA-F]+", gas_used)
                    or not isinstance(gas_price, str)
                    or not re.fullmatch(r"0x[0-9a-fA-F]+", gas_price)
                ):
                    raise RuntimeError(
                        f"settlement receipt is not a successful facilitator token call: {receipt}"
                    )
                gas_used_value = int(gas_used, 16)
                gas_price_value = int(gas_price, 16)
                gas_fee = gas_used_value * gas_price_value
                if gas_used_value <= 0 or gas_price_value <= 0 or gas_fee <= 0:
                    raise RuntimeError(
                        f"settlement receipt has no accountable gas fee: {receipt}"
                    )
                total_gas_fee += gas_fee
                receipts.append(
                    {
                        "transaction_hash": transaction_hash,
                        "from": sender,
                        "to": target,
                        "gas_used": gas_used_value,
                        "effective_gas_price": gas_price_value,
                        "gas_fee_wei": gas_fee,
                    }
                )

            final_facilitator_native = self._native_balance(FACILITATOR_ADDRESS)
            native_spent = initial_facilitator_native - final_facilitator_native
            if native_spent != total_gas_fee:
                raise RuntimeError(
                    "facilitator native balance delta does not equal the two settlement receipt fees: "
                    f"initial={initial_facilitator_native} final={final_facilitator_native} "
                    f"delta={native_spent} receipts={total_gas_fee}"
                )
            return {
                "network": "eip155:31337",
                "facilitator": FACILITATOR_ADDRESS,
                "initial_facilitator_native": initial_facilitator_native,
                "final_facilitator_native": final_facilitator_native,
                "facilitator_native_spent": native_spent,
                "receipt_gas_fees": total_gas_fee,
                "ledger_rows": rows,
                "receipts": receipts,
            }

        def _receipt(self, scenario: str, intent: str, direction: str) -> None:
            status, _, raw = self._http(
                self._url(
                    "E2E_AGENTVAULT_PORT",
                    f"/api/v1/payments/{intent}/receipt?scenario_id={scenario}",
                )
            )
            value = json.loads(raw)
            asset = value.get("asset", {})
            if (
                status != 200
                or value.get("intent_id") != intent
                or value.get("status") != "settled"
                or value.get("direction") != direction
                or value.get("protocol") != "x402"
                or asset.get("network") != "eip155:31337"
                or asset.get("asset") != "USDC"
                or asset.get("decimals") != 6
                or str(asset.get("token_contract", "")).lower() != TOKEN_CONTRACT
                or str(value.get("recipient", "")).lower() != PAYEE_ADDRESS
                or not value.get("tx_hash")
                or not value.get("evidence_hash")
                or value.get("settled_amount") != "1000000"
            ):
                raise RuntimeError(
                    f"invalid settled receipt for {intent}: {raw[:2048]}"
                )

        def _assert_durable_invariants(self, payer: str, payee: str) -> str:
            identity = payer_identity(payer)
            config = yaml.safe_load(self.agentvault_config.read_text())
            # HTTP identity is authoritative only when AuthGuard is enabled.
            # The standalone matrix deliberately uses the explicit open mode.
            approver = (
                identity.approver_principal
                if config["server"]["authguard"]["enabled"]
                else "agentvault:internal:unrestricted"
            )
            approval = self._sql(
                payer,
                payee,
                "SELECT count(*) FROM pay_approval WHERE status='consumed' AND requested_amount=1000000 "
                f"AND approved_amount=1000000 AND approved_by='{approver}' AND consumed_at IS NOT NULL "
                "AND length(grant_jti)>0 AND grant_claims_hash ~ '^[0-9a-f]{64}$' "
                "AND (sealed_grant_token->>'version')::INTEGER>0 AND sealed_grant_token->>'key_id'='recovery-v1' "
                "AND length(sealed_grant_token->>'nonce')>16 AND length(sealed_grant_token->>'ciphertext')>32;",
            )
            if approval != "1":
                raise RuntimeError(
                    "one-time encrypted approval grant was not atomically consumed "
                    f"by the configured HTTP principal: expected={approver} matching_rows={approval}"
                )
            selection = self._sql(
                payer,
                payee,
                "SELECT count(*),count(DISTINCT wallet_id),string_agg(DISTINCT wallet_id, ',' ORDER BY wallet_id) "
                f"FROM pay_intent WHERE scenario_id='{identity.scenario_id}' "
                "AND direction='outbound' AND status='settled';",
            )
            expected_wallets = ",".join(sorted(identity.wallet_ids))
            if selection != f"2|2|{expected_wallets}":
                raise RuntimeError(
                    f"AgentVault wallet-pool selection is invalid: {selection}"
                )
            membership = self._sql(
                payer,
                payee,
                f"SELECT count(*) FROM pay_policy_wallet WHERE policy_id='{identity.policy_id}' "
                f"AND wallet_id IN ('{identity.primary_wallet_id}','{identity.secondary_wallet_id}');",
            )
            if membership != "2":
                raise RuntimeError("policy is not bound to both wallet-pool members")
            foreign_usage = self._sql(
                payer,
                payee,
                "SELECT count(*) FROM pay_intent WHERE direction='outbound' AND status='settled' "
                f"AND (scenario_id<>'{identity.scenario_id}' OR wallet_id NOT IN "
                f"('{identity.primary_wallet_id}','{identity.secondary_wallet_id}'));",
            )
            if foreign_usage != "0":
                raise RuntimeError(
                    f"Payer wallet isolation was violated by {foreign_usage} settled intents"
                )
            return (
                f"approval grant consumed once; payer={payer}; client={identity.client_id}; "
                f"scenario={identity.scenario_id}; wallet pool evidence={selection}; foreign_usage=0"
            )

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        """No separate deployment is required by this evidence-only phase."""

    @staticmethod
    def init(context: VerificationContext) -> None:
        """Initialization is optional for this scenario."""

    @staticmethod
    def verify_evidence(evidence: FullFlowEvidence) -> str:
        initial = evidence.initial_balances
        final = evidence.final_balances
        identity = payer_identity(evidence.payer_language)
        expected_keys = {
            "payee",
            *(
                wallet_id
                for candidate in PAYER_IDENTITIES.values()
                for wallet_id in candidate.wallet_ids
            ),
        }
        if set(initial) != expected_keys or set(final) != expected_keys:
            raise RuntimeError(
                f"balance evidence must cover ten isolated Payer wallets plus Payee: "
                f"initial={sorted(initial)} final={sorted(final)}"
            )
        expected = dict(initial)
        for wallet_id in identity.wallet_ids:
            expected[wallet_id] -= UNIT_PRICE
        expected["payee"] += 2 * UNIT_PRICE
        if final != expected:
            raise RuntimeError(
                f"settlement balance mismatch: expected={expected}, actual={final}"
            )
        if sum(initial.values()) != sum(final.values()):
            raise RuntimeError(
                "gateway token balance was not conserved across settlement"
            )
        accounting = evidence.chain_accounting
        ledger_rows = accounting.get("ledger_rows")
        receipts = accounting.get("receipts")
        if accounting.get("network") != "eip155:31337" or accounting.get(
            "facilitator_native_spent"
        ) != accounting.get("receipt_gas_fees"):
            raise RuntimeError(
                "facilitator native balance and receipt gas accounting do not conserve"
            )
        if not isinstance(ledger_rows, list) or len(ledger_rows) != 4:
            raise RuntimeError(
                "chain accounting must contain four exact bidirectional payment-ledger rows"
            )
        if not isinstance(receipts, list) or len(receipts) != 2:
            raise RuntimeError(
                "chain accounting must contain exactly two settlement receipts"
            )
        transaction_hashes = {
            str(receipt.get("transaction_hash", "")) for receipt in receipts
        }
        if (
            len(transaction_hashes) != 2
            or {str(row.get("tx_hash", "")) for row in ledger_rows}
            != transaction_hashes
        ):
            raise RuntimeError(
                "payment-ledger transaction hashes are not exactly bound to settlement receipts"
            )
        if any(int(receipt.get("gas_fee_wei", 0)) <= 0 for receipt in receipts):
            raise RuntimeError("a settlement receipt has no positive gas fee")
        run_pairs: dict[str, list[dict[str, object]]] = {}
        for row in ledger_rows:
            run_pairs.setdefault(str(row.get("run_id", "")), []).append(row)
        if len(run_pairs) != 2 or any(
            len(rows) != 2
            or {row.get("direction") for row in rows} != {"outbound", "inbound"}
            or len({row.get("tx_hash") for row in rows}) != 1
            for rows in run_pairs.values()
        ):
            raise RuntimeError(
                "each workflow run must have one outbound/inbound ledger pair for one transaction"
            )
        return (
            "verified exact post-settlement ERC-20 balances, bidirectional ledger/receipt binding, and native Gas "
            "conservation across ten isolated Payer wallets: "
            f"active_wallets={identity.wallet_ids}, inactive_wallets_unchanged=8, "
            f"initial={initial}, final={final}, unit_price={UNIT_PRICE}, "
            f"transactions={sorted(transaction_hashes)}, gas_wei={accounting['receipt_gas_fees']}"
        )

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return E2ES24AccountingVerifier.verify_evidence(
            context.require("purchase")["evidence"]
        )
