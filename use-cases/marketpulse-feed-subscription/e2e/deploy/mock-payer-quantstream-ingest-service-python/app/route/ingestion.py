from __future__ import annotations

from fastapi import APIRouter, FastAPI

from app.handler.ingestion import IngestionHandler


class IngestionRouter:
    """Owns only FastAPI route composition."""

    def __init__(self, handler: IngestionHandler | None = None) -> None:
        self._handler = handler or IngestionHandler()

    def register(self, app: FastAPI) -> None:
        router = APIRouter()
        router.add_api_route("/healthz", self._handler.health, methods=["GET"])
        router.add_api_route("/metrics", self._handler.metrics, methods=["GET"])
        app.include_router(router)
