"""Create real local-chain assets before importing the manual Vault catalog."""
from __future__ import annotations

import json
import re

from common.payer_identity import LANGUAGES
from .kubernetes import PAYMENT_NAMESPACE


class ManualChainFixture:
    def __init__(self, deployer):
        self.deployer = deployer

    def solana(self) -> tuple[dict, dict]:
        d = self.deployer
        rpc = "http://127.0.0.1:8899"
        roles = ["facilitator", "recipient", "mint", *[
            f"{language}-{slot}" for language in LANGUAGES for slot in ("primary", "secondary")
        ]]
        paths, addresses = {}, {}
        for role in roles:
            paths[role] = f"/tmp/agentvault-manual-{role}.json"
            d._solana_command("solana-keygen", "new", "--silent", "--no-bip39-passphrase",
                              "--force", "--outfile", paths[role])
            addresses[role] = d._solana_pubkey(paths[role])
        d._solana_command("solana", "airdrop", "10", addresses["facilitator"], "--url", rpc, timeout=180)
        common = ("--url", rpc, "--fee-payer", paths["facilitator"], "--output", "json-compact")
        created = d._solana_json("spl-token", "create-token", *common,
                                 "--mint-authority", addresses["facilitator"], "--decimals", "6", paths["mint"], timeout=180)
        mint = created["commandOutput"]["address"]
        if mint != addresses["mint"]:
            raise RuntimeError("manual SPL mint identity mismatch")
        accounts, secrets, key_envs = {}, {}, {}
        for role in ["recipient", *[r for r in roles if r not in {"recipient", "mint", "facilitator"}]]:
            d._solana_json("spl-token", "create-account", *common, "--owner", addresses[role], mint, timeout=180)
            wire = d._solana_json("spl-token", "address", "--url", rpc, "--verbose",
                                  "--fee-payer", paths["facilitator"], "--output", "json-compact", "--token", mint, "--owner", addresses[role])
            accounts[role] = wire["associatedTokenAddress"]
            if role != "recipient":
                d._solana_command("solana", "airdrop", "1", addresses[role], "--url", rpc, timeout=180)
                d._solana_json("spl-token", "mint", *common, "--mint-authority", paths["facilitator"],
                               mint, "1000", accounts[role], timeout=180)
                language, slot = role.split("-", 1)
                key_env = f"HELPER_SECRET_MANUAL_SOLANA_{language.upper()}_{slot.upper()}"
                key = json.loads(d._solana_keypair_json(paths[role]))
                if len(key) != 64:
                    raise RuntimeError("unexpected Solana keypair size")
                secrets[key_env] = bytes(key[:32]).hex()
                key_envs[f"wallet-quantstream-{language}-{slot}"] = key_env
        with d._forward_service(PAYMENT_NAMESPACE, "solana-validator", 8899) as port:
            genesis = d._json_rpc_result(f"http://127.0.0.1:{port}", "Solana", "getGenesisHash", [])
        if not isinstance(genesis, str) or not re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{32,88}", genesis):
            raise RuntimeError("invalid Solana genesis identity")
        public = {"network": f"solana:{genesis[:32]}", "mint": mint, "decimals": 6,
                  "recipient": addresses["recipient"], "fee_payer": addresses["facilitator"],
                  "accounts": accounts, "wallet_addresses": addresses, "key_envs": key_envs,
                  "token_program": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"}
        tempo, tempo_secrets = self.tempo_wallets()
        public.update(tempo)
        secrets.update(tempo_secrets)
        secrets["SOLANA_FACILITATOR_KEYPAIR"] = d._solana_keypair_json(paths["facilitator"])
        return public, secrets

    def tempo_wallets(self) -> tuple[dict, dict]:
        # Deterministic E2E-only keys 101..111 are separate from Anvil's ten
        # payer keys and dev revenue/faucet addresses. Production never uses
        # this fixture. Only their public addresses enter reports.
        pairs = [(language, slot) for language in LANGUAGES for slot in ("primary", "secondary")]
        keys = [f"{number:064x}" for number in range(101, 112)]
        output = self.deployer._kubectl(
            "exec", "--namespace", PAYMENT_NAMESPACE, "deployment/anvil", "--",
            "sh", "-ec", 'for key in "$@"; do cast wallet address --private-key "$key"; done',
            "manual-tempo-addresses", *keys,
        )
        addresses = re.findall(r"(?im)^\s*(0x[0-9a-f]{40})\s*$", output)
        if len(addresses) != 11 or len({address.lower() for address in addresses}) != 11:
            raise RuntimeError("manual Tempo fixture requires eleven distinct EVM addresses")
        secrets, wallet_addresses, key_envs = {}, {}, {}
        for (language, slot), key, address in zip(pairs, keys[:10], addresses[:10], strict=True):
            wallet_id = f"wallet-quantstream-{language}-{slot}"
            key_env = f"HELPER_SECRET_MANUAL_TEMPO_{language.upper()}_{slot.upper()}"
            secrets[key_env] = key
            wallet_addresses[wallet_id + "-tempo"] = address.lower()
            key_envs[wallet_id] = key_env
        return {"tempo_recipient": addresses[-1].lower(), "tempo_wallet_addresses": wallet_addresses,
                "tempo_key_envs": key_envs}, secrets

    def fund_tempo(self, fixture: dict) -> dict:
        d = self.deployer
        transactions = {}
        with d._forward_service(PAYMENT_NAMESPACE, "manual-payment-provider", 8087) as port:
            for wallet_id, address in fixture["tempo_wallet_addresses"].items():
                if not re.fullmatch(r"0x[0-9A-Fa-f]{40}", address):
                    raise RuntimeError("invalid Tempo fixture wallet address")
                status, _, raw = d._http(f"http://127.0.0.1:{port}/fund", "POST",
                                        json.dumps({"address": address}).encode(),
                                        {"content-type": "application/json"}, 120)
                wire = json.loads(raw)
                if status != 200 or not wire.get("transaction"):
                    raise RuntimeError("Tempo funding did not return a confirmed transaction")
                transactions[wallet_id] = {"address": address, **wire}
        return transactions
