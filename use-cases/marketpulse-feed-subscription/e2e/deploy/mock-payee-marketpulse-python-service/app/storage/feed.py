from __future__ import annotations

import json
import sqlite3
import threading
import time
from dataclasses import dataclass


ACTORS = [
    ("Donald Trump", "United States president"), ("Marco Rubio", "United States secretary of state"),
    ("Ursula von der Leyen", "European Commission president"), ("Keir Starmer", "United Kingdom prime minister"),
    ("Emmanuel Macron", "French president"), ("Friedrich Merz", "German chancellor"),
    ("Shigeru Ishiba", "Japanese prime minister"), ("Narendra Modi", "Indian prime minister"),
    ("Luiz Inacio Lula da Silva", "Brazilian president"), ("Anthony Albanese", "Australian prime minister"),
]
TOPICS = [
    ("tariff", "proposed reciprocal tariff review for strategic imports", "SPY"),
    ("chips", "outlined semiconductor export-control consultations", "SOXX"),
    ("energy", "commented on energy-security supply coordination", "XLE"),
    ("shipping", "addressed Red Sea shipping-risk countermeasures", "BDRY"),
    ("defense", "discussed allied defense procurement commitments", "ITA"),
    ("currency", "responded to cross-border currency volatility", "DXY"),
    ("sanctions", "announced a review of targeted trade sanctions", "EEM"),
    ("agriculture", "set out agricultural import quota negotiations", "DBA"),
    ("critical-minerals", "signed a critical-minerals supply memorandum", "LIT"),
    ("digital-trade", "opened digital-services tax and AI trade talks", "QQQ"),
]


@dataclass(frozen=True)
class DeliveryState:
    intent_id: str = ""
    response: str = ""


class FeedSQLiteStorage:
    def __init__(self, path: str) -> None:
        if not path or path == ":memory:":
            raise ValueError("MARKETPULSE_DATABASE must be persistent")
        self._lock = threading.RLock()
        self._connection = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        self._connection.executescript("""
            PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS feed_item(
              id TEXT PRIMARY KEY,observed_at TEXT NOT NULL,author TEXT NOT NULL,role TEXT NOT NULL,topic TEXT NOT NULL,
              symbols_json TEXT NOT NULL,headline TEXT NOT NULL,source TEXT NOT NULL,sentiment_bps INTEGER NOT NULL,
              access_tier TEXT NOT NULL CHECK(access_tier IN ('yesterday','realtime')));
            CREATE TABLE IF NOT EXISTS mcp_delivery(
              run_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,intent_id TEXT NOT NULL,settlement_json TEXT NOT NULL,
              response_json TEXT,status TEXT NOT NULL CHECK(status IN ('settled','delivered')),
              failure_injections INTEGER NOT NULL DEFAULT 0 CHECK(failure_injections IN (0,1)),updated_at INTEGER NOT NULL);
        """)
        self._seed()

    def close(self) -> None:
        self._connection.close()

    def _seed(self) -> None:
        count = self._connection.execute("SELECT COUNT(*) FROM feed_item").fetchone()[0]
        if count == 100:
            return
        if count != 0:
            raise ValueError(f"MarketPulse feed seed must contain exactly 100 rows, found {count}")
        with self._connection:
            for index in range(100):
                actor, role = ACTORS[index % 10]
                topic, event, symbol = TOPICS[(index // 10) % 10]
                tier = "yesterday" if index < 50 else "realtime"
                day = 14 if index < 50 else 15
                source = "MarketPulse public archive" if index < 50 else "MarketPulse live wire"
                self._connection.execute("INSERT INTO feed_item VALUES(?,?,?,?,?,?,?,?,?,?)", (
                    f"marketpulse-{index:03d}", f"2026-10-{day:02d}T{index % 24:02d}:{index % 60:02d}:00Z",
                    actor, role, topic, json.dumps([symbol]),
                    f"{actor} {event}; markets assess geopolitical and tariff implications",
                    source, (index - 50) * 7, tier,
                ))

    def news(self, tier: str, limit: int) -> list[dict[str, object]]:
        with self._lock:
            rows = self._connection.execute("""SELECT id,observed_at,author,role,topic,symbols_json,
                headline,source,sentiment_bps FROM feed_item WHERE access_tier=? ORDER BY id LIMIT ?""",
                (tier, limit)).fetchall()
        return [{"id": row[0], "observedAt": row[1], "author": row[2], "role": row[3], "topic": row[4],
                 "symbols": json.loads(row[5]), "headline": row[6], "source": row[7], "sentimentBps": row[8]}
                for row in rows]

    def delivery(self, run_id: str, request_hash: str) -> DeliveryState:
        row = self._connection.execute("SELECT request_hash,intent_id,COALESCE(response_json,''),status FROM mcp_delivery WHERE run_id=?",
                                       (run_id,)).fetchone()
        if row is None:
            return DeliveryState()
        if row[0] != request_hash or row[3] not in ("settled", "delivered"):
            raise ValueError("MCP run_id conflicts with durable delivery")
        return DeliveryState(row[1], row[2])

    def record_settled(self, run_id: str, request_hash: str, payment) -> None:
        if payment.intent.run_id != run_id:
            raise ValueError("settlement is not bound to the workflow run")
        intent_id = payment.intent.id
        with self._connection:
            self._connection.execute("""INSERT INTO mcp_delivery
                (run_id,request_hash,intent_id,settlement_json,response_json,status,updated_at)
                VALUES(?,?,?,?,NULL,'settled',?) ON CONFLICT(run_id) DO NOTHING""",
                (run_id, request_hash, intent_id,
                 json.dumps({"intentId": intent_id, "runId": run_id}), int(time.time())))
        if self.delivery(run_id, request_hash).intent_id != intent_id:
            raise ValueError("MCP run_id is already bound to another settlement")

    def record_delivered(self, run_id: str, request_hash: str, response: str) -> None:
        with self._connection:
            cursor = self._connection.execute("""UPDATE mcp_delivery SET response_json=?,status='delivered',updated_at=?
                WHERE run_id=? AND request_hash=? AND status='settled'""",
                (response, int(time.time()), run_id, request_hash))
        if cursor.rowcount != 1:
            raise ValueError("MCP delivery was not in settled state")

    def record_failure(self, run_id: str, request_hash: str) -> bool:
        with self._connection:
            cursor = self._connection.execute("""UPDATE mcp_delivery SET failure_injections=1,updated_at=?
                WHERE run_id=? AND request_hash=? AND status='settled' AND failure_injections=0""",
                (int(time.time()), run_id, request_hash))
        return cursor.rowcount == 1
