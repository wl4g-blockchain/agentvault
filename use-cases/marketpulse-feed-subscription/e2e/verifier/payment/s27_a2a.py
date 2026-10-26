"""Native SDK boundary acceptance; the controlled Vault port is not a chain proof."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

from common.context import VerificationContext
from common.project import REPOSITORY_ROOT


class A2ASdkAcceptance:
    LANGUAGES = ("golang", "python", "typescript", "java", "rust")

    def __init__(self, context: VerificationContext) -> None:
        self.context = context
        self.root = REPOSITORY_ROOT / "src" / "adapters"

    def commands(self) -> dict[str, list[str]]:
        return {
            "golang": ["go", "test", "-count=1", "-race", "-p", "2", "-json", "-run", "(?i)a2a",
                       "./tests/model", "./tests/payer", "./tests/payee", "./tests/protocol"],
            "python": [sys.executable, "-m", "unittest", "discover", "-s", "tests/agentvault",
                       "-t", "tests/agentvault", "-p", "a2a_test.py"],
            "typescript": ["node", "--import", "tsx", "--test", *[
                f"tests/agentvault/{role}/a2a_test.ts" for role in ("model", "payer", "payee")]],
            "java": ["mvn", "-B", "-q", "-Dtest=A2a*Test", "-DfailIfNoTests=true", "test"],
            "rust": ["cargo", "test", "--locked", "--lib", "a2a", "--", "--nocapture"],
        }

    @staticmethod
    def passed_count(language: str, output: str, root: Path) -> int:
        if language == "golang":
            records = [json.loads(line) for line in output.splitlines() if line.startswith('{')]
            return sum(row.get("Action") == "pass" and bool(row.get("Test")) for row in records)
        if language == "java":
            reports = list((root / "target" / "surefire-reports").glob("TEST-*A2a*Test.xml"))
            count = 0
            for report in reports:
                suite = ET.parse(report).getroot()
                if int(suite.get("failures", "0")) or int(suite.get("errors", "0")):
                    raise RuntimeError("A2A Java test report contains a failure")
                count += int(suite.get("tests", "0")) - int(suite.get("skipped", "0"))
            return count
        expression = {
            "python": r"Ran (\d+) tests? in .*\n\s*OK",
            "typescript": r"# pass (\d+)",
            "rust": r"test result: ok\. (\d+) passed; 0 failed",
        }[language]
        matched = re.search(expression, output)
        return int(matched[1]) if matched else 0

    def run(self) -> str:
        evidence = []
        directory = self.context.report_root / "payment" / "a2a"
        directory.mkdir(parents=True, exist_ok=True)
        for language, command in self.commands().items():
            root = self.root / language
            environment = dict(os.environ, PYTHONPATH=str(root), PYTHONDONTWRITEBYTECODE="1",
                               GOMAXPROCS="2", CARGO_BUILD_JOBS="2", CARGO_INCREMENTAL="0")
            if language == "rust":
                target = Path(environment.get("CARGO_TARGET_DIR", str(root / "target")))
                if target.exists() and sum(path.stat().st_size for path in target.rglob("*") if path.is_file()) > 10 * 1024**3:
                    subprocess.run(["cargo", "clean", "--target-dir", str(target)], cwd=root, check=True, env=environment)
            started = time.monotonic()
            process = subprocess.Popen(command, cwd=root, env=environment, stdout=subprocess.PIPE,
                                       stderr=subprocess.STDOUT, text=True, start_new_session=True)
            try:
                output, _ = process.communicate(timeout=self.context.timeout_seconds)
            except BaseException:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise
            (directory / f"{language}.log").write_text(output, encoding="utf-8")
            if process.returncode:
                raise RuntimeError(f"{language} A2A acceptance failed: {output[-5000:]}")
            count = self.passed_count(language, output, root)
            if count <= 0:
                raise RuntimeError(f"{language} did not execute any A2A tests")
            evidence.append({"language": language, "passed": count, "seconds": round(time.monotonic()-started, 2)})
        fixture = self.root / "tests" / "payment_protocol_a2a.json"
        result = {"scope": "SDK HTTP adapters with controlled Vault ports", "a2a": "0.3 message/send",
                  "protocols": ["x402-v2", "mpp-tempo-charge"], "languages": evidence,
                  "fixture_sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(), "chain_settlement": False}
        (directory / "evidence.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        return "Five SDK A2A boundary suites passed (controlled Vault; no chain-settlement claim).\n\n" + json.dumps(result, indent=2)


def verify(context: VerificationContext) -> str:
    return A2ASdkAcceptance(context).run()
