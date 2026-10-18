from __future__ import annotations

from dataclasses import dataclass


LANGUAGES = ("rust", "go", "python", "typescript", "java")


@dataclass(frozen=True)
class PayerIdentity:
    language: str
    client_id: str
    scenario_id: str
    policy_id: str
    approver_principal: str
    primary_wallet_id: str
    secondary_wallet_id: str
    primary_private_key_env: str
    secondary_private_key_env: str
    primary_agentvault_key_env: str
    secondary_agentvault_key_env: str
    mqtt_username: str
    approver_mqtt_username: str

    @property
    def wallet_ids(self) -> tuple[str, str]:
        return self.primary_wallet_id, self.secondary_wallet_id


def _identity(language: str) -> PayerIdentity:
    upper = language.upper()
    prefix = f"quantstream-{language}"
    return PayerIdentity(
        language=language,
        client_id=f"client-{prefix}",
        scenario_id=f"scenario-{prefix}-market-ingestion",
        policy_id=f"policy-{prefix}-market-data",
        approver_principal=f"principal-{prefix}-admin",
        primary_wallet_id=f"wallet-{prefix}-primary",
        secondary_wallet_id=f"wallet-{prefix}-secondary",
        primary_private_key_env=f"E2E_QUANTSTREAM_{upper}_PRIMARY_PAYER_PRIVATE_KEY",
        secondary_private_key_env=f"E2E_QUANTSTREAM_{upper}_SECONDARY_PAYER_PRIVATE_KEY",
        primary_agentvault_key_env=f"AGENTVAULT_QUANTSTREAM_{upper}_PRIMARY_WALLET_PRIVATE_KEY",
        secondary_agentvault_key_env=f"AGENTVAULT_QUANTSTREAM_{upper}_SECONDARY_WALLET_PRIVATE_KEY",
        mqtt_username=f"quantstream-{language}-workload",
        approver_mqtt_username=f"quantstream-{language}-approver",
    )


PAYER_IDENTITIES = {language: _identity(language) for language in LANGUAGES}


def payer_identity(language: str) -> PayerIdentity:
    try:
        return PAYER_IDENTITIES[language]
    except KeyError as error:
        raise ValueError(f"unsupported payer language: {language}") from error
