from fastapi import Response


class IngestionHandler:
    """Maps FastAPI requests to the ingestion application boundary."""

    async def health(self) -> dict[str, str]:
        return {"status": "ready", "service": "quantstream-python"}

    async def metrics(self) -> Response:
        return Response(
            content="# HELP quantstream_up QuantStream process readiness.\n"
                    "# TYPE quantstream_up gauge\nquantstream_up 1\n",
            media_type="text/plain; version=0.0.4",
        )
