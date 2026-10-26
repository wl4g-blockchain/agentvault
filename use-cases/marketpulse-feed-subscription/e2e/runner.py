#!/usr/bin/env python3
from __future__ import annotations

import argparse
import importlib
import logging
import re
import sys
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable, Iterator

from common.context import VerificationContext
from common.project import DEPLOY_ROOT, REPORT_ROOT
from common.report import ReportBook
from common.sdk_matrix import SDK_SCOPES
from deploy.deployer import create_deployer
from deploy.deployer.kubernetes import KubernetesE2ECleaner


LOGGER = logging.getLogger("agentvault.e2e.runner")


class ProgressLogger:
    def __init__(self, heartbeat_seconds: float = 30.0) -> None:
        if heartbeat_seconds <= 0:
            raise ValueError("heartbeat_seconds must be positive")
        self._heartbeat_seconds = heartbeat_seconds

    @classmethod
    def configured(cls) -> ProgressLogger:
        if not LOGGER.handlers:
            handler = logging.StreamHandler(sys.stdout)
            handler.setFormatter(
                logging.Formatter(
                    fmt="%(asctime)s %(levelname)s %(message)s",
                    datefmt="%Y-%m-%dT%H:%M:%S%z",
                )
            )
            LOGGER.addHandler(handler)
        LOGGER.setLevel(logging.INFO)
        LOGGER.propagate = False
        return cls()

    @contextmanager
    def step(self, name: str, *, heartbeat: bool = True, **fields: object) -> Iterator[None]:
        details = self._details(fields)
        started_at = time.monotonic()
        completed = threading.Event()
        heartbeat_thread: threading.Thread | None = None
        LOGGER.info("START step=%s%s", name, details)

        if heartbeat:
            heartbeat_thread = threading.Thread(
                target=self._heartbeat,
                args=(completed, name, details, started_at),
                name=f"e2e-progress-{name}",
                daemon=True,
            )
            heartbeat_thread.start()

        try:
            yield
        except BaseException:
            LOGGER.error(
                "FAIL step=%s%s elapsed_seconds=%.1f",
                name,
                details,
                time.monotonic() - started_at,
            )
            raise
        else:
            LOGGER.info(
                "PASS step=%s%s elapsed_seconds=%.1f",
                name,
                details,
                time.monotonic() - started_at,
            )
        finally:
            completed.set()
            if heartbeat_thread is not None:
                heartbeat_thread.join()

    def _heartbeat(
        self,
        completed: threading.Event,
        name: str,
        details: str,
        started_at: float,
    ) -> None:
        while not completed.wait(self._heartbeat_seconds):
            LOGGER.info(
                "RUNNING step=%s%s elapsed_seconds=%.1f",
                name,
                details,
                time.monotonic() - started_at,
            )

    @staticmethod
    def _details(fields: dict[str, object]) -> str:
        if not fields:
            return ""
        return " " + " ".join(f"{key}={value}" for key, value in fields.items())


@dataclass(frozen=True)
class Scenario:
    name: str
    module: str
    report_module: str
    report_name: str
    description: str


SCENARIOS = (
    Scenario("infrastructure", "verifier.infra.s00_infrastructure", "infra", "s00_infrastructure.md", "render deployment configuration"),
    Scenario("structure", "verifier.infra.s01_structure", "infra", "s01_structure.md", "validate source and E2E matrices"),
    Scenario("payer", "verifier.payer.s10_dependencies", "payer", "s10_dependencies.md", "start and verify payer dependencies"),
    Scenario("payee", "verifier.payee.s10_dependencies", "payee", "s10_dependencies.md", "start and verify payee dependencies"),
    Scenario("full", "verifier.payment.s20_full", "payment", "s20_full.md", "run one language through the complete payment flow"),
    Scenario("mpp-tempo", "verifier.payment.s25_mpp_tempo", "payment", "s25_mpp_tempo.md", "run MPP Tempo charge, recovery, and gas accounting"),
    Scenario("x402-solana", "verifier.payment.s26_x402_solana", "payment", "s26_x402_solana.md", "run x402 v2 exact on a real local Solana validator"),
    Scenario("a2a", "verifier.payment.s27_a2a", "payment", "s27_a2a.md", "run five native A2A SDK boundary suites with controlled Vault ports"),
    Scenario("x402-evm", "verifier.payment.s28_x402_evm", "payment", "s28_x402_evm.md", "run complete x402 v2 EIP-3009 payments on local Anvil"),
    Scenario("matrix", "verifier.payment.s30_matrix", "payment", "s30_matrix.md", "run the selected SDK matrix scope"),
    Scenario("web", "verifier.web.s40_dashboard", "web", "s40_dashboard.md", "render Matrix evidence through real Chromium Dashboard journeys"),
)
SCENARIO_BY_NAME = {scenario.name: scenario for scenario in SCENARIOS}


def parse_duration(value: str) -> float:
    match = re.fullmatch(r"([1-9][0-9]*)(s|m|h)?", value.strip())
    if match is None:
        raise argparse.ArgumentTypeError("duration must be a positive integer followed by s, m, or h")
    multipliers = {None: 1, "s": 1, "m": 60, "h": 3600}
    return float(int(match.group(1)) * multipliers[match.group(2)])


def selected_scenarios(values: list[str] | None) -> list[Scenario]:
    names = [name.strip() for value in values or ["full"] for name in value.split(",") if name.strip()]
    unknown = sorted(set(names) - SCENARIO_BY_NAME.keys())
    if unknown:
        raise ValueError("unknown scenarios: " + ", ".join(unknown))
    seen: set[str] = set()
    return [SCENARIO_BY_NAME[name] for name in names if not (name in seen or seen.add(name))]


def create_run_root() -> tuple[str, Path]:
    instant = datetime.now(timezone.utc)
    while True:
        run_id = instant.strftime("%Y%m%d%H%M%S")
        path = REPORT_ROOT / run_id
        if not path.exists():
            path.mkdir(parents=True)
            return run_id, path
        instant += timedelta(seconds=1)


def verifier(scenario: Scenario) -> Callable[[VerificationContext], str | dict[str, str]]:
    candidate = getattr(importlib.import_module(scenario.module), "verify", None)
    if not callable(candidate):
        raise RuntimeError(f"{scenario.module} must expose verify(context)")
    return candidate


def cleanup(
    timeout_seconds: float,
    deployer: str,
    matrix_workers: int = 2,
    e2e_sdk_scope: str = "simple",
    progress: ProgressLogger | None = None,
    comprehensive: bool = True,
) -> None:
    progress = progress or ProgressLogger()
    if deployer == "kubernetes":
        context = VerificationContext(
            run_id="cleanup",
            report_root=REPORT_ROOT,
            deploy_root=DEPLOY_ROOT,
            scenario="cleanup",
            round_number=0,
            payer_language="rust",
            payee_language="rust",
            transport="mqtt",
            deployer=deployer,
            timeout_seconds=timeout_seconds,
            skip_build=True,
            reuse_build=True,
            e2e_sdk_scope=e2e_sdk_scope,
            matrix_workers=1,
        )
        owned_deployer = create_deployer(context)
        with progress.step("cleanup_kubernetes", scope="e2e-agentvault"):
            owned_deployer.cleanup()
        if comprehensive:
            with progress.step("cleanup_images", deployer=deployer):
                owned_deployer.cleanup_images()
        return
    cleanup_workers = 2 if comprehensive else matrix_workers
    image_deployer = None
    for worker_slot in (None, *range(cleanup_workers)):
        context = VerificationContext(
            run_id="cleanup",
            report_root=REPORT_ROOT,
            deploy_root=DEPLOY_ROOT,
            scenario="cleanup",
            round_number=0,
            payer_language="rust",
            payee_language="rust",
            transport="mqtt",
            deployer=deployer,
            timeout_seconds=timeout_seconds,
            skip_build=True,
            reuse_build=True,
            e2e_sdk_scope=e2e_sdk_scope,
            matrix_workers=cleanup_workers,
            worker_slot=worker_slot,
        )
        owned_deployer = create_deployer(context)
        if image_deployer is None:
            image_deployer = owned_deployer
        with progress.step(
            "cleanup_worker",
            deployer=deployer,
            worker_slot="shared" if worker_slot is None else worker_slot,
        ):
            owned_deployer.cleanup()
    if comprehensive and image_deployer is not None:
        with progress.step("cleanup_images", deployer=deployer):
            image_deployer.cleanup_images()
    if comprehensive:
        with progress.step("cleanup_kubernetes", scope="e2e-agentvault"):
            KubernetesE2ECleaner(timeout_seconds).cleanup()


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description="MarketPulse/QuantStream scenario E2E runner")
    result.add_argument("command", nargs="?", choices=("run", "cleanup"), default="run")
    result.add_argument("-s", "--scenario", action="append", help="repeat or use commas; default: full")
    result.add_argument("-r", "--rounds", type=int, default=1)
    result.add_argument(
        "--deployer",
        choices=("docker", "kubernetes"),
        default="docker",
        help="isolated deployment backend; Docker remains the CI default",
    )
    result.add_argument("--timeout", type=parse_duration, default=parse_duration("45m"))
    languages = ("rust", "go", "python", "typescript", "java")
    result.add_argument("--payer-language", choices=languages, default="rust")
    result.add_argument("--payee-language", choices=languages, default="rust")
    result.add_argument(
        "--transport",
        choices=("mqtt", "grpc"),
        default="mqtt",
        help="AgentVault payer/payee control-plane transport; matrix always verifies both",
    )
    result.add_argument("--skip-build", action="store_true")
    result.add_argument("--reuse-build", action="store_true")
    result.add_argument(
        "--cleanup",
        action="store_true",
        help="remove owned E2E containers, volumes, images, and Kubernetes releases",
    )
    result.add_argument("--cleanup-after-run", action="store_true")
    result.add_argument("--matrix-workers", type=int, default=2, help="isolated matrix workers; maximum 2")
    result.add_argument(
        "--e2e-sdk-scope",
        choices=SDK_SCOPES,
        default="simple",
        help="SDK matrix: simple=Rust/Go 2x2x2 (default), full=five-language 5x5x2",
    )
    result.add_argument(
        "--resume-run",
        metavar="YYYYMMDDHHMMSS",
        help="resume an interrupted matrix from validated case checkpoints",
    )
    result.add_argument(
        "--matrix-evidence-run",
        metavar="YYYYMMDDHHMMSS",
        help="completed Matrix report used by the Web screenshot scenario",
    )
    result.add_argument("--list", action="store_true", help="list scenarios and exit")
    return result


def main() -> None:
    progress = ProgressLogger.configured()
    args = parser().parse_args()
    if args.list:
        for scenario in SCENARIOS:
            print(f"{scenario.name:14} {scenario.description}")
        return
    if args.command == "cleanup" or args.cleanup:
        if args.cleanup_after_run:
            raise SystemExit("--cleanup and --cleanup-after-run cannot be combined")
        cleanup(
            args.timeout,
            args.deployer,
            args.matrix_workers,
            args.e2e_sdk_scope,
            progress,
        )
        print("E2E containers, volumes, local images, and Kubernetes releases removed")
        return
    if args.rounds < 1:
        raise SystemExit("--rounds must be positive")
    if args.skip_build and args.reuse_build:
        raise SystemExit("--skip-build and --reuse-build are mutually exclusive")
    if not 1 <= args.matrix_workers <= 2:
        raise SystemExit("--matrix-workers must be 1 or 2")
    scenarios = selected_scenarios(args.scenario)
    if args.matrix_evidence_run and not re.fullmatch(r"[0-9]{14}", args.matrix_evidence_run):
        raise SystemExit("--matrix-evidence-run must be a YYYYMMDDHHMMSS report directory")
    if args.resume_run:
        if not re.fullmatch(r"[0-9]{14}", args.resume_run):
            raise SystemExit("--resume-run must be a YYYYMMDDHHMMSS report directory")
        if args.rounds != 1 or [scenario.name for scenario in scenarios] != ["matrix"]:
            raise SystemExit("--resume-run is supported only for one matrix round")
        run_id = args.resume_run
        report_root = REPORT_ROOT / run_id
        if not report_root.is_dir():
            raise SystemExit(f"matrix report does not exist: {report_root}")
    else:
        run_id, report_root = create_run_root()
    reports = ReportBook("MarketPulse Feed Subscription E2E", report_root, run_id)
    with progress.step(
        "run",
        heartbeat=False,
        run_id=run_id,
        scenarios=",".join(scenario.name for scenario in scenarios),
        rounds=args.rounds,
        payer_language=args.payer_language,
        payee_language=args.payee_language,
        transport=args.transport,
    ):
        try:
            for round_number in range(1, args.rounds + 1):
                with progress.step("round", heartbeat=False, run_id=run_id, round=round_number):
                    for scenario in scenarios:
                        context = VerificationContext(
                            run_id=run_id,
                            report_root=report_root,
                            deploy_root=DEPLOY_ROOT,
                            scenario=scenario.name,
                            round_number=round_number,
                            payer_language=args.payer_language,
                            payee_language=args.payee_language,
                            transport=args.transport,
                            deployer=args.deployer,
                            timeout_seconds=args.timeout,
                            skip_build=args.skip_build,
                            reuse_build=args.reuse_build,
                            e2e_sdk_scope=args.e2e_sdk_scope,
                            matrix_workers=args.matrix_workers,
                            evidence_run_id=args.matrix_evidence_run,
                        )
                        with progress.step(
                            "scenario",
                            run_id=run_id,
                            round=round_number,
                            scenario=scenario.name,
                            payer_language=args.payer_language,
                            payee_language=args.payee_language,
                            transport=args.transport,
                        ):
                            output = verifier(scenario)(context)
                        if isinstance(output, dict):
                            for filename, evidence in output.items():
                                with progress.step(
                                    "report",
                                    heartbeat=False,
                                    run_id=run_id,
                                    scenario=scenario.name,
                                    filename=filename,
                                ):
                                    reports.record(
                                        scenario.report_module,
                                        filename,
                                        scenario.name,
                                        round_number,
                                        evidence,
                                    )
                        else:
                            with progress.step(
                                "report",
                                heartbeat=False,
                                run_id=run_id,
                                scenario=scenario.name,
                                filename=scenario.report_name,
                            ):
                                reports.record(
                                    scenario.report_module,
                                    scenario.report_name,
                                    scenario.name,
                                    round_number,
                                    output,
                                )
            with progress.step("summary", heartbeat=False, run_id=run_id):
                summary = reports.write_summary()
            print(f"E2E scenarios passed; report={summary}")
        finally:
            if args.cleanup_after_run:
                cleanup(
                    args.timeout,
                    args.deployer,
                    args.matrix_workers,
                    args.e2e_sdk_scope,
                    progress,
                    comprehensive=False,
                )


if __name__ == "__main__":
    main()
