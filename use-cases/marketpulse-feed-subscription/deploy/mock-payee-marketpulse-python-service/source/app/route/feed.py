from fastapi import APIRouter, FastAPI

from app.handler.feed import FeedHandler


class FeedRouter:
    def __init__(self, handler: FeedHandler) -> None:
        self.router = APIRouter()
        self.router.add_api_route("/healthz", handler.health, methods=["GET"])
        self.router.add_api_route("/metrics", handler.metrics, methods=["GET"])

    def register(self, application: FastAPI) -> None:
        application.include_router(self.router)
