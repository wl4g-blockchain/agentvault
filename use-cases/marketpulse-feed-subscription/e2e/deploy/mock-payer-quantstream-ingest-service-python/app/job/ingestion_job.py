from __future__ import annotations

import json
import os
import sqlite3
import time
from typing import Any

from apscheduler.schedulers.asyncio import AsyncIOScheduler
from apscheduler.triggers.cron import CronTrigger

from app.agent import MCPHelper, ToolResult


class IngestionJob:
    def __init__(self, mcp: MCPHelper, database: str) -> None:
        self._mcp = mcp
        self._database = database
        if self._database == ":memory:":
            raise RuntimeError("QUANTSTREAM_DATABASE must be persistent")
        self._yesterday_cron = os.environ.get("QUANTSTREAM_YESTERDAY_CRON", "0 2 * * *")
        self._realtime_cron = os.environ.get("QUANTSTREAM_REALTIME_CRON", "*/1 * * * *")
        self._fail_after_settlement = os.environ.get("QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT", "") == "once"

    @classmethod
    async def from_environment(cls) -> "IngestionJob":
        database = _required("QUANTSTREAM_DATABASE")
        if database == ":memory:":
            raise RuntimeError("QUANTSTREAM_DATABASE must be persistent")
        return cls(await MCPHelper.from_environment(), database)

    async def close(self) -> None:
        await self._mcp.close()

    # First periodic function: the public delayed-news tool.
    async def ingest_yesterday_news(self, run_id: str) -> ToolResult:
        return await self._mcp.yesterday_news(run_id)

    # Second periodic function: the paid realtime-news tool.
    async def ingest_realtime_news(self, run_id: str, inject_failure: bool = False) -> ToolResult:
        return await self._mcp.realtime_news(run_id, inject_failure)

    async def run_once(self, sequence: int = 1) -> dict[str, Any]:
        run_id = f"python-{int(time.time())}-{sequence}"
        free = await self.ingest_yesterday_news(run_id)
        paid = await self.ingest_realtime_news(run_id, self._fail_after_settlement and sequence == 1)
        _persist(self._database, run_id, free, paid)
        return {"runId": run_id, "intentId": paid.payment_intent_id,
                "freeStatus": 200, "paidStatus": 200}

    def start(self) -> AsyncIOScheduler:
        scheduler = AsyncIOScheduler()

        async def ingest_yesterday() -> None:
            await self.ingest_yesterday_news(f"python-yesterday-{int(time.time())}")

        async def ingest_realtime() -> None:
            await self.ingest_realtime_news(f"python-realtime-{int(time.time())}")

        scheduler.add_job(ingest_yesterday, CronTrigger.from_crontab(self._yesterday_cron),
                          id="yesterday-news")
        scheduler.add_job(ingest_realtime, CronTrigger.from_crontab(self._realtime_cron),
                          id="realtime-news")
        scheduler.start()
        return scheduler


async def run_once(sequence: int = 1) -> dict[str, Any]:
    job = await IngestionJob.from_environment()
    try:
        return await job.run_once(sequence)
    finally:
        await job.close()


def _persist(path: str, run_id: str, free: ToolResult, paid: ToolResult) -> None:
    if not paid.payment_intent_id:
        raise RuntimeError("paid result omitted payment intent")
    free_json, paid_json = json.loads(free.body), json.loads(paid.body)
    with sqlite3.connect(path) as database:
        database.execute("PRAGMA journal_mode=DELETE")
        database.execute("PRAGMA synchronous=FULL")
        database.execute("""CREATE TABLE IF NOT EXISTS ingestion_run (
          run_id TEXT PRIMARY KEY, free_response_json TEXT NOT NULL, paid_response_json TEXT NOT NULL,
          payment_intent_id TEXT NOT NULL UNIQUE, completed_at INTEGER NOT NULL)""")
        database.execute("INSERT INTO ingestion_run VALUES(?,?,?,?,?)", (
            run_id, json.dumps(free_json, separators=(",", ":")),
            json.dumps(paid_json, separators=(",", ":")), paid.payment_intent_id, int(time.time())))


def _required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"{name} is required")
    return value
