from __future__ import annotations

import json
import os
import re
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import replace
from pathlib import Path

from util.constant import E2EOtherDefinitions as Project
from util.report import E2EReportHelper

from verifier.context import VerificationContext, create_deployer
from verifier.infra.s00_infrastructure import RUNTIME_ROOT


class E2ES30MatrixVerifier:
    class Operations:
        def prepare_matrix(self) -> None:
            checkpoint_root = self.context.report_root / "payment"
            if (checkpoint_root / "matrix-plan.json").is_file():
                journal = E2ES30MatrixVerifier.SdkMatrixJournal(
                    checkpoint_root, self.context.e2e_sdk_scope, self.backend
                )
                if all(
                    journal.load(number, case.name) is not None
                    for number, case in enumerate(
                        Project.matrix_cases(self.context.e2e_sdk_scope), 1
                    )
                ):
                    self.context.state["matrix_prepared"] = True
                    return
            self.cleanup()
            self._build_matrix_images(
                Project.matrix_languages(self.context.e2e_sdk_scope)
            )
            self.context.state["matrix_prepared"] = True

        def matrix(self) -> dict[str, str]:
            from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

            RUNTIME_ROOT.mkdir(parents=True, exist_ok=True)
            scope = self.context.e2e_sdk_scope
            languages = Project.matrix_languages(scope)
            selected_cases = Project.matrix_cases(scope)
            total = len(selected_cases)
            journal = E2ES30MatrixVerifier.SdkMatrixJournal(
                self.context.report_root / "payment", scope, self.backend
            )
            progress_path = RUNTIME_ROOT / "matrix-progress.log"
            progress_path.write_text(
                f"matrix scope={scope} 0/{total} initialized\n", encoding="utf-8"
            )
            progress_lock = threading.Lock()
            stop = threading.Event()

            def progress(message: str) -> None:
                with progress_lock:
                    print(message, flush=True)
                    with progress_path.open("a", encoding="utf-8") as stream:
                        stream.write(message + "\n")

            cases = [
                (case_number, case.transport, case.payer, case.payee)
                for case_number, case in enumerate(selected_cases, 1)
            ]
            completed: list[tuple[int, str, dict[str, str]]] = []
            pending: list[tuple[int, str, str, str]] = []
            for case_number, transport, payer, payee in cases:
                name = f"payer={payer} payee={payee} transport={transport}"
                outputs = journal.load(case_number, name)
                if outputs is None:
                    pending.append((case_number, transport, payer, payee))
                else:
                    completed.append((case_number, name, outputs))
            progress(
                f"matrix scope={scope} resume: restored {len(completed)}/{total} validated checkpoints"
            )

            workers = min(self.context.matrix_workers, len(Project.TRANSPORTS))
            groups: list[list[tuple[int, str, str, str]]] = [[] for _ in range(workers)]
            for case in pending:
                transport_index = Project.TRANSPORTS.index(case[1])
                groups[transport_index % workers].append(case)

            if not self.context.state.get("matrix_prepared"):
                self.prepare_matrix()

            def run_group(
                worker_slot: int,
                group: list[tuple[int, str, str, str]],
            ) -> list[tuple[int, str, dict[str, str]]]:
                worker_context = replace(
                    self.context,
                    state={},
                    worker_slot=worker_slot,
                    transport=group[0][1],
                    skip_build=True,
                    reuse_build=True,
                )
                worker = E2ES20PurchaseVerifier.DockerRuntime(worker_context)
                worker._render_agentvault_config()
                worker.cleanup()
                completed: list[tuple[int, str, dict[str, str]]] = []
                for case_number, transport, payer, payee in group:
                    if stop.is_set():
                        break
                    progress(
                        f"matrix scope={scope} case {case_number}/{total} start: payer={payer} payee={payee} "
                        f"transport={transport} worker={worker_slot + 1}",
                    )
                    case_context = replace(
                        self.context,
                        state={},
                        payer_language=payer,
                        payee_language=payee,
                        transport=transport,
                        skip_build=True,
                        reuse_build=True,
                        worker_slot=worker_slot,
                    )
                    case = E2ES20PurchaseVerifier.DockerRuntime(case_context)
                    name = f"payer={payer} payee={payee} transport={transport}"
                    try:
                        outputs = case.full(
                            payer, payee, clean_start=False, render_config=False
                        )
                        journal.save(case_number, name, outputs)
                        completed.append((case_number, name, outputs))
                    except BaseException as error:
                        stop.set()
                        progress(
                            f"matrix scope={scope} case {case_number}/{total} failed: payer={payer} payee={payee} "
                            f"transport={transport} worker={worker_slot + 1} error={error!r}",
                        )
                        raise
                    finally:
                        case.cleanup()
                    progress(
                        f"matrix scope={scope} case {case_number}/{total} passed: "
                        f"{name} worker={worker_slot + 1}"
                    )
                    worker._wait_host_capacity()
                return completed

            failure: BaseException | None = None
            worker_groups = [
                (slot, group) for slot, group in enumerate(groups) if group
            ]
            with ThreadPoolExecutor(
                max_workers=max(1, len(worker_groups)),
                thread_name_prefix="agentvault-e2e",
            ) as executor:
                futures = [
                    executor.submit(run_group, slot, group)
                    for slot, group in worker_groups
                ]
                for future in as_completed(futures):
                    try:
                        completed.extend(future.result())
                    except BaseException as error:
                        stop.set()
                        if failure is None:
                            failure = error
            if failure is not None:
                raise failure
            completed.sort(key=lambda item: item[0])
            if len(completed) != total:
                raise RuntimeError(
                    f"SDK matrix scope={scope} completed only {len(completed)} of {total} cases"
                )
            details: dict[str, list[str]] = {
                "s21_payer_signing.md": [],
                "s22_payee_settlement.md": [],
                "s23_balance_sync.md": [],
                "s24_gateway_balance.md": [],
            }
            passed: list[str] = []
            for _, name, outputs in completed:
                passed.append(name)
                for report, entries in details.items():
                    entries.append(f"[{name}]\n{outputs[report]}")
            return {
                "s30_matrix.md": (
                    f"scope={scope} languages={','.join(languages)} transports={','.join(Project.TRANSPORTS)}\n"
                    f"passed {total}/{total} isolated cases\n\n" + "\n".join(passed)
                ),
                **{report: "\n\n".join(entries) for report, entries in details.items()},
            }

        def _wait_host_capacity(self) -> None:
            if not hasattr(os, "getloadavg"):
                return
            processors = os.cpu_count() or 1
            threshold = max(1.0, processors * 1.5)
            for _ in range(30):
                if os.getloadavg()[0] <= threshold:
                    return
                time.sleep(2)

    class KubernetesOperations:
        def matrix(self) -> dict[str, str]:
            from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

            RUNTIME_ROOT.mkdir(parents=True, exist_ok=True)
            scope = self.context.e2e_sdk_scope
            languages = Project.matrix_languages(scope)
            selected_cases = Project.matrix_cases(scope)
            total = len(selected_cases)
            journal = E2ES30MatrixVerifier.SdkMatrixJournal(
                self.context.report_root / "payment", scope, self.backend
            )
            progress_path = RUNTIME_ROOT / "matrix-progress.log"
            progress_path.write_text(
                f"matrix scope={scope} 0/{total} initialized backend=kubernetes\n",
                encoding="utf-8",
            )

            def progress(message: str) -> None:
                print(message, flush=True)
                with progress_path.open("a", encoding="utf-8") as stream:
                    stream.write(message + "\n")

            completed: list[tuple[int, str, dict[str, str]]] = []
            pending: list[tuple[int, str, str, str]] = []
            for case_number, case in enumerate(selected_cases, 1):
                name = case.name
                outputs = journal.load(case_number, name)
                if outputs is None:
                    pending.append(
                        (case_number, case.transport, case.payer, case.payee)
                    )
                else:
                    completed.append((case_number, name, outputs))

            progress(
                f"matrix scope={scope} backend=kubernetes resume: "
                f"restored {len(completed)}/{total} validated checkpoints"
            )
            if self.context.matrix_workers != 1:
                progress(
                    "matrix backend=kubernetes uses one shared cluster; "
                    "cases run serially despite --matrix-workers"
                )

            if pending and not self.context.state.get("matrix_prepared"):
                self.prepare_matrix()
            for case_number, transport, payer, payee in pending:
                name = f"payer={payer} payee={payee} transport={transport}"
                progress(
                    f"matrix scope={scope} case {case_number}/{total} start: {name}"
                )
                case_context = replace(
                    self.context,
                    state={},
                    payer_language=payer,
                    payee_language=payee,
                    transport=transport,
                    skip_build=True,
                    reuse_build=True,
                    matrix_workers=1,
                    worker_slot=None,
                )
                case_deployer = E2ES20PurchaseVerifier.KubernetesRuntime(case_context)
                outputs = case_deployer.full(
                    payer,
                    payee,
                    clean_start=True,
                    render_config=False,
                )
                journal.save(case_number, name, outputs)
                completed.append((case_number, name, outputs))
                progress(
                    f"matrix scope={scope} case {case_number}/{total} passed: {name}"
                )

            completed.sort(key=lambda item: item[0])
            if len(completed) != total:
                raise RuntimeError(
                    f"Kubernetes SDK matrix scope={scope} completed only "
                    f"{len(completed)} of {total} cases"
                )
            details: dict[str, list[str]] = {
                "s21_payer_signing.md": [],
                "s22_payee_settlement.md": [],
                "s23_balance_sync.md": [],
                "s24_gateway_balance.md": [],
            }
            passed: list[str] = []
            for _, name, outputs in completed:
                passed.append(name)
                for report, entries in details.items():
                    entries.append(f"[{name}]\n{outputs[report]}")
            return {
                "s30_matrix.md": (
                    f"scope={scope} languages={','.join(languages)} "
                    f"transports={','.join(Project.TRANSPORTS)} deployer=kubernetes\n"
                    f"passed {total}/{total} isolated cases\n\n" + "\n".join(passed)
                ),
                **{report: "\n\n".join(entries) for report, entries in details.items()},
            }

    class SdkMatrixJournal:
        """Atomic matrix checkpoints bound to one immutable scope and deployment backend."""

        REPORTS = frozenset(
            {
                "s20_full.md",
                "s21_payer_signing.md",
                "s22_payee_settlement.md",
                "s23_balance_sync.md",
                "s24_gateway_balance.md",
            }
        )

        def __init__(self, root: Path, scope: str, backend: str) -> None:
            if backend not in {"docker", "kubernetes"}:
                raise ValueError(f"unsupported matrix backend: {backend}")
            self.root = root
            self.scope = scope
            self.backend = backend
            self._cases = tuple(case.name for case in Project.matrix_cases(scope))
            plan = {
                "backend": backend,
                "source": E2EReportHelper.source_identity(Project.REPOSITORY_ROOT),
                "scope": scope,
                "languages": list(Project.matrix_languages(scope)),
                "transports": list(Project.TRANSPORTS),
                "cases": list(self._cases),
            }
            target = root / "matrix-plan.json"
            if target.is_file():
                if self._read(target) != plan:
                    raise RuntimeError(
                        f"matrix scope/backend/source does not match the immutable resume plan: {target}"
                    )
            else:
                self._write(target, plan)

        def load(self, case_number: int, name: str) -> dict[str, str] | None:
            target = self._target(case_number, name)
            if not target.is_file():
                return None
            payload = self._read(target)
            if (
                not isinstance(payload, dict)
                or payload.get("backend") != self.backend
                or payload.get("case") != name
            ):
                raise RuntimeError(
                    f"matrix checkpoint backend/identity mismatch: {target}"
                )
            manifest = payload.get("manifest")
            if not isinstance(manifest, str):
                raise RuntimeError(
                    "legacy matrix checkpoint has no content-verified evidence; rerun this case"
                )
            directory = self.root / manifest
            if not directory.resolve().is_relative_to(self.root.resolve()):
                raise RuntimeError("matrix evidence path escapes its report")
            verified = E2EReportHelper.verify_manifest(directory)
            if verified["metadata"]["source"] != E2EReportHelper.source_identity(
                Project.REPOSITORY_ROOT
            ):
                raise RuntimeError(
                    "matrix evidence source differs from the current working tree"
                )
            reports = self._reports(payload.get("reports"), target)
            self._verify_financial_evidence(directory, reports)
            return reports

        @staticmethod
        def _verify_financial_evidence(
            directory: Path, reports: dict[str, str]
        ) -> None:
            from verifier.context import FullFlowEvidence
            from verifier.purchase.s21_payer_signing import E2ES21PayerSigningVerifier
            from verifier.purchase.s22_payee_settlement import (
                E2ES22PayeeSettlementVerifier,
            )
            from verifier.purchase.s23_balance_sync import E2ES23BalanceSyncVerifier
            from verifier.purchase.s24_accounting import E2ES24AccountingVerifier

            payload = json.loads((directory / "evidence.json").read_text())
            payload.pop("backend")
            balance = payload.pop("balance_sync")
            payload["logs"] = {
                service: (directory / f"{service}.log").read_text()
                for service in ("payer", "payee", "agentvault", "facilitator")
            }
            evidence = FullFlowEvidence(**payload)
            actual = {
                "s21_payer_signing.md": E2ES21PayerSigningVerifier.verify_evidence(
                    evidence
                ),
                "s22_payee_settlement.md": E2ES22PayeeSettlementVerifier.verify_evidence(
                    evidence
                ),
                "s23_balance_sync.md": E2ES23BalanceSyncVerifier.verify_evidence(
                    evidence, balance
                ),
                "s24_gateway_balance.md": E2ES24AccountingVerifier.verify_evidence(
                    evidence
                ),
            }
            if any(reports[name] != result for name, result in actual.items()):
                raise RuntimeError(
                    "matrix report text does not match reverified raw financial evidence"
                )

        def save(self, case_number: int, name: str, outputs: dict[str, str]) -> None:
            target = self._target(case_number, name)
            case = Project.matrix_cases(self.scope)[case_number - 1]
            manifest = Path("full-flow") / f"{case.payer}-{case.payee}-{case.transport}"
            E2EReportHelper.verify_manifest(self.root / manifest)
            self._write(
                target,
                {
                    "manifest": manifest.as_posix(),
                    "backend": self.backend,
                    "case": name,
                    "reports": self._reports(outputs, target),
                },
            )

        def _target(self, case_number: int, name: str) -> Path:
            if (
                not 1 <= case_number <= len(self._cases)
                or self._cases[case_number - 1] != name
            ):
                raise ValueError(
                    "matrix checkpoint does not identify a case in the selected scope"
                )
            return self.root / "matrix-cases" / f"case-{case_number:02d}.json"

        @classmethod
        def _reports(cls, outputs: object, target: Path) -> dict[str, str]:
            if not isinstance(outputs, dict) or not cls.REPORTS.issubset(outputs):
                raise RuntimeError(
                    f"matrix checkpoint evidence is incomplete: {target}"
                )
            if any(
                not isinstance(outputs[key], str) or not outputs[key].strip()
                for key in cls.REPORTS
            ):
                raise RuntimeError(f"matrix checkpoint evidence is empty: {target}")
            return {str(key): str(value) for key, value in outputs.items()}

        @staticmethod
        def _read(target: Path) -> object:
            try:
                return json.loads(target.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as error:
                raise RuntimeError(
                    f"invalid matrix journal {target}: {error}"
                ) from error

        @staticmethod
        def _write(target: Path, payload: object) -> None:
            target.parent.mkdir(parents=True, exist_ok=True)
            temporary = target.with_suffix(".tmp")
            temporary.write_text(
                json.dumps(payload, ensure_ascii=False, sort_keys=True) + "\n",
                encoding="utf-8",
            )
            temporary.replace(target)

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        create_deployer(context).prepare_matrix()

    @staticmethod
    def init(context: VerificationContext) -> None:
        E2ES30MatrixVerifier.SdkMatrixJournal(
            context.report_root / "payment", context.e2e_sdk_scope, context.deployer
        )

    @staticmethod
    def verify(context: VerificationContext) -> dict[str, str]:
        reports = create_deployer(context).matrix()
        evidence = reports.get("s30_matrix.md", "")
        cases = Project.matrix_cases(context.e2e_sdk_scope)
        expected = {case.name for case in cases}
        total = len(cases)
        reported = {line for line in evidence.splitlines() if line.startswith("payer=")}
        if f"scope={context.e2e_sdk_scope} " not in evidence:
            raise RuntimeError(f"matrix evidence omitted scope={context.e2e_sdk_scope}")
        if (
            f"passed {total}/{total} isolated cases" not in evidence
            or reported != expected
        ):
            raise RuntimeError(
                f"matrix evidence does not match scope={context.e2e_sdk_scope}; "
                f"missing={sorted(expected - reported)}, extra={sorted(reported - expected)}"
            )
        for report in (
            "s21_payer_signing.md",
            "s22_payee_settlement.md",
            "s23_balance_sync.md",
            "s24_gateway_balance.md",
        ):
            detail = reports.get(report, "")
            detailed_cases = re.findall(r"^\[(payer=.*)\]$", detail, re.MULTILINE)
            detailed_set = set(detailed_cases)
            if len(detailed_cases) != total or detailed_set != expected:
                raise RuntimeError(
                    f"matrix scope={context.e2e_sdk_scope} did not preserve all "
                    f"{total} {report} verifications: missing={sorted(expected - detailed_set)}, "
                    f"extra={sorted(detailed_set - expected)}"
                )
        return reports
