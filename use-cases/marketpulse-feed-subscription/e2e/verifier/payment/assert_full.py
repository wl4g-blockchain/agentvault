#!/usr/bin/env python3
"""Fail-closed business-delivery assertions for the real Docker E2E."""

from __future__ import annotations

import json
import os
import sqlite3
from pathlib import Path


def one(connection: sqlite3.Connection, sql: str) -> tuple[object, ...]:
    row = connection.execute(sql).fetchone()
    if row is None:
        raise RuntimeError(f"no evidence for query: {sql}")
    return row


languages = {"rust", "go", "python", "typescript", "java"}
payer_language = os.environ.get("E2E_PAYER_LANGUAGE", "").strip()
payee_language = os.environ.get("E2E_PAYEE_LANGUAGE", "").strip()
if payer_language not in languages:
    raise RuntimeError("E2E_PAYER_LANGUAGE must select one supported ingestor")
if payee_language not in languages:
    raise RuntimeError("E2E_PAYEE_LANGUAGE must select one supported feed service")

market_path = Path(f"/evidence/marketpulse-{payee_language}/marketpulse.sqlite3")
quant_path = Path(f"/evidence/quantstream-{payer_language}/ingestion.sqlite3")
if not market_path.is_file() or not quant_path.is_file():
    raise RuntimeError("durable business databases are missing")
for database in (market_path, quant_path):
    if Path(f"{database}-wal").exists() or Path(f"{database}-shm").exists():
        raise RuntimeError("business evidence database is not a quiescent read-only snapshot")

with sqlite3.connect(f"file:{quant_path}?mode=ro", uri=True) as quant:
    runs = quant.execute(
        "SELECT run_id,free_response_json,paid_response_json,payment_intent_id "
        "FROM ingestion_run ORDER BY completed_at,run_id"
    ).fetchall()
if len(runs) != 2:
    raise RuntimeError(f"expected two completed ingestion runs, found {len(runs)}")

outbound_intents: list[str] = []
inbound_intents: list[str] = []
run_ids: list[str] = []
for run_id, free_raw, paid_raw, outbound_intent in runs:
    free_response = json.loads(str(free_raw))
    paid_response = json.loads(str(paid_raw))

    # Every language stores exactly the MCP structuredContent business object.
    # Refuse JSON-RPC envelopes here so implementation drift cannot be hidden by
    # an overly permissive verifier.
    if not isinstance(free_response, dict) or set(free_response) != {"news", "paid", "intentId"}:
        raise RuntimeError("free MCP delivery evidence has a non-canonical shape")
    if not isinstance(paid_response, dict) or set(paid_response) != {"news", "paid", "intentId"}:
        raise RuntimeError("paid MCP delivery evidence has a non-canonical shape")
    if free_response.get("paid") is not False or not isinstance(free_response.get("news"), list) or not free_response["news"]:
        raise RuntimeError("free MCP delivery evidence is invalid")
    if free_response.get("intentId") is not None:
        raise RuntimeError("free MCP delivery unexpectedly contains a payment intent")
    if paid_response.get("paid") is not True or not isinstance(paid_response.get("news"), list) or not paid_response["news"]:
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
    raise RuntimeError(f"expected two durable MarketPulse deliveries, found {len(deliveries)}")
delivered_by_intent = {str(row[1]): row for row in deliveries}
for run_index, (run_id, inbound_intent) in enumerate(zip(run_ids, inbound_intents, strict=True)):
    delivery = delivered_by_intent.get(inbound_intent)
    if delivery is None:
        raise RuntimeError("MarketPulse did not durably bind settlement to delivery")
    delivery_run_id, _, status, response_raw, failure_injections = delivery
    if status != "delivered" or delivery_run_id != run_id:
        raise RuntimeError("MarketPulse delivery run does not match the workflow run")
    replay = json.loads(str(response_raw))
    replay_result = replay.get("result", {}) if isinstance(replay, dict) else {}
    replay_content = replay_result.get("structuredContent", {}) if isinstance(replay_result, dict) else {}
    if replay_content.get("intentId") != inbound_intent:
        raise RuntimeError("MarketPulse replay body is not bound to the settled intent")
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
