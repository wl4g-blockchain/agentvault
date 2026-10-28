"""Persistent manual-acceptance catalog; all custody values remain in Secrets."""
from __future__ import annotations

import copy
import yaml

from common.payer_identity import LANGUAGES, payer_identity
from common.project import CONFIG_ROOT
from .docker import TOKEN_CONTRACT, PAYEE_ADDRESS
from .kubernetes import LANGUAGE_CHART_NAMES, PAYEE_NAMESPACE


class ManualProfiles:
    NAMES = ("evm", "solana", "tempo")
    TEMPO_TOKEN = "0x20c0000000000000000000000000000000000000"

    @staticmethod
    def suffix(profile: str) -> str:
        if profile not in ManualProfiles.NAMES:
            raise ValueError("unknown manual payment profile")
        return "" if profile == "evm" else f"-{profile}"

    @staticmethod
    def service(language: str, profile: str) -> str:
        return f"marketpulse-{LANGUAGE_CHART_NAMES[language]}-{profile}"

    @classmethod
    def origin(cls, language: str, profile: str) -> str:
        return f"http://{cls.service(language, profile)}.{PAYEE_NAMESPACE}.svc.cluster.local:8081"

    @staticmethod
    def asset(profile: str, solana: dict) -> dict:
        network, symbol, token = {
            "evm": ("eip155:31337", "USDC", TOKEN_CONTRACT.lower()),
            "solana": (solana["network"], "USDC", solana["mint"]),
            "tempo": ("eip155:1337", "pathUSD", ManualProfiles.TEMPO_TOKEN),
        }[profile]
        return {"network": network, "asset": symbol, "token_contract": token, "decimals": 6}

    @classmethod
    def resources(cls, solana: dict) -> str:
        source = list(yaml.safe_load_all((CONFIG_ROOT / "agentvault/init.import.yaml").read_text()))
        documents = [copy.deepcopy(d) for d in source if d and d["kind"] == "Client"]
        for profile in cls.NAMES:
            suffix = cls.suffix(profile)
            asset = cls.asset(profile, solana)
            recipient = {"solana": solana["recipient"], "tempo": solana["tempo_recipient"],
                         "evm": PAYEE_ADDRESS.lower()}[profile]
            for original in source:
                if not original or original["kind"] in {"Client", "Price"}:
                    continue
                d = copy.deepcopy(original)
                kind, data = d["kind"], d["data"]
                original_id = d["metadata"]["name"]
                d["metadata"]["name"] += suffix
                if kind == "Wallet":
                    data["budget_unit"] = {"asset": asset["asset"], "decimals": 6}
                    data.update(max_per_payment="10000000", max_per_day="5000000000")
                    if profile == "solana":
                        data["algorithm"] = "ed25519"
                        if data["custody"] == "external":
                            data["address"] = recipient
                        else:
                            data["private_key_env"] = solana["key_envs"][original_id]
                    elif profile == "tempo":
                        if data["custody"] == "external":
                            data["address"] = recipient
                        else:
                            data["private_key_env"] = solana["tempo_key_envs"][original_id]
                elif kind in {"Policy", "Approver"}:
                    data["scenario_id"] += suffix
                    if kind == "Approver":
                        d["metadata"]["name"] = data["scenario_id"] + "--" + data["auth_principal"]
                    else:
                        data["wallet_ids"] = [w + suffix for w in data["wallet_ids"]]
                        data["protocols"] = ["mpp" if profile == "tempo" else "x402"]
                        data["networks"] = [asset["network"]]
                        data["assets"] = [asset]
                        data["budget_unit"] = {"asset": asset["asset"], "decimals": 6}
                        data["allowed_recipients"] = [recipient]
                        data["allowed_origins"] = [cls.origin(language, profile) for language in LANGUAGES]
                        limits = data.get("inbound_limits", data.get("outbound_limits"))
                        limits["max_per_day"] = "5000000000"
                        if "outbound_limits" in data:
                            limits["approval_overage"] = "5000000000"
                documents.append(d)
            for language in LANGUAGES:
                for message in ("mcp", "a2a"):
                    documents.append({
                        "consoleVersion": "core.agentvault.io/v1", "kind": "Price",
                        "metadata": {"name": f"price-{language}-{profile}-{message}", "status": "active"},
                        "data": {
                            "scenario_id": f"scenario-marketpulse-feed{suffix}",
                            "policy_id": f"policy-marketpulse-sales{suffix}",
                            "resource": f"{cls.origin(language, profile)}/{message}",
                            "protocols": ["mpp" if profile == "tempo" else "x402"],
                            "asset": asset, "amount": "1000000", "wallet_id": f"wallet-marketpulse-revenue{suffix}",
                        },
                    })
        return yaml.safe_dump_all(documents, sort_keys=False)

    @classmethod
    def payer_environment(cls, payer: str, payee: str, profile: str, message: str, transport: str) -> dict:
        identity = payer_identity(payer)
        return {
            "MARKETPULSE_URL": cls.origin(payee, profile) + "/mcp",
            "MARKETPULSE_A2A_URL": cls.origin(payee, profile) + "/a2a",
            "MARKETPULSE_MESSAGE_PROTOCOL": message,
            "QUANTSTREAM_CLIENT_ID": identity.client_id,
            "QUANTSTREAM_SCENARIO_ID": identity.scenario_id + cls.suffix(profile),
            "AGENTVAULT_TRANSPORT": transport,
            "AGENTVAULT_GRPC_ENDPOINT": "127.0.0.1:50051",
            "AGENTVAULT_GRPC_TLS": "false", "AGENTVAULT_GRPC_TIMEOUT_SECONDS": "30",
            "AGENTVAULT_MQTT_TIMEOUT_SECONDS": "30",
            "AGENTVAULT_MQTT_USERNAME": identity.mqtt_username,
            "AGENTVAULT_APPROVAL_TIMEOUT_SECONDS": "900",
        }
