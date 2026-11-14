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
from pathlib import Path
from typing import Callable, Iterator

from util.constant import E2EOtherDefinitions as Project
from util.kubernetes import E2EKubernetesCleaner
from util.report import E2EReportHelper
from verifier.context import VerificationContext, create_deployer

#!/usr/bin/env python3


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
    def step(
        self, name: str, *, heartbeat: bool = True, **fields: object
    ) -> Iterator[None]:
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
    Scenario(
        "infrastructure",
        "verifier.infra.s00_infrastructure",
        "infra",
        "s00_infrastructure.md",
        "render deployment configuration",
    ),
    Scenario(
        "structure",
        "verifier.infra.s01_structure",
        "infra",
        "s01_structure.md",
        "validate source and E2E matrices",
    ),
    Scenario(
        "payer",
        "verifier.infra.s00_infrastructure",
        "payer",
        "s10_dependencies.md",
        "start and verify payer dependencies",
    ),
    Scenario(
        "payee",
        "verifier.infra.s00_infrastructure",
        "payee",
        "s10_dependencies.md",
        "start and verify payee dependencies",
    ),
    Scenario(
        "full",
        "verifier.purchase.s20_purchase",
        "payment",
        "s20_full.md",
        "run one language through the complete payment flow",
    ),
    Scenario(
        "mpp-tempo",
        "verifier.tempo_charge.s25_charge_recovery",
        "payment",
        "s25_mpp_tempo.md",
        "run MPP Tempo charge, recovery, and gas accounting",
    ),
    Scenario(
        "x402-solana",
        "verifier.solana_settlement.s26_exact_settlement",
        "payment",
        "s26_x402_solana.md",
        "run x402 v2 exact on a real local Solana validator",
    ),
    Scenario(
        "a2a",
        "verifier.a2a_payment.s27_message_payment",
        "payment",
        "s27_a2a.md",
        "run five native A2A SDK boundary suites with controlled Vault ports",
    ),
    Scenario(
        "manual",
        "verifier.acceptance.s29_retained_purchase",
        "payment",
        "s29_manual.md",
        "redeploy and retain all five-language manual acceptance profiles",
    ),
    Scenario(
        "manual-services",
        "verifier.acceptance.s29_retained_purchase",
        "payment",
        "s29_manual_services.md",
        "resume retained manual services without resetting chains or Vault",
    ),
    Scenario(
        "manual-payment",
        "verifier.acceptance.s29_retained_purchase",
        "payment",
        "s29_manual_payment.md",
        "execute one retained-stack SDK payment Job",
    ),
    Scenario(
        "manual-matrix",
        "verifier.acceptance.s29_retained_purchase",
        "payment",
        "s29_manual_matrix.md",
        "execute 25 cross-language payments without resetting the stack",
    ),
    Scenario(
        "matrix",
        "verifier.purchase.s30_matrix",
        "payment",
        "s30_matrix.md",
        "run the selected SDK matrix scope",
    ),
    Scenario(
        "web",
        "verifier.dashboard.s40_dashboard",
        "web",
        "s40_dashboard.md",
        "render Matrix evidence through real Chromium Dashboard journeys",
    ),
)
SCENARIOS += (
    Scenario(
        "live-wallet",
        "verifier.dashboard.s41_live_wallet",
        "dashboard",
        "s41_live_wallet.md",
        "verify browser encryption against live Vault custody and PostgreSQL",
    ),
)
SCENARIOS += tuple(
    Scenario(name, module, name, filename, description)
    for name, module, filename, description in (
        (
            "budget-race",
            "verifier.authorization.s16_budget_race",
            "s16_budget_race.md",
            "exercise concurrent budget reservation and atomic settlement in both SQL dialects",
        ),
        (
            "key-lifecycle",
            "verifier.custody.s17_key_lifecycle",
            "s17_key_lifecycle.md",
            "exercise intent-bound active-key custody in both SQL dialects",
        ),
        (
            "outbox-redelivery",
            "verifier.recovery.s34_outbox_redelivery",
            "s34_outbox_redelivery.md",
            "exercise persistent delivery leases and restart replay in both SQL dialects",
        ),
    )
)
SCENARIOS += tuple(
    Scenario(name, f"verifier.purchase.{stem}", "purchase", stem + ".md", description)
    for name, stem, description in (
        (
            "payer-signing",
            "s21_payer_signing",
            "verify purchase authorization and signature evidence",
        ),
        (
            "payee-settlement",
            "s22_payee_settlement",
            "verify trusted settlement before merchant delivery",
        ),
        (
            "balance-sync",
            "s23_balance_sync",
            "verify non-authoritative snapshots against chain observations",
        ),
        (
            "accounting",
            "s24_accounting",
            "verify ledger, token, receipt and gas conservation",
        ),
    )
)
SCENARIOS += tuple(
    Scenario(name, module, name, filename, description)
    for name, module, filename, description in (
        (
            "authorization",
            "verifier.authorization.s11_authorization_boundary",
            "s11_authorization_boundary.md",
            "reject caller projections and tampered merchant challenges",
        ),
        (
            "approval",
            "verifier.approval.s12_approval_boundary",
            "s12_approval_boundary.md",
            "reject approval replay and body impersonation",
        ),
        (
            "pricing",
            "verifier.pricing.s13_price_governance",
            "s13_price_governance.md",
            "exercise real Price revisions, ownership and history constraints",
        ),
        (
            "custody",
            "verifier.custody.s14_wallet_import",
            "s14_wallet_import.md",
            "import an encrypted Wallet using the real one-time API",
        ),
        (
            "identity",
            "verifier.identity.s15_identity_boundary",
            "s15_identity_boundary.md",
            "verify Vault's signed identity adapter and cross-client authorization",
        ),
        (
            "delivery-recovery",
            "verifier.delivery.s32_delivery_recovery",
            "s32_delivery_recovery.md",
            "bind failed and resumed delivery to the same settled purchase",
        ),
        (
            "settlement-restart",
            "verifier.recovery.s33_settlement_restart",
            "s33_settlement_restart.md",
            "kill/restart Vault and verify durable receipts and fail-closed readiness",
        ),
    )
)
SCENARIOS += tuple(
    Scenario(
        name,
        f"verifier.otel.s{number}_{name}",
        "otel",
        f"s{number}_{name}.md",
        description,
    )
    for number, name, description in (
        (50, "metrics", "verify purchase metrics on the configured management API"),
        (51, "tracing", "verify 100% sampled purchase traces in Jaeger"),
        (52, "logs", "verify correlated purchase transitions in workload logs"),
    )
)
SCENARIO_BY_NAME = {scenario.name: scenario for scenario in SCENARIOS}
SCENARIO_BY_NAME["x402-evm"] = SCENARIO_BY_NAME["full"]


def parse_duration(value: str) -> float:
    match = re.fullmatch(r"([1-9][0-9]*)(s|m|h)?", value.strip())
    if match is None:
        raise argparse.ArgumentTypeError(
            "duration must be a positive integer followed by s, m, or h"
        )
    multipliers = {None: 1, "s": 1, "m": 60, "h": 3600}
    return float(int(match.group(1)) * multipliers[match.group(2)])


def selected_scenarios(values: list[str] | None) -> list[Scenario]:
    names = [
        name.strip()
        for value in values or ["full"]
        for name in value.split(",")
        if name.strip()
    ]
    unknown = sorted(set(names) - SCENARIO_BY_NAME.keys())
    if unknown:
        raise ValueError("unknown scenarios: " + ", ".join(unknown))
    names = [SCENARIO_BY_NAME[name].name for name in names]
    dependent = {
        "live-wallet",
        "budget-race",
        "key-lifecycle",
        "outbox-redelivery",
        "metrics",
        "tracing",
        "logs",
        "authorization",
        "approval",
        "pricing",
        "custody",
        "identity",
        "delivery-recovery",
        "settlement-restart",
        "payer-signing",
        "payee-settlement",
        "balance-sync",
        "accounting",
    }
    ordered = []
    for name in names:
        if name in dependent and "full" not in ordered:
            ordered.append("full")
        if name not in ordered:
            ordered.append(name)
    return [SCENARIO_BY_NAME[name] for name in ordered]


def create_run_root() -> tuple[str, Path]:
    return E2EReportHelper.create_run_root(Project.REPORT_ROOT)


def verifier(
    scenario: Scenario,
) -> Callable[[VerificationContext], str | dict[str, str]]:
    module = importlib.import_module(scenario.module)
    stem = scenario.module.rsplit(".", 1)[-1]
    name = (
        "E2E"
        + "".join(word[:1].upper() + word[1:] for word in stem.split("_"))
        + "Verifier"
    )
    candidate = getattr(module, name, None)
    if candidate is None or any(
        not callable(getattr(candidate, phase, None))
        for phase in ("deploy", "init", "verify")
    ):
        raise RuntimeError(
            f"{scenario.module} must define {name}.deploy/init/verify(context)"
        )

    def execute(context: VerificationContext):
        try:
            candidate.deploy(context)
            candidate.init(context)
            return candidate.verify(context)
        finally:
            session = context.state.pop("session", None)
            if session is not None:
                session.close()

    return execute


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
            report_root=Project.REPORT_ROOT,
            deploy_root=Project.DEPLOY_ROOT,
            scenario="cleanup",
            round_number=0,
            payer_language="rust",
            payee_language="rust",
            transport="http",
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
            report_root=Project.REPORT_ROOT,
            deploy_root=Project.DEPLOY_ROOT,
            scenario="cleanup",
            round_number=0,
            payer_language="rust",
            payee_language="rust",
            transport="http",
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
            E2EKubernetesCleaner(timeout_seconds).cleanup()


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="MarketPulse/QuantStream scenario E2E runner"
    )
    result.add_argument("command", nargs="?", choices=("run", "cleanup"), default="run")
    result.add_argument(
        "-s", "--scenario", action="append", help="repeat or use commas; default: full"
    )
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
        "--payment-profile", choices=("evm", "solana", "tempo"), default="evm"
    )
    result.add_argument("--message-protocol", choices=("mcp", "a2a"), default="mcp")
    result.add_argument(
        "--fail-after-settlement",
        action="store_true",
        help="manual payments: inject one application failure after settlement, then retry delivery",
    )
    result.add_argument(
        "--transport",
        choices=("http",),
        default="http",
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
    result.add_argument(
        "--matrix-workers",
        type=int,
        default=2,
        help="isolated matrix workers; maximum 2",
    )
    result.add_argument(
        "--e2e-sdk-scope",
        choices=Project.SDK_SCOPES,
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
    if args.matrix_evidence_run and not re.fullmatch(
        r"[0-9]{14}", args.matrix_evidence_run
    ):
        raise SystemExit(
            "--matrix-evidence-run must be a YYYYMMDDHHMMSS report directory"
        )
    if args.resume_run:
        if not re.fullmatch(r"[0-9]{14}", args.resume_run):
            raise SystemExit("--resume-run must be a YYYYMMDDHHMMSS report directory")
        if args.rounds != 1 or [scenario.name for scenario in scenarios] != ["matrix"]:
            raise SystemExit("--resume-run is supported only for one matrix round")
        run_id = args.resume_run
        report_root = Project.REPORT_ROOT / run_id
        if not report_root.is_dir():
            raise SystemExit(f"matrix report does not exist: {report_root}")
    else:
        run_id, report_root = create_run_root()
    reports = E2EReportHelper("MarketPulse Feed Subscription E2E", report_root, run_id)
    completed = {}
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
                completed.clear()
                with progress.step(
                    "round", heartbeat=False, run_id=run_id, round=round_number
                ):
                    for scenario in scenarios:
                        context = VerificationContext(
                            run_id=run_id,
                            report_root=report_root,
                            completed=completed,
                            deploy_root=Project.DEPLOY_ROOT,
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
                            payment_profile=args.payment_profile,
                            message_protocol=args.message_protocol,
                            fail_after_settlement=args.fail_after_settlement,
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
            E2EReportHelper.archive(
                Project.REPORT_ROOT, protected=(run_id, args.matrix_evidence_run or "")
            )
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
