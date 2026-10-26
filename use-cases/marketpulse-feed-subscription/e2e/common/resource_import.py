"""Read and render the canonical AgentVault console-resource YAML fixture."""

from __future__ import annotations

from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import yaml


COLLECTION_BY_KIND = {
    "Client": "clients",
    "Wallet": "wallets",
    "Scenario": "scenarios",
    "Policy": "policies",
    "Price": "prices",
    "Approver": "approvers",
}

_CANONICAL_MARKETPULSE_ORIGIN = "http://127.0.0.1:8081"
_MARKETPULSE_PRICE = "price-realtime-news"


class ConsoleResourceFixture:
    @staticmethod
    def _resource_documents(path: Path) -> list[dict[str, Any]]:
        documents: list[dict[str, Any]] = []
        try:
            parsed_documents = yaml.safe_load_all(path.read_text(encoding="utf-8"))
            for index, document in enumerate(parsed_documents, start=1):
                if document is None:
                    continue
                if not isinstance(document, dict):
                    raise ValueError(f"resource document {index} must be a YAML mapping")
                if set(document) != {"consoleVersion", "kind", "metadata", "data"}:
                    raise ValueError(f"resource document {index} has an invalid envelope")
                if document["consoleVersion"] != "core.agentvault.io/v1":
                    raise ValueError(f"resource document {index} has an unsupported consoleVersion")
                kind = document["kind"]
                if kind not in COLLECTION_BY_KIND:
                    raise ValueError(f"resource document {index} has an unsupported kind: {kind}")
                metadata = document["metadata"]
                data = document["data"]
                if not isinstance(metadata, dict) or set(metadata) != {"name", "status"}:
                    raise ValueError(f"resource document {index} has invalid metadata")
                if not isinstance(data, dict):
                    raise ValueError(f"resource document {index} has invalid data")
                documents.append(document)
        except yaml.YAMLError as error:
            raise ValueError(f"invalid resource YAML: {error}") from error
        if not documents:
            raise ValueError("resource fixture is empty")
        return documents


    @staticmethod
    def _normalized_origin(origin: str) -> str:
        parsed = urlsplit(origin)
        if (
            parsed.scheme not in {"http", "https"}
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.query
            or parsed.fragment
            or parsed.path not in {"", "/"}
        ):
            raise ValueError("MarketPulse origin must be an absolute HTTP(S) origin without credentials or a path")
        return urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))


    @staticmethod
    def render_agentvault_resources(path: Path, marketpulse_origin: str) -> str:
        """Render the canonical import fixture for one runtime origin.

        Environment projection is intentionally limited to the origin-bound Policy
        and Price fields.  All authoritative identifiers and payment constraints
        remain byte-for-byte equivalent to the canonical fixture.
        """
        origin = ConsoleResourceFixture._normalized_origin(marketpulse_origin)
        documents = ConsoleResourceFixture._resource_documents(path)
        policy_count = 0
        price_count = 0
        for document in documents:
            data = document["data"]
            if document["kind"] == "Policy":
                if data.get("allowed_origins") != [_CANONICAL_MARKETPULSE_ORIGIN]:
                    raise ValueError("canonical Policy allowed_origins drifted from the expected E2E origin")
                data["allowed_origins"] = [origin]
                policy_count += 1
            elif document["kind"] == "Price" and document["metadata"]["name"] == _MARKETPULSE_PRICE:
                if data.get("resource") != f"{_CANONICAL_MARKETPULSE_ORIGIN}/mcp":
                    raise ValueError("canonical MarketPulse Price resource drifted from the expected E2E URL")
                data["resource"] = f"{origin}/mcp"
                price_count += 1
        if policy_count == 0 or price_count != 1:
            raise ValueError("canonical fixture must contain Policies and exactly one MarketPulse Price")
        return yaml.safe_dump_all(
            documents,
            allow_unicode=True,
            default_flow_style=False,
            explicit_start=True,
            sort_keys=False,
        )


    @staticmethod
    def load_agentvault_resources(path: Path) -> dict[str, list[dict[str, Any]]]:
        """Load strict resource documents and expose their domain data by kind."""
        collections = {name: [] for name in COLLECTION_BY_KIND.values()}
        for document in ConsoleResourceFixture._resource_documents(path):
            kind = document["kind"]
            collection = COLLECTION_BY_KIND[kind]
            metadata = document["metadata"]
            data = document["data"]
            resource = dict(data)
            if kind != "Approver":
                resource["id"] = metadata["name"]
            resource["enabled"] = metadata["status"] == "active"
            collections[collection].append(resource)
        return collections
