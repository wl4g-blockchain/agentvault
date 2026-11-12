from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from verifier.context import VerificationContext
import base64
import json
import os
import sqlite3

#!/usr/bin/env python3
import sys
from pathlib import Path
from typing import Any

if __package__:
    from util.constant import E2EOtherDefinitions as Project
if __package__:
    from util.images import E2EImagesDefinitions
if __package__:
    from util.process import E2EProcessHelper

if __package__:
    from verifier.infra.s00_infrastructure import (
        BUSINESS_EVIDENCE_LABEL,
        PAYEE_NAMESPACE,
        PAYER_NAMESPACE,
    )
    from verifier.infra.s00_infrastructure import (
        E2ES00InfrastructureVerifier as Infra,
    )


class E2ES32DeliveryRecoveryVerifier:
    class Operations:
        def _audit(self, payer: str, payee: str) -> dict[str, Any]:
            output = self._full(
                payer, payee, "run", "-T", "--rm", "--no-deps", "e2e-auditor"
            )
            records = [line for line in output.splitlines() if line.startswith("{")]
            if not records:
                raise RuntimeError("business evidence auditor returned no JSON")
            value = json.loads(records[-1])
            if (
                len(value.get("outbound_intent_ids", [])) != 2
                or len(value.get("inbound_intent_ids", [])) != 2
            ):
                raise RuntimeError(
                    "auditor did not return two bidirectional payment pairs"
                )
            return value

    class KubernetesOperations:
        def _snapshot_business_database(
            self, role: str, language: str, directory: Path
        ) -> None:
            namespace = PAYER_NAMESPACE if role == "payer" else PAYEE_NAMESPACE
            name = f"snapshot-{role}-{language}"
            filename = "ingestion.sqlite3" if role == "payer" else "marketpulse.sqlite3"
            script = (
                "import base64,json,sqlite3,sys; from pathlib import Path; "
                "source=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True); "
                "target=sqlite3.connect(sys.argv[2]); source.backup(target); "
                'mode=target.execute("PRAGMA journal_mode=DELETE").fetchone()[0]; '
                'assert mode=="delete", "snapshot journal must be standalone"; '
                "target.close(); source.close(); "
                "print(json.dumps({'sqlite_snapshot':base64.b64encode(Path(sys.argv[2]).read_bytes()).decode()}))"
            )
            labels = {
                "app.kubernetes.io/part-of": "agentvault-e2e",
                BUSINESS_EVIDENCE_LABEL: "true",
            }
            auditor = self._prepare_deployment_image(
                E2EImagesDefinitions.reference("auditor", self.environment)
            )
            self._apply_document(
                {
                    "apiVersion": "batch/v1",
                    "kind": "Job",
                    "metadata": {
                        "name": name,
                        "namespace": namespace,
                        "labels": labels,
                    },
                    "spec": {
                        "backoffLimit": 0,
                        "activeDeadlineSeconds": 120,
                        "template": {
                            "metadata": {"labels": labels},
                            "spec": {
                                "restartPolicy": "Never",
                                "automountServiceAccountToken": False,
                                "securityContext": {
                                    "runAsNonRoot": True,
                                    "runAsUser": 65532,
                                    "fsGroup": 65532,
                                },
                                "containers": [
                                    {
                                        "name": "snapshot",
                                        "image": auditor,
                                        "imagePullPolicy": "Never",
                                        "command": [
                                            "python3",
                                            "-c",
                                            script,
                                            f"/data/{filename}",
                                            "/tmp/snapshot.sqlite3",
                                        ],
                                        "securityContext": {
                                            "allowPrivilegeEscalation": False,
                                            "readOnlyRootFilesystem": True,
                                            "capabilities": {"drop": ["ALL"]},
                                        },
                                        "resources": {
                                            "requests": {
                                                "cpu": "20m",
                                                "memory": "32Mi",
                                            },
                                            "limits": {
                                                "cpu": "500m",
                                                "memory": "128Mi",
                                            },
                                        },
                                        "volumeMounts": [
                                            {
                                                "name": "evidence",
                                                "mountPath": "/data",
                                                "readOnly": True,
                                            },
                                            {"name": "temporary", "mountPath": "/tmp"},
                                        ],
                                    }
                                ],
                                "volumes": [
                                    {
                                        "name": "evidence",
                                        "persistentVolumeClaim": {
                                            "claimName": self._business_claim(
                                                role, language
                                            ),
                                            "readOnly": True,
                                        },
                                    },
                                    {"name": "temporary", "emptyDir": {}},
                                ],
                            },
                        },
                    },
                }
            )
            self._kubectl(
                "wait",
                "--namespace",
                namespace,
                f"job/{name}",
                "--for=condition=complete",
                "--timeout=180s",
                timeout=190,
            )
            raw = self._kubectl(
                "logs",
                "--namespace",
                namespace,
                f"job/{name}",
                "--container",
                "snapshot",
            )
            encoded = json.loads(raw).get("sqlite_snapshot")
            if not isinstance(encoded, str) or len(encoded) > 16 * 1024 * 1024:
                raise RuntimeError("business evidence snapshot is missing or oversized")
            snapshot = base64.b64decode(encoded, validate=True)
            if not snapshot.startswith(b"SQLite format 3\0"):
                raise RuntimeError("business evidence snapshot is not SQLite")
            component = "quantstream" if role == "payer" else "marketpulse"
            target = directory / f"{component}-{language}" / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(snapshot)

        def _audit(self, payer: str, payee: str) -> dict[str, Any]:
            directory = (
                self.context.report_root
                / "payment"
                / "business-evidence"
                / f"{payer}-{payee}-{self.context.transport}"
            )
            self._snapshot_business_database("payer", payer, directory)
            self._snapshot_business_database("payee", payee, directory)
            environment = {
                **self.environment,
                "E2E_PAYER_LANGUAGE": payer,
                "E2E_PAYEE_LANGUAGE": payee,
                "E2E_EVIDENCE_ROOT": str(directory.resolve()),
            }
            output = E2EProcessHelper.run(
                [
                    sys.executable,
                    str(
                        Project.E2E_ROOT / "verifier/delivery/s32_delivery_recovery.py"
                    ),
                ],
                Project.E2E_ROOT,
                environment,
                30,
            )
            evidence = json.loads(output)
            if (
                len(evidence.get("outbound_intent_ids", [])) != 2
                or len(evidence.get("inbound_intent_ids", [])) != 2
            ):
                raise RuntimeError(
                    "business evidence auditor did not return two bidirectional payment pairs"
                )
            return evidence

    @staticmethod
    def one(connection: sqlite3.Connection, sql: str) -> tuple[object, ...]:
        row = connection.execute(sql).fetchone()
        if row is None:
            raise RuntimeError(f"no evidence for query: {sql}")
        return row

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def _verify_databases():
        languages = {"rust", "go", "python", "typescript", "java"}
        payer_language = os.environ.get("E2E_PAYER_LANGUAGE", "").strip()
        payee_language = os.environ.get("E2E_PAYEE_LANGUAGE", "").strip()
        if payer_language not in languages:
            raise RuntimeError("E2E_PAYER_LANGUAGE must select one supported ingestor")
        if payee_language not in languages:
            raise RuntimeError(
                "E2E_PAYEE_LANGUAGE must select one supported feed service"
            )

        evidence_root = Path(os.environ.get("E2E_EVIDENCE_ROOT", "/evidence"))
        market_path = (
            evidence_root / f"marketpulse-{payee_language}" / "marketpulse.sqlite3"
        )
        quant_path = (
            evidence_root / f"quantstream-{payer_language}" / "ingestion.sqlite3"
        )
        if not market_path.is_file() or not quant_path.is_file():
            raise RuntimeError("durable business databases are missing")
        for database in (market_path, quant_path):
            if Path(f"{database}-wal").exists() or Path(f"{database}-shm").exists():
                raise RuntimeError(
                    "business evidence database is not a quiescent read-only snapshot"
                )

        with sqlite3.connect(f"file:{quant_path}?mode=ro", uri=True) as quant:
            runs = quant.execute(
                "SELECT run_id,free_response_json,paid_response_json,payment_intent_id "
                "FROM ingestion_run ORDER BY completed_at,run_id"
            ).fetchall()
        if len(runs) != 2:
            raise RuntimeError(
                f"expected two completed ingestion runs, found {len(runs)}"
            )

        outbound_intents: list[str] = []
        inbound_intents: list[str] = []
        run_ids: list[str] = []
        for run_id, free_raw, paid_raw, outbound_intent in runs:
            free_response = json.loads(str(free_raw))
            paid_response = json.loads(str(paid_raw))

            # Every language stores exactly the MCP structuredContent business object.
            # Refuse JSON-RPC envelopes here so implementation drift cannot be hidden by
            # an overly permissive verifier.
            if not isinstance(free_response, dict) or set(free_response) != {
                "news",
                "paid",
                "intentId",
            }:
                raise RuntimeError(
                    "free MCP delivery evidence has a non-canonical shape"
                )
            if not isinstance(paid_response, dict) or set(paid_response) != {
                "news",
                "paid",
                "intentId",
            }:
                raise RuntimeError(
                    "paid MCP delivery evidence has a non-canonical shape"
                )
            if (
                free_response.get("paid") is not False
                or not isinstance(free_response.get("news"), list)
                or not free_response["news"]
            ):
                raise RuntimeError("free MCP delivery evidence is invalid")
            if free_response.get("intentId") is not None:
                raise RuntimeError(
                    "free MCP delivery unexpectedly contains a payment intent"
                )
            if (
                paid_response.get("paid") is not True
                or not isinstance(paid_response.get("news"), list)
                or not paid_response["news"]
            ):
                raise RuntimeError("paid MCP delivery evidence is invalid")
            inbound_intent = paid_response.get("intentId")
            if not isinstance(inbound_intent, str) or not inbound_intent:
                raise RuntimeError("paid MCP delivery omitted its inbound intent")
            if not isinstance(outbound_intent, str) or not outbound_intent:
                raise RuntimeError("ingestor omitted its outbound intent")
            run_ids.append(str(run_id))
            outbound_intents.append(outbound_intent)
            inbound_intents.append(inbound_intent)

        with sqlite3.connect(f"file:{market_path}?mode=ro", uri=True) as market:
            deliveries = market.execute(
                "SELECT run_id,intent_id,status,response_json,failure_injections FROM mcp_delivery "
                "WHERE status='delivered' ORDER BY updated_at,run_id"
            ).fetchall()
        if len(deliveries) != 2:
            raise RuntimeError(
                f"expected two durable MarketPulse deliveries, found {len(deliveries)}"
            )
        delivered_by_intent = {str(row[1]): row for row in deliveries}
        for run_index, (run_id, inbound_intent) in enumerate(
            zip(run_ids, inbound_intents, strict=True)
        ):
            delivery = delivered_by_intent.get(inbound_intent)
            if delivery is None:
                raise RuntimeError(
                    "MarketPulse did not durably bind settlement to delivery"
                )
            delivery_run_id, _, status, response_raw, failure_injections = delivery
            if status != "delivered" or delivery_run_id != run_id:
                raise RuntimeError(
                    "MarketPulse delivery run does not match the workflow run"
                )
            replay = json.loads(str(response_raw))
            replay_result = replay.get("result", {}) if isinstance(replay, dict) else {}
            replay_content = (
                replay_result.get("structuredContent", {})
                if isinstance(replay_result, dict)
                else {}
            )
            if replay_content.get("intentId") != inbound_intent:
                raise RuntimeError(
                    "MarketPulse replay body is not bound to the settled intent"
                )
            expected_injections = 1 if run_index == 0 else 0
            if failure_injections != expected_injections:
                raise RuntimeError(
                    "post-settlement recovery evidence is missing or attached to the wrong workflow run"
                )

        print(
            json.dumps(
                {
                    "payer_language": payer_language,
                    "payee_language": payee_language,
                    "run_ids": run_ids,
                    "outbound_intent_ids": outbound_intents,
                    "inbound_intent_ids": inbound_intents,
                    "free_deliveries": 2,
                    "paid_deliveries": 2,
                    "merchant_delivery_status": "delivered",
                    "post_settlement_failure_recoveries": 1,
                },
                separators=(",", ":"),
                sort_keys=True,
            )
        )

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = Infra.session(context)
        audit = runtime._audit(context.payer_language, context.payee_language)
        if (
            audit.get("post_settlement_failure_recoveries") != 1
            or audit.get("paid_deliveries") != 2
            or audit.get("free_deliveries") != 2
        ):
            raise RuntimeError(
                "delivery recovery requires two purchases and exactly one post-settlement business failure"
            )
        if audit != context.require("purchase")["evidence"].audit:
            raise RuntimeError("delivery records changed after purchase verification")
        return Infra.api_evidence(
            context,
            [
                "one post-settlement delivery failure",
                "same run resumes delivery",
                "exactly two paid and two free deliveries",
                "business SQLite agrees with settled intents",
            ],
        )


if __name__ == "__main__":
    E2ES32DeliveryRecoveryVerifier._verify_databases()
