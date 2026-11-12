from __future__ import annotations

import json
import re
import time

from verifier.context import VerificationContext


class E2ES51TracingVerifier:
    @staticmethod
    def check(runtime, evidence) -> str:
        import yaml
        from util.constant import E2EOtherDefinitions as Project

        settings = yaml.safe_load(
            (Project.AGENTVAULT_CONFIG / "e2e-values.yaml").read_text()
        )["agentvault"]["agentvault-config"]
        settings = yaml.safe_load(settings)["mgmt"]["otel"]
        if not settings["enabled"] or settings["sample_rate"] != 1.0:
            raise RuntimeError("deterministic E2E tracing requires 100% sampling")
        logs = evidence.logs["agentvault"]
        identifiers = set(
            re.findall(r'(?:trace[._]?id|traceId)["\s:=]+([0-9a-f]{32})', logs)
        )
        if not identifiers:
            raise RuntimeError("purchase logs have no trace identity")
        directory = (
            runtime.context.report_root
            / "otel"
            / f"{evidence.payer_language}-{evidence.payee_language}-{evidence.transport}-traces"
        )
        directory.mkdir(parents=True, exist_ok=True)
        found = {}
        deadline = time.monotonic() + 45
        pending = set(identifiers)
        while pending and time.monotonic() < deadline:
            for trace_id in tuple(pending):
                status, _, raw = runtime._http(
                    runtime._url("E2E_JAEGER_UI_PORT", f"/api/traces/{trace_id}")
                )
                if status != 200:
                    continue
                payload = json.loads(raw)
                traces = payload.get("data", [])
                spans = [
                    span
                    for trace in traces
                    for span in trace.get("spans", [])
                    if span.get("traceID") == trace_id
                ]
                if not spans:
                    continue
                span_ids = {span["spanID"] for span in spans}
                if len(span_ids) != len(spans):
                    raise RuntimeError(f"duplicate Jaeger span IDs: {trace_id}")
                if not any(
                    process.get("serviceName") == "agentvault"
                    for trace in traces
                    for process in trace.get("processes", {}).values()
                ):
                    raise RuntimeError(f"trace has no AgentVault process: {trace_id}")
                if any(span.get("duration", -1) < 0 for span in spans):
                    raise RuntimeError(f"negative Jaeger span duration: {trace_id}")
                producers = set()
                for line in logs.splitlines():
                    if trace_id in line:
                        producers.update(
                            re.findall(
                                r'trace[._]producer_span_id["\s:=]+([0-9a-f]{16})', line
                            )
                        )
                transports = set()
                for line in logs.splitlines():
                    if trace_id in line:
                        transports.update(
                            re.findall(
                                r"agentvault\.transport\.(grpc|mqtt|http)\.request",
                                line,
                            )
                        )
                try:
                    for transport in transports:
                        E2ES51TracingVerifier.validate_parents(
                            spans, producers, transport
                        )
                    if not transports:
                        raise RuntimeError("trace has no correlated transport")
                except RuntimeError:
                    # A child batch can reach Jaeger before its server span.
                    # Keep polling the same identity until the whole graph arrives.
                    continue
                (directory / f"{trace_id}.json").write_text(
                    json.dumps(payload, indent=2) + "\n"
                )
                found[trace_id] = len(spans)
                pending.remove(trace_id)
            if pending:
                time.sleep(1)
        if pending:
            raise RuntimeError(
                f"Jaeger omitted purchase traces after export deadline: {sorted(pending)}"
            )
        return json.dumps({"sample_rate": 1.0, "traces": found}, sort_keys=True)

    @staticmethod
    def validate_parents(
        spans: list[dict], producers: set[str], transport: str
    ) -> None:
        ids = {span["spanID"] for span in spans}
        consumers = [
            span
            for span in spans
            if span.get("operationName") == f"{transport}.server.request"
        ]
        if not producers or not consumers:
            raise RuntimeError(
                "trace lacks a correlated SDK producer or Vault consumer"
            )
        linked = {
            reference["spanID"]
            for span in consumers
            for reference in span.get("references", [])
            if reference.get("refType") == "CHILD_OF"
        }
        if not linked.intersection(producers):
            raise RuntimeError(
                "Vault consumer trace is not parented to the logged SDK producer"
            )
        for span in spans:
            for reference in span.get("references", []):
                if (
                    reference.get("refType") == "CHILD_OF"
                    and reference.get("spanID") not in ids | producers
                ):
                    raise RuntimeError("Jaeger span has an unaccounted parent")

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def init(context: VerificationContext) -> None:
        context.require("purchase")

    @staticmethod
    def verify(context: VerificationContext) -> str:
        return context.require("purchase")["otel"]["tracing"]
