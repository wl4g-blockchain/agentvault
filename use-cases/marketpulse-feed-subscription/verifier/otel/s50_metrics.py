from __future__ import annotations

import json
import math
import re
from collections import defaultdict

import yaml
from util.constant import E2EOtherDefinitions as Project

from verifier.context import VerificationContext


class E2ES50MetricsVerifier:
    @staticmethod
    def samples(text: str) -> dict[tuple[str, tuple[tuple[str, str], ...]], float]:
        result = {}
        for line in text.splitlines():
            if not line or line.startswith("#"):
                continue
            match = re.fullmatch(
                r"([a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{(.*)\})?\s+([^\s]+)(?:\s+#.*)?", line
            )
            if match is None:
                raise RuntimeError(f"invalid OpenMetrics sample: {line}")
            labels = tuple(
                sorted(
                    (key, json.loads(value))
                    for key, value in re.findall(
                        r'(\w+)=("(?:[^"\\]|\\.)*")', match[2] or ""
                    )
                )
            )
            key = (match[1], labels)
            if key in result or not math.isfinite(float(match[3])):
                raise RuntimeError(f"duplicate or non-finite metric: {key}")
            result[key] = float(match[3])
        return result

    @staticmethod
    def scrape(runtime) -> str:
        settings = yaml.safe_load(
            (Project.AGENTVAULT_CONFIG / "e2e-values.yaml").read_text()
        )["agentvault"]["agentvault-config"]
        settings = yaml.safe_load(settings)["mgmt"]
        canonical = yaml.safe_load(
            (Project.REPOSITORY_ROOT / "etc/agentvault.yaml").read_text()
        )["mgmt"]
        if settings["port"] != canonical["port"] or not settings["metrics"]["enabled"]:
            raise RuntimeError(
                "E2E management listener must match the project's configured metrics listener"
            )
        path = (
            settings.get("context_path", "/").rstrip("/")
            + "/"
            + settings["metrics"]["path"].lstrip("/")
        )
        status, _, body = runtime._http(runtime._url("E2E_AGENTVAULT_MGMT_PORT", path))
        if status != 200:
            raise RuntimeError(f"metrics scrape returned {status}")
        return body

    @staticmethod
    def begin(runtime) -> str:
        return E2ES50MetricsVerifier.scrape(runtime)

    @staticmethod
    def check(before: str, after: str) -> dict:
        baseline = E2ES50MetricsVerifier.samples(before)
        current = E2ES50MetricsVerifier.samples(after)
        families = {name for name, labels in current}
        required = {
            "agentvault_payment_operations_total",
            "agentvault_payment_duration_seconds_count",
            "agentvault_payment_duration_seconds_sum",
            "agentvault_payment_duration_seconds_bucket",
            "agentvault_http_requests_total",
            "agentvault_payments_in_flight",
            "agentvault_wallet_selections_total",
            "agentvault_wallet_balance_sync_total",
        }
        if not required <= families:
            raise RuntimeError(f"missing metrics: {sorted(required - families)}")
        operations = defaultdict(float)
        durations = {}
        buckets = defaultdict(list)
        deltas = {}
        for (name, labels), value in current.items():
            label = dict(labels)
            if name.endswith(
                ("_total", "_count", "_sum", "_bucket")
            ) and value < baseline.get((name, labels), 0):
                raise RuntimeError(f"counter reset during purchase: {name} {label}")
            if name == "agentvault_payment_operations_total":
                operations[(label["operation"], label["direction"])] += value
                deltas[
                    "/".join(
                        label[key] for key in ("operation", "direction", "outcome")
                    )
                ] = value - baseline.get((name, labels), 0)
            elif name == "agentvault_payment_duration_seconds_count":
                durations[(label["operation"], label["direction"])] = value
            elif name == "agentvault_payment_duration_seconds_bucket":
                buckets[(label["operation"], label["direction"])].append(
                    (float(label["le"]), value)
                )
            elif name == "agentvault_payments_in_flight" and value != 0:
                raise RuntimeError(
                    f"payment operation gauge did not return to zero: {value}"
                )
        if dict(operations) != durations:
            raise RuntimeError(
                "payment histogram counts disagree with outcome counters"
            )
        for key, points in buckets.items():
            points.sort()
            if any(a[1] > b[1] for a, b in zip(points, points[1:])) or points[-1] != (
                float("inf"),
                durations[key],
            ):
                raise RuntimeError(f"invalid cumulative latency buckets: {key}")
        for operation in ("sign/outbound/success", "approve/outbound/success"):
            expected = 2 if operation.startswith("sign/") else 1
            if deltas.get(operation) != expected:
                raise RuntimeError(
                    f"purchase metric {operation}: expected delta {expected}, got {deltas.get(operation)}"
                )
        selection_delta = sum(
            value - baseline.get((name, labels), 0)
            for (name, labels), value in current.items()
            if name == "agentvault_wallet_selections_total"
        )
        sync_delta = sum(
            value - baseline.get((name, labels), 0)
            for (name, labels), value in current.items()
            if name == "agentvault_wallet_balance_sync_total"
            and dict(labels).get("outcome") == "success"
        )
        if selection_delta != 2 or sync_delta < 10:
            raise RuntimeError(
                f"wallet metrics do not cover the purchase: selections={selection_delta}, sync observations={sync_delta}"
            )
        return {
            "payment_operation_deltas": deltas,
            "histogram_counter_consistency": True,
            "in_flight": 0,
        }

    @staticmethod
    def finish(runtime, baseline: str) -> str:
        after = E2ES50MetricsVerifier.scrape(runtime)
        result = E2ES50MetricsVerifier.check(baseline, after)
        previous = E2ES50MetricsVerifier.samples(after)
        status, _, _ = runtime._http(
            runtime._url("E2E_AGENTVAULT_PORT", "/api/v1/payments/balance")
        )
        if status != 400:
            raise RuntimeError(
                f"deterministic invalid-query metrics probe returned {status}"
            )
        after_probe = E2ES50MetricsVerifier.scrape(runtime)
        current = E2ES50MetricsVerifier.samples(after_probe)
        key = (
            "agentvault_http_requests_total",
            tuple(
                sorted(
                    {
                        "route": "/api/v1/payments/balance",
                        "method": "GET",
                        "status": str(status),
                    }.items()
                )
            ),
        )
        if current.get(key, 0) - previous.get(key, 0) != 1:
            raise RuntimeError(
                "HTTP request counter did not record exactly one known request"
            )
        result["http_probe_delta"] = 1
        after = after_probe
        directory = runtime.context.report_root / "otel"
        directory.mkdir(parents=True, exist_ok=True)
        stem = f"{runtime.context.payer_language}-{runtime.context.payee_language}-{runtime.context.transport}"
        (directory / f"{stem}-metrics-before.txt").write_text(baseline)
        (directory / f"{stem}-metrics-after.txt").write_text(after)
        return json.dumps(result, sort_keys=True)

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return context.require("purchase")["otel"]["metrics"]
