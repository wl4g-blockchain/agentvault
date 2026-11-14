from __future__ import annotations

import json
import re
import time

from verifier.context import PAYER_IDENTITIES, FullFlowEvidence, VerificationContext
from verifier.infra.s00_infrastructure import (
    ANSI_ESCAPE_PATTERN,
    EVM_ADDRESS_PATTERN,
    PAYEE_ADDRESS,
    PAYMENT_NAMESPACE,
    TOKEN_CONTRACT,
    UINT_OUTPUT_PATTERN,
)


class E2ES23BalanceSyncVerifier:
    class Operations:
        def _wallet_addresses(self, payer: str, payee: str) -> dict[str, str]:
            if self._wallet_addresses_cache is not None:
                return self._wallet_addresses_cache
            wallet_keys: list[tuple[str, str]] = []
            for identity in PAYER_IDENTITIES.values():
                wallet_keys.extend(
                    (
                        (
                            identity.primary_wallet_id,
                            self.environment[identity.primary_private_key_env],
                        ),
                        (
                            identity.secondary_wallet_id,
                            self.environment[identity.secondary_private_key_env],
                        ),
                    )
                )
            command = (
                'for key in "$@"; do cast wallet address --private-key "$key"; done'
            )
            raw = self._full(
                payer,
                payee,
                "exec",
                "-T",
                "anvil",
                "sh",
                "-eu",
                "-c",
                command,
                "wallet-addresses",
                *(f"0x{key.removeprefix('0x')}" for _, key in wallet_keys),
            )
            addresses = EVM_ADDRESS_PATTERN.findall(ANSI_ESCAPE_PATTERN.sub("", raw))
            if len(addresses) != len(wallet_keys) or len(
                {address.lower() for address in addresses}
            ) != len(wallet_keys):
                raise RuntimeError(
                    "the ten Payer wallet keys did not resolve to ten distinct EVM addresses"
                )
            self._wallet_addresses_cache = {
                wallet_id: address
                for (wallet_id, _), address in zip(wallet_keys, addresses, strict=True)
            }
            return self._wallet_addresses_cache

        def _token_balances(self, payer: str, payee: str) -> dict[str, int]:
            addresses = {**self._wallet_addresses(payer, payee), "payee": PAYEE_ADDRESS}
            command = (
                'token="$1"; shift; '
                'for address in "$@"; do '
                'cast call --rpc-url http://127.0.0.1:8545 "$token" '
                "'balanceOf(address)(uint256)' \"$address\"; "
                "done"
            )
            raw = self._full(
                payer,
                payee,
                "exec",
                "-T",
                "anvil",
                "sh",
                "-eu",
                "-c",
                command,
                "wallet-balances",
                TOKEN_CONTRACT,
                *addresses.values(),
            )
            values = [
                int(value)
                for value in UINT_OUTPUT_PATTERN.findall(
                    ANSI_ESCAPE_PATTERN.sub("", raw)
                )
            ]
            if len(values) != len(addresses):
                raise RuntimeError(
                    f"expected {len(addresses)} token balances, received {len(values)}: {raw[-2048:]}"
                )
            return dict(zip(addresses, values, strict=True))

        def _wait_balance_snapshots(
            self, payer: str, payee: str, expected: dict[str, int]
        ) -> str:
            query = (
                "SELECT wallet_id,asset_available FROM pay_wallet_balance WHERE network='eip155:31337' "
                "AND asset='USDC' AND lower(token_contract)='"
                + TOKEN_CONTRACT
                + "' ORDER BY wallet_id;"
            )
            last = ""
            expected_rows = {
                wallet_id: expected[wallet_id]
                for identity in PAYER_IDENTITIES.values()
                for wallet_id in identity.wallet_ids
            }
            for _ in range(30):
                last = self._sql(payer, payee, query)
                rows = {}
                for line in last.splitlines():
                    if "|" in line:
                        wallet, amount = line.split("|", 1)
                        rows[wallet] = int(amount)
                if all(
                    rows.get(wallet) == amount
                    for wallet, amount in expected_rows.items()
                ):
                    return f"periodic SQL balance snapshots converged: {json.dumps(rows, sort_keys=True)}"
                time.sleep(1)
            raise RuntimeError(
                f"periodic wallet balance snapshots did not converge: {last}"
            )

    class KubernetesOperations:
        def _wallet_addresses(self, payer: str, payee: str) -> dict[str, str]:
            del payer, payee
            if self._wallet_addresses_cache is not None:
                return self._wallet_addresses_cache
            wallet_keys = [
                (wallet_id, self.environment[private_key_env])
                for identity in PAYER_IDENTITIES.values()
                for wallet_id, private_key_env in (
                    (identity.primary_wallet_id, identity.primary_private_key_env),
                    (identity.secondary_wallet_id, identity.secondary_private_key_env),
                )
            ]
            output = self._kubectl(
                "exec",
                "--namespace",
                PAYMENT_NAMESPACE,
                "deployment/anvil",
                "--",
                "sh",
                "-eu",
                "-c",
                'for key in "$@"; do cast wallet address --private-key "$key"; done',
                "wallet-addresses",
                *(f"0x{key.removeprefix('0x')}" for _, key in wallet_keys),
            )
            addresses = re.findall(r"(?im)^\s*(0x[0-9a-f]{40})\s*$", output)
            if len(addresses) != len(wallet_keys) or len(
                {item.lower() for item in addresses}
            ) != len(wallet_keys):
                raise RuntimeError(
                    "the ten Kubernetes payer keys did not resolve to distinct EVM addresses"
                )
            self._wallet_addresses_cache = {
                wallet_id: address
                for (wallet_id, _), address in zip(wallet_keys, addresses, strict=True)
            }
            return self._wallet_addresses_cache

        def _token_balances(self, payer: str, payee: str) -> dict[str, int]:
            addresses = {**self._wallet_addresses(payer, payee), "payee": PAYEE_ADDRESS}
            output = self._kubectl(
                "exec",
                "--namespace",
                PAYMENT_NAMESPACE,
                "deployment/anvil",
                "--",
                "sh",
                "-eu",
                "-c",
                (
                    'token="$1"; shift; for address in "$@"; do '
                    'cast call --rpc-url http://127.0.0.1:8545 "$token" '
                    "'balanceOf(address)(uint256)' \"$address\"; done"
                ),
                "wallet-balances",
                TOKEN_CONTRACT,
                *addresses.values(),
            )
            values = [
                int(value)
                for value in re.findall(
                    r"(?m)^\s*([0-9]+)(?:\s+\[[^\n]*\])?\s*$", output
                )
            ]
            if len(values) != len(addresses):
                raise RuntimeError(
                    f"expected {len(addresses)} Kubernetes token balances, got {len(values)}"
                )
            return dict(zip(addresses, values, strict=True))

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        """No separate deployment is required by this evidence-only phase."""

    @staticmethod
    def init(context: VerificationContext) -> None:
        """Initialization is optional for this scenario."""

    @staticmethod
    def verify_evidence(evidence: FullFlowEvidence, database_evidence: str) -> str:
        vault_logs = evidence.logs.get("agentvault", "")
        for event in (
            "agentvault.wallet.balance_synced",
            "agentvault.wallet.balance_sync_completed",
        ):
            if event not in vault_logs:
                raise RuntimeError(f"periodic balance synchronization omitted {event}")
        if "periodic SQL balance snapshots converged" not in database_evidence:
            raise RuntimeError(
                "periodic balance snapshots did not converge to chain state"
            )
        return (
            "verified AgentVault-owned periodic chain observation -> SQL snapshot flow; "
            "Payer application evidence contains no chain operation; "
            + database_evidence
        )

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return E2ES23BalanceSyncVerifier.verify_evidence(
            context.require("purchase")["evidence"],
            context.require("purchase")["balance_sync"],
        )
