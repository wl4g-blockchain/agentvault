from __future__ import annotations

import copy
import json
import re
import time
from pathlib import Path

import yaml
from util.constant import E2EOtherDefinitions as Project
from util.images import E2EImagesDefinitions
from util.kubernetes import MANUAL_IMAGE_PIN, MANUAL_IMAGE_PIN_OWNER
from util.process import E2EProcessHelper

from verifier.context import LANGUAGES, VerificationContext, payer_identity
from verifier.infra.s00_infrastructure import (
    AGENTVAULT_CHART,
    AGENTVAULT_NAMESPACE,
    AGENTVAULT_VALUES,
    BUSINESS_EVIDENCE_LABEL,
    FACILITATOR_ADDRESS,
    INFRA_NAMESPACE,
    PAYEE_ADDRESS,
    PAYEE_NAMESPACE,
    PAYER_NAMESPACE,
    PAYMENT_NAMESPACE,
    RUNTIME_ROOT,
    TOKEN_CONTRACT,
)
from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier


class E2ES29RetainedPurchaseVerifier:
    class ManualProfiles:
        NAMES = ("evm", "solana", "tempo")
        TEMPO_TOKEN = "0x20c0000000000000000000000000000000000000"

        @staticmethod
        def suffix(profile: str) -> str:
            if profile not in E2ES29RetainedPurchaseVerifier.ManualProfiles.NAMES:
                raise ValueError("unknown manual payment profile")
            return "" if profile == "evm" else f"-{profile}"

        @staticmethod
        def service(language: str, profile: str) -> str:
            return f"marketpulse-{E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES[language]}-{profile}"

        @classmethod
        def origin(cls, language: str, profile: str) -> str:
            return f"http://{cls.service(language, profile)}.{PAYEE_NAMESPACE}.svc.cluster.local:8081"

        @staticmethod
        def asset(profile: str, solana: dict) -> dict:
            network, symbol, token = {
                "evm": ("eip155:31337", "USDC", TOKEN_CONTRACT.lower()),
                "solana": (solana["network"], "USDC", solana["mint"]),
                "tempo": (
                    "eip155:1337",
                    "pathUSD",
                    E2ES29RetainedPurchaseVerifier.ManualProfiles.TEMPO_TOKEN,
                ),
            }[profile]
            return {
                "network": network,
                "asset": symbol,
                "token_contract": token,
                "decimals": 6,
            }

        @classmethod
        def resources(cls, solana: dict) -> str:
            source = list(
                yaml.safe_load_all(
                    (Project.AGENTVAULT_CONFIG / "init.import.yaml").read_text()
                )
            )
            documents = [
                copy.deepcopy(d) for d in source if d and d["kind"] == "Client"
            ]
            for profile in cls.NAMES:
                suffix = cls.suffix(profile)
                asset = cls.asset(profile, solana)
                recipient = {
                    "solana": solana["recipient"],
                    "tempo": solana["tempo_recipient"],
                    "evm": PAYEE_ADDRESS.lower(),
                }[profile]
                for original in source:
                    if not original or original["kind"] in {"Client", "Price"}:
                        continue
                    d = copy.deepcopy(original)
                    kind, data = d["kind"], d["data"]
                    original_id = d["metadata"]["name"]
                    d["metadata"]["name"] += suffix
                    if kind == "Wallet":
                        data["budget_unit"] = {"asset": asset["asset"], "decimals": 6}
                        data.update(
                            max_per_payment="10000000", max_per_day="5000000000"
                        )
                        if profile == "solana":
                            data["algorithm"] = "ed25519"
                            if data["custody"] == "external":
                                data["address"] = recipient
                            else:
                                data["private_key_env"] = solana["key_envs"][
                                    original_id
                                ]
                        elif profile == "tempo":
                            if data["custody"] == "external":
                                data["address"] = recipient
                            else:
                                data["private_key_env"] = solana["tempo_key_envs"][
                                    original_id
                                ]
                    elif kind in {"Policy", "Approver"}:
                        data["scenario_id"] += suffix
                        if kind == "Approver":
                            d["metadata"]["name"] = (
                                data["scenario_id"] + "--" + data["auth_principal"]
                            )
                        else:
                            data["wallet_ids"] = [
                                w + suffix for w in data["wallet_ids"]
                            ]
                            data["protocols"] = [
                                "mpp" if profile == "tempo" else "x402"
                            ]
                            data["networks"] = [asset["network"]]
                            data["assets"] = [asset]
                            data["budget_unit"] = {
                                "asset": asset["asset"],
                                "decimals": 6,
                            }
                            data["allowed_recipients"] = [recipient]
                            data["allowed_origins"] = [
                                cls.origin(language, profile) for language in LANGUAGES
                            ]
                            limits = data.get(
                                "inbound_limits", data.get("outbound_limits")
                            )
                            limits["max_per_day"] = "5000000000"
                            if "outbound_limits" in data:
                                limits["approval_overage"] = "5000000000"
                    documents.append(d)
                for language in LANGUAGES:
                    for message in ("mcp", "a2a"):
                        documents.append(
                            {
                                "consoleVersion": "core.agentvault.io/v1",
                                "kind": "Price",
                                "metadata": {
                                    "name": f"price-{language}-{profile}-{message}",
                                    "status": "active",
                                },
                                "data": {
                                    "scenario_id": f"scenario-marketpulse-feed{suffix}",
                                    "policy_id": f"policy-marketpulse-sales{suffix}",
                                    "resource": f"{cls.origin(language, profile)}/{message}",
                                    "protocols": [
                                        "mpp" if profile == "tempo" else "x402"
                                    ],
                                    "asset": asset,
                                    "amount": "1000000",
                                    "wallet_id": f"wallet-marketpulse-revenue{suffix}",
                                },
                            }
                        )
            return yaml.safe_dump_all(documents, sort_keys=False)

        @classmethod
        def payer_environment(
            cls, payer: str, payee: str, profile: str, message: str, transport: str
        ) -> dict:
            identity = payer_identity(payer)
            return {
                "MARKETPULSE_URL": cls.origin(payee, profile) + "/mcp",
                "MARKETPULSE_A2A_URL": cls.origin(payee, profile) + "/a2a",
                "MARKETPULSE_MESSAGE_PROTOCOL": message,
                "QUANTSTREAM_CLIENT_ID": identity.client_id,
                "QUANTSTREAM_SCENARIO_ID": identity.scenario_id + cls.suffix(profile),
                "AGENTVAULT_TRANSPORT": transport,
                "AGENTVAULT_GRPC_ENDPOINT": "127.0.0.1:50051",
                "AGENTVAULT_GRPC_TLS": "false",
                "AGENTVAULT_GRPC_TIMEOUT_SECONDS": "30",
                "AGENTVAULT_MQTT_TIMEOUT_SECONDS": "30",
                "AGENTVAULT_MQTT_USERNAME": identity.mqtt_username,
                "AGENTVAULT_APPROVAL_TIMEOUT_SECONDS": "900",
            }

    class ManualChainFixture:
        def __init__(self, deployer):
            self.deployer = deployer

        def solana(self) -> tuple[dict, dict]:
            d = self.deployer
            rpc = "http://127.0.0.1:8899"
            roles = [
                "facilitator",
                "recipient",
                "mint",
                *[
                    f"{language}-{slot}"
                    for language in LANGUAGES
                    for slot in ("primary", "secondary")
                ],
            ]
            paths, addresses = {}, {}
            for role in roles:
                paths[role] = f"/tmp/agentvault-manual-{role}.json"
                d._solana_command(
                    "solana-keygen",
                    "new",
                    "--silent",
                    "--no-bip39-passphrase",
                    "--force",
                    "--outfile",
                    paths[role],
                )
                addresses[role] = d._solana_pubkey(paths[role])
            d._solana_command(
                "solana",
                "airdrop",
                "10",
                addresses["facilitator"],
                "--url",
                rpc,
                timeout=180,
            )
            common = (
                "--url",
                rpc,
                "--fee-payer",
                paths["facilitator"],
                "--output",
                "json-compact",
            )
            created = d._solana_json(
                "spl-token",
                "create-token",
                *common,
                "--mint-authority",
                addresses["facilitator"],
                "--decimals",
                "6",
                paths["mint"],
                timeout=180,
            )
            mint = created["commandOutput"]["address"]
            if mint != addresses["mint"]:
                raise RuntimeError("manual SPL mint identity mismatch")
            accounts, secrets, key_envs = {}, {}, {}
            for role in [
                "recipient",
                *[r for r in roles if r not in {"recipient", "mint", "facilitator"}],
            ]:
                d._solana_json(
                    "spl-token",
                    "create-account",
                    *common,
                    "--owner",
                    addresses[role],
                    mint,
                    timeout=180,
                )
                wire = d._solana_json(
                    "spl-token",
                    "address",
                    "--url",
                    rpc,
                    "--verbose",
                    "--fee-payer",
                    paths["facilitator"],
                    "--output",
                    "json-compact",
                    "--token",
                    mint,
                    "--owner",
                    addresses[role],
                )
                accounts[role] = wire["associatedTokenAddress"]
                if role != "recipient":
                    d._solana_command(
                        "solana",
                        "airdrop",
                        "1",
                        addresses[role],
                        "--url",
                        rpc,
                        timeout=180,
                    )
                    d._solana_json(
                        "spl-token",
                        "mint",
                        *common,
                        "--mint-authority",
                        paths["facilitator"],
                        mint,
                        "1000",
                        accounts[role],
                        timeout=180,
                    )
                    language, slot = role.split("-", 1)
                    key_env = (
                        f"HELPER_SECRET_MANUAL_SOLANA_{language.upper()}_{slot.upper()}"
                    )
                    key = json.loads(d._solana_keypair_json(paths[role]))
                    if len(key) != 64:
                        raise RuntimeError("unexpected Solana keypair size")
                    secrets[key_env] = bytes(key[:32]).hex()
                    key_envs[f"wallet-quantstream-{language}-{slot}"] = key_env
            with d._forward_service(
                PAYMENT_NAMESPACE, "solana-validator", 8899
            ) as port:
                genesis = d._json_rpc_result(
                    f"http://127.0.0.1:{port}", "Solana", "getGenesisHash", []
                )
            if not isinstance(genesis, str) or not re.fullmatch(
                r"[1-9A-HJ-NP-Za-km-z]{32,88}", genesis
            ):
                raise RuntimeError("invalid Solana genesis identity")
            public = {
                "network": f"solana:{genesis[:32]}",
                "mint": mint,
                "decimals": 6,
                "recipient": addresses["recipient"],
                "fee_payer": addresses["facilitator"],
                "accounts": accounts,
                "wallet_addresses": addresses,
                "key_envs": key_envs,
                "token_program": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
            }
            tempo, tempo_secrets = self.tempo_wallets()
            public.update(tempo)
            secrets.update(tempo_secrets)
            secrets["SOLANA_FACILITATOR_KEYPAIR"] = d._solana_keypair_json(
                paths["facilitator"]
            )
            return public, secrets

        def tempo_wallets(self) -> tuple[dict, dict]:
            # Deterministic E2E-only keys 101..111 are separate from Anvil's ten
            # payer keys and dev revenue/faucet addresses. Production never uses
            # this fixture. Only their public addresses enter reports.
            pairs = [
                (language, slot)
                for language in LANGUAGES
                for slot in ("primary", "secondary")
            ]
            keys = [f"{number:064x}" for number in range(101, 112)]
            output = self.deployer._kubectl(
                "exec",
                "--namespace",
                PAYMENT_NAMESPACE,
                "deployment/anvil",
                "--",
                "sh",
                "-ec",
                'for key in "$@"; do cast wallet address --private-key "$key"; done',
                "manual-tempo-addresses",
                *keys,
            )
            addresses = re.findall(r"(?im)^\s*(0x[0-9a-f]{40})\s*$", output)
            if (
                len(addresses) != 11
                or len({address.lower() for address in addresses}) != 11
            ):
                raise RuntimeError(
                    "manual Tempo fixture requires eleven distinct EVM addresses"
                )
            secrets, wallet_addresses, key_envs = {}, {}, {}
            for (language, slot), key, address in zip(
                pairs, keys[:10], addresses[:10], strict=True
            ):
                wallet_id = f"wallet-quantstream-{language}-{slot}"
                key_env = (
                    f"HELPER_SECRET_MANUAL_TEMPO_{language.upper()}_{slot.upper()}"
                )
                secrets[key_env] = key
                wallet_addresses[wallet_id + "-tempo"] = address.lower()
                key_envs[wallet_id] = key_env
            return {
                "tempo_recipient": addresses[-1].lower(),
                "tempo_wallet_addresses": wallet_addresses,
                "tempo_key_envs": key_envs,
            }, secrets

        def fund_tempo(self, fixture: dict) -> dict:
            d = self.deployer
            transactions = {}
            with d._forward_service(
                PAYMENT_NAMESPACE, "manual-payment-provider", 8087
            ) as port:
                for wallet_id, address in fixture["tempo_wallet_addresses"].items():
                    if not re.fullmatch(r"0x[0-9A-Fa-f]{40}", address):
                        raise RuntimeError("invalid Tempo fixture wallet address")
                    status, _, raw = d._http(
                        f"http://127.0.0.1:{port}/fund",
                        "POST",
                        json.dumps({"address": address}).encode(),
                        {"content-type": "application/json"},
                        120,
                    )
                    wire = json.loads(raw)
                    if status != 200 or not wire.get("transaction"):
                        raise RuntimeError(
                            "Tempo funding did not return a confirmed transaction"
                        )
                    transactions[wallet_id] = {"address": address, **wire}
            return transactions

    class ManualPaymentEvidence:
        def __init__(self, deployer, profile, payer, payee, message):
            if (
                payer not in LANGUAGES
                or payee not in LANGUAGES
                or message not in {"mcp", "a2a"}
            ):
                raise ValueError("invalid manual evidence identity")
            self.d, self.profile, self.payer, self.payee, self.message = (
                deployer,
                profile,
                payer,
                payee,
                message,
            )
            self.state = json.loads(deployer.state_path.read_text())
            self.asset = E2ES29RetainedPurchaseVerifier.ManualProfiles.asset(
                profile, self.state["solana"]
            )
            self.network = self.asset["network"]

        def rows(self, sql):
            return json.loads(
                self.d._sql(
                    "", "", f"SELECT coalesce(json_agg(t),'[]'::json) FROM ({sql}) t"
                )
            )

        def run_ids(self):
            return {
                r["run_id"]
                for r in self.rows(
                    f"SELECT run_id FROM pay_intent WHERE network='{self.network}'"
                )
            }

        def validate_clock(self):
            service, port = self.endpoint()
            with self.d._forward_service(PAYMENT_NAMESPACE, service, port) as local:
                url = f"http://127.0.0.1:{local}"
                if self.profile == "solana":
                    slot = self.d._json_rpc_result(
                        url, "Solana", "getSlot", [{"commitment": "confirmed"}]
                    )
                    stamp = self.d._json_rpc_result(
                        url, "Solana", "getBlockTime", [slot]
                    )
                else:
                    block = self.d._json_rpc_result(
                        url, self.profile, "eth_getBlockByNumber", ["latest", False]
                    )
                    stamp = int(block["timestamp"], 16)
                    if self.profile == "evm":
                        # This is an explicit local test-node preparation step, never
                        # a relaxation of credential/receipt expiry on other chains.
                        version = self.d._json_rpc_result(
                            url, "Anvil", "web3_clientVersion", []
                        )
                        chain = self.d._json_rpc_result(url, "Anvil", "eth_chainId", [])
                        observed = int(time.time())
                        if (
                            not str(version).lower().startswith("anvil/")
                            or int(chain, 16) != 31337
                        ):
                            raise RuntimeError(
                                "manual EVM clock preparation requires local Anvil chain 31337"
                            )
                        if stamp > observed + 60:
                            raise RuntimeError(
                                "manual evm chain clock is ahead; refusing to create a payment Job"
                            )
                        self.d._json_rpc_result(
                            url, "Anvil", "evm_mine", [max(observed, stamp + 1)]
                        )
                        block = self.d._json_rpc_result(
                            url, "Anvil", "eth_getBlockByNumber", ["latest", False]
                        )
                        stamp = int(block["timestamp"], 16)
            observed = int(time.time())
            if type(stamp) is not int or abs(observed - stamp) > 60:
                raise RuntimeError(
                    f"manual {self.profile} chain clock is stale; refusing to create a payment Job"
                )
            return {
                "block_time": stamp,
                "observed_at": observed,
                "skew_seconds": observed - stamp,
            }

        def recipient(self):
            if self.profile == "solana":
                return self.state["solana"]["recipient"]
            if self.profile == "tempo":
                return self.state["solana"]["tempo_recipient"]
            return PAYEE_ADDRESS.lower()

        def wallet_ids(self):
            suffix = E2ES29RetainedPurchaseVerifier.ManualProfiles.suffix(self.profile)
            return [
                f"wallet-quantstream-{language}-{slot}{suffix}"
                for language in LANGUAGES
                for slot in ("primary", "secondary")
            ]

        def balances(self):
            expected = self.wallet_ids()
            ids = ",".join(f"'{wallet}'" for wallet in expected)
            wallets = self.rows(
                f"SELECT id,address FROM pay_wallet WHERE id IN ({ids}) ORDER BY id"
            )
            if {wallet["id"] for wallet in wallets} != set(expected):
                raise RuntimeError(
                    "manual balance snapshot does not contain the ten expected wallet identities"
                )
            result = {}
            service, port = self.endpoint()
            with self.d._forward_service(PAYMENT_NAMESPACE, service, port) as local:
                url = f"http://127.0.0.1:{local}"
                if self.profile == "solana":
                    solana = self.state["solana"]
                    for label, account in solana["accounts"].items():
                        key = (
                            "recipient"
                            if label == "recipient"
                            else f"wallet-quantstream-{label}-solana"
                        )
                        value = self.d._json_rpc_result(
                            url,
                            "Solana",
                            "getTokenAccountBalance",
                            [account, {"commitment": "confirmed"}],
                        )
                        result[key] = int(value["value"]["amount"])
                    for wallet in wallets:
                        value = self.d._json_rpc_result(
                            url,
                            "Solana",
                            "getBalance",
                            [wallet["address"], {"commitment": "confirmed"}],
                        )
                        result["native/" + wallet["id"]] = int(value["value"])
                    result["fee_payer_native"] = int(
                        self.d._json_rpc_result(
                            url,
                            "Solana",
                            "getBalance",
                            [solana["fee_payer"], {"commitment": "confirmed"}],
                        )["value"]
                    )
                else:
                    for wallet in [
                        *wallets,
                        {"id": "recipient", "address": self.recipient()},
                    ]:
                        call = {
                            "to": self.asset["token_contract"],
                            "data": "0x70a08231"
                            + wallet["address"][2:].lower().zfill(64),
                        }
                        wire = self.d._json_rpc_result(
                            url, self.profile, "eth_call", [call, "latest"]
                        )
                        if not isinstance(wire, str) or wire == "0x":
                            raise RuntimeError(
                                f"manual {self.profile} token contract missing or invalid balance response; "
                                "refusing to create a payment Job"
                            )
                        result[wallet["id"]] = int(wire, 16)
                    if self.profile == "evm":
                        result["fee_payer_native"] = int(
                            self.d._json_rpc_result(
                                url,
                                "Anvil",
                                "eth_getBalance",
                                [FACILITATOR_ADDRESS, "latest"],
                            ),
                            16,
                        )
            if not set(expected).issubset(result):
                raise RuntimeError(
                    "manual chain snapshot omitted an expected token account"
                )
            return result

        def verify(self, before_runs, before):
            rows = []
            for _ in range(60):
                all_rows = self.rows(
                    "SELECT id,scenario_id,policy_id,price_id,run_id,wallet_id,direction,protocol,"
                    "status,amount::text,tx_hash,resource,network,asset,token_contract,asset_decimals,recipient "
                    f"FROM pay_intent WHERE network='{self.network}'"
                )
                rows = [r for r in all_rows if r["run_id"] not in before_runs]
                if len(rows) == 2 and all(r["status"] == "settled" for r in rows):
                    break
                time.sleep(1)
            if len(rows) != 2 or {r["direction"] for r in rows} != {
                "inbound",
                "outbound",
            }:
                raise RuntimeError(
                    f"manual Job did not create exactly two directional intents: {rows}"
                )
            if any(
                r["status"] != "settled" or int(r["amount"]) != 1_000_000 for r in rows
            ):
                raise RuntimeError(
                    f"manual payment intent is not fully settled: {rows}"
                )
            if (
                len({r["run_id"] for r in rows}) != 1
                or len({r["tx_hash"] for r in rows}) != 1
                or not rows[0]["tx_hash"]
            ):
                raise RuntimeError(
                    "manual payment is not one chain transaction bound to one run"
                )
            outbound = self.validate_intents(rows)
            run_id, tx = outbound["run_id"], outbound["tx_hash"]
            intent_ids = ",".join(
                "'" + row["id"].replace("'", "''") + "'" for row in rows
            )
            ledger = self.rows(
                "SELECT l.id,l.intent_id,l.wallet_id,l.direction,l.entry_type,l.amount::text,l.tx_hash "
                "FROM pay_ledger l JOIN pay_intent i ON i.id=l.intent_id "
                f"WHERE i.id IN ({intent_ids}) AND l.entry_type='payment'"
            )
            intents = {row["id"]: row for row in rows}
            if (
                len(ledger) != 2
                or {r["direction"] for r in ledger} != {"inbound", "outbound"}
                or any(
                    r["intent_id"] not in intents
                    or r["tx_hash"] != tx
                    or int(r["amount"]) != 1_000_000
                    or r["direction"] != intents[r["intent_id"]]["direction"]
                    or r["wallet_id"] != intents[r["intent_id"]]["wallet_id"]
                    for r in ledger
                )
            ):
                raise RuntimeError(
                    f"manual payment does not have exactly two matching ledger facts: {ledger}"
                )
            receipt, fee = self.receipt(tx)
            after = self.balances()
            self.validate_balances(outbound["wallet_id"], before, after, fee)
            return {
                "run_id": run_id,
                "transaction": tx,
                "intents": rows,
                "ledger": ledger,
                "chain_receipt": receipt,
                "fee": str(fee),
                "before": before,
                "after": after,
            }

        def validate_intents(self, rows):
            identity = payer_identity(self.payer)
            suffix = E2ES29RetainedPurchaseVerifier.ManualProfiles.suffix(self.profile)
            selected = {wallet + suffix for wallet in identity.wallet_ids}
            resource = (
                E2ES29RetainedPurchaseVerifier.ManualProfiles.origin(
                    self.payee, self.profile
                )
                + "/"
                + self.message
            )
            outbound = next(row for row in rows if row["direction"] == "outbound")
            inbound = next(row for row in rows if row["direction"] == "inbound")
            if (
                outbound["wallet_id"] not in selected
                or outbound["scenario_id"] != identity.scenario_id + suffix
                or outbound["policy_id"] != identity.policy_id + suffix
                or outbound["price_id"] is not None
                or inbound["scenario_id"] != "scenario-marketpulse-feed" + suffix
                or inbound["policy_id"] != "policy-marketpulse-sales" + suffix
                or inbound["wallet_id"] != "wallet-marketpulse-revenue" + suffix
                or inbound["price_id"]
                != f"price-{self.payee}-{self.profile}-{self.message}"
            ):
                raise RuntimeError(
                    "manual payment used a different payer pool or payee price than requested"
                )
            protocol = "mpp" if self.profile == "tempo" else "x402"
            normalize = str.lower if self.profile != "solana" else str
            for row in rows:
                if (
                    row["protocol"] != protocol
                    or row["resource"] != resource
                    or row["network"] != self.asset["network"]
                    or row["asset"] != self.asset["asset"]
                    or row["asset_decimals"] != self.asset["decimals"]
                    or normalize(row["token_contract"] or "")
                    != normalize(self.asset["token_contract"])
                    or normalize(row["recipient"]) != normalize(self.recipient())
                ):
                    raise RuntimeError(
                        "manual payment resource or chain asset differs from the selected profile"
                    )
            return outbound

        def validate_balances(self, selected, before, after, fee):
            if before.keys() != after.keys():
                raise RuntimeError(
                    "manual wallet isolation snapshot identities changed"
                )
            if after["recipient"] - before["recipient"] != 1_000_000:
                raise RuntimeError("manual recipient did not receive exactly one token")
            expected_debit = 1_000_000 + (fee if self.profile == "tempo" else 0)
            for key in self.wallet_ids():
                if before[key] - after[key] != (
                    expected_debit if key == selected else 0
                ):
                    raise RuntimeError(
                        f"manual wallet isolation/token+fee conservation failed for {key}"
                    )
            if any(
                before[key] != after[key] for key in before if key.startswith("native/")
            ):
                raise RuntimeError(
                    "manual Solana payer native balance changed despite facilitator fee sponsorship"
                )
            if (
                self.profile != "tempo"
                and before["fee_payer_native"] - after["fee_payer_native"] != fee
            ):
                raise RuntimeError(
                    "manual facilitator native balance delta differs from receipt fee"
                )

        def endpoint(self):
            return {
                "evm": ("anvil", 8545),
                "tempo": ("tempo", 8545),
                "solana": ("solana-validator", 8899),
            }[self.profile]

        def receipt(self, tx):
            service, port = self.endpoint()
            with self.d._forward_service(PAYMENT_NAMESPACE, service, port) as local:
                url = f"http://127.0.0.1:{local}"
                if self.profile == "solana":
                    receipt = self.d._json_rpc_result(
                        url,
                        "Solana",
                        "getTransaction",
                        [
                            tx,
                            {
                                "encoding": "jsonParsed",
                                "commitment": "confirmed",
                                "maxSupportedTransactionVersion": 0,
                            },
                        ],
                    )
                    if not receipt or receipt["meta"]["err"] is not None:
                        raise RuntimeError(
                            "manual Solana transfer did not confirm successfully"
                        )
                    signatures = receipt.get("transaction", {}).get("signatures", [])
                    if not signatures or signatures[0] != tx:
                        raise RuntimeError(
                            "manual Solana transaction identity differs from the payment"
                        )
                    return receipt, int(receipt["meta"]["fee"])
                receipt = self.d._json_rpc_result(
                    url, self.profile, "eth_getTransactionReceipt", [tx]
                )
                if not receipt or receipt.get("status") != "0x1":
                    raise RuntimeError(
                        "manual EVM/Tempo transfer did not confirm successfully"
                    )
                if receipt.get("transactionHash", "").lower() != tx.lower():
                    raise RuntimeError(
                        "manual EVM/Tempo transaction identity differs from the payment"
                    )
                gas = int(receipt["gasUsed"], 16) * int(
                    receipt["effectiveGasPrice"], 16
                )
                return receipt, (
                    (gas + 10**12 - 1) // 10**12 if self.profile == "tempo" else gas
                )

    class ManualAcceptanceDeployer(E2ES20PurchaseVerifier.KubernetesRuntime):
        IMAGE_PIN = MANUAL_IMAGE_PIN
        IMAGE_PIN_OWNER = MANUAL_IMAGE_PIN_OWNER

        def __init__(self, context):
            super().__init__(context)
            self.manual_secrets = {}
            self.state_path = RUNTIME_ROOT / "manual-public.json"

        def _apply_document(self, document):
            document.setdefault("metadata", {}).setdefault("labels", {})[
                "agentvault.io/manual-acceptance"
            ] = "true"
            return super()._apply_document(document)

        def cleanup(self):
            for namespace in (
                AGENTVAULT_NAMESPACE,
                INFRA_NAMESPACE,
                PAYMENT_NAMESPACE,
                PAYEE_NAMESPACE,
                PAYER_NAMESPACE,
            ):
                self._kubectl(
                    "delete",
                    "deployment,service,cronjob,job",
                    "-n",
                    namespace,
                    "-l",
                    "agentvault.io/manual-acceptance=true",
                    "--ignore-not-found",
                    "--wait=true",
                    "--timeout=90s",
                    timeout=100,
                )
            return super().cleanup()

        def _ensure_cluster_image(self, reference):
            super()._ensure_cluster_image(reference)
            retained = {self.images.get(f"payer-{language}") for language in LANGUAGES}
            if reference in retained:
                self._pin_payer_image(reference)

        def _pin_payer_image(self, reference):
            self._image_loader.pin_image(
                reference, PAYER_NAMESPACE, self._apply_document, self._kubectl
            )

        def _apply_secret(self, namespace, name, values):
            if namespace == AGENTVAULT_NAMESPACE and name == "agentvault-secrets":
                values = {**values, **self.manual_secrets}
            super()._apply_secret(namespace, name, values)

        def deploy(self) -> None:
            self.cleanup()
            self._apply_namespaces()
            self._bootstrap_manifests()
            self._preload_local_dependencies()
            self._install_dependencies("payee", include_solana=True)
            tempo = yaml.safe_load(
                (self._component_chart("tempo") / "values.yaml").read_text()
            )
            tempo["args"][3] = "100ms"
            source = self._prepare_deployment_image(
                E2EImagesDefinitions.reference("tempo", self.environment)
            )
            tempo["image"] = {
                key.removeprefix("image."): value
                for key, value in self._image_reference_values(source).items()
            }
            self.state_path.parent.mkdir(parents=True, exist_ok=True)
            tempo_path = self.state_path.parent / "manual-tempo-values.yaml"
            tempo_path.write_text(yaml.safe_dump(tempo))
            self._helm_install(
                "tempo",
                self._component_chart("tempo"),
                PAYMENT_NAMESPACE,
                values=(tempo_path,),
                set_strings={
                    f"image.{key}": str(value) for key, value in tempo["image"].items()
                },
            )
            with self._forward_service(PAYMENT_NAMESPACE, "tempo", 8545) as port:
                self._wait_tempo_faucet_window(f"http://127.0.0.1:{port}")

        def initialize(self) -> dict:
            self._initialize_base()
            solana, keys = E2ES29RetainedPurchaseVerifier.ManualChainFixture(
                self
            ).solana()
            self.manual_secrets = {
                k: v for k, v in keys.items() if k.startswith("HELPER_SECRET_")
            }
            self.manual_secrets["HELPER_SECRET_MANUAL_MPP_CHALLENGE"] = (
                "manual-e2e-mpp-challenge-key-32-bytes-only"
            )
            self._bootstrap_manifests()
            self._apply_config_map(
                AGENTVAULT_NAMESPACE,
                "agentvault-e2e-resources",
                {
                    "resources.yaml": E2ES29RetainedPurchaseVerifier.ManualProfiles.resources(
                        solana
                    )
                },
            )
            self._install_provider(solana, keys["SOLANA_FACILITATOR_KEYPAIR"])
            funding = E2ES29RetainedPurchaseVerifier.ManualChainFixture(
                self
            ).fund_tempo(solana)
            self._configure_vault(solana)
            state = {
                "run_id": self.context.run_id,
                "solana": solana,
                "tempo_funding": funding,
                "profiles": list(E2ES29RetainedPurchaseVerifier.ManualProfiles.NAMES),
                "languages": list(LANGUAGES),
                "images": self.images,
                "provider_image": self._provider_image(),
                "services_ready": False,
            }
            # Persist public chain identities before any application rollout. A PVC or
            # image failure can then resume services without resetting funded chains.
            self._record_state(state)
            self._build_matrix_images(LANGUAGES)
            self._install_agentvault_manual()
            return self.services(state)

        def _record_state(self, state: dict) -> None:
            self.state_path.parent.mkdir(parents=True, exist_ok=True)
            temporary = self.state_path.with_suffix(".tmp")
            temporary.write_text(json.dumps(state, indent=2) + "\n")
            temporary.replace(self.state_path)
            target = self.context.report_root / "payment/manual"
            target.mkdir(parents=True, exist_ok=True)
            (target / "deployment.json").write_text(json.dumps(state, indent=2) + "\n")

        def services(self, state: dict | None = None) -> dict:
            if state is None:
                if not self.state_path.is_file():
                    raise RuntimeError(
                        "manual-services requires existing manual-public.json; it never initializes chains"
                    )
                state = json.loads(self.state_path.read_text())
            if state.get("profiles") != list(
                E2ES29RetainedPurchaseVerifier.ManualProfiles.NAMES
            ) or state.get("languages") != list(LANGUAGES):
                raise RuntimeError(
                    "manual-services public state has an incompatible profile/language catalog"
                )
            images = state.get("images", {})
            required = {
                "agentvault",
                "admin-ui",
                *(
                    f"{role}-{language}"
                    for role in ("payer", "payee")
                    for language in LANGUAGES
                ),
            }
            if not required.issubset(images):
                raise RuntimeError(
                    "manual-services public state omits a required frozen application image"
                )
            self.images = dict(images)
            state = {**state, "services_ready": False}
            self._record_state(state)
            self._preload_local_dependencies()
            for language in LANGUAGES:
                self._ensure_cluster_image(self.images[f"payer-{language}"])
                self._apply_business_claims(language, language)
                for profile in E2ES29RetainedPurchaseVerifier.ManualProfiles.NAMES:
                    self._deploy_payee(language, profile)
                self._suspended_payer(language)
            self._install_admin_ui("rust")
            state["services_ready"] = True
            self._record_state(state)
            return state

        def _helm_install(self, release, chart, namespace, *, values=(), **options):
            if release == "anvil" and namespace == PAYMENT_NAMESPACE:
                # Automine avoids unbounded idle blocks. Each payment preflight
                # advances local Anvil time once before credentials are issued.
                source = yaml.safe_load((chart / "values.yaml").read_text())
                claim = "manual-anvil-state"
                self._apply_document(
                    {
                        "apiVersion": "v1",
                        "kind": "PersistentVolumeClaim",
                        "metadata": {
                            "name": claim,
                            "namespace": PAYMENT_NAMESPACE,
                            "labels": {
                                "app.kubernetes.io/part-of": "agentvault-e2e",
                                BUSINESS_EVIDENCE_LABEL: "true",
                            },
                        },
                        "spec": {
                            "accessModes": ["ReadWriteOnce"],
                            "resources": {"requests": {"storage": "4Gi"}},
                        },
                    }
                )
                args = [
                    *source["args"],
                    "--state=/var/lib/anvil/state.json",
                    "--state-interval=1",
                    "--preserve-historical-states",
                ]
                self.state_path.parent.mkdir(parents=True, exist_ok=True)
                override = self.state_path.parent / "manual-anvil-values.yaml"
                override.write_text(
                    yaml.safe_dump(
                        {
                            "args": args,
                            "persistence": {
                                "enabled": True,
                                "existingClaim": claim,
                                "mountPath": "/var/lib/anvil",
                            },
                            "resources": {"limits": {"memory": "2Gi"}},
                        }
                    )
                )
                values = (*values, override)
            if release == "solana" and namespace == PAYMENT_NAMESPACE:
                # Manual initialization already explicitly cleans the previous stack.
                # Ordinary container/Pod restarts must resume the same funded ledger.
                source = yaml.safe_load((chart / "values.yaml").read_text())
                claim = "manual-solana-ledger"
                self._apply_document(
                    {
                        "apiVersion": "v1",
                        "kind": "PersistentVolumeClaim",
                        "metadata": {
                            "name": claim,
                            "namespace": PAYMENT_NAMESPACE,
                            "labels": {
                                "app.kubernetes.io/part-of": "agentvault-e2e",
                                BUSINESS_EVIDENCE_LABEL: "true",
                            },
                        },
                        "spec": {
                            "accessModes": ["ReadWriteOnce"],
                            "resources": {"requests": {"storage": "8Gi"}},
                        },
                    }
                )
                self.state_path.parent.mkdir(parents=True, exist_ok=True)
                override = self.state_path.parent / "manual-solana-values.yaml"
                override.write_text(
                    yaml.safe_dump(
                        {
                            "args": [
                                arg
                                for arg in source["args"]
                                if arg.split("=", 1)[0]
                                not in {"--reset", "-r", "--log"}
                            ]
                            + ["--log"],
                            "env": {"RUST_LOG": "warn"},
                            "persistence": {"enabled": True, "existingClaim": claim},
                            "resources": {"limits": {"cpu": "500m"}},
                        }
                    )
                )
                values = (*values, override)
            return super()._helm_install(
                release, chart, namespace, values=values, **options
            )

        def _provider_image(self):
            return E2EImagesDefinitions.reference("manual-provider", self.environment)

        def _preload_local_dependencies(self):
            references = {
                E2EImagesDefinitions.reference("envoy", self.environment),
                E2EImagesDefinitions.reference("volume-helper", self.environment),
            }
            for component in (
                "postgres",
                "emqx",
                "jaeger",
                "anvil",
                "solana",
                "facilitator",
            ):
                references.add(
                    E2EImagesDefinitions.reference(component, self.environment)
                )
            for reference in sorted(references):
                platform = E2EImagesDefinitions.reference_platform(
                    reference, self.environment
                )
                if (
                    self._image_loader.image_platform(
                        self.docker, self.environment, reference, platform
                    )
                    == platform
                ):
                    self._ensure_cluster_image(reference)
            self._retain_volume_helper_image()

        def _retain_volume_helper_image(self):
            # CronJobs intentionally remain suspended. Keep the tiny provisioner
            # image referenced so image GC cannot break their first PVC consumer.
            image = self._prepare_deployment_image(
                E2EImagesDefinitions.reference("volume-helper", self.environment)
            )
            labels = {
                "app.kubernetes.io/name": "manual-volume-helper",
                "app.kubernetes.io/part-of": "agentvault-e2e",
            }
            self._apply_document(
                {
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "metadata": {
                        "name": "manual-volume-helper",
                        "namespace": INFRA_NAMESPACE,
                        "labels": labels,
                    },
                    "spec": {
                        "replicas": 1,
                        "selector": {"matchLabels": labels},
                        "template": {
                            "metadata": {"labels": labels},
                            "spec": {
                                "containers": [
                                    {
                                        "name": "helper",
                                        "image": image,
                                        "imagePullPolicy": "Never",
                                        "command": ["/bin/sleep", "2147483647"],
                                        "resources": {
                                            "requests": {"cpu": "1m", "memory": "2Mi"},
                                            "limits": {"cpu": "10m", "memory": "16Mi"},
                                        },
                                    }
                                ]
                            },
                        },
                    },
                }
            )

        def _await_image(self, image):
            deadline = time.monotonic() + self.context.timeout_seconds
            while time.monotonic() < deadline:
                result = E2EProcessHelper.execute(
                    [*self.docker, "image", "inspect", image],
                    Project.E2E_ROOT,
                    self.environment,
                    30,
                )
                if result.returncode == 0:
                    return
                time.sleep(5)
            raise RuntimeError(f"manual acceptance source image not built: {image}")

        def _install_provider(self, solana, keypair):
            image = self._provider_image()
            self._await_image(image)
            self._ensure_cluster_image(image)
            self._apply_secret(
                PAYMENT_NAMESPACE,
                "manual-provider-secrets",
                {
                    "SOLANA_FACILITATOR_KEYPAIR": keypair,
                    "TEMPO_DEV_PRIVATE_KEY": self.environment.get(
                        "E2E_FACILITATOR_PRIVATE_KEY",
                        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                    ),
                },
            )
            environment = {
                "SOLANA_RPC_URL": "http://solana-validator:8899",
                "SOLANA_CHAIN_REFERENCE": solana["network"].split(":", 1)[1],
                "EVM_FACILITATOR_URL": "http://x402-facilitator:8085",
                "TEMPO_RPC_URL": "http://tempo:8545",
                "TEMPO_RECIPIENT": solana["tempo_recipient"],
                "TOKIO_WORKER_THREADS": "2",
            }
            labels = {
                "app.kubernetes.io/part-of": "agentvault-e2e",
                "app.kubernetes.io/name": "manual-payment-provider",
            }
            self._apply_document(
                {
                    "apiVersion": "apps/v1",
                    "kind": "Deployment",
                    "metadata": {
                        "name": "manual-payment-provider",
                        "namespace": PAYMENT_NAMESPACE,
                        "labels": labels,
                    },
                    "spec": {
                        "replicas": 1,
                        "selector": {"matchLabels": labels},
                        "template": {
                            "metadata": {"labels": labels},
                            "spec": {
                                "containers": [
                                    {
                                        "name": "provider",
                                        "image": image,
                                        "imagePullPolicy": "Never",
                                        "env": [
                                            {"name": k, "value": v}
                                            for k, v in environment.items()
                                        ],
                                        "envFrom": [
                                            {
                                                "secretRef": {
                                                    "name": "manual-provider-secrets"
                                                }
                                            }
                                        ],
                                        "ports": [{"containerPort": 8087}],
                                        "readinessProbe": {
                                            "httpGet": {
                                                "path": "/healthz",
                                                "port": 8087,
                                            },
                                            "periodSeconds": 3,
                                        },
                                        "resources": {
                                            "requests": {
                                                "cpu": "20m",
                                                "memory": "64Mi",
                                            },
                                            "limits": {
                                                "cpu": "500m",
                                                "memory": "512Mi",
                                            },
                                        },
                                    }
                                ]
                            },
                        },
                    },
                }
            )
            self._apply_document(
                {
                    "apiVersion": "v1",
                    "kind": "Service",
                    "metadata": {
                        "name": "manual-payment-provider",
                        "namespace": PAYMENT_NAMESPACE,
                        "labels": labels,
                    },
                    "spec": {
                        "selector": labels,
                        "ports": [{"port": 8087, "targetPort": 8087}],
                    },
                }
            )
            self._kubectl(
                "rollout",
                "status",
                "deployment/manual-payment-provider",
                "-n",
                PAYMENT_NAMESPACE,
                "--timeout=180s",
                timeout=190,
            )

        def _configure_vault(self, solana):
            values = yaml.safe_load(AGENTVAULT_VALUES.read_text())
            for container in values["agentvault"]["backend"].get("extraContainers", []):
                container["imagePullPolicy"] = "Never"
            config = yaml.safe_load(values["agentvault"]["agentvault-config"])
            domain = config["agentvault"]
            asset = E2ES29RetainedPurchaseVerifier.ManualProfiles.asset(
                "solana", solana
            )
            xasset = {
                "network": asset["network"],
                "contract": asset["token_contract"],
                "symbol": "USDC",
                "decimals": 6,
                "fee_payer": solana["fee_payer"],
            }
            masset = {
                "network": "eip155:1337",
                "currency": E2ES29RetainedPurchaseVerifier.ManualProfiles.TEMPO_TOKEN,
                "symbol": "pathUSD",
                "token_contract": E2ES29RetainedPurchaseVerifier.ManualProfiles.TEMPO_TOKEN,
                "decimals": 6,
            }
            for role in ("payer", "payee"):
                domain[role]["x402"]["assets"].append(xasset)
                domain[role]["mpp"].update(enabled=True, assets=[masset])
            domain["payee"]["x402"]["facilitator"]["base_url"] = (
                "http://127.0.0.1:8087/x402/"
            )
            domain["payee"]["x402"]["facilitator"]["timeout"] = "60s"
            domain["payee"]["mpp"].update(
                realm="agentvault-manual",
                challenge_secret={
                    "file": "",
                    "env": "HELPER_SECRET_MANUAL_MPP_CHALLENGE",
                },
            )
            domain["payee"]["mpp"]["provider"].update(
                id="manual-tempo",
                base_url="http://127.0.0.1:8087/",
                timeout="60s",
                allow_insecure_loopback=True,
            )
            for network, family, port in (
                (asset["network"], "solana", 8899),
                ("eip155:1337", "evm", 8546),
            ):
                chain = copy.deepcopy(domain["chains"]["eip155:31337"])
                chain.update(family=family)
                if network == "eip155:1337":
                    # Tempo charges gas in TIP-20, with no native-token balance requirement.
                    chain["minimum_native_balance"] = "0"
                chain["rpc"]["endpoint"] = f"http://127.0.0.1:{port}"
                chain["rpc"]["timeout"] = "30s"
                domain["chains"][network] = chain
            domain["wallet_balance_sync"]["interval"] = "15s"
            domain["reconciliation"]["interval"] = "10s"
            self._extend_vault_bridge()
            values["agentvault"]["agentvault-config"] = yaml.safe_dump(
                config, sort_keys=False
            )
            self.manual_values = self.state_path.parent / "manual-values.yaml"
            self.manual_values.parent.mkdir(parents=True, exist_ok=True)
            self.manual_values.write_text(yaml.safe_dump(values, sort_keys=False))

        def _extend_vault_bridge(self):
            raw = self._kubectl(
                "get",
                "configmap",
                "agentvault-local-envoy-cm",
                "-n",
                AGENTVAULT_NAMESPACE,
                "-o",
                "json",
            )
            config = yaml.safe_load(json.loads(raw)["data"]["envoy.yaml"])
            resources = config["static_resources"]
            for name, port, service, target in (
                ("solana", 8899, "solana-validator", 8899),
                ("tempo", 8546, "tempo", 8545),
                ("manual-provider", 8087, "manual-payment-provider", 8087),
            ):
                listener = copy.deepcopy(resources["listeners"][0])
                listener["name"] = name
                listener["address"]["socket_address"]["port_value"] = port
                proxy = listener["filter_chains"][0]["filters"][0]["typed_config"]
                proxy.update(stat_prefix=name, cluster=name)
                cluster = copy.deepcopy(resources["clusters"][0])
                cluster["name"] = name
                cluster["load_assignment"]["cluster_name"] = name
                endpoint = cluster["load_assignment"]["endpoints"][0]["lb_endpoints"][
                    0
                ]["endpoint"]["address"]["socket_address"]
                endpoint.update(
                    address=f"{service}.{PAYMENT_NAMESPACE}.svc.cluster.local",
                    port_value=target,
                )
                resources["listeners"].append(listener)
                resources["clusters"].append(cluster)
            self._apply_config_map(
                AGENTVAULT_NAMESPACE,
                "agentvault-local-envoy-cm",
                {"envoy.yaml": yaml.safe_dump(config)},
            )

        def _install_agentvault_manual(self):
            self._helm_install(
                "agentvault",
                AGENTVAULT_CHART,
                AGENTVAULT_NAMESPACE,
                values=(self.manual_values,),
                set_strings=self._image_values(
                    "agentvault", "agentvault.backend.image"
                ),
            )

        def _template(
            self, chart: Path, name: str, namespace: str, settings: dict
        ) -> list[dict]:
            args = [
                "template",
                name,
                str(chart),
                "--namespace",
                namespace,
                *self._chart_image_flags(
                    chart, self._shared_overrides_for_chart(chart)
                ),
            ]
            for path in self._shared_overrides_for_chart(chart):
                args += ["--values", str(path)]
            for key, value in settings.items():
                args += ["--set-string", f"{key}={value}"]
            documents = [d for d in yaml.safe_load_all(self._helm(*args)) if d]
            for document in documents:
                spec = document.get("spec", {}).get("template", {}).get("spec", {})
                for container in [
                    *spec.get("containers", []),
                    *spec.get("initContainers", []),
                ]:
                    if container.get("name") == "local-envoy-cm":
                        container["image"] = self._prepare_deployment_image(
                            E2EImagesDefinitions.reference("envoy", self.environment)
                        )
                        container["imagePullPolicy"] = "Never"
            return documents

        def _deploy_payee(self, language, profile):
            self._ensure_cluster_image(self.images[f"payee-{language}"])
            name = E2ES29RetainedPurchaseVerifier.ManualProfiles.service(
                language, profile
            )
            suffix = E2ES29RetainedPurchaseVerifier.ManualProfiles.suffix(profile)
            self._apply_document(
                {
                    "apiVersion": "v1",
                    "kind": "PersistentVolumeClaim",
                    "metadata": {
                        "name": name,
                        "namespace": PAYEE_NAMESPACE,
                        "labels": {
                            "app.kubernetes.io/part-of": "agentvault-e2e",
                            BUSINESS_EVIDENCE_LABEL: "true",
                        },
                    },
                    "spec": {
                        "accessModes": ["ReadWriteOnce"],
                        "resources": {"requests": {"storage": "64Mi"}},
                    },
                }
            )
            settings = {
                **self._image_values(f"payee-{language}"),
                "env.AGENTVAULT_TRANSPORT": "grpc",
                "persistence.existingClaim": name,
                "env.MARKETPULSE_SCENARIO_ID": f"scenario-marketpulse-feed{suffix}",
                "env.MARKETPULSE_PRICE_ID": f"price-{language}-{profile}-mcp",
                "env.MARKETPULSE_A2A_PRICE_ID": f"price-{language}-{profile}-a2a",
                "env.MARKETPULSE_PAYMENT_PROTOCOL": "mpp"
                if profile == "tempo"
                else "x402",
                "env.MARKETPULSE_RESOURCE_URL": E2ES29RetainedPurchaseVerifier.ManualProfiles.origin(
                    language, profile
                )
                + "/mcp",
                "env.MARKETPULSE_A2A_RESOURCE_URL": E2ES29RetainedPurchaseVerifier.ManualProfiles.origin(
                    language, profile
                )
                + "/a2a",
                "env.MARKETPULSE_ORIGIN": E2ES29RetainedPurchaseVerifier.ManualProfiles.origin(
                    language, profile
                ),
            }
            for document in self._template(
                self._payee_chart(language), name, PAYEE_NAMESPACE, settings
            ):
                if document["kind"] in {"Deployment", "Service", "ConfigMap"}:
                    document["metadata"]["name"] = name
                document["metadata"].setdefault("labels", {})[
                    "app.kubernetes.io/part-of"
                ] = "agentvault-e2e"
                if document["kind"] == "Deployment":
                    document["spec"]["template"]["spec"].setdefault(
                        "securityContext", {}
                    )["fsGroup"] = 65532
                    for container in document["spec"]["template"]["spec"]["containers"]:
                        if any(
                            port.get("containerPort") == 8081
                            for port in container.get("ports", [])
                        ):
                            container["startupProbe"] = {
                                "httpGet": {"path": "/healthz", "port": 8081},
                                "periodSeconds": 2,
                                "timeoutSeconds": 2,
                                "failureThreshold": 90,
                            }
                self._apply_document(document)
            self._kubectl(
                "rollout",
                "status",
                f"deployment/{name}",
                "-n",
                PAYEE_NAMESPACE,
                "--timeout=180s",
                timeout=190,
            )

        def _payer_job(self, payer, payee, profile, message, transport):
            environment = (
                E2ES29RetainedPurchaseVerifier.ManualProfiles.payer_environment(
                    payer, payee, profile, message, transport
                )
            )
            environment["QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT"] = (
                "once" if self.context.fail_after_settlement else ""
            )
            settings = {
                **self._image_values(f"payer-{payer}"),
                "persistence.existingClaim": self._business_claim("payer", payer),
                **{f"env.{key}": value for key, value in environment.items()},
            }
            documents = self._template(
                self._payer_chart(payer),
                self._payer_name(payer),
                PAYER_NAMESPACE,
                settings,
            )
            job = next(d for d in documents if d["kind"] == "Job")
            job["metadata"].setdefault("labels", {}).update(
                {
                    "app.kubernetes.io/part-of": "agentvault-e2e",
                    BUSINESS_EVIDENCE_LABEL: "true",
                }
            )
            return job

        def _suspended_payer(self, language):
            job = self._payer_job(language, "rust", "evm", "mcp", "grpc")
            self._apply_document(
                {
                    "apiVersion": "batch/v1",
                    "kind": "CronJob",
                    "metadata": {
                        "name": self._payer_name(language),
                        "namespace": PAYER_NAMESPACE,
                        "labels": job["metadata"]["labels"],
                    },
                    "spec": {
                        "schedule": "0 0 1 1 *",
                        "suspend": True,
                        "concurrencyPolicy": "Forbid",
                        "jobTemplate": {"spec": job["spec"]},
                    },
                }
            )

        def pay(self, payer=None, payee=None) -> dict:
            payer, payee = (
                payer or self.context.payer_language,
                payee or self.context.payee_language,
            )
            profile, message = (
                self.context.payment_profile,
                self.context.message_protocol,
            )
            name = f"manual-{payer}-{payee}-{profile}-{message}-{time.time_ns() % 1000000000}"
            directory = self.context.report_root / "payment/manual" / name
            directory.mkdir(parents=True, exist_ok=True)
            identity = {
                "job": name,
                "payer": payer,
                "payee": payee,
                "profile": profile,
                "message": message,
                "transport": self.context.transport,
                "fail_after_settlement": self.context.fail_after_settlement,
            }
            (directory / "request.json").write_text(
                json.dumps(identity, indent=2) + "\n"
            )
            phase = "prepare"
            try:
                self._ensure_cluster_image(self.images[f"payer-{payer}"])
                evidence = E2ES29RetainedPurchaseVerifier.ManualPaymentEvidence(
                    self, profile, payer, payee, message
                )
                evidence.validate_clock()
                before_runs, before = evidence.run_ids(), evidence.balances()
                (directory / "before.json").write_text(
                    json.dumps(
                        {
                            **identity,
                            "known_run_ids": sorted(before_runs),
                            "balances": before,
                        },
                        indent=2,
                    )
                    + "\n"
                )
                job = self._payer_job(
                    payer, payee, profile, message, self.context.transport
                )
                job["metadata"]["name"] = name
                job["spec"]["activeDeadlineSeconds"] = 960
                phase = "create_job"
                self._apply_document(job)
                phase = "wait_job"
                logs = self._wait_job(name, timeout_seconds=960)
                (directory / "payer.log").write_text(self._bounded_public_logs(logs))
                phase = "verify_financial_evidence"
                financial = evidence.verify(before_runs, before)
                (directory / "evidence.json").write_text(
                    json.dumps(financial, indent=2) + "\n"
                )
            except Exception as error:
                try:
                    logs = self._bounded_public_logs(self._job_logs(name))
                except Exception:
                    logs = "payer logs unavailable; inspect the retained Job\n"
                (directory / "payer.log").write_text(logs)
                # An exception can contain raw upstream responses. Persist its type,
                # never that response body or a rendered Pod/Secret specification.
                (directory / "failure.json").write_text(
                    json.dumps(
                        {
                            **identity,
                            "status": "failed",
                            "phase": phase,
                            "error_type": type(error).__name__,
                            "message": "Inspect the retained Job, bounded payer.log, and before.json; no financial result is assumed.",
                        },
                        indent=2,
                    )
                    + "\n"
                )
                raise RuntimeError(
                    f"manual payment {name} failed during {phase} ({type(error).__name__}); "
                    f"inspect {directory / 'failure.json'} and bounded payer.log"
                ) from None
            return {
                **identity,
                "status": "verified",
                "run_id": financial["run_id"],
                "transaction": financial["transaction"],
            }

        @staticmethod
        def _bounded_public_logs(logs: str) -> str:
            sensitive = re.compile(
                r"authorization|payment-signature|payment-authorization|private[_ -]?key|password|"
                r"secret|sealed_credential|credential[\"']?\s*[:=]|signature[\"']?\s*[:=]|"
                r"signedTransaction|signed_transaction|signed_tx|serializedTransaction|0x[0-9a-f]{129,}|"
                r"access[_ -]?token|refresh[_ -]?token",
                re.IGNORECASE,
            )
            lines = [
                "[redacted sensitive log line]" if sensitive.search(line) else line
                for line in logs.splitlines()
            ]
            return (
                ("\n".join(lines) + "\n")
                .encode("utf-8")[-131072:]
                .decode("utf-8", errors="ignore")
            )

        def acceptance_matrix(self) -> list[dict]:
            results = []
            for payer in LANGUAGES:
                for payee in LANGUAGES:
                    result = self.pay(payer, payee)
                    results.append(result)
                    path = self.context.report_root / "payment/manual/matrix.json"
                    path.write_text(json.dumps(results, indent=2) + "\n")
            return results

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        if context.deployer != "kubernetes":
            raise ValueError("manual acceptance requires --deployer kubernetes")
        runtime = E2ES29RetainedPurchaseVerifier.ManualAcceptanceDeployer(context)
        context.state["runtime"] = runtime
        if context.scenario == "manual":
            runtime.deploy()
        elif context.scenario == "manual-services":
            context.state["manual_result"] = runtime.services()

    @staticmethod
    def init(context: VerificationContext) -> None:
        if context.scenario == "manual":
            context.state["manual_result"] = context.state["runtime"].initialize()

    @staticmethod
    def verify(context: VerificationContext) -> str:
        if context.deployer != "kubernetes":
            raise ValueError("manual acceptance requires --deployer kubernetes")
        deployer = context.state["runtime"]
        if context.scenario == "manual":
            result = context.state["manual_result"]
            statement = "Redeployed retained stack. No payer payment was triggered."
        elif context.scenario == "manual-services":
            result = context.state["manual_result"]
            statement = "Resumed retained services. Existing chains, Vault, custody, and financial history were preserved."
        elif context.scenario == "manual-payment":
            result = deployer.pay()
            statement = (
                "Executed one real SDK payment; retained Job and chain/ledger evidence."
            )
        elif context.scenario == "manual-matrix":
            result = deployer.acceptance_matrix()
            statement = (
                "Executed 25 cross-language real payments on the retained stack."
            )
        else:
            raise ValueError("unsupported manual action")
        return f"# Manual acceptance\n\n{statement}\n\n```json\n{json.dumps(result, indent=2)}\n```\n"
