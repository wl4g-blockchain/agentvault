#!/usr/bin/env python3
from __future__ import annotations

import argparse
import importlib
import re
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Callable

from common.context import VerificationContext
from common.deploy import create_deployer
from common.project import DEPLOY_ROOT, REPORT_ROOT
from common.report import ReportBook


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
    Scenario("matrix", "verifier.payment.s30_matrix", "payment", "s30_matrix.md", "run the complete five-language matrix"),
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


def verifier(scenario: Scenario) -> Callable[[VerificationContext], str]:
    candidate = getattr(importlib.import_module(scenario.module), "verify", None)
    if not callable(candidate):
        raise RuntimeError(f"{scenario.module} must expose verify(context)")
    return candidate


def cleanup(timeout_seconds: float, deployer: str) -> None:
    context = VerificationContext(
        run_id="cleanup",
        report_root=REPORT_ROOT,
        deploy_root=DEPLOY_ROOT,
        scenario="cleanup",
        round_number=0,
        payer_language="rust",
        payee_language="rust",
        deployer=deployer,
        timeout_seconds=timeout_seconds,
        skip_build=True,
        reuse_build=True,
    )
    create_deployer(context).cleanup()


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description="MarketPulse/QuantStream scenario E2E runner")
    result.add_argument("command", nargs="?", choices=("run", "cleanup"), default="run")
    result.add_argument("-s", "--scenario", action="append", help="repeat or use commas; default: full")
    result.add_argument("-r", "--rounds", type=int, default=1)
    result.add_argument(
        "--deployer",
        choices=("docker",),
        default="docker",
        help="isolated deployment backend; only implemented backends are advertised",
    )
    result.add_argument("--timeout", type=parse_duration, default=parse_duration("45m"))
    languages = ("rust", "go", "python", "typescript", "java")
    result.add_argument("--payer-language", choices=languages, default="rust")
    result.add_argument("--payee-language", choices=languages, default="rust")
    result.add_argument("--skip-build", action="store_true")
    result.add_argument("--reuse-build", action="store_true")
    result.add_argument("--cleanup", action="store_true", help="alias for the cleanup command")
    result.add_argument("--cleanup-after-run", action="store_true")
    result.add_argument("--list", action="store_true", help="list scenarios and exit")
    return result


def main() -> None:
    args = parser().parse_args()
    if args.list:
        for scenario in SCENARIOS:
            print(f"{scenario.name:14} {scenario.description}")
        return
    if args.command == "cleanup" or args.cleanup:
        cleanup(args.timeout, args.deployer)
        print("E2E deployment removed")
        return
    if args.rounds < 1:
        raise SystemExit("--rounds must be positive")
    if args.skip_build and args.reuse_build:
        raise SystemExit("--skip-build and --reuse-build are mutually exclusive")
    scenarios = selected_scenarios(args.scenario)
    run_id, report_root = create_run_root()
    reports = ReportBook("MarketPulse Feed Subscription E2E", report_root, run_id)
    try:
        for round_number in range(1, args.rounds + 1):
            for scenario in scenarios:
                context = VerificationContext(
                    run_id=run_id,
                    report_root=report_root,
                    deploy_root=DEPLOY_ROOT,
                    scenario=scenario.name,
                    round_number=round_number,
                    payer_language=args.payer_language,
                    payee_language=args.payee_language,
                    deployer=args.deployer,
                    timeout_seconds=args.timeout,
                    skip_build=args.skip_build,
                    reuse_build=args.reuse_build,
                )
                output = verifier(scenario)(context)
                reports.record(
                    scenario.report_module,
                    scenario.report_name,
                    scenario.name,
                    round_number,
                    output,
                )
        summary = reports.write_summary()
        print(f"E2E scenarios passed; report={summary}")
    finally:
        if args.cleanup_after_run:
            cleanup(args.timeout, args.deployer)


if __name__ == "__main__":
    main()
