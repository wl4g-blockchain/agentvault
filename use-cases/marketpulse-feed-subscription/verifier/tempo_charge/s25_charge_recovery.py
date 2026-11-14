from __future__ import annotations

import json
import re
from contextlib import ExitStack

from util.constant import E2EOtherDefinitions as Project

from verifier.context import VerificationContext, create_deployer

EVIDENCE_PATTERN = re.compile(r"MPP_TEMPO_EVIDENCE=(\{[^\n]+\})")


class E2ES25ChargeRecoveryVerifier:
    class MppTempoDockerLifecycle:
        def prepare_mpp_tempo(self):
            self.cleanup()
            self._compose(("tempo",), "up", "-d", "tempo")
            rpc_url = self._url("E2E_TEMPO_RPC_PORT")
            self._wait_rpc(rpc_url, "eth_chainId", "0x539", 90)
            self.environment["TEMPO_RPC_URL"] = rpc_url
            self.environment.setdefault("CARGO_INCREMENTAL", "0")
            self.environment.setdefault("CARGO_PROFILE_TEST_DEBUG", "0")
            self._target_budget()

        def verify_mpp_tempo(self) -> str:
            return self._run(
                [
                    "cargo",
                    "test",
                    "--locked",
                    "protocol::mpp::tests::live_tempo_vault_handlers_recover_once_and_reconcile_both_ledgers",
                    "--",
                    "--ignored",
                    "--exact",
                    "--nocapture",
                ],
                self.context.timeout_seconds,
                Project.REPOSITORY_ROOT,
            )

        def mpp_tempo(self):
            self.prepare_mpp_tempo()
            self._wait_tempo_faucet_window(self.environment["TEMPO_RPC_URL"])
            return self.verify_mpp_tempo()

    class MppTempoKubernetesLifecycle:
        def prepare_mpp_tempo(self):
            self.cleanup()
            self._apply_namespaces()
            self._helm_install(
                "tempo", self._component_chart("tempo"), self.payment_namespace
            )
            session = self.context.state.setdefault("session", ExitStack())
            session.enter_context(self._runtime_forwards(("tempo",)))
            rpc_url = self._url("E2E_TEMPO_RPC_PORT")
            self._wait_rpc(rpc_url, "eth_chainId", "0x539", 90)
            self.environment["TEMPO_RPC_URL"] = rpc_url

        def verify_mpp_tempo(self) -> str:
            return self._run_live_rust_test(
                "protocol::mpp::tests::live_tempo_vault_handlers_recover_once_and_reconcile_both_ledgers"
            )

        def mpp_tempo(self):
            self.prepare_mpp_tempo()
            self._wait_tempo_faucet_window(self.environment["TEMPO_RPC_URL"])
            return self.verify_mpp_tempo()

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        create_deployer(context).prepare_mpp_tempo()

    @staticmethod
    def init(context: VerificationContext) -> None:
        runtime = create_deployer(context)
        runtime._wait_tempo_faucet_window(runtime.environment["TEMPO_RPC_URL"])

    @staticmethod
    def verify(context: VerificationContext) -> str:
        output = create_deployer(context).verify_mpp_tempo()
        match = EVIDENCE_PATTERN.search(output)
        if match is None:
            raise RuntimeError("MPP Tempo test omitted its machine-readable evidence")
        try:
            evidence = json.loads(match.group(1))
        except json.JSONDecodeError as error:
            raise RuntimeError(
                "MPP Tempo test emitted invalid evidence JSON"
            ) from error

        required = {
            "run_id",
            "protocol",
            "method",
            "chain_id",
            "transaction",
            "amount",
            "gas_fee",
            "gas_limit",
            "gas_used",
            "payer_before",
            "payer_after",
            "recipient_before",
            "recipient_after",
            "broadcasts",
            "recovered_after_lost_response",
            "vault_handler_roundtrip",
            "encrypted_custody_signing",
            "outbound_receipt_reconciled",
            "payer_ledger_entries",
            "payee_ledger_entries",
            "settlement_requested_events",
        }
        if not required.issubset(evidence):
            raise RuntimeError(
                f"MPP Tempo evidence is incomplete: {sorted(required - evidence.keys())}"
            )
        if (
            evidence["protocol"] != "mpp"
            or evidence["method"] != "tempo"
            or evidence["chain_id"] != 1337
            or evidence["broadcasts"] != 1
            or evidence["recovered_after_lost_response"] is not True
            or evidence["vault_handler_roundtrip"] is not True
            or evidence["encrypted_custody_signing"] is not True
            or evidence["outbound_receipt_reconciled"] is not True
            or evidence["payer_ledger_entries"] != 1
            or evidence["payee_ledger_entries"] != 1
            or evidence["settlement_requested_events"] != 1
            or not re.fullmatch(r"0x[0-9a-f]{64}", str(evidence["transaction"]))
        ):
            raise RuntimeError(
                f"MPP Tempo protocol or recovery evidence is invalid: {evidence}"
            )

        amount = int(evidence["amount"])
        gas_fee = int(evidence["gas_fee"])
        gas_limit = int(evidence["gas_limit"])
        gas_used = int(evidence["gas_used"])
        payer_before = int(evidence["payer_before"])
        payer_after = int(evidence["payer_after"])
        recipient_before = int(evidence["recipient_before"])
        recipient_after = int(evidence["recipient_after"])
        if amount <= 0 or gas_fee <= 0:
            raise RuntimeError("MPP Tempo amount and gas fee must both be positive")
        if gas_used <= 0 or gas_used > gas_limit:
            raise RuntimeError(
                "MPP Tempo confirmed gas usage exceeds its signed gas cap"
            )
        if recipient_after - recipient_before != amount:
            raise RuntimeError("MPP Tempo recipient balance does not match the charge")
        if payer_before - payer_after != amount + gas_fee:
            raise RuntimeError(
                "MPP Tempo payer balance does not include the exact charge and gas"
            )
        if payer_before + recipient_before != payer_after + recipient_after + gas_fee:
            raise RuntimeError("MPP Tempo aggregate token accounting is not conserved")

        return (
            "real MPP/Tempo charge passed: encrypted Vault custody -> type-0x76 credential -> "
            "HTTP verify -> one on-chain settle -> lost-response verify recovery -> independent RPC "
            "reconciliation -> payer/payee ledger; "
            f"transaction={evidence['transaction']} amount={amount} gas_fee={gas_fee} "
            f"gas_used={gas_used}/{gas_limit} broadcasts=1; "
            f"payer={payer_before}->{payer_after} recipient={recipient_before}->{recipient_after} "
            f"conserved={payer_before + recipient_before}="
            f"{payer_after + recipient_after}+{gas_fee}"
        )
