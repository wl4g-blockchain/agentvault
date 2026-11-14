from __future__ import annotations

import json
import re
from contextlib import ExitStack

from util.constant import E2EOtherDefinitions as Project

from verifier.context import VerificationContext, create_deployer

EVIDENCE_PATTERN = re.compile(r"X402_SOLANA_EVIDENCE=(\{[^\n]+\})")
BASE58_PATTERN = re.compile(r"[1-9A-HJ-NP-Za-km-z]+")


class E2ES26ExactSettlementVerifier:
    class X402SolanaDockerLifecycle:
        def prepare_x402_solana(self):
            self.cleanup()
            self._compose(("payer",), "up", "-d", "solana")
            rpc_url = self._url("E2E_SOLANA_RPC_PORT")
            self._wait_rpc(rpc_url, "getHealth", "ok", 120)

        def init_x402_solana(self):
            rpc_url = self._url("E2E_SOLANA_RPC_PORT")
            keypairs = {
                role: f"/tmp/agentvault-x402-{role}.json"
                for role in ("payer", "recipient", "facilitator", "mint")
            }
            for path in keypairs.values():
                self._solana_command(
                    "solana-keygen",
                    "new",
                    "--silent",
                    "--no-bip39-passphrase",
                    "--force",
                    "--outfile",
                    path,
                )
            payer = self._solana_pubkey(keypairs["payer"])
            recipient = self._solana_pubkey(keypairs["recipient"])
            facilitator = self._solana_pubkey(keypairs["facilitator"])
            mint = self._solana_pubkey(keypairs["mint"])
            container_rpc = "http://127.0.0.1:8899"
            self._solana_command(
                "solana",
                "airdrop",
                "10",
                facilitator,
                "--url",
                container_rpc,
                timeout=120,
            )
            created = self._solana_json(
                "spl-token",
                "create-token",
                "--url",
                container_rpc,
                "--fee-payer",
                keypairs["facilitator"],
                "--mint-authority",
                payer,
                "--decimals",
                "6",
                "--output",
                "json-compact",
                keypairs["mint"],
                timeout=120,
            )
            command_output = created.get("commandOutput")
            if (
                not isinstance(command_output, dict)
                or command_output.get("address") != mint
            ):
                raise RuntimeError("SPL token fixture created an unexpected mint")
            for owner in (payer, recipient):
                self._solana_json(
                    "spl-token",
                    "create-account",
                    "--url",
                    container_rpc,
                    "--fee-payer",
                    keypairs["facilitator"],
                    "--owner",
                    owner,
                    "--output",
                    "json-compact",
                    mint,
                    timeout=120,
                )
            payer_account_wire = self._solana_json(
                "spl-token",
                "address",
                "--url",
                container_rpc,
                "--verbose",
                "--output",
                "json-compact",
                "--token",
                mint,
                "--owner",
                payer,
            )
            recipient_account_wire = self._solana_json(
                "spl-token",
                "address",
                "--url",
                container_rpc,
                "--verbose",
                "--output",
                "json-compact",
                "--token",
                mint,
                "--owner",
                recipient,
            )
            payer_account = payer_account_wire.get("associatedTokenAddress")
            recipient_account = recipient_account_wire.get("associatedTokenAddress")
            if not all(
                (
                    isinstance(value, str)
                    and re.fullmatch("[1-9A-HJ-NP-Za-km-z]{32,44}", value)
                    for value in (payer_account, recipient_account)
                )
            ):
                raise RuntimeError(
                    "SPL token fixture emitted an invalid associated token account"
                )
            self._solana_json(
                "spl-token",
                "mint",
                "--url",
                container_rpc,
                "--fee-payer",
                keypairs["facilitator"],
                "--mint-authority",
                keypairs["payer"],
                "--output",
                "json-compact",
                mint,
                "1000",
                payer_account,
                timeout=120,
            )
            self._wait_solana_token_balance(payer_account, 1000000000)
            self._wait_solana_token_balance(recipient_account, 0)
            genesis = self._solana_rpc_result("getGenesisHash", [])
            if not isinstance(genesis, str) or not re.fullmatch(
                "[1-9A-HJ-NP-Za-km-z]{32,88}", genesis
            ):
                raise RuntimeError("Solana validator emitted an invalid genesis hash")
            self.environment.update(
                {
                    "SOLANA_RPC_URL": rpc_url,
                    "SOLANA_CHAIN_REFERENCE": genesis[:32],
                    "SOLANA_PAYER_KEYPAIR_JSON": self._solana_keypair_json(
                        keypairs["payer"]
                    ),
                    "SOLANA_FACILITATOR_KEYPAIR_JSON": self._solana_keypair_json(
                        keypairs["facilitator"]
                    ),
                    "SOLANA_PAYER_TOKEN_ACCOUNT": payer_account,
                    "SOLANA_RECIPIENT_TOKEN_ACCOUNT": recipient_account,
                    "SOLANA_RECIPIENT_ADDRESS": recipient,
                    "SOLANA_TOKEN_MINT": mint,
                }
            )
            self.environment.setdefault("CARGO_INCREMENTAL", "0")
            self.environment.setdefault("CARGO_PROFILE_TEST_DEBUG", "0")
            self._target_budget()

        def verify_x402_solana(self) -> str:
            return self._run(
                [
                    "cargo",
                    "test",
                    "--package",
                    "agentvault-protocol",
                    "--lib",
                    "--locked",
                    "x402::tests::live_solana_x402_vault_handlers_reconcile_both_ledgers",
                    "--",
                    "--ignored",
                    "--exact",
                    "--nocapture",
                ],
                self.context.timeout_seconds,
                Project.REPOSITORY_ROOT,
            )

        def x402_solana(self):
            self.prepare_x402_solana()
            self.init_x402_solana()
            return self.verify_x402_solana()

    class X402SolanaKubernetesLifecycle:
        def prepare_x402_solana(self):
            self.cleanup()
            self._apply_namespaces()
            self._helm_install(
                "solana", self._component_chart("solana"), self.payment_namespace
            )
            session = self.context.state.setdefault("session", ExitStack())
            session.enter_context(self._runtime_forwards(("solana",)))
            rpc_url = self._url("E2E_SOLANA_RPC_PORT")
            self._wait_rpc(rpc_url, "getHealth", "ok", 120)

        def init_x402_solana(self):
            rpc_url = self._url("E2E_SOLANA_RPC_PORT")
            keypairs = {
                role: f"/tmp/agentvault-x402-{role}.json"
                for role in ("payer", "recipient", "facilitator", "mint")
            }
            for path in keypairs.values():
                self._solana_command(
                    "solana-keygen",
                    "new",
                    "--silent",
                    "--no-bip39-passphrase",
                    "--force",
                    "--outfile",
                    path,
                )
            payer = self._solana_pubkey(keypairs["payer"])
            recipient = self._solana_pubkey(keypairs["recipient"])
            facilitator = self._solana_pubkey(keypairs["facilitator"])
            mint = self._solana_pubkey(keypairs["mint"])
            container_rpc = "http://127.0.0.1:8899"
            self._solana_command(
                "solana",
                "airdrop",
                "10",
                facilitator,
                "--url",
                container_rpc,
                timeout=120,
            )
            created = self._solana_json(
                "spl-token",
                "create-token",
                "--url",
                container_rpc,
                "--fee-payer",
                keypairs["facilitator"],
                "--mint-authority",
                payer,
                "--decimals",
                "6",
                "--output",
                "json-compact",
                keypairs["mint"],
                timeout=120,
            )
            command_output = created.get("commandOutput")
            if (
                not isinstance(command_output, dict)
                or command_output.get("address") != mint
            ):
                raise RuntimeError("SPL token fixture created an unexpected mint")
            for owner in (payer, recipient):
                self._solana_json(
                    "spl-token",
                    "create-account",
                    "--url",
                    container_rpc,
                    "--fee-payer",
                    keypairs["facilitator"],
                    "--owner",
                    owner,
                    "--output",
                    "json-compact",
                    mint,
                    timeout=120,
                )
            payer_account_wire = self._solana_json(
                "spl-token",
                "address",
                "--url",
                container_rpc,
                "--verbose",
                "--output",
                "json-compact",
                "--token",
                mint,
                "--owner",
                payer,
            )
            recipient_account_wire = self._solana_json(
                "spl-token",
                "address",
                "--url",
                container_rpc,
                "--verbose",
                "--output",
                "json-compact",
                "--token",
                mint,
                "--owner",
                recipient,
            )
            payer_account = payer_account_wire.get("associatedTokenAddress")
            recipient_account = recipient_account_wire.get("associatedTokenAddress")
            if not all(
                (
                    isinstance(value, str)
                    and re.fullmatch("[1-9A-HJ-NP-Za-km-z]{32,44}", value)
                    for value in (payer_account, recipient_account)
                )
            ):
                raise RuntimeError(
                    "SPL token fixture emitted an invalid associated token account"
                )
            self._solana_json(
                "spl-token",
                "mint",
                "--url",
                container_rpc,
                "--fee-payer",
                keypairs["facilitator"],
                "--mint-authority",
                keypairs["payer"],
                "--output",
                "json-compact",
                mint,
                "1000",
                payer_account,
                timeout=120,
            )
            self._wait_solana_token_balance(payer_account, 1000000000)
            self._wait_solana_token_balance(recipient_account, 0)
            genesis = self._solana_rpc_result("getGenesisHash", [])
            if not isinstance(genesis, str) or not re.fullmatch(
                "[1-9A-HJ-NP-Za-km-z]{32,88}", genesis
            ):
                raise RuntimeError("Solana validator emitted an invalid genesis hash")
            self.environment.update(
                {
                    "SOLANA_RPC_URL": rpc_url,
                    "SOLANA_CHAIN_REFERENCE": genesis[:32],
                    "SOLANA_PAYER_KEYPAIR_JSON": self._solana_keypair_json(
                        keypairs["payer"]
                    ),
                    "SOLANA_FACILITATOR_KEYPAIR_JSON": self._solana_keypair_json(
                        keypairs["facilitator"]
                    ),
                    "SOLANA_PAYER_TOKEN_ACCOUNT": payer_account,
                    "SOLANA_RECIPIENT_TOKEN_ACCOUNT": recipient_account,
                    "SOLANA_RECIPIENT_ADDRESS": recipient,
                    "SOLANA_TOKEN_MINT": mint,
                }
            )

        def verify_x402_solana(self) -> str:
            return self._run_live_rust_test(
                "x402::tests::live_solana_x402_vault_handlers_reconcile_both_ledgers"
            )

        def x402_solana(self):
            self.prepare_x402_solana()
            self.init_x402_solana()
            return self.verify_x402_solana()

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        create_deployer(context).prepare_x402_solana()

    @staticmethod
    def init(context: VerificationContext) -> None:
        create_deployer(context).init_x402_solana()

    @staticmethod
    def verify(context: VerificationContext) -> str:
        output = create_deployer(context).verify_x402_solana()
        match = EVIDENCE_PATTERN.search(output)
        if match is None:
            raise RuntimeError("x402 Solana test omitted its machine-readable evidence")
        try:
            evidence = json.loads(match.group(1))
        except json.JSONDecodeError as error:
            raise RuntimeError(
                "x402 Solana test emitted invalid evidence JSON"
            ) from error

        required = {
            "version",
            "scheme",
            "network",
            "transaction",
            "payer",
            "recipient",
            "mint",
            "amount",
            "payer_token_before",
            "payer_token_after",
            "recipient_token_before",
            "recipient_token_after",
            "payer_native_before",
            "payer_native_after",
            "facilitator_native_before",
            "facilitator_native_after",
            "transaction_fee",
            "verify_before_settle",
            "broadcast_count",
            "vault_protocol_adapter",
            "vault_custody_signing",
            "vault_handler_roundtrip",
            "outbound_receipt_reconciled",
            "payer_ledger_entries",
            "payee_ledger_entries",
            "settlement_requested_events",
        }
        if not required.issubset(evidence):
            raise RuntimeError(
                f"x402 Solana evidence is incomplete: {sorted(required - evidence.keys())}"
            )
        network = str(evidence["network"])
        transaction = str(evidence["transaction"])
        identities = (str(evidence[name]) for name in ("payer", "recipient", "mint"))
        if (
            evidence["version"] != 2
            or evidence["scheme"] != "exact"
            or not re.fullmatch(r"solana:[1-9A-HJ-NP-Za-km-z]{32}", network)
            or not BASE58_PATTERN.fullmatch(transaction)
            or not 80 <= len(transaction) <= 90
            or any(
                not BASE58_PATTERN.fullmatch(value) or not 32 <= len(value) <= 44
                for value in identities
            )
            or evidence["verify_before_settle"] is not True
            or evidence["broadcast_count"] != 1
            or evidence["vault_protocol_adapter"] is not True
            or evidence["vault_custody_signing"] is not True
            or evidence["vault_handler_roundtrip"] is not True
            or evidence["outbound_receipt_reconciled"] is not True
            or evidence["payer_ledger_entries"] != 1
            or evidence["payee_ledger_entries"] != 1
            or evidence["settlement_requested_events"] != 1
        ):
            raise RuntimeError(f"x402 Solana protocol evidence is invalid: {evidence}")

        amount = int(evidence["amount"])
        payer_before = int(evidence["payer_token_before"])
        payer_after = int(evidence["payer_token_after"])
        recipient_before = int(evidence["recipient_token_before"])
        recipient_after = int(evidence["recipient_token_after"])
        payer_native_before = int(evidence["payer_native_before"])
        payer_native_after = int(evidence["payer_native_after"])
        facilitator_before = int(evidence["facilitator_native_before"])
        facilitator_after = int(evidence["facilitator_native_after"])
        fee = int(evidence["transaction_fee"])
        if (
            amount <= 0
            or payer_before - payer_after != amount
            or recipient_after - recipient_before != amount
        ):
            raise RuntimeError(
                "x402 Solana SPL token deltas do not match the exact payment"
            )
        if payer_before + recipient_before != payer_after + recipient_after:
            raise RuntimeError(
                "x402 Solana aggregate SPL token accounting is not conserved"
            )
        if payer_native_before != payer_native_after:
            raise RuntimeError(
                "x402 Solana payer was charged native gas despite the facilitator fee-payer contract"
            )
        if fee <= 0 or facilitator_before - facilitator_after != fee:
            raise RuntimeError(
                "x402 Solana facilitator lamport delta does not equal the confirmed transaction fee"
            )

        return (
            "real x402 v2/Solana exact payment passed: Vault custody + official client builder -> official facilitator verify -> "
            "one on-chain settle; "
            f"transaction={transaction} amount={amount} fee={fee} broadcasts=1; "
            "payer/payee_ledgers=1/1 trusted_receipt_reconciled=true; "
            f"payer_token={payer_before}->{payer_after} recipient_token={recipient_before}->{recipient_after} "
            f"token_conserved={payer_before + recipient_before}; "
            f"payer_lamports={payer_native_before}->{payer_native_after} "
            f"facilitator_lamports={facilitator_before}->{facilitator_after}"
        )
