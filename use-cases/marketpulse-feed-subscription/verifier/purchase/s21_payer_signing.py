from __future__ import annotations

import re

from verifier.context import (
    PAYER_IDENTITIES,
    FullFlowEvidence,
    VerificationContext,
    payer_identity,
)

PAYER_EVENTS = (
    "agentvault.sdk.payer.payment_required",
    "agentvault.sdk.payer.signing_requested",
    "agentvault.sdk.payer.signed",
    "agentvault.sdk.payer.retrying",
)


import json
import time
from typing import Any

from verifier.infra.s00_infrastructure import E2ES00InfrastructureVerifier


class E2ES21PayerSigningVerifier:
    class Operations:
        def _wait_and_approve(self, payer: str, payee: str, container: str) -> str:
            token = self.environment.get(
                "E2E_QUANTSTREAM_UI_TOKEN",
                "quantstream-e2e-ui-token-0000000000000000001",
            )
            endpoint = self._url("E2E_QUANTSTREAM_UI_PORT", "/api/approvals")
            approval: dict[str, Any] | None = None
            last_status = 0
            last_raw = ""
            stopped_evidence: str | None = None
            for _ in range(45):
                status, _, raw = self._http(
                    endpoint, headers={"authorization": f"Bearer {token}"}
                )
                last_status, last_raw = status, raw
                if status == 200:
                    items = json.loads(raw).get("items", [])
                    if len(items) == 1:
                        approval = items[0]
                        break
                stopped_evidence = self._stopped_container_evidence(container)
                if stopped_evidence is not None:
                    break
                time.sleep(1)
            if approval is None or approval.get("requestedAmount") != "1000000":
                pending = self._sql(
                    payer,
                    payee,
                    "SELECT count(*),coalesce(string_agg(id || ':' || status, ',' ORDER BY id),'') "
                    "FROM pay_approval WHERE status IN ('pending','approved');",
                )
                raise RuntimeError(
                    "Admin UI did not expose the expected pending approval; "
                    f"status={last_status} response={last_raw[:2048]!r} database={pending!r} "
                    f"ingestor={stopped_evidence or 'still running'}"
                )
            approval_id = str(approval.get("approvalId", ""))
            intent_id = str(approval.get("intentId", ""))
            if not E2ES00InfrastructureVerifier.EnvironmentOperations._safe_id(
                approval_id
            ) or not E2ES00InfrastructureVerifier.EnvironmentOperations._safe_id(
                intent_id
            ):
                raise RuntimeError("Admin UI returned unsafe approval evidence")
            status, _, raw = self._http(
                f"{endpoint}/{approval_id}/decision",
                "POST",
                b'{"decision":"approve"}',
                {
                    "authorization": f"Bearer {token}",
                    "content-type": "application/json",
                },
            )
            if status not in {200, 201}:
                raise RuntimeError(
                    f"Admin UI approval failed with HTTP {status}: {raw[:1024]}"
                )
            exit_code = self._run([*self.docker, "wait", container], 180).strip()
            logs = self._run([*self.docker, "logs", container], 30)
            if exit_code != "0":
                raise RuntimeError(
                    f"approved ingestor exited with code {exit_code}\n{logs}"
                )
            self._run([*self.docker, "rm", container], 30)
            return f"approval {approval_id} for intent {intent_id} was consumed through the business Admin UI\n{logs}"

        def _stopped_container_evidence(self, container: str) -> str | None:
            status, raw_state = self._try(
                [
                    *self.docker,
                    "inspect",
                    "--format",
                    "{{.State.Status}}|{{.State.ExitCode}}",
                    container,
                ],
                10,
            )
            if status != 0:
                return None
            state = raw_state.strip()
            lifecycle = state.partition("|")[0]
            if lifecycle not in {"dead", "exited"}:
                return None
            logs_status, logs = self._try([*self.docker, "logs", container], 30)
            return f"state={state!r} logs_status={logs_status} logs={logs[-4096:]!r}"

    class KubernetesOperations:
        def _wait_and_approve_job(self, payer: str, payee: str, job: str) -> str:
            token = self.environment.get(
                "E2E_QUANTSTREAM_UI_TOKEN",
                "quantstream-e2e-ui-token-0000000000000000001",
            )
            endpoint = self._url("E2E_QUANTSTREAM_UI_PORT", "/api/approvals")
            approval: dict[str, Any] | None = None
            last_status = 0
            last_raw = ""
            stopped_evidence: str | None = None
            for _ in range(60):
                status, _, raw = self._http(
                    endpoint, headers={"authorization": f"Bearer {token}"}
                )
                last_status, last_raw = status, raw
                if status == 200:
                    items = json.loads(raw).get("items", [])
                    if len(items) == 1:
                        approval = items[0]
                        break
                stopped_evidence = self._job_terminal_evidence(job)
                if stopped_evidence is not None:
                    break
                time.sleep(1)
            if approval is None or approval.get("requestedAmount") != "1000000":
                pending = self._sql(
                    payer,
                    payee,
                    "SELECT count(*),coalesce(string_agg(id || ':' || status, ',' ORDER BY id),'') "
                    "FROM pay_approval WHERE status IN ('pending','approved');",
                )
                raise RuntimeError(
                    "Kubernetes Admin UI did not expose the expected approval; "
                    f"status={last_status} response={last_raw[:2048]!r} database={pending!r} "
                    f"payer_job={stopped_evidence or 'still running'}"
                )
            approval_id = str(approval.get("approvalId", ""))
            intent_id = str(approval.get("intentId", ""))
            if not re.fullmatch(
                r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}", approval_id
            ) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}", intent_id):
                raise RuntimeError(
                    "Kubernetes Admin UI returned unsafe approval identifiers"
                )
            status, _, raw = self._http(
                f"{endpoint}/{approval_id}/decision",
                "POST",
                b'{"decision":"approve"}',
                {
                    "authorization": f"Bearer {token}",
                    "content-type": "application/json",
                },
            )
            if status not in {200, 201}:
                raise RuntimeError(
                    f"Kubernetes Admin UI approval failed with HTTP {status}: {raw[:1024]}"
                )
            logs = self._wait_job(job)
            return f"approval {approval_id} for intent {intent_id} was consumed through the Kubernetes Admin UI\n{logs}"

    @staticmethod
    def _event_lines(logs: str, event: str) -> list[str]:
        return [line for line in logs.splitlines() if event in line]

    @staticmethod
    def _bound_line(logs: str, event: str, *identities: str) -> str:
        matches = [
            line
            for line in E2ES21PayerSigningVerifier._event_lines(logs, event)
            if all(value in line for value in identities)
        ]
        if not matches:
            raise RuntimeError(f"{event} is not bound to {', '.join(identities)}")
        return matches[0]

    @staticmethod
    def _position(logs: str, event: str, identity: str) -> int:
        for position, line in enumerate(logs.splitlines()):
            if event in line and identity in line:
                return position
        raise RuntimeError(f"{event} is not bound to {identity}")

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        """No separate deployment is required by this evidence-only phase."""

    @staticmethod
    def init(context: VerificationContext) -> None:
        """Initialization is optional for this scenario."""

    @staticmethod
    def verify_evidence(evidence: FullFlowEvidence) -> str:
        identity = payer_identity(evidence.payer_language)
        payer_logs = evidence.logs.get("payer", "")
        vault_logs = evidence.logs.get("agentvault", "")
        missing = [
            event
            for event in ("agentvault.sdk.payer.transport_selected", *PAYER_EVENTS)
            if event not in payer_logs
        ]
        transport_event = f"agentvault.transport.{evidence.transport}.request"
        for event in (
            transport_event,
            "agentvault.wallet.selected",
            "agentvault.wallet.signed",
        ):
            if event not in vault_logs:
                missing.append(event)
        if missing:
            raise RuntimeError(
                "payer/signing evidence omitted events: " + ", ".join(missing)
            )
        outbound = evidence.audit.get("outbound_intent_ids", [])
        if not isinstance(outbound, list) or len(outbound) != 2:
            raise RuntimeError(
                "payer evidence must contain two settled outbound intents"
            )
        run_ids = evidence.audit.get("run_ids", [])
        if not isinstance(run_ids, list) or len(run_ids) != 2 or len(set(run_ids)) != 2:
            raise RuntimeError(
                "payer evidence must contain two distinct workflow run IDs"
            )
        if (
            len(E2ES21PayerSigningVerifier._event_lines(vault_logs, transport_event))
            < 4
        ):
            raise RuntimeError(
                f"{transport_event} did not carry authorize and report calls for both runs"
            )
        producer_event = f"agentvault.sdk.transport.{evidence.transport}.produced"
        produced = E2ES21PayerSigningVerifier._event_lines(payer_logs, producer_event)
        span_ids = {
            match.group(1)
            for line in produced
            if (
                match := re.search(
                    r'(?:span[._]?id|spanId)["\s:=]+([0-9a-f]{16})', line
                )
            )
        }
        if len(span_ids) < 4 or any(span_id not in vault_logs for span_id in span_ids):
            raise RuntimeError(
                f"{evidence.transport} producer spans are not propagated one-to-one into AgentVault consumer logs"
            )
        for run_id, intent_id in zip(run_ids, outbound, strict=True):
            positions = [
                E2ES21PayerSigningVerifier._position(payer_logs, event, run_id)
                for event in PAYER_EVENTS
            ]
            if positions != sorted(positions):
                raise RuntimeError(
                    f"payer middleware events are out of order for run {run_id}: {positions}"
                )
            E2ES21PayerSigningVerifier._bound_line(
                payer_logs, "agentvault.sdk.payer.signed", run_id, intent_id
            )
            E2ES21PayerSigningVerifier._bound_line(
                payer_logs, "agentvault.sdk.payer.retrying", run_id, intent_id
            )
            selected = E2ES21PayerSigningVerifier._bound_line(
                vault_logs, "agentvault.wallet.selected", run_id
            )
            signed = E2ES21PayerSigningVerifier._bound_line(
                vault_logs, "agentvault.wallet.signed", run_id, intent_id
            )
            if vault_logs.index(selected) > vault_logs.index(signed):
                raise RuntimeError(
                    f"wallet signing preceded scenario selection for run {run_id}"
                )
        for wallet in identity.wallet_ids:
            if not any(
                wallet in line
                for event in ("agentvault.wallet.selected", "agentvault.wallet.signed")
                for line in E2ES21PayerSigningVerifier._event_lines(vault_logs, event)
            ):
                raise RuntimeError(f"wallet-pool signing evidence omitted {wallet}")
        foreign_wallets = {
            wallet_id
            for candidate in PAYER_IDENTITIES.values()
            if candidate.language != identity.language
            for wallet_id in candidate.wallet_ids
        }
        leaked = sorted(
            wallet_id
            for wallet_id in foreign_wallets
            if any(
                wallet_id in line
                for event in ("agentvault.wallet.selected", "agentvault.wallet.signed")
                for line in E2ES21PayerSigningVerifier._event_lines(vault_logs, event)
            )
        )
        if leaked:
            raise RuntimeError(f"payer signing crossed language wallet pools: {leaked}")
        return (
            f"verified {evidence.payer_language} PayerMiddleware over {evidence.transport}: "
            "each run is bound across 402 interception -> signing request -> scenario wallet-pool selection -> "
            "EOA signature -> paid retry -> response report; "
            f"runs={','.join(run_ids)} outbound_intents={','.join(outbound)} "
            f"wallets={','.join(identity.wallet_ids)}"
        )

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return E2ES21PayerSigningVerifier.verify_evidence(
            context.require("purchase")["evidence"]
        )
