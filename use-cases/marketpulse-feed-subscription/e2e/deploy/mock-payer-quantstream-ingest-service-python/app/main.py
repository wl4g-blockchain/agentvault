from __future__ import annotations

import argparse
import asyncio
import json
import logging
from contextlib import asynccontextmanager

import uvicorn
from fastapi import FastAPI

from app.job.ingestion_job import IngestionJob, run_once
from app.route import IngestionRouter


@asynccontextmanager
async def lifespan(application: FastAPI):
    ingestion = await IngestionJob.from_environment()
    application.state.ingestion_job = ingestion
    scheduler = ingestion.start()
    try:
        yield
    finally:
        scheduler.shutdown(wait=False)
        await ingestion.close()


app = FastAPI(title="QuantStream Python Ingestor", lifespan=lifespan)
IngestionRouter().register(app)


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(message)s")
    parser = argparse.ArgumentParser(description="QuantStream Python MCP ingestor")
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args()
    if args.once:
        print(json.dumps(asyncio.run(run_once()), separators=(",", ":")), flush=True)
        return
    uvicorn.run(app, host="0.0.0.0", port=8080, access_log=False)


if __name__ == "__main__":
    main()
