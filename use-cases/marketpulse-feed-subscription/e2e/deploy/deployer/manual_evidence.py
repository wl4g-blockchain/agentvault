"""Bind each manual Job to its requested identity, two ledgers, and independent RPC facts."""
from __future__ import annotations

import json
import time

from common.payer_identity import LANGUAGES, payer_identity
from .docker import FACILITATOR_ADDRESS, PAYEE_ADDRESS
from .kubernetes import PAYMENT_NAMESPACE
from .manual_profiles import ManualProfiles


class ManualPaymentEvidence:
    def __init__(self, deployer, profile, payer, payee, message):
        if payer not in LANGUAGES or payee not in LANGUAGES or message not in {"mcp", "a2a"}:
            raise ValueError("invalid manual evidence identity")
        self.d, self.profile, self.payer, self.payee, self.message = deployer, profile, payer, payee, message
        self.state = json.loads(deployer.state_path.read_text())
        self.asset = ManualProfiles.asset(profile, self.state["solana"])
        self.network = self.asset["network"]

    def rows(self, sql):
        return json.loads(self.d._sql("", "", f"SELECT coalesce(json_agg(t),'[]'::json) FROM ({sql}) t"))

    def run_ids(self):
        return {r["run_id"] for r in self.rows(f"SELECT run_id FROM pay_intent WHERE network='{self.network}'")}

    def validate_clock(self):
        service, port = self.endpoint()
        with self.d._forward_service(PAYMENT_NAMESPACE, service, port) as local:
            url = f"http://127.0.0.1:{local}"
            if self.profile == "solana":
                slot = self.d._json_rpc_result(url, "Solana", "getSlot", [{"commitment": "confirmed"}])
                stamp = self.d._json_rpc_result(url, "Solana", "getBlockTime", [slot])
            else:
                block = self.d._json_rpc_result(url, self.profile, "eth_getBlockByNumber", ["latest", False])
                stamp = int(block["timestamp"], 16)
                if self.profile == "evm":
                    # This is an explicit local test-node preparation step, never
                    # a relaxation of credential/receipt expiry on other chains.
                    version = self.d._json_rpc_result(url, "Anvil", "web3_clientVersion", [])
                    chain = self.d._json_rpc_result(url, "Anvil", "eth_chainId", [])
                    observed = int(time.time())
                    if not str(version).lower().startswith("anvil/") or int(chain, 16) != 31337:
                        raise RuntimeError("manual EVM clock preparation requires local Anvil chain 31337")
                    if stamp > observed + 60:
                        raise RuntimeError("manual evm chain clock is ahead; refusing to create a payment Job")
                    self.d._json_rpc_result(url, "Anvil", "evm_mine", [max(observed, stamp + 1)])
                    block = self.d._json_rpc_result(url, "Anvil", "eth_getBlockByNumber", ["latest", False])
                    stamp = int(block["timestamp"], 16)
        observed = int(time.time())
        if type(stamp) is not int or abs(observed - stamp) > 60:
            raise RuntimeError(f"manual {self.profile} chain clock is stale; refusing to create a payment Job")
        return {"block_time": stamp, "observed_at": observed, "skew_seconds": observed - stamp}

    def recipient(self):
        if self.profile == "solana":
            return self.state["solana"]["recipient"]
        if self.profile == "tempo":
            return self.state["solana"]["tempo_recipient"]
        return PAYEE_ADDRESS.lower()

    def wallet_ids(self):
        suffix = ManualProfiles.suffix(self.profile)
        return [f"wallet-quantstream-{language}-{slot}{suffix}"
                for language in LANGUAGES for slot in ("primary", "secondary")]

    def balances(self):
        expected = self.wallet_ids()
        ids = ",".join(f"'{wallet}'" for wallet in expected)
        wallets = self.rows(f"SELECT id,address FROM pay_wallet WHERE id IN ({ids}) ORDER BY id")
        if {wallet["id"] for wallet in wallets} != set(expected):
            raise RuntimeError("manual balance snapshot does not contain the ten expected wallet identities")
        result = {}
        service, port = self.endpoint()
        with self.d._forward_service(PAYMENT_NAMESPACE, service, port) as local:
            url = f"http://127.0.0.1:{local}"
            if self.profile == "solana":
                solana = self.state["solana"]
                for label, account in solana["accounts"].items():
                    key = "recipient" if label == "recipient" else f"wallet-quantstream-{label}-solana"
                    value = self.d._json_rpc_result(url, "Solana", "getTokenAccountBalance",
                                                    [account, {"commitment": "confirmed"}])
                    result[key] = int(value["value"]["amount"])
                for wallet in wallets:
                    value = self.d._json_rpc_result(url, "Solana", "getBalance",
                                                    [wallet["address"], {"commitment": "confirmed"}])
                    result["native/" + wallet["id"]] = int(value["value"])
                result["fee_payer_native"] = int(self.d._json_rpc_result(url, "Solana", "getBalance",
                    [solana["fee_payer"], {"commitment": "confirmed"}])["value"])
            else:
                for wallet in [*wallets, {"id": "recipient", "address": self.recipient()}]:
                    call = {"to": self.asset["token_contract"],
                            "data": "0x70a08231" + wallet["address"][2:].lower().zfill(64)}
                    wire = self.d._json_rpc_result(url, self.profile, "eth_call", [call, "latest"])
                    if not isinstance(wire, str) or wire == "0x":
                        raise RuntimeError(f"manual {self.profile} token contract missing or invalid balance response; "
                                           "refusing to create a payment Job")
                    result[wallet["id"]] = int(wire, 16)
                if self.profile == "evm":
                    result["fee_payer_native"] = int(self.d._json_rpc_result(url, "Anvil", "eth_getBalance",
                                                                                 [FACILITATOR_ADDRESS, "latest"]), 16)
        if not set(expected).issubset(result):
            raise RuntimeError("manual chain snapshot omitted an expected token account")
        return result

    def verify(self, before_runs, before):
        rows = []
        for _ in range(60):
            all_rows = self.rows("SELECT id,scenario_id,policy_id,price_id,run_id,wallet_id,direction,protocol,"
                "status,amount::text,tx_hash,resource,network,asset,token_contract,asset_decimals,recipient "
                f"FROM pay_intent WHERE network='{self.network}'")
            rows = [r for r in all_rows if r["run_id"] not in before_runs]
            if len(rows) == 2 and all(r["status"] == "settled" for r in rows):
                break
            time.sleep(1)
        if len(rows) != 2 or {r["direction"] for r in rows} != {"inbound", "outbound"}:
            raise RuntimeError(f"manual Job did not create exactly two directional intents: {rows}")
        if any(r["status"] != "settled" or int(r["amount"]) != 1_000_000 for r in rows):
            raise RuntimeError(f"manual payment intent is not fully settled: {rows}")
        if len({r["run_id"] for r in rows}) != 1 or len({r["tx_hash"] for r in rows}) != 1 or not rows[0]["tx_hash"]:
            raise RuntimeError("manual payment is not one chain transaction bound to one run")
        outbound = self.validate_intents(rows)
        run_id, tx = outbound["run_id"], outbound["tx_hash"]
        intent_ids = ",".join("'" + row["id"].replace("'", "''") + "'" for row in rows)
        ledger = self.rows("SELECT l.id,l.intent_id,l.wallet_id,l.direction,l.entry_type,l.amount::text,l.tx_hash "
                           "FROM pay_ledger l JOIN pay_intent i ON i.id=l.intent_id "
                           f"WHERE i.id IN ({intent_ids}) AND l.entry_type='payment'")
        intents = {row["id"]: row for row in rows}
        if len(ledger) != 2 or {r["direction"] for r in ledger} != {"inbound", "outbound"} or any(
            r["intent_id"] not in intents or r["tx_hash"] != tx or int(r["amount"]) != 1_000_000
            or r["direction"] != intents[r["intent_id"]]["direction"]
            or r["wallet_id"] != intents[r["intent_id"]]["wallet_id"] for r in ledger
        ):
            raise RuntimeError(f"manual payment does not have exactly two matching ledger facts: {ledger}")
        receipt, fee = self.receipt(tx)
        after = self.balances()
        self.validate_balances(outbound["wallet_id"], before, after, fee)
        return {"run_id": run_id, "transaction": tx, "intents": rows, "ledger": ledger,
                "chain_receipt": receipt, "fee": str(fee), "before": before, "after": after}

    def validate_intents(self, rows):
        identity = payer_identity(self.payer)
        suffix = ManualProfiles.suffix(self.profile)
        selected = {wallet + suffix for wallet in identity.wallet_ids}
        resource = ManualProfiles.origin(self.payee, self.profile) + "/" + self.message
        outbound = next(row for row in rows if row["direction"] == "outbound")
        inbound = next(row for row in rows if row["direction"] == "inbound")
        if (outbound["wallet_id"] not in selected or outbound["scenario_id"] != identity.scenario_id + suffix
            or outbound["policy_id"] != identity.policy_id + suffix or outbound["price_id"] is not None
            or inbound["scenario_id"] != "scenario-marketpulse-feed" + suffix
            or inbound["policy_id"] != "policy-marketpulse-sales" + suffix
            or inbound["wallet_id"] != "wallet-marketpulse-revenue" + suffix
            or inbound["price_id"] != f"price-{self.payee}-{self.profile}-{self.message}"):
            raise RuntimeError("manual payment used a different payer pool or payee price than requested")
        protocol = "mpp" if self.profile == "tempo" else "x402"
        normalize = str.lower if self.profile != "solana" else str
        for row in rows:
            if (row["protocol"] != protocol or row["resource"] != resource
                or row["network"] != self.asset["network"] or row["asset"] != self.asset["asset"]
                or row["asset_decimals"] != self.asset["decimals"]
                or normalize(row["token_contract"] or "") != normalize(self.asset["token_contract"])
                or normalize(row["recipient"]) != normalize(self.recipient())):
                raise RuntimeError("manual payment resource or chain asset differs from the selected profile")
        return outbound

    def validate_balances(self, selected, before, after, fee):
        if before.keys() != after.keys():
            raise RuntimeError("manual wallet isolation snapshot identities changed")
        if after["recipient"] - before["recipient"] != 1_000_000:
            raise RuntimeError("manual recipient did not receive exactly one token")
        expected_debit = 1_000_000 + (fee if self.profile == "tempo" else 0)
        for key in self.wallet_ids():
            if before[key] - after[key] != (expected_debit if key == selected else 0):
                raise RuntimeError(f"manual wallet isolation/token+fee conservation failed for {key}")
        if any(before[key] != after[key] for key in before if key.startswith("native/")):
            raise RuntimeError("manual Solana payer native balance changed despite facilitator fee sponsorship")
        if self.profile != "tempo" and before["fee_payer_native"] - after["fee_payer_native"] != fee:
            raise RuntimeError("manual facilitator native balance delta differs from receipt fee")

    def endpoint(self):
        return {"evm": ("anvil", 8545), "tempo": ("tempo", 8545), "solana": ("solana-validator", 8899)}[self.profile]

    def receipt(self, tx):
        service, port = self.endpoint()
        with self.d._forward_service(PAYMENT_NAMESPACE, service, port) as local:
            url = f"http://127.0.0.1:{local}"
            if self.profile == "solana":
                receipt = self.d._json_rpc_result(url, "Solana", "getTransaction", [tx,
                    {"encoding": "jsonParsed", "commitment": "confirmed", "maxSupportedTransactionVersion": 0}])
                if not receipt or receipt["meta"]["err"] is not None:
                    raise RuntimeError("manual Solana transfer did not confirm successfully")
                signatures = receipt.get("transaction", {}).get("signatures", [])
                if not signatures or signatures[0] != tx:
                    raise RuntimeError("manual Solana transaction identity differs from the payment")
                return receipt, int(receipt["meta"]["fee"])
            receipt = self.d._json_rpc_result(url, self.profile, "eth_getTransactionReceipt", [tx])
            if not receipt or receipt.get("status") != "0x1":
                raise RuntimeError("manual EVM/Tempo transfer did not confirm successfully")
            if receipt.get("transactionHash", "").lower() != tx.lower():
                raise RuntimeError("manual EVM/Tempo transaction identity differs from the payment")
            gas = int(receipt["gasUsed"], 16) * int(receipt["effectiveGasPrice"], 16)
            return receipt, ((gas + 10**12 - 1) // 10**12 if self.profile == "tempo" else gas)
